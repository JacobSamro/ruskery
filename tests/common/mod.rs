//! Shared test harness: an in-process S3 stub and a launcher for the real
//! `ruskery` binary. No Docker, no external network — everything runs locally
//! and deterministically, so `cargo test` exercises the whole stack over the
//! wire (token auth, blob uploads to "S3", presigned-redirect pulls, the
//! dashboard API, GC, and rate limiting).

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use sha2::{Digest, Sha256};

pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Pick a currently-free TCP port on loopback.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// ───────────────────────── in-process S3 stub ─────────────────────────

#[derive(Default)]
struct Store {
    /// Final objects, keyed by S3 object key.
    objects: HashMap<String, Vec<u8>>,
    /// In-progress multipart uploads: upload-id -> (part-number -> bytes).
    uploads: HashMap<String, BTreeMap<i32, Vec<u8>>>,
    counter: u64,
}

type Shared = Arc<Mutex<Store>>;

/// Spawn the S3 stub on a random loopback port; returns its `host:port`.
pub async fn spawn_s3_stub() -> String {
    let store: Shared = Arc::new(Mutex::new(Store::default()));
    let app = Router::new()
        .fallback(handle)
        .layer(DefaultBodyLimit::disable())
        .with_state(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("{}:{}", addr.ip(), addr.port())
}

fn query_map(uri: &Uri) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(q) = uri.query() {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("").to_string();
            let v = it.next().unwrap_or("").to_string();
            m.insert(k, v);
        }
    }
    m
}

/// `/{bucket}/{key...}` -> key (single-bucket stub; bucket segment ignored).
fn object_key(path: &str) -> String {
    let p = path.trim_start_matches('/');
    match p.split_once('/') {
        Some((_bucket, key)) => key.to_string(),
        None => String::new(),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn handle(
    State(store): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let key = object_key(uri.path());
    let q = query_map(&uri);
    let mut s = store.lock().unwrap();

    match method {
        // CreateMultipartUpload: POST ...?uploads
        Method::POST if q.contains_key("uploads") => {
            s.counter += 1;
            let upload_id = format!("upload-{}", s.counter);
            s.uploads.insert(upload_id.clone(), BTreeMap::new());
            xml(format!(
                "<?xml version=\"1.0\"?><InitiateMultipartUploadResult>\
                 <Bucket>test</Bucket><Key>{key}</Key><UploadId>{upload_id}</UploadId>\
                 </InitiateMultipartUploadResult>"
            ))
        }
        // CompleteMultipartUpload: POST ...?uploadId=
        Method::POST if q.contains_key("uploadId") => {
            let id = &q["uploadId"];
            let assembled: Vec<u8> = s
                .uploads
                .remove(id)
                .map(|parts| parts.into_values().flatten().collect())
                .unwrap_or_default();
            s.objects.insert(key.clone(), assembled);
            xml(format!(
                "<?xml version=\"1.0\"?><CompleteMultipartUploadResult>\
                 <Location>http://stub/test/{key}</Location><Bucket>test</Bucket>\
                 <Key>{key}</Key><ETag>\"complete\"</ETag></CompleteMultipartUploadResult>"
            ))
        }
        // UploadPart: PUT ...?partNumber=&uploadId=
        Method::PUT if q.contains_key("partNumber") && q.contains_key("uploadId") => {
            let id = q["uploadId"].clone();
            let pn: i32 = q["partNumber"].parse().unwrap_or(0);
            s.uploads.entry(id).or_default().insert(pn, body.to_vec());
            etag(format!("\"part-{pn}\""))
        }
        // CopyObject: PUT with x-amz-copy-source
        Method::PUT if headers.contains_key("x-amz-copy-source") => {
            let src_raw = headers
                .get("x-amz-copy-source")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let src = object_key(&format!(
                "/{}",
                percent_decode(src_raw).trim_start_matches('/')
            ));
            if let Some(data) = s.objects.get(&src).cloned() {
                s.objects.insert(key.clone(), data);
            }
            xml("<?xml version=\"1.0\"?><CopyObjectResult>\
                 <ETag>\"copied\"</ETag><LastModified>2026-01-01T00:00:00.000Z</LastModified>\
                 </CopyObjectResult>"
                .to_string())
        }
        // PutObject
        Method::PUT => {
            s.objects.insert(key, body.to_vec());
            etag("\"put\"".to_string())
        }
        // GetObject (also serves presigned URLs — signature ignored)
        Method::GET => match s.objects.get(&key) {
            Some(data) => (StatusCode::OK, data.clone()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        // HeadObject
        Method::HEAD => match s.objects.get(&key) {
            Some(data) => (
                StatusCode::OK,
                [(axum::http::header::CONTENT_LENGTH, data.len().to_string())],
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        // AbortMultipartUpload
        Method::DELETE if q.contains_key("uploadId") => {
            s.uploads.remove(&q["uploadId"]);
            StatusCode::NO_CONTENT.into_response()
        }
        // DeleteObject
        Method::DELETE => {
            s.objects.remove(&key);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

fn xml(body: String) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/xml")],
        body,
    )
        .into_response()
}

fn etag(tag: String) -> Response {
    (StatusCode::OK, [(axum::http::header::ETAG, tag)]).into_response()
}

// ───────────────────────── ruskery binary launcher ─────────────────────────

/// A running `ruskery serve` child process; killed on drop.
pub struct Ruskery {
    child: Child,
    pub base: String,
    pub db_path: String,
    pub stub: String,
    _tmp: tempfile::TempDir,
}

impl Ruskery {
    /// Boot the real binary against the given S3 stub and wait until healthy.
    pub async fn spawn(stub: &str) -> Ruskery {
        Self::spawn_with(stub, &[]).await
    }

    /// Like [`Ruskery::spawn`] but with extra `RUSKERY_*` environment overrides,
    /// for exercising config-driven behaviour (e.g. quotas / size limits).
    pub async fn spawn_with(stub: &str, extra_env: &[(&str, &str)]) -> Ruskery {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("ruskery.db").to_string_lossy().to_string();
        let cfg = tmp.path().join("none.toml").to_string_lossy().to_string();
        let port = free_port();
        let base = format!("http://127.0.0.1:{port}");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ruskery"));
        cmd.args(["--config", &cfg, "serve"])
            .env("RUST_LOG", "error")
            .env("RUSKERY_DATABASE__PATH", &db_path)
            .env("RUSKERY_SERVER__HTTP_ADDR", format!("127.0.0.1:{port}"))
            // TLS defaults on now; the e2e drives plain HTTP on a loopback port.
            .env("RUSKERY_TLS__ENABLED", "false")
            // Flush analytics counters every second so the test can observe them.
            .env("RUSKERY_ANALYTICS__ROLLUP_SECS", "1")
            .env("RUSKERY_STORAGE__ENDPOINT", format!("http://{stub}"))
            .env("RUSKERY_STORAGE__BUCKET", "test")
            .env("RUSKERY_STORAGE__REGION", "us-east-1")
            .env("RUSKERY_STORAGE__ACCESS_KEY_ID", "test")
            .env("RUSKERY_STORAGE__SECRET_ACCESS_KEY", "test")
            .env("RUSKERY_STORAGE__FORCE_PATH_STYLE", "true")
            .env("RUSKERY_GC__GRACE_SECS", "0");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("failed to spawn ruskery binary");

        let r = Ruskery {
            child,
            base,
            db_path,
            stub: stub.to_string(),
            _tmp: tmp,
        };

        // Wait for readiness.
        let client = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(resp) = client.get(format!("{}/healthz", r.base)).send().await {
                if resp.status().is_success() {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "ruskery did not become ready");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        r
    }

    /// Run a one-off `ruskery gc` against the same db + storage; returns stdout.
    pub fn run_gc(&self) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_ruskery"))
            .args(["--config", "/nonexistent", "gc"])
            .env("RUST_LOG", "error")
            .env("RUSKERY_DATABASE__PATH", &self.db_path)
            .env("RUSKERY_STORAGE__ENDPOINT", format!("http://{}", self.stub))
            .env("RUSKERY_STORAGE__BUCKET", "test")
            .env("RUSKERY_STORAGE__REGION", "us-east-1")
            .env("RUSKERY_STORAGE__ACCESS_KEY_ID", "test")
            .env("RUSKERY_STORAGE__SECRET_ACCESS_KEY", "test")
            .env("RUSKERY_STORAGE__FORCE_PATH_STYLE", "true")
            .env("RUSKERY_GC__GRACE_SECS", "0")
            .output()
            .expect("failed to run gc");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Ruskery {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ───────────────────────── OCI client helpers ─────────────────────────

/// Obtain a registry bearer token via the Docker token flow (Basic auth).
pub async fn registry_token(
    client: &reqwest::Client,
    base: &str,
    user: &str,
    secret: &str,
    scope: &str,
) -> String {
    let resp = client
        .get(format!("{base}/v2/token"))
        .query(&[("service", "127.0.0.1"), ("scope", scope)])
        .basic_auth(user, Some(secret))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "token request failed: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// Push a blob via the monolithic POST→PUT flow; returns its digest.
pub async fn push_blob(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    content: &[u8],
) -> String {
    let digest = sha256_digest(content);
    let start = client
        .post(format!("{base}/v2/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "upload start failed");
    let upload = start
        .headers()
        .get("docker-upload-uuid")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let put = client
        .put(format!("{base}/v2/{repo}/blobs/uploads/{upload}"))
        .query(&[("digest", &digest)])
        .bearer_auth(token)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 201, "blob finalize failed");
    digest
}

/// Attempt a monolithic blob push and return the finalize status without
/// asserting success — for exercising rejection paths (quota, size limits).
pub async fn try_push_blob(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    content: &[u8],
) -> reqwest::StatusCode {
    let digest = sha256_digest(content);
    let start = client
        .post(format!("{base}/v2/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "upload start failed");
    let upload = start
        .headers()
        .get("docker-upload-uuid")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let put = client
        .put(format!("{base}/v2/{repo}/blobs/uploads/{upload}"))
        .query(&[("digest", &digest)])
        .bearer_auth(token)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    put.status()
}

/// Push a blob via the chunked flow: POST start, one PATCH per chunk, then a
/// PUT finalize with no body. Returns the digest of the concatenation.
pub async fn push_blob_chunked(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    chunks: &[&[u8]],
) -> String {
    let mut all = Vec::new();
    for c in chunks {
        all.extend_from_slice(c);
    }
    let digest = sha256_digest(&all);
    let start = client
        .post(format!("{base}/v2/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "chunked upload start");
    let upload = start
        .headers()
        .get("docker-upload-uuid")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    for c in chunks {
        let r = client
            .patch(format!("{base}/v2/{repo}/blobs/uploads/{upload}"))
            .bearer_auth(token)
            .body(c.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 202, "patch chunk");
    }
    let put = client
        .put(format!("{base}/v2/{repo}/blobs/uploads/{upload}"))
        .query(&[("digest", &digest)])
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 201, "chunked finalize");
    digest
}

/// Build + push a minimal image manifest (config + one layer); returns digest.
#[allow(clippy::too_many_arguments)]
pub async fn push_manifest(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    config_digest: &str,
    config_size: usize,
    layer_digest: &str,
    layer_size: usize,
    reference: &str,
) -> (String, reqwest::StatusCode) {
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_size,
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": layer_digest,
            "size": layer_size,
        }],
    });
    let body = serde_json::to_vec(&manifest).unwrap();
    let digest = sha256_digest(&body);
    let resp = client
        .put(format!("{base}/v2/{repo}/manifests/{reference}"))
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .unwrap();
    (digest, resp.status())
}

/// PUT a raw manifest/index body under its own digest (no tag), returning the
/// digest + status. Used for image indexes and by-digest child manifests.
pub async fn push_manifest_raw(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    media_type: &str,
    body: &str,
) -> (String, reqwest::StatusCode) {
    let digest = sha256_digest(body.as_bytes());
    let resp = client
        .put(format!("{base}/v2/{repo}/manifests/{digest}"))
        .header("content-type", media_type)
        .bearer_auth(token)
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    (digest, resp.status())
}

/// A reqwest client that does NOT follow redirects (to assert the 307 itself).
pub fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
