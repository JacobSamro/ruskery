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

    // ── multi-arch image index ───────────────────────────────────────
    // Two distinct child manifests, then an index that references them.
    let (child1, s1) = push_manifest(
        &reg,
        &base,
        &jwt,
        "acme/app",
        &config_d,
        config.len(),
        &layer_d,
        layer.len(),
        "arch1",
    )
    .await;
    let (child2, s2) = push_manifest(
        &reg,
        &base,
        &jwt,
        "acme/app",
        &config_d,
        config.len(),
        &big_d,
        big.len(),
        "arch2",
    )
    .await;
    assert_eq!(s1, 201);
    assert_eq!(s2, 201);
    assert_ne!(child1, child2, "children must be distinct manifests");

    let index = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [
            {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest": child1, "size": 100,
             "platform": {"architecture":"amd64","os":"linux"}},
            {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest": child2, "size": 100,
             "platform": {"architecture":"arm64","os":"linux"}}
        ]
    })
    .to_string();
    let put_idx = reg
        .put(format!("{base}/v2/acme/app/manifests/multi"))
        .header("content-type", "application/vnd.oci.image.index.v1+json")
        .bearer_auth(&jwt)
        .body(index)
        .send()
        .await
        .unwrap();
    assert_eq!(put_idx.status(), 201, "index push");

    let got_idx = reg
        .get(format!("{base}/v2/acme/app/manifests/multi"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(got_idx.status(), 200);
    assert_eq!(
        got_idx.headers().get("content-type").unwrap(),
        "application/vnd.oci.image.index.v1+json"
    );
    // Both children resolve by digest (a multi-arch pull then fetches the right one).
    for child in [&child1, &child2] {
        assert_eq!(
            reg.get(format!("{base}/v2/acme/app/manifests/{child}"))
                .bearer_auth(&jwt)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }

    // ── tag listing pagination (?n= + Link, then ?last=) ─────────────
    // Four tags now exist (arch1, arch2, multi, v1), lexically ordered.
    let page1 = reg
        .get(format!("{base}/v2/acme/app/tags/list?n=2"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert!(
        page1.headers().get("link").is_some(),
        "first page must advertise a next-page Link"
    );
    let p1: serde_json::Value = page1.json().await.unwrap();
    assert_eq!(p1["tags"].as_array().unwrap().len(), 2);
    let last = p1["tags"][1].as_str().unwrap().to_string();
    let p2: serde_json::Value = reg
        .get(format!("{base}/v2/acme/app/tags/list?n=2&last={last}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        p2["tags"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.as_str().unwrap() > last.as_str()),
        "second page must continue strictly after `last`"
    );

    // ── referrers (OCI 1.1) ──────────────────────────────────────────
    let referrer = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "artifactType": "application/vnd.example.sbom",
        "config": {"mediaType":"application/vnd.oci.image.config.v1+json","digest": config_d, "size": config.len()},
        "layers": [{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest": layer_d, "size": layer.len()}],
        "subject": {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest": manifest_d, "size": 100},
        "annotations": {"org.example.tool": "scanner-1.0"}
    })
    .to_string();
    let referrer_digest = sha256_digest(referrer.as_bytes());
    let put_ref = reg
        .put(format!("{base}/v2/acme/app/manifests/{referrer_digest}"))
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .bearer_auth(&jwt)
        .body(referrer)
        .send()
        .await
        .unwrap();
    assert_eq!(put_ref.status(), 201, "referrer push");
    assert_eq!(
        put_ref.headers().get("oci-subject").unwrap(),
        manifest_d.as_str(),
        "OCI-Subject header echoes the subject"
    );

    let refs: serde_json::Value = reg
        .get(format!("{base}/v2/acme/app/referrers/{manifest_d}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refs["mediaType"], "application/vnd.oci.image.index.v1+json");
    let referrer_entry = refs["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["digest"] == referrer_digest)
        .unwrap_or_else(|| panic!("referrer must be listed: {refs}"));
    assert_eq!(
        referrer_entry["artifactType"],
        "application/vnd.example.sbom"
    );
    // The descriptor carries the referrer's annotations (OCI).
    assert_eq!(
        referrer_entry["annotations"]["org.example.tool"], "scanner-1.0",
        "referrer descriptor must include annotations"
    );
    // A syntactically invalid subject digest is rejected.
    assert_eq!(
        reg.get(format!("{base}/v2/acme/app/referrers/not-a-digest"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        400,
        "invalid referrers digest must be rejected"
    );

    // artifactType filter narrows the list and sets OCI-Filters-Applied.
    let filtered = reg
        .get(format!(
            "{base}/v2/acme/app/referrers/{manifest_d}?artifactType=application/vnd.example.sbom"
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(
        filtered.headers().get("oci-filters-applied").unwrap(),
        "artifactType"
    );
    let fbody: serde_json::Value = filtered.json().await.unwrap();
    assert_eq!(fbody["manifests"].as_array().unwrap().len(), 1);
    // A non-matching filter yields an empty index.
    let empty: serde_json::Value = reg
        .get(format!(
            "{base}/v2/acme/app/referrers/{manifest_d}?artifactType=application/vnd.none"
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty["manifests"].as_array().unwrap().len(), 0);

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
        403,
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
        403,
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
    assert_eq!(denied.status(), 403, "member without grant must be denied");

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
        403,
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
        403,
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
    // Simulated reconnect: ask where we left off (OCI: 204 No Content + Range).
    let status = reg
        .get(format!("{base}/v2/acme/resume/blobs/uploads/{up}"))
        .bearer_auth(&res_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), 204);
    assert_eq!(
        status.headers().get("range").unwrap().to_str().unwrap(),
        format!("0-{}", p1.len() - 1),
        "status reports bytes received so far"
    );
    // An out-of-order chunk (Content-Range start ≠ current offset) is rejected.
    let bad_range = reg
        .patch(format!("{base}/v2/acme/resume/blobs/uploads/{up}"))
        .header("content-range", "999-1100")
        .bearer_auth(&res_jwt)
        .body(p2.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        bad_range.status(),
        416,
        "out-of-order chunk must be rejected"
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

    // ── usage analytics ──────────────────────────────────────────────
    // All the pushes, manifest GETs and 307 blob pulls above are captured in
    // memory; wait for a rollup flush (RUSKERY_ANALYTICS__ROLLUP_SECS=1).
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let an: serde_json::Value = dash
        .get(format!("{base}/api/v1/orgs/acme/analytics?range=30d"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        an["overview"]["pushes"].as_i64().unwrap() >= 1,
        "expected pushes recorded: {an}"
    );
    assert!(
        an["overview"]["pulls"].as_i64().unwrap() >= 1,
        "expected pulls recorded: {an}"
    );
    assert!(
        an["overview"]["storage_bytes"].as_i64().unwrap() > 0,
        "expected storage bytes: {an}"
    );
    assert!(
        an["top_repos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["repo"] == "app"),
        "acme/app should appear in top repos: {an}"
    );
    // Tenant isolation: the outsider (admin of 'rival' only) sees no acme data.
    let rival_an: serde_json::Value = outsider
        .get(format!("{base}/api/v1/orgs/rival/analytics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        rival_an["overview"]["pulls"].as_i64().unwrap(),
        0,
        "rival org must not see acme pulls"
    );

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

/// Full multi-arch (image index) lifecycle: by-digest child manifests, an index,
/// the tag→index→child→blob pull path, and GC reference-counting through the
/// index (blobs survive while children exist, and are collected once removed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_arch_lifecycle() {
    let stub = spawn_s3_stub().await;
    let rk = Ruskery::spawn(&stub).await;
    let base = rk.base.clone();
    let dash = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let reg = reqwest::Client::new();
    let noredir = no_redirect_client();

    dash.post(format!("{base}/api/v1/setup"))
        .json(&json!({
            "email":"admin@example.com","username":"admin","password":"supersecret",
            "org_slug":"acme","org_name":"Acme Inc"
        }))
        .send()
        .await
        .unwrap();
    let pat = dash
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({ "name": "ci" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let jwt = registry_token(&reg, &base, "admin", &pat, "repository:acme/img:pull,push").await;

    // Distinct blobs reachable ONLY through the index's children.
    let config = br#"{"architecture":"multi","os":"linux"}"#.to_vec();
    let layer_amd = b"amd64-layer-bytes".to_vec();
    let layer_arm = b"arm64-layer-bytes".to_vec();
    let config_d = push_blob(&reg, &base, &jwt, "acme/img", &config).await;
    let amd_d = push_blob(&reg, &base, &jwt, "acme/img", &layer_amd).await;
    let arm_d = push_blob(&reg, &base, &jwt, "acme/img", &layer_arm).await;

    // Per-arch child manifests, pushed by digest (the buildx flow: only the
    // index gets a tag).
    let child_body = |layer_d: &str, layer_len: usize| {
        json!({
            "schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
            "config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":config_d,"size":config.len()},
            "layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":layer_d,"size":layer_len}]
        })
        .to_string()
    };
    let amd_body = child_body(&amd_d, layer_amd.len());
    let arm_body = child_body(&arm_d, layer_arm.len());
    let (amd_child, s1) = push_manifest_raw(
        &reg,
        &base,
        &jwt,
        "acme/img",
        "application/vnd.oci.image.manifest.v1+json",
        &amd_body,
    )
    .await;
    let (arm_child, s2) = push_manifest_raw(
        &reg,
        &base,
        &jwt,
        "acme/img",
        "application/vnd.oci.image.manifest.v1+json",
        &arm_body,
    )
    .await;
    assert_eq!(s1, 201);
    assert_eq!(s2, 201);

    // The index, tagged "v1".
    let index_body = json!({
        "schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json",
        "manifests":[
            {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":amd_child,"size":amd_body.len(),
             "platform":{"architecture":"amd64","os":"linux"}},
            {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":arm_child,"size":arm_body.len(),
             "platform":{"architecture":"arm64","os":"linux"}}
        ]
    })
    .to_string();
    let put_idx = reg
        .put(format!("{base}/v2/acme/img/manifests/v1"))
        .header("content-type", "application/vnd.oci.image.index.v1+json")
        .bearer_auth(&jwt)
        .body(index_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(put_idx.status(), 201);
    let index_digest = put_idx
        .headers()
        .get("docker-content-digest")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // Pull the index by tag and by digest; HEAD; byte-exact; digest header.
    let by_tag = reg
        .get(format!("{base}/v2/acme/img/manifests/v1"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(by_tag.status(), 200);
    assert_eq!(
        by_tag.headers().get("content-type").unwrap(),
        "application/vnd.oci.image.index.v1+json"
    );
    assert_eq!(
        by_tag.headers().get("docker-content-digest").unwrap(),
        index_digest.as_str()
    );
    let body_text = by_tag.text().await.unwrap();
    assert_eq!(body_text, index_body, "index round-trips byte-for-byte");
    assert_eq!(
        reg.head(format!("{base}/v2/acme/img/manifests/{index_digest}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // Full multi-arch pull: tag → index → amd64 child → child manifest → layer 307.
    let idx: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    let picked = idx["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["platform"]["architecture"] == "amd64")
        .unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(picked, amd_child);
    let child: serde_json::Value = reg
        .get(format!("{base}/v2/acme/img/manifests/{picked}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let layer_digest = child["layers"][0]["digest"].as_str().unwrap().to_string();
    assert_eq!(layer_digest, amd_d);
    assert_eq!(
        noredir
            .get(format!("{base}/v2/acme/img/blobs/{layer_digest}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        307
    );
    let pulled = reg
        .get(format!("{base}/v2/acme/img/blobs/{layer_digest}"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(pulled.as_ref(), layer_amd.as_slice());

    // GC keeps every blob — they're referenced via the children's blob records.
    rk.run_gc();
    for d in [&config_d, &amd_d, &arm_d] {
        assert_eq!(
            reg.head(format!("{base}/v2/acme/img/blobs/{d}"))
                .bearer_auth(&jwt)
                .send()
                .await
                .unwrap()
                .status(),
            200,
            "blob {d} must survive GC while a child references it"
        );
    }

    // `delete` is only granted once the repo exists (owner ⇒ admin), so request
    // a delete-capable token now.
    let del_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:acme/img:pull,push,delete",
    )
    .await;

    // Deleting the index leaves the children (and their blobs) intact.
    assert_eq!(
        reg.delete(format!("{base}/v2/acme/img/manifests/{index_digest}"))
            .bearer_auth(&del_jwt)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert_eq!(
        reg.get(format!("{base}/v2/acme/img/manifests/{amd_child}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "children remain after the index is deleted"
    );
    rk.run_gc();
    assert_eq!(
        reg.head(format!("{base}/v2/acme/img/blobs/{amd_d}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "child blob survives after the index is deleted"
    );

    // Deleting both children unreferences their blobs → GC collects them.
    for child in [&amd_child, &arm_child] {
        assert_eq!(
            reg.delete(format!("{base}/v2/acme/img/manifests/{child}"))
                .bearer_auth(&del_jwt)
                .send()
                .await
                .unwrap()
                .status(),
            202
        );
    }
    rk.run_gc();
    for d in [&config_d, &amd_d, &arm_d] {
        assert_eq!(
            reg.head(format!("{base}/v2/acme/img/blobs/{d}"))
                .bearer_auth(&jwt)
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "blob {d} must be collected once nothing references it"
        );
    }
}

/// Quota enforcement: a single-blob size cap and a per-org storage cap, both
/// configured via env. Verifies an over-size blob is rejected with `413`, that
/// pushes are allowed up to the org cap, and that the push which would exceed it
/// is rejected with `403`.
#[tokio::test]
async fn quota_enforcement() {
    let stub = spawn_s3_stub().await;
    // 1 KiB max per blob; 4 KiB total per org.
    let rk = Ruskery::spawn_with(
        &stub,
        &[
            ("RUSKERY_QUOTA__MAX_BLOB_BYTES", "1024"),
            ("RUSKERY_QUOTA__DEFAULT_STORAGE_BYTES", "4096"),
        ],
    )
    .await;
    let base = rk.base.clone();

    let dash = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let reg = reqwest::Client::new();

    // First-run setup + a registry token.
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
    let jwt = registry_token(&reg, &base, "admin", &pat, "repository:acme/app:pull,push").await;

    // A blob over the 1 KiB single-blob limit is rejected (413), before it is
    // committed — so it doesn't consume the org's storage budget.
    let too_big = vec![0xAAu8; 2048];
    assert_eq!(
        try_push_blob(&reg, &base, &jwt, "acme/app", &too_big).await,
        413,
        "over-size blob must be rejected"
    );

    // Exactly 1 KiB is allowed. Fill the 4 KiB org cap with four distinct blobs.
    for i in 0u8..4 {
        let blob = vec![i + 1; 1024];
        assert_eq!(
            try_push_blob(&reg, &base, &jwt, "acme/app", &blob).await,
            201,
            "blob {i} within quota must succeed"
        );
    }

    // The org is now at 4 KiB used; the next distinct blob would exceed the cap.
    let over = vec![0xEEu8; 1024];
    assert_eq!(
        try_push_blob(&reg, &base, &jwt, "acme/app", &over).await,
        403,
        "push over the storage quota must be denied"
    );

    // A re-push of an already-stored blob consumes nothing, so it still succeeds
    // even though the org is at its cap (content-addressed dedup).
    let dup = vec![1u8; 1024]; // same content as the first accepted blob
    assert_eq!(
        try_push_blob(&reg, &base, &jwt, "acme/app", &dup).await,
        201,
        "re-push of an existing blob must not be quota-blocked"
    );
}

/// Pull-through cache: an org configured with an upstream serves images it
/// doesn't hold by fetching + caching them from that upstream (manifests in
/// SQLite, blobs streamed into object storage), and refuses direct pushes.
#[tokio::test]
async fn pull_through_cache() {
    let stub = spawn_s3_stub().await;
    let up = spawn_upstream_stub().await;
    let rk = Ruskery::spawn(&stub).await;
    let base = rk.base.clone();

    let dash = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let reg = reqwest::Client::new();

    // First-run setup (admin + the default org), then a separate mirror org.
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

    rk.run_admin(&["create-org", "--slug", "mirror", "--name", "Mirror"]);
    rk.run_admin(&[
        "add-member",
        "--org",
        "mirror",
        "--username",
        "admin",
        "--role",
        "owner",
    ]);
    rk.run_admin(&["set-upstream", "--org", "mirror", "--url", &up.base]);

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

    let jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:mirror/library/test:pull",
    )
    .await;

    // Pull the manifest by tag: a local miss that the proxy fills from upstream.
    let m = reg
        .get(format!("{base}/v2/mirror/library/test/manifests/latest"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(m.status(), 200, "manifest should be proxied from upstream");
    assert_eq!(
        m.headers()
            .get("docker-content-digest")
            .unwrap()
            .to_str()
            .unwrap(),
        up.manifest_digest
    );
    let m_bytes = m.bytes().await.unwrap();
    assert_eq!(sha256_digest(&m_bytes), up.manifest_digest);

    // The config + layer blobs are cached into object storage and served as a
    // 307 redirect to the (stub) presigned URL.
    let noredir = no_redirect_client();
    for d in [&up.config_digest, &up.layer_digest] {
        let r = noredir
            .get(format!("{base}/v2/mirror/library/test/blobs/{d}"))
            .bearer_auth(&jwt)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 307, "blob {d} should be cached + redirected");
    }

    // Following the redirect yields the real bytes, with the right digest.
    let layer = reg
        .get(format!(
            "{base}/v2/mirror/library/test/blobs/{}",
            up.layer_digest
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(layer.status(), 200);
    assert_eq!(
        sha256_digest(&layer.bytes().await.unwrap()),
        up.layer_digest
    );

    // A second manifest pull is served from the local cache.
    let m2 = reg
        .get(format!("{base}/v2/mirror/library/test/manifests/latest"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(m2.status(), 200);

    // Pull by digest also works (now cached locally).
    let by_digest = reg
        .get(format!(
            "{base}/v2/mirror/library/test/manifests/{}",
            up.manifest_digest
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(by_digest.status(), 200);

    // A pull-through cache is read-only: an upload to it is refused even with a
    // push-capable token.
    let push_jwt = registry_token(
        &reg,
        &base,
        "admin",
        &pat,
        "repository:mirror/library/test:pull,push",
    )
    .await;
    let start = reg
        .post(format!("{base}/v2/mirror/library/test/blobs/uploads/"))
        .bearer_auth(&push_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(
        start.status(),
        403,
        "push to a pull-through cache must be denied"
    );
}
