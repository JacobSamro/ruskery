//! End-to-end test: boots the real `ruskery` binary against an in-process S3
//! stub and exercises the whole stack over the wire — the OCI push/pull
//! protocol (monolithic, chunked, and multipart uploads; presigned-redirect
//! pulls), the dashboard API (setup, orgs, members, teams, tokens, domains,
//! audit), garbage collection, security headers, and auth rate limiting.
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
