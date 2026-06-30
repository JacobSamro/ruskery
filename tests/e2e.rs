//! End-to-end test: boots the real `ruskery` binary against an in-process S3
//! stub and exercises the whole stack over the wire — the OCI push/pull
//! protocol (monolithic, chunked, multipart, empty, and cross-repo-mount
//! uploads; presigned-redirect pulls; tag overwrite; manifest/blob delete),
//! scoped + permission-capped tokens, cross-org and upload-session tenant
//! isolation, client-side network disconnects + retries (transient failure
//! recovery, abrupt disconnect, resumable upload, retry idempotency), the
//! dashboard API (setup, orgs, members, teams, tokens, storage + CDN, Google
//! sign-in, domains, audit), garbage collection, security headers, and rate
//! limiting.
//!
//! No Docker and no external network: the stub stores objects in memory and the
//! binary talks to it over loopback, so the test is fully deterministic.

mod common;

use common::*;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end() {
    let stub = spawn_s3_stub().await;
    let rk = Ruskery::spawn(&stub).await;
    let base = rk.base.clone();

    // Dashboard client keeps the session cookie; registry client follows 307s.
    let dash = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let reg = reqwest::Client::new();

    // ── first-run setup ──────────────────────────────────────────────
    let r = dash
        .post(format!("{base}/api/v1/setup"))
        .json(&json!({
            "email": "admin@example.com", "username": "admin", "password": "supersecret",
            "org_slug": "acme", "org_name": "Acme Inc"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "setup failed");

    let me: serde_json::Value = dash
        .get(format!("{base}/api/v1/auth/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["user"]["username"], "admin");
    assert_eq!(me["orgs"][0]["slug"], "acme");
    assert_eq!(me["orgs"][0]["role"], "owner");

    // Setup is one-shot.
    let needs: serde_json::Value = dash
        .get(format!("{base}/api/v1/setup/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(needs["needs_setup"], false);

    // ── personal access token + registry login ───────────────────────
    let tok: serde_json::Value = dash
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({ "name": "ci" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pat = tok["token"].as_str().unwrap().to_string();
    assert!(pat.starts_with("rsk_"));

    let jwt = registry_token(&reg, &base, "admin", &pat, "repository:acme/app:pull,push").await;

    // /v2/ requires a valid bearer.
    let v2 = reg
        .get(format!("{base}/v2/"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), 200);
    assert_eq!(
        v2.headers().get("docker-distribution-api-version").unwrap(),
        "registry/2.0"
    );
    // Anonymous /v2/ is challenged.
    let anon = reg.get(format!("{base}/v2/")).send().await.unwrap();
    assert_eq!(anon.status(), 401);
    assert!(anon.headers().contains_key("www-authenticate"));

    // ── push blobs: monolithic config/layer + a 10 MiB multipart blob ──
    let layer = b"this-is-a-fake-but-deterministic-layer-payload".to_vec();
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let big = vec![0xABu8; 10 * 1024 * 1024]; // > part size -> exercises multipart

    let layer_d = push_blob(&reg, &base, &jwt, "acme/app", &layer).await;
    let config_d = push_blob(&reg, &base, &jwt, "acme/app", &config).await;
    let big_d = push_blob(&reg, &base, &jwt, "acme/app", &big).await;

    // HEAD existing blob.
    let head = reg
        .head(format!("{base}/v2/acme/app/blobs/{layer_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);

    // GET returns a 307 to the (stub) presigned URL.
    let noredir = no_redirect_client();
    let redirect = noredir
        .get(format!("{base}/v2/acme/app/blobs/{layer_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(redirect.status(), 307);
    let loc = redirect
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        loc.contains(&stub),
        "redirect should target the storage endpoint: {loc}"
    );

    // Following the redirect yields the exact bytes — byte-for-byte, via "CDN".
    let pulled_layer = reg
        .get(format!("{base}/v2/acme/app/blobs/{layer_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(pulled_layer.as_ref(), layer.as_slice());

    // The multipart blob round-trips intact.
    let pulled_big = reg
        .get(format!("{base}/v2/acme/app/blobs/{big_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(pulled_big.len(), big.len());
    assert_eq!(sha256_digest(&pulled_big), big_d);

    // ── manifest push/pull ───────────────────────────────────────────
    let (manifest_d, status) = push_manifest(
        &reg,
        &base,
        &jwt,
        "acme/app",
        &config_d,
        config.len(),
        &layer_d,
        layer.len(),
        "v1",
    )
    .await;
    assert_eq!(status, 201, "manifest push failed");

    let by_tag = reg
        .get(format!("{base}/v2/acme/app/manifests/v1"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(by_tag.status(), 200);
    let by_digest = reg
        .get(format!("{base}/v2/acme/app/manifests/{manifest_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(by_digest.status(), 200);

    // A manifest referencing a missing blob is rejected.
    let bad = reg
        .put(format!("{base}/v2/acme/app/manifests/bad"))
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .bearer_auth(&jwt)
        .body(
            json!({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": {"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:dead","size":1},
                "layers": []
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    // A non-JSON manifest body is rejected outright.
    let not_json = reg
        .put(format!("{base}/v2/acme/app/manifests/junk"))
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .bearer_auth(&jwt)
        .body("this is not json")
        .send()
        .await
        .unwrap();
    assert_eq!(not_json.status(), 400, "non-JSON manifest must be rejected");

    // tags + catalog.
    let tags: serde_json::Value = reg
        .get(format!("{base}/v2/acme/app/tags/list"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tags["tags"][0], "v1");

    let catalog: serde_json::Value = reg
        .get(format!("{base}/v2/_catalog"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(catalog["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "acme/app"));

    // ── scoped personal access tokens ────────────────────────────────
    // A token scoped to acme/app reaches it, but nothing else — even though its
    // owner (admin) has full access to the whole org.
    let scoped_pat = dash
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({ "name": "repo-scoped", "org": "acme", "repo": "app" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    // Allowed on the scoped repo.
    let app_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &scoped_pat,
        "repository:acme/app:pull,push",
    )
    .await;
    assert_eq!(
        reg.get(format!("{base}/v2/acme/app/manifests/v1"))
            .bearer_auth(&app_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "repo-scoped token must reach its repo"
    );

    // Denied on a different repo (token scope excludes it; can't even create it).
    let other_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &scoped_pat,
        "repository:acme/other:pull,push",
    )
    .await;
    assert_eq!(
        reg.post(format!("{base}/v2/acme/other/blobs/uploads/"))
            .bearer_auth(&other_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "repo-scoped token must be denied on other repos"
    );

    // The token list surfaces the scope.
    let token_list: serde_json::Value = dash
        .get(format!("{base}/api/v1/tokens"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(token_list["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["scope"] == "acme/app"));

    // ── permission-capped tokens ─────────────────────────────────────
    // A pull-capped token can read but not write, even though its owner can push.
    let pull_pat = dash
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({ "name": "pull-only", "permission": "pull" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let pull_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pull_pat,
        "repository:acme/app:pull,push",
    )
    .await;
    assert_eq!(
        reg.get(format!("{base}/v2/acme/app/manifests/v1"))
            .bearer_auth(&pull_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "pull-capped token may read"
    );
    assert_eq!(
        reg.post(format!("{base}/v2/acme/app/blobs/uploads/"))
            .bearer_auth(&pull_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "pull-capped token must not be able to push"
    );

    // ── storage settings + CDN pull host ─────────────────────────────
    // Reads back the env-configured backend, then points the CDN URL at the
    // same stub via a different hostname; pull redirects must now target it.
    let st: serde_json::Value = dash
        .get(format!("{base}/api/v1/settings/storage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["bucket"], "test");
    assert_eq!(
        st["secret_set"], true,
        "secret must never be returned but be marked set"
    );

    // 127.0.0.1 and localhost reach the same stub but are distinct host strings.
    let cdn = format!("http://{}", stub.replace("127.0.0.1", "localhost"));
    let put = dash
        .put(format!("{base}/api/v1/settings/storage"))
        .json(&json!({ "cdn_url": cdn }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200, "storage update should apply live");

    // The pull redirect now points at the CDN host...
    let redir = no_redirect_client()
        .get(format!("{base}/v2/acme/app/blobs/{layer_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(redir.status(), 307);
    let loc = redir.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        loc.contains("localhost"),
        "pull should redirect to the CDN host: {loc}"
    );

    // ...and the bytes still come back correctly through it.
    let via_cdn = reg
        .get(format!("{base}/v2/acme/app/blobs/{layer_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        via_cdn.as_ref(),
        layer.as_slice(),
        "pull via CDN host must match"
    );

    // ── Google sign-in configuration + flow entrypoints ──────────────
    let prov: serde_json::Value = reg
        .get(format!("{base}/api/v1/auth/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prov["google"], false, "google off until configured");

    let oauth_put = dash
        .put(format!("{base}/api/v1/settings/oauth"))
        .json(&json!({
            "enabled": true,
            "client_id": "test-client.apps.googleusercontent.com",
            "client_secret": "test-secret",
            "allowed_domain": "example.com"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(oauth_put.status(), 200);

    let oauth_get: serde_json::Value = dash
        .get(format!("{base}/api/v1/settings/oauth"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(oauth_get["enabled"], true);
    assert_eq!(
        oauth_get["secret_set"], true,
        "secret stored but not returned"
    );
    assert!(
        oauth_get["client_secret"].is_null(),
        "secret must never be returned"
    );
    assert!(oauth_get["redirect_uri"]
        .as_str()
        .unwrap()
        .ends_with("/api/v1/auth/google/callback"));

    // Provider now advertised on the login page.
    let prov2: serde_json::Value = reg
        .get(format!("{base}/api/v1/auth/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prov2["google"], true);

    // /login starts the dance: 303 to Google with a CSRF state cookie.
    let start = no_redirect_client()
        .get(format!("{base}/api/v1/auth/google/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 303);
    let goto = start.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        goto.contains("accounts.google.com"),
        "redirects to Google: {goto}"
    );
    assert!(goto.contains("test-client.apps.googleusercontent.com"));
    assert!(start
        .headers()
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap_or("").contains("ruskery_oauth_state")));

    // Callback with a forged CSRF state bounces back to the login page.
    let cb = no_redirect_client()
        .get(format!(
            "{base}/api/v1/auth/google/callback?code=x&state=forged"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(cb.status(), 303);
    assert!(cb
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("/login"));

    // ── dashboard: users, members, teams ─────────────────────────────
    let mk_user = dash
        .post(format!("{base}/api/v1/users"))
        .json(&json!({"email":"dev@example.com","username":"dev","password":"devpassword"}))
        .send()
        .await
        .unwrap();
    assert_eq!(mk_user.status(), 200);

    let add_member = dash
        .post(format!("{base}/api/v1/orgs/acme/members"))
        .json(&json!({"login":"dev","role":"member"}))
        .send()
        .await
        .unwrap();
    assert_eq!(add_member.status(), 200);

    let members: serde_json::Value = dash
        .get(format!("{base}/api/v1/orgs/acme/members"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(members["members"].as_array().unwrap().len(), 2);

    let mk_team = dash
        .post(format!("{base}/api/v1/orgs/acme/teams"))
        .json(&json!({"slug":"backend","name":"Backend"}))
        .send()
        .await
        .unwrap();
    assert_eq!(mk_team.status(), 200);

    // A member with no grants gets an empty registry scope (RBAC isolation).
    let dev_pat_login = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    dev_pat_login
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({"login":"dev","password":"devpassword"}))
        .send()
        .await
        .unwrap();
    let dev_tok = dev_pat_login
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({"name":"dev"}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let dev_jwt = registry_token(
        &reg,
        &base,
        "dev",
        &dev_tok,
        "repository:acme/app:pull,push",
    )
    .await;
    // dev has no team grant on acme/app -> pull is denied.
    let denied = reg
        .get(format!("{base}/v2/acme/app/manifests/v1"))
        .bearer_auth(&dev_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401, "member without grant must be denied");

    // ── domains / TLS management ──────────────────────────────────────
    // A contact email is mandatory before any domain can be added.
    let add_no_email = dash
        .post(format!("{base}/api/v1/domains"))
        .json(&json!({"domain":"registry.example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        add_no_email.status(),
        400,
        "adding a domain without a contact email must be rejected"
    );

    // ── ACME contact email is settable from the dashboard ─────────────
    // Invalid address is rejected.
    let bad_email = dash
        .put(format!("{base}/api/v1/settings/tls"))
        .json(&json!({"contact_email":"not-an-email"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        bad_email.status(),
        400,
        "invalid contact email must be rejected"
    );
    // A valid address persists and is reflected by both endpoints.
    let set_email = dash
        .put(format!("{base}/api/v1/settings/tls"))
        .json(&json!({"contact_email":"ops@example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(set_email.status(), 200);
    let tls_settings: serde_json::Value = dash
        .get(format!("{base}/api/v1/settings/tls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tls_settings["contact_email"], "ops@example.com");

    // Now the domain can be added.
    let add_domain = dash
        .post(format!("{base}/api/v1/domains"))
        .json(&json!({"domain":"registry.example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(add_domain.status(), 200);
    let domains: serde_json::Value = dash
        .get(format!("{base}/api/v1/domains"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(domains["domains"][0]["domain"], "registry.example.com");
    assert_eq!(domains["domains"][0]["status"], "pending");

    let domains2: serde_json::Value = dash
        .get(format!("{base}/api/v1/domains"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(domains2["contact_email"], "ops@example.com");

    // ── audit log records the push ────────────────────────────────────
    let audit: serde_json::Value = dash
        .get(format!("{base}/api/v1/orgs/acme/audit"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["action"] == "image.push"));

    // ── security headers ──────────────────────────────────────────────
    let root = dash.get(format!("{base}/")).send().await.unwrap();
    let h = root.headers();
    assert!(h.contains_key("content-security-policy"));
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");

    // The dashboard CSP must whitelist its own inline bootstrap script by hash,
    // or the SPA fails to boot (Nuxt's `window.__NUXT__` script is blocked under
    // a bare `script-src 'self'`). This is a regression guard for that outage.
    {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let csp = h
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("script-src 'self'"));
        assert!(
            !csp.contains("'unsafe-inline'; script") || csp.contains("script-src 'self' 'sha256-"),
            "script-src must not rely on 'unsafe-inline'"
        );
        let body = root.text().await.unwrap();
        // Every executable inline script in the served HTML must be covered by a
        // matching 'sha256-…' token in the CSP.
        let mut checked = 0;
        let mut rest = body.as_str();
        while let Some(s) = rest.find("<script") {
            let after = &rest[s..];
            let gt = after.find('>').unwrap();
            let open = &after[..gt];
            let bstart = s + gt + 1;
            let end = rest[bstart..].find("</script>").unwrap();
            let inner = &rest[bstart..bstart + end];
            rest = &rest[bstart + end + "</script>".len()..];
            let is_data = open.contains("type=") && open.contains("application/json");
            if open.contains("src=") || is_data || inner.is_empty() {
                continue;
            }
            let tok = format!(
                "sha256-{}",
                base64::engine::general_purpose::STANDARD.encode(Sha256::digest(inner.as_bytes()))
            );
            assert!(
                csp.contains(&tok),
                "inline script not whitelisted in CSP: {tok}\nscript: {inner}"
            );
            checked += 1;
        }
        assert!(
            checked >= 1,
            "expected at least one inline script in dashboard HTML"
        );
    }

    // ── cross-repo blob mount ────────────────────────────────────────
    // The layer already exists in this org, so mounting it into another repo
    // is instant (201) without re-uploading.
    let mount_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/mounted:pull,push",
    )
    .await;
    let mount = reg
        .post(format!("{base}/v2/acme/mounted/blobs/uploads/"))
        .query(&[("mount", layer_d.as_str()), ("from", "acme/app")])
        .bearer_auth(&mount_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(mount.status(), 201, "cross-repo mount should be instant");
    assert_eq!(
        reg.head(format!("{base}/v2/acme/mounted/blobs/{layer_d}"))
            .bearer_auth(&mount_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // ── chunked upload (multiple PATCHes) ────────────────────────────
    let chunked_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/chunked:pull,push",
    )
    .await;
    let chunked_d = push_blob_chunked(
        &reg,
        &base,
        &chunked_jwt,
        "acme/chunked",
        &[b"chunk-one-", b"chunk-two-", b"chunk-three"],
    )
    .await;
    let chunked_bytes = reg
        .get(format!("{base}/v2/acme/chunked/blobs/{chunked_d}"))
        .bearer_auth(&chunked_jwt)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(chunked_bytes.as_ref(), b"chunk-one-chunk-two-chunk-three");

    // ── empty (zero-byte) blob ───────────────────────────────────────
    let empty_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/empty:pull,push",
    )
    .await;
    let empty_d = push_blob(&reg, &base, &empty_jwt, "acme/empty", b"").await;
    assert_eq!(
        empty_d,
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        reg.get(format!("{base}/v2/acme/empty/blobs/{empty_d}"))
            .bearer_auth(&empty_jwt)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .len(),
        0
    );

    // ── tag overwrite (re-point a tag to a new manifest) ─────────────
    // Manifest A = config + layer; B = config only → different digest.
    let (dig_a, _) = push_manifest(
        &reg,
        &base,
        &jwt,
        "acme/app",
        &config_d,
        config.len(),
        &layer_d,
        layer.len(),
        "rolling",
    )
    .await;
    let manifest_b = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {"mediaType":"application/vnd.oci.image.config.v1+json","digest": config_d, "size": config.len()},
        "layers": []
    })
    .to_string();
    let dig_b = sha256_digest(manifest_b.as_bytes());
    assert_ne!(dig_a, dig_b);
    reg.put(format!("{base}/v2/acme/app/manifests/rolling"))
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .bearer_auth(&jwt)
        .body(manifest_b)
        .send()
        .await
        .unwrap();
    let rolling_digest = reg
        .get(format!("{base}/v2/acme/app/manifests/rolling"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .headers()
        .get("docker-content-digest")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        rolling_digest, dig_b,
        "tag must now point to the new manifest"
    );

    // ── manifest + blob delete lifecycle ─────────────────────────────
    // Use a throwaway repo + a unique blob so deletes don't touch shared data.
    let trash_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/trash:pull,push",
    )
    .await;
    let unique = b"unique-blob-for-the-trash-repo".to_vec();
    let unique_d = push_blob(&reg, &base, &trash_jwt, "acme/trash", &unique).await;
    let (trash_manifest_d, st) = push_manifest(
        &reg,
        &base,
        &trash_jwt,
        "acme/trash",
        &config_d,
        config.len(),
        &unique_d,
        unique.len(),
        "t",
    )
    .await;
    assert_eq!(st, 201);
    // `delete` is only granted once the repo exists (admin owner ⇒ admin), so
    // request the delete-capable token after the push created the repo.
    let trash_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/trash:pull,push,delete",
    )
    .await;
    // Delete the manifest -> tag no longer resolves.
    assert_eq!(
        reg.delete(format!("{base}/v2/acme/trash/manifests/{trash_manifest_d}"))
            .bearer_auth(&trash_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert_eq!(
        reg.get(format!("{base}/v2/acme/trash/manifests/t"))
            .bearer_auth(&trash_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        404,
        "deleted manifest's tag must 404"
    );
    // Delete the (now unreferenced) unique blob.
    assert_eq!(
        reg.delete(format!("{base}/v2/acme/trash/blobs/{unique_d}"))
            .bearer_auth(&trash_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert_eq!(
        reg.head(format!("{base}/v2/acme/trash/blobs/{unique_d}"))
            .bearer_auth(&trash_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    // ── cross-org isolation ──────────────────────────────────────────
    // A second user owns their own org; neither admin nor the outsider can
    // reach the other's repositories.
    dash.post(format!("{base}/api/v1/users"))
        .json(
            &json!({"email":"outsider@example.com","username":"outsider","password":"outsiderpw"}),
        )
        .send()
        .await
        .unwrap();
    let outsider = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    outsider
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({"login":"outsider","password":"outsiderpw"}))
        .send()
        .await
        .unwrap();
    outsider
        .post(format!("{base}/api/v1/orgs"))
        .json(&json!({"slug":"rival","name":"Rival"}))
        .send()
        .await
        .unwrap();
    let outsider_pat = outsider
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({"name":"o"}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    // admin is not a member of 'rival' -> no push there.
    let admin_on_rival =
        registry_token(&reg, &base, "admin", &pat, "repository:rival/app:pull,push").await;
    assert_eq!(
        reg.post(format!("{base}/v2/rival/app/blobs/uploads/"))
            .bearer_auth(&admin_on_rival)
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "admin must not push into another org"
    );
    // outsider is not a member of 'acme' -> no read there.
    let outsider_on_acme = registry_token(
        &reg,
        &base,
        "outsider",
        &outsider_pat,
        "repository:acme/app:pull",
    )
    .await;
    assert_eq!(
        reg.get(format!("{base}/v2/acme/app/manifests/v1"))
            .bearer_auth(&outsider_on_acme)
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "outsider must not read another org"
    );

    // ── super-admin: list every org on the instance ──────────────────
    let all_orgs: serde_json::Value = dash
        .get(format!("{base}/api/v1/admin/orgs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slugs: Vec<&str> = all_orgs["orgs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["slug"].as_str().unwrap())
        .collect();
    assert!(
        slugs.contains(&"acme") && slugs.contains(&"rival"),
        "admin must see all orgs, got {slugs:?}"
    );
    let acme_summary = all_orgs["orgs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["slug"] == "acme")
        .unwrap();
    assert!(acme_summary["member_count"].as_i64().unwrap() >= 1);
    assert!(acme_summary["repo_count"].as_i64().unwrap() >= 1);
    // A non-admin cannot enumerate every org.
    assert_eq!(
        outsider
            .get(format!("{base}/api/v1/admin/orgs"))
            .send()
            .await
            .unwrap()
            .status(),
        403,
        "non-admin must not list all orgs"
    );

    // ── upload-session tenant isolation ──────────────────────────────
    // An upload started in one org can't be driven from another org's path,
    // even by an admin authorized on both (verifies the session org check).
    dash.post(format!("{base}/api/v1/orgs"))
        .json(&json!({"slug":"team2","name":"Team Two"}))
        .send()
        .await
        .unwrap();
    let acme_up_jwt =
        registry_token(&reg, &base, "admin", &pat, "repository:acme/iso:pull,push").await;
    let team2_jwt =
        registry_token(&reg, &base, "admin", &pat, "repository:team2/iso:pull,push").await;
    let start = reg
        .post(format!("{base}/v2/acme/iso/blobs/uploads/"))
        .bearer_auth(&acme_up_jwt)
        .send()
        .await
        .unwrap();
    let leaked_uuid = start
        .headers()
        .get("docker-upload-uuid")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        reg.patch(format!("{base}/v2/team2/iso/blobs/uploads/{leaked_uuid}"))
            .bearer_auth(&team2_jwt)
            .body("x")
            .send()
            .await
            .unwrap()
            .status(),
        404,
        "an upload session must not be drivable from another org"
    );

    // ── client-side network disconnects + retries ───────────────────
    // 1) A transient connection failure (first hit lands on a dead port) is
    //    recovered by a client retry — exactly how docker behaves on a flaky
    //    network.
    let dead_port = free_port(); // nothing is listening here
    let mut attempts = 0u32;
    let recovered = loop {
        attempts += 1;
        let url = if attempts == 1 {
            format!("http://127.0.0.1:{dead_port}/v2/acme/app/blobs/{layer_d}")
        } else {
            format!("{base}/v2/acme/app/blobs/{layer_d}")
        };
        match reg.get(&url).bearer_auth(&jwt).send().await {
            Ok(resp) => break resp.bytes().await.unwrap(),
            Err(_) if attempts < 5 => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("retry never recovered: {e}"),
        }
    };
    assert_eq!(attempts, 2, "first attempt fails, the retry succeeds");
    assert_eq!(recovered.as_ref(), layer.as_slice());

    // 2) An abrupt mid-request disconnect must not wedge the server.
    {
        use std::io::Write;
        let addr = base.strip_prefix("http://").unwrap();
        let mut sock = std::net::TcpStream::connect(addr).unwrap();
        // Write an incomplete request, then drop the socket (connection reset).
        let _ = sock.write_all(b"GET /v2/ HTTP/1.1\r\nHost: localhost\r\n");
    }
    assert!(
        reg.get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success(),
        "server must survive an abrupt client disconnect"
    );
    // ...and a fresh upload still works after the disconnect.
    let disc_jwt =
        registry_token(&reg, &base, "admin", &pat, "repository:acme/disc:pull,push").await;
    let disc_d = push_blob(
        &reg,
        &base,
        &disc_jwt,
        "acme/disc",
        b"recovered-after-disconnect",
    )
    .await;
    assert_eq!(
        reg.head(format!("{base}/v2/acme/disc/blobs/{disc_d}"))
            .bearer_auth(&disc_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // 3) Resumable upload: PATCH a chunk, "reconnect" and query the upload
    //    status (Range), then resume and finalize.
    let res_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/resume:pull,push",
    )
    .await;
    let p1: &[u8] = b"resume-part-1-";
    let p2: &[u8] = b"resume-part-2-end";
    let mut full = p1.to_vec();
    full.extend_from_slice(p2);
    let res_digest = sha256_digest(&full);
    let start = reg
        .post(format!("{base}/v2/acme/resume/blobs/uploads/"))
        .bearer_auth(&res_jwt)
        .send()
        .await
        .unwrap();
    let up = start
        .headers()
        .get("docker-upload-uuid")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    reg.patch(format!("{base}/v2/acme/resume/blobs/uploads/{up}"))
        .bearer_auth(&res_jwt)
        .body(p1.to_vec())
        .send()
        .await
        .unwrap();
    // Simulated reconnect: ask where we left off.
    let status = reg
        .get(format!("{base}/v2/acme/resume/blobs/uploads/{up}"))
        .bearer_auth(&res_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), 202);
    assert_eq!(
        status.headers().get("range").unwrap().to_str().unwrap(),
        format!("0-{}", p1.len() - 1),
        "status reports bytes received so far"
    );
    // Resume + finalize.
    reg.patch(format!("{base}/v2/acme/resume/blobs/uploads/{up}"))
        .bearer_auth(&res_jwt)
        .body(p2.to_vec())
        .send()
        .await
        .unwrap();
    let fin = reg
        .put(format!("{base}/v2/acme/resume/blobs/uploads/{up}"))
        .query(&[("digest", &res_digest)])
        .bearer_auth(&res_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(fin.status(), 201, "resumed upload finalizes");
    let resumed = reg
        .get(format!("{base}/v2/acme/resume/blobs/{res_digest}"))
        .bearer_auth(&res_jwt)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(resumed.as_ref(), full.as_slice());

    // 4) Retried requests are idempotent (the lost-response case): pushing the
    //    same blob or manifest twice is safe and consistent.
    let idem1 = push_blob(&reg, &base, &jwt, "acme/app", b"idempotent-blob").await;
    let idem2 = push_blob(&reg, &base, &jwt, "acme/app", b"idempotent-blob").await;
    assert_eq!(
        idem1, idem2,
        "re-pushed blob is content-addressed, no duplicate"
    );
    let (im1, is1) = push_manifest(
        &reg,
        &base,
        &jwt,
        "acme/app",
        &config_d,
        config.len(),
        &layer_d,
        layer.len(),
        "idem",
    )
    .await;
    let (im2, is2) = push_manifest(
        &reg,
        &base,
        &jwt,
        "acme/app",
        &config_d,
        config.len(),
        &layer_d,
        layer.len(),
        "idem",
    )
    .await;
    assert_eq!(is1, 201);
    assert_eq!(is2, 201);
    assert_eq!(im1, im2, "re-pushed manifest is idempotent");

    // ── garbage collection ────────────────────────────────────────────
    // Push a blob that no manifest references, sweep, and confirm only the
    // orphan is removed while the referenced layer survives.
    let orphan = b"unreferenced-orphan-blob".to_vec();
    let orphan_d = push_blob(&reg, &base, &jwt, "acme/app", &orphan).await;
    assert_eq!(
        reg.head(format!("{base}/v2/acme/app/blobs/{orphan_d}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let gc_out = rk.run_gc();
    assert!(gc_out.contains("garbage collected"), "gc output: {gc_out}");

    assert_eq!(
        reg.head(format!("{base}/v2/acme/app/blobs/{orphan_d}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        404,
        "orphan blob should be collected"
    );
    assert_eq!(
        reg.head(format!("{base}/v2/acme/app/blobs/{layer_d}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "referenced layer must survive GC"
    );

    // ── auth rate limiting ────────────────────────────────────────────
    let mut limited = false;
    for _ in 0..40 {
        let resp = reqwest::Client::new()
            .post(format!("{base}/api/v1/auth/login"))
            .json(&json!({"login":"nobody","password":"nope"}))
            .send()
            .await
            .unwrap();
        if resp.status() == 429 {
            limited = true;
            break;
        }
    }
    assert!(limited, "auth endpoint should rate-limit a burst");
}
