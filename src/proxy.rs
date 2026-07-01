//! Pull-through cache: fetch manifests and blobs from an upstream OCI registry
//! on a local miss, cache them under the org (manifests in SQLite, blobs in
//! object storage), and serve them.
//!
//! Caching is per-request and lazy: a manifest miss caches only that manifest,
//! a blob miss caches only that blob. A normal `docker pull` drives the rest —
//! it fetches the index, then the platform manifest, then the config and layer
//! blobs — so each lands here as its own miss and is cached on the way through.
//! Blobs are digest-verified before they're recorded; manifests are verified
//! when pulled by digest.
//!
//! Not yet implemented (a cached tag is served indefinitely): re-validating a
//! tag against the upstream when it may have moved. Pull by digest is always
//! exact. Configure an org with `ruskery admin set-upstream`.

use std::sync::LazyLock;

use futures::StreamExt;
use sha2::{Digest, Sha256};

use crate::db;
use crate::db::orgs::{Org, OrgUpstream};
use crate::error::{Error, Result};
use crate::registry::manifests;
use crate::state::AppState;
use crate::storage::Storage;

/// Multipart part size when streaming an upstream blob into object storage.
const PART_SIZE: usize = 8 * 1024 * 1024;

/// Caps on *buffered* upstream responses, so a malicious/compromised upstream
/// can't drive us to OOM. Blobs are streamed (not buffered) and bounded
/// elsewhere; these cover the JSON/manifest bodies we hold in memory.
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024; // manifests/indexes are small
const MAX_LIST_BYTES: usize = 64 * 1024 * 1024; // one _catalog / tags/list page
const MAX_TOKEN_BYTES: usize = 1024 * 1024; // token-endpoint JSON
/// Safety cap on pagination pages followed, so an upstream that always returns a
/// `next` link can't loop forever.
const MAX_PAGES: usize = 10_000;

/// Media types we'll accept from the upstream — OCI and Docker, image and index.
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.docker.distribution.manifest.v2+json";

/// Shared HTTP client. Redirects are followed (a blob `GET` the upstream answers
/// with a CDN redirect must be chased), but the custom policy refuses any hop to
/// a link-local or unspecified IP literal — so a malicious upstream can't bounce
/// a request onto the cloud-metadata endpoint (`169.254.169.254`).
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("ruskery/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().host_str().is_some_and(is_blocked_ip_literal) {
                attempt.error("refusing to follow a redirect to a link-local/unspecified address")
            } else if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .expect("build proxy http client")
});

/// True if `host` is an IP literal in a range we never fetch from — link-local
/// (cloud metadata: `169.254/16`, `fe80::/10`) or unspecified. Loopback and
/// private ranges are intentionally allowed (LAN registries are a legitimate
/// upstream). Non-literal hostnames return `false` here and are checked via DNS
/// by [`guard_fetch_target`].
fn is_blocked_ip_literal(host: &str) -> bool {
    use std::net::IpAddr;
    let h = host.trim_start_matches('[').trim_end_matches(']');
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_link_local() || v4.is_unspecified(),
        Ok(IpAddr::V6(v6)) => (v6.segments()[0] & 0xffc0) == 0xfe80 || v6.is_unspecified(),
        Err(_) => false,
    }
}

/// True for `localhost` or a loopback IP literal — the one host class allowed to
/// use a cleartext token realm (self-hosted / test upstreams).
fn is_loopback_hostish(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Read a response body into memory, refusing to buffer more than `max` bytes
/// (checking `Content-Length` first, then enforcing while streaming so a lying
/// or absent length can't get past the cap).
async fn read_capped(resp: reqwest::Response, max: usize, what: &str) -> Result<bytes::Bytes> {
    if let Some(len) = resp.content_length() {
        if len > max as u64 {
            return Err(upstream_error(format!(
                "upstream {what} too large ({len} bytes, limit {max})"
            )));
        }
    }
    let mut stream = resp.bytes_stream();
    let mut buf = bytes::BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| upstream_error(format!("reading upstream {what}: {e}")))?;
        if buf.len() + chunk.len() > max {
            return Err(upstream_error(format!(
                "upstream {what} exceeded {max} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

/// Resolve `url`'s host and refuse if any address is link-local or unspecified.
/// Applied before fetching an *upstream-controlled* URL — the `Bearer` realm and
/// pagination `Link` targets — so those can't steer the token dance (and its
/// credentials) at an internal metadata endpoint, even via a hostname that
/// resolves there. Loopback stays permitted (LAN/self-hosted upstreams).
async fn guard_fetch_target(url: &str) -> Result<()> {
    use std::net::IpAddr;
    let parsed = url::Url::parse(url).map_err(|_| upstream_error("invalid upstream URL"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| upstream_error("upstream URL is missing a host"))?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| upstream_error(format!("cannot resolve upstream host: {e}")))?;
    for addr in addrs {
        let ip = addr.ip();
        let link_local = match ip {
            IpAddr::V4(v4) => v4.is_link_local(),
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        };
        if link_local || ip.is_unspecified() {
            return Err(upstream_error(
                "refusing to follow the upstream to a link-local/unspecified address",
            ));
        }
    }
    Ok(())
}

/// A `502` for an upstream that is unreachable or misbehaving (distinct from a
/// clean upstream `404`, which surfaces to the client as a normal not-found).
fn upstream_error(msg: impl Into<String>) -> Error {
    Error::oci(
        axum::http::StatusCode::BAD_GATEWAY,
        "UPSTREAM_UNAVAILABLE",
        msg.into(),
    )
}

/// `413` when an upstream blob exceeds the configured single-blob size limit —
/// mirrors the client-upload guard so the cache/import path is bounded too.
fn blob_too_large(max: u64) -> Error {
    Error::oci(
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
        "SIZE_INVALID",
        format!("upstream blob exceeds the maximum allowed size of {max} bytes"),
    )
}

/// `403` when caching an upstream blob would push the org over its storage quota.
fn quota_exceeded() -> Error {
    Error::oci(
        axum::http::StatusCode::FORBIDDEN,
        "DENIED",
        "organization storage quota exceeded",
    )
}

/// Fetch + cache the manifest `reference` of `repo_name` from the org's upstream.
/// A clean upstream `404` caches nothing (the caller then returns not-found).
pub async fn cache_manifest(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    reference: &str,
    up: &OrgUpstream,
) -> Result<()> {
    let Some((bytes, media_type)) = fetch_manifest(up, repo_name, reference).await? else {
        return Ok(());
    };
    store_manifest(state, org, repo_name, reference, &bytes, &media_type).await?;
    Ok(())
}

/// Record an already-fetched manifest under the org's repo (creating the repo if
/// needed): parse its blob/child/subject links, store it content-addressed, and
/// point the tag at it (when `reference` is a tag). Returns the stored digest.
///
/// Shared by the pull-through cache (which fetches then stores) and the bulk
/// importer (which fetches once, copies blobs, then stores). Unlike a client
/// push, the referenced blobs are not required to be present at store time —
/// the proxy caches them lazily, the importer copies them just before this call.
pub(crate) async fn store_manifest(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    reference: &str,
    bytes: &[u8],
    media_type: &str,
) -> Result<String> {
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    if manifests::is_digest(reference) && !crate::registry::digests_equal(reference, &digest) {
        return Err(upstream_error(
            "upstream manifest digest did not match the requested digest",
        ));
    }

    let doc: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| upstream_error("upstream manifest is not valid JSON"))?;
    let blob_refs = manifests::parse_blob_refs_value(&doc);
    let child_refs = manifests::parse_child_refs(&doc);
    let subject = manifests::parse_subject(&doc);

    let repo = match db::orgs::find_repo(state.db(), &org.id, repo_name).await? {
        Some(r) => r,
        None => db::orgs::create_repo(state.db(), &org.id, repo_name).await?,
    };

    let links = db::content::ManifestLinks {
        blobs: &blob_refs,
        children: &child_refs,
        subject: subject.as_ref().map(|(d, at)| (d.as_str(), at.clone())),
    };
    db::content::put_manifest(state.db(), &repo.id, &digest, media_type, bytes, &links).await?;

    if !manifests::is_digest(reference) {
        db::content::upsert_tag(state.db(), &repo.id, reference, &digest).await?;
        state.cache().put_tag(&repo.id, reference, &digest);
    }
    state.cache().invalidate_manifest(&repo.id, &digest);
    Ok(digest)
}

/// Fetch + cache a single blob from the org's upstream into object storage,
/// verifying its digest before recording it. The repo only scopes the upstream
/// auth/URL; the blob is content-addressed and stored under the org.
pub async fn cache_blob(
    state: &AppState,
    org_id: &str,
    repo: &str,
    digest: &str,
    up: &OrgUpstream,
) -> Result<()> {
    let url = format!("{}/v2/{}/blobs/{}", up.url, repo, digest);
    let scope = format!("repository:{repo}:pull");
    let resp = send_authed(up, &url, None, &scope).await?;
    let status = resp.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::oci(
                axum::http::StatusCode::NOT_FOUND,
                "BLOB_UNKNOWN",
                "blob unknown to upstream",
            ));
        }
        return Err(upstream_error(format!("upstream blob status {status}")));
    }

    let storage = state.storage();
    let key = Storage::blob_key(org_id, digest);
    let max_blob = state.config().quota.max_blob_bytes;

    // Effective storage quota (org override, else instance default; 0 = unlimited).
    // cache_blob is only reached on a genuine local miss, so this blob is new to
    // the org and does consume quota.
    let effective: u64 = match db::orgs::org_quota_bytes(state.db(), org_id).await? {
        Some(bytes) => bytes.max(0) as u64,
        None => state.config().quota.default_storage_bytes,
    };
    // Fast reject on the advertised length before downloading anything.
    if let Some(len) = resp.content_length() {
        if max_blob > 0 && len > max_blob {
            return Err(blob_too_large(max_blob));
        }
        if effective > 0 {
            let used = db::content::org_storage_used(state.db(), org_id).await?;
            if (used.max(0) as u64).saturating_add(len) > effective {
                return Err(quota_exceeded());
            }
        }
    }

    let total = stream_to_storage(&storage, &key, resp, digest, max_blob).await?;

    // Authoritative quota check against the actual bytes (Content-Length may be
    // absent or wrong): on breach, delete what we just wrote and don't record it.
    if effective > 0 {
        let used = db::content::org_storage_used(state.db(), org_id).await?;
        if (used.max(0) as u64).saturating_add(total as u64) > effective {
            let _ = storage.delete(&key).await;
            return Err(quota_exceeded());
        }
    }
    db::content::record_blob(state.db(), org_id, digest, total).await?;
    Ok(())
}

/// Abort an in-progress multipart upload, logging (not swallowing) a failure.
/// Abort is best-effort — the bucket's incomplete-multipart lifecycle rule is
/// the ultimate backstop for leaked parts — but discarding the error silently
/// would hide a leak entirely, so surface it at `warn`.
async fn abort_multipart_logged(storage: &Storage, key: &str, upload_id: &str) {
    if let Err(e) = storage.abort_multipart(key, upload_id).await {
        tracing::warn!(
            %key,
            upload_id,
            error = %e,
            "failed to abort multipart upload; leaked parts rely on the bucket's incomplete-multipart lifecycle rule"
        );
    }
}

/// Stream an upstream response body into object storage at `key`, computing the
/// SHA-256 as it flows and verifying it against `expected_digest` before
/// committing. Uses a multipart upload, flushing fixed-size parts, so a large
/// layer never has to be buffered whole.
async fn stream_to_storage(
    storage: &Storage,
    key: &str,
    resp: reqwest::Response,
    expected_digest: &str,
    max_blob: u64,
) -> Result<i64> {
    let upload_id = storage.create_multipart(key).await?;

    let mut hasher = Sha256::new();
    let mut buffer: Vec<u8> = Vec::new();
    let mut parts = Vec::new();
    let mut next_part = 1;
    let mut total: u64 = 0;

    let mut stream = resp.bytes_stream();
    // Any failure mid-stream aborts the multipart so we don't leak parts.
    let mut error: Option<Error> = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                error = Some(upstream_error(format!("reading upstream blob: {e}")));
                break;
            }
        };
        hasher.update(&chunk);
        total += chunk.len() as u64;
        // Refuse an over-size blob mid-stream so a malicious upstream can't push
        // an unbounded object into storage before we'd notice.
        if max_blob > 0 && total > max_blob {
            error = Some(blob_too_large(max_blob));
            break;
        }
        buffer.extend_from_slice(&chunk);
        while buffer.len() >= PART_SIZE {
            let part: Vec<u8> = buffer.drain(..PART_SIZE).collect();
            match storage.upload_part(key, &upload_id, next_part, part).await {
                Ok(p) => {
                    parts.push(p);
                    next_part += 1;
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        if error.is_some() {
            break;
        }
    }

    if let Some(e) = error {
        abort_multipart_logged(storage, key, &upload_id).await;
        return Err(e);
    }

    // Verify the content matches the digest the client asked for before we
    // commit it under the content-addressed key.
    let computed = format!("sha256:{}", hex::encode(hasher.finalize()));
    if !crate::registry::digests_equal(&computed, expected_digest) {
        abort_multipart_logged(storage, key, &upload_id).await;
        return Err(upstream_error(format!(
            "upstream blob digest mismatch: got {computed}, expected {expected_digest}"
        )));
    }

    // Commit: a small blob (no flushed parts) via PutObject, else complete the
    // multipart after flushing any tail. A failure on the commit path aborts the
    // multipart so we don't leak an in-progress upload that GC can't reach.
    if parts.is_empty() {
        abort_multipart_logged(storage, key, &upload_id).await;
        storage.put(key, std::mem::take(&mut buffer)).await?;
    } else {
        if !buffer.is_empty() {
            let last = std::mem::take(&mut buffer);
            match storage.upload_part(key, &upload_id, next_part, last).await {
                Ok(p) => parts.push(p),
                Err(e) => {
                    abort_multipart_logged(storage, key, &upload_id).await;
                    return Err(e);
                }
            }
        }
        if let Err(e) = storage.complete_multipart(key, &upload_id, parts).await {
            abort_multipart_logged(storage, key, &upload_id).await;
            return Err(e);
        }
    }

    Ok(total as i64)
}

/// Verify the upstream is reachable and the credentials work: hit `/v2/` and do
/// the bearer dance. Returns Ok on a 2xx, else an error describing the failure.
/// Used to give fast feedback before kicking off a long import job.
pub(crate) async fn probe(up: &OrgUpstream) -> Result<()> {
    let url = format!("{}/v2/", up.url);
    let resp = send_authed(up, &url, None, "registry:catalog:*").await?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else if status == reqwest::StatusCode::UNAUTHORIZED {
        Err(upstream_error(
            "upstream rejected the credentials (401) — check the host, username, and password/token",
        ))
    } else {
        Err(upstream_error(format!(
            "upstream /v2/ returned status {status}"
        )))
    }
}

/// List every repository in the upstream catalog (`GET /v2/_catalog`), following
/// `Link: rel="next"` pagination. Requires a registry that exposes `_catalog`
/// (registry:2, Harbor, DOCR, …). Repository names are returned verbatim.
pub(crate) async fn list_catalog(up: &OrgUpstream) -> Result<Vec<String>> {
    // Repository names are small, so buffering the whole catalog is a few MB
    // even for tens of thousands of repos (the large blob bytes are streamed,
    // not buffered). If that ever becomes a concern, page through and drive the
    // import per-page instead, updating `repos_total` incrementally.
    let mut out = Vec::new();
    // Page with a bounded size; keep following the server's `next` link.
    let mut next = format!("{}/v2/_catalog?n=200", up.url);
    let mut pages = 0usize;
    loop {
        pages += 1;
        if pages > MAX_PAGES {
            tracing::warn!("upstream _catalog exceeded {MAX_PAGES} pages; truncating listing");
            break;
        }
        let resp = send_authed(up, &next, None, "registry:catalog:*").await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(upstream_error(format!(
                "upstream _catalog returned status {status} (does this registry support catalog listing?)"
            )));
        }
        let link = link_next(resp.headers(), &up.url);
        let raw = read_capped(resp, MAX_LIST_BYTES, "catalog").await?;
        let body: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|e| upstream_error(format!("reading upstream catalog: {e}")))?;
        if let Some(repos) = body.get("repositories").and_then(|r| r.as_array()) {
            out.extend(repos.iter().filter_map(|r| r.as_str().map(String::from)));
        }
        match link {
            // The next link is upstream-controlled; guard its host before following.
            Some(url) => {
                guard_fetch_target(&url).await?;
                next = url;
            }
            None => break,
        }
    }
    Ok(out)
}

/// List the tags of one upstream repository (`GET /v2/<repo>/tags/list`),
/// following pagination. A clean `404` yields an empty list (repo has no tags).
pub(crate) async fn list_tags(up: &OrgUpstream, repo: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let scope = format!("repository:{repo}:pull");
    let mut next = format!("{}/v2/{}/tags/list?n=200", up.url, repo);
    let mut pages = 0usize;
    loop {
        pages += 1;
        if pages > MAX_PAGES {
            tracing::warn!("upstream tags/list for {repo} exceeded {MAX_PAGES} pages; truncating");
            break;
        }
        let resp = send_authed(up, &next, None, &scope).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            break;
        }
        if !status.is_success() {
            return Err(upstream_error(format!(
                "upstream tags/list for {repo} returned status {status}"
            )));
        }
        let link = link_next(resp.headers(), &up.url);
        let raw = read_capped(resp, MAX_LIST_BYTES, "tags").await?;
        let body: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|e| upstream_error(format!("reading upstream tags for {repo}: {e}")))?;
        if let Some(tags) = body.get("tags").and_then(|t| t.as_array()) {
            out.extend(tags.iter().filter_map(|t| t.as_str().map(String::from)));
        }
        match link {
            // The next link is upstream-controlled; guard its host before following.
            Some(url) => {
                guard_fetch_target(&url).await?;
                next = url;
            }
            None => break,
        }
    }
    Ok(out)
}

/// Resolve the `Link: <...>; rel="next"` pagination header to an absolute URL.
/// The registry spec makes the link relative to the registry root.
fn link_next(headers: &reqwest::header::HeaderMap, base: &str) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    if !link.contains("rel=\"next\"") {
        return None;
    }
    let start = link.find('<')? + 1;
    let end = link[start..].find('>')? + start;
    let target = &link[start..end];
    if target.starts_with("http://") || target.starts_with("https://") {
        Some(target.to_string())
    } else {
        Some(format!("{}{}", base.trim_end_matches('/'), target))
    }
}

/// Fetch a manifest from the upstream. `Ok(None)` for a clean upstream `404`.
pub(crate) async fn fetch_manifest(
    up: &OrgUpstream,
    repo: &str,
    reference: &str,
) -> Result<Option<(bytes::Bytes, String)>> {
    let url = format!("{}/v2/{}/manifests/{}", up.url, repo, reference);
    let scope = format!("repository:{repo}:pull");
    let resp = send_authed(up, &url, Some(MANIFEST_ACCEPT), &scope).await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(upstream_error(format!("upstream manifest status {status}")));
    }
    let media_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/vnd.oci.image.manifest.v1+json")
        .to_string();
    let bytes = read_capped(resp, MAX_MANIFEST_BYTES, "manifest").await?;
    Ok(Some((bytes, media_type)))
}

/// `GET` a URL from the upstream, performing the registry bearer-token dance on
/// a `401`: parse the `WWW-Authenticate` challenge, obtain a token (with the
/// org's optional credentials), and retry. If no token can be obtained the
/// original `401` response is returned for the caller to interpret.
async fn send_authed(
    up: &OrgUpstream,
    url: &str,
    accept: Option<&str>,
    scope: &str,
) -> Result<reqwest::Response> {
    let build = |bearer: Option<&str>| {
        let mut req = CLIENT.get(url);
        if let Some(a) = accept {
            req = req.header(reqwest::header::ACCEPT, a);
        }
        if let Some(b) = bearer {
            req = req.bearer_auth(b);
        }
        req
    };

    let resp = build(None)
        .send()
        .await
        .map_err(|e| upstream_error(format!("upstream request failed: {e}")))?;
    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(resp);
    }

    let challenge = resp
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    match bearer_token(up, &challenge, scope).await? {
        Some(token) => build(Some(&token))
            .send()
            .await
            .map_err(|e| upstream_error(format!("upstream request failed: {e}"))),
        None => Ok(resp),
    }
}

/// Obtain a bearer token from the realm named in a `Bearer` challenge.
async fn bearer_token(up: &OrgUpstream, challenge: &str, scope: &str) -> Result<Option<String>> {
    let Some(realm) = challenge_param(challenge, "realm") else {
        return Ok(None); // not a Bearer challenge we can satisfy
    };
    // The realm comes from the upstream's WWW-Authenticate header and is where we
    // send the org's Basic credentials — refuse to resolve it to a metadata
    // endpoint so a hostile upstream can't harvest creds via SSRF.
    guard_fetch_target(&realm).await?;
    // When we'd attach credentials, refuse a cleartext realm on a non-loopback
    // host: a hostile upstream could otherwise downgrade the token request to
    // plain HTTP and harvest the creds off the wire. (Real registries use HTTPS
    // realms; loopback is exempted for self-hosted/test upstreams.) A *public*
    // HTTPS attacker realm is an inherent residual of the WWW-Authenticate model.
    if up.username.is_some() {
        let cleartext_public = url::Url::parse(&realm)
            .ok()
            .map(|u| u.scheme() == "http" && !u.host_str().is_some_and(is_loopback_hostish))
            .unwrap_or(false);
        if cleartext_public {
            return Err(upstream_error(
                "refusing to send upstream credentials to a non-HTTPS token realm",
            ));
        }
    }
    let service = challenge_param(challenge, "service");

    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = &service {
        query.push(("service", s));
    }
    query.push(("scope", scope));

    let mut req = CLIENT.get(&realm).query(&query);
    if let Some(user) = &up.username {
        req = req.basic_auth(user, up.password.clone());
    }

    let resp = req
        .send()
        .await
        .map_err(|e| upstream_error(format!("upstream token request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(upstream_error(format!(
            "upstream token endpoint status {}",
            resp.status()
        )));
    }
    let raw = read_capped(resp, MAX_TOKEN_BYTES, "token response").await?;
    let body: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| upstream_error(format!("upstream token response: {e}")))?;
    let token = body
        .get("token")
        .or_else(|| body.get("access_token"))
        .and_then(|v| v.as_str())
        .map(String::from);
    Ok(token)
}

/// Extract `key="value"` from a `WWW-Authenticate: Bearer …` challenge value.
fn challenge_param(challenge: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = challenge.find(&needle)? + needle.len();
    let rest = &challenge[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::{challenge_param, is_blocked_ip_literal, is_loopback_hostish};

    #[test]
    fn loopback_hosts_are_recognized() {
        assert!(is_loopback_hostish("localhost"));
        assert!(is_loopback_hostish("LOCALHOST"));
        assert!(is_loopback_hostish("127.0.0.1"));
        assert!(is_loopback_hostish("[::1]"));
        assert!(!is_loopback_hostish("registry.example.com"));
        assert!(!is_loopback_hostish("10.0.0.1"));
    }

    #[test]
    fn blocks_metadata_and_unspecified_ip_literals() {
        // Cloud metadata + unspecified are refused as redirect/realm targets.
        assert!(is_blocked_ip_literal("169.254.169.254"));
        assert!(is_blocked_ip_literal("0.0.0.0"));
        assert!(is_blocked_ip_literal("[fe80::1]"));
        assert!(is_blocked_ip_literal("[::]"));
        // Public, loopback, and private ranges are allowed (LAN registries).
        assert!(!is_blocked_ip_literal("registry-1.docker.io"));
        assert!(!is_blocked_ip_literal("127.0.0.1"));
        assert!(!is_blocked_ip_literal("10.0.0.5"));
        assert!(!is_blocked_ip_literal("52.1.2.3"));
    }

    #[test]
    fn parses_bearer_challenge_params() {
        let c = "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",scope=\"repository:library/nginx:pull\"";
        assert_eq!(
            challenge_param(c, "realm").as_deref(),
            Some("https://auth.docker.io/token")
        );
        assert_eq!(
            challenge_param(c, "service").as_deref(),
            Some("registry.docker.io")
        );
        assert_eq!(challenge_param(c, "missing"), None);
    }
}
