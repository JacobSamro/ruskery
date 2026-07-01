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

/// Shared HTTP client. Automatic redirects are **disabled**: a redirect target
/// is upstream-controlled, and reqwest's redirect policy is synchronous so it
/// can't DNS-resolve a hostname before following. We follow manually in
/// [`send_get`], DNS-guarding every hop, so a redirect to a name that resolves
/// to a link-local/metadata address can't be chased.
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("ruskery/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build proxy http client")
});

/// Cap on redirect hops we'll follow (matches reqwest's old default).
const MAX_REDIRECTS: usize = 10;

/// Credentials to attach to an upstream GET (dropped on cross-host redirects).
enum Auth<'a> {
    None,
    Bearer(&'a str),
    Basic(&'a str, Option<String>),
}

/// GET a URL, following redirects manually and DNS-guarding **every** hop so an
/// upstream can't redirect us onto a link-local/metadata address — even via a
/// hostname that resolves there (which the synchronous reqwest redirect policy
/// can't catch). Credentials are attached to the first request and to same-host
/// redirects only (matching reqwest's cross-host header-stripping); a presigned
/// CDN target, the usual redirect, needs none.
async fn send_get(url: &str, accept: Option<&str>, auth: Auth<'_>) -> Result<reqwest::Response> {
    let origin_host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()));
    let mut current = url.to_string();
    let mut attach_auth = true;
    let mut hops = 0usize;
    loop {
        let mut req = CLIENT.get(&current);
        if let Some(a) = accept {
            req = req.header(reqwest::header::ACCEPT, a);
        }
        if attach_auth {
            match &auth {
                Auth::None => {}
                Auth::Bearer(b) => req = req.bearer_auth(b),
                Auth::Basic(u, p) => req = req.basic_auth(u, p.clone()),
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| upstream_error(format!("upstream request failed: {e}")))?;
        if !resp.status().is_redirection() {
            return Ok(resp);
        }
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(upstream_error("too many redirects from upstream"));
        }
        let Some(loc) = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
        else {
            return Ok(resp); // a redirect with no Location; hand it back as-is
        };
        let next = reqwest::Url::parse(&current)
            .and_then(|base| base.join(loc))
            .map_err(|_| upstream_error("invalid redirect location from upstream"))?;
        let next_str = next.to_string();
        // The whole point: resolve + refuse a link-local/unspecified redirect hop.
        guard_fetch_target(&next_str).await?;
        let next_host = next.host_str().map(|h| h.to_ascii_lowercase());
        attach_auth = attach_auth && next_host == origin_host;
        current = next_str;
    }
}

/// Built-in token-realm hosts trusted to receive credentials even though they
/// differ from the registry host — well-known public registries whose auth
/// service lives on a separate domain. (GHCR/DOCR use a same-host realm, so
/// they're covered by the host-match rule without needing an entry here.)
const WELL_KNOWN_REALM_HOSTS: &[&str] = &["auth.docker.io"];

/// Whether it's safe to send the upstream's credentials to `realm`. Trusted when
/// the realm host matches the upstream host, is loopback, is a built-in
/// well-known auth host, or is in the admin's `trusted_realm_hosts` allowlist —
/// so a hostile upstream can't name an attacker host in `WWW-Authenticate` and
/// harvest the credentials.
fn realm_is_trusted(realm: &str, upstream_url: &str, extra: &[String]) -> bool {
    let host = |u: &str| {
        url::Url::parse(u)
            .ok()
            .and_then(|p| p.host_str().map(|h| h.to_ascii_lowercase()))
    };
    let Some(realm_host) = host(realm) else {
        return false;
    };
    if host(upstream_url).is_some_and(|up| up == realm_host) {
        return true; // same host as the registry itself
    }
    if is_loopback_hostish(&realm_host) {
        return true;
    }
    if WELL_KNOWN_REALM_HOSTS
        .iter()
        .any(|w| w.eq_ignore_ascii_case(&realm_host))
    {
        return true;
    }
    extra.iter().any(|h| h.eq_ignore_ascii_case(&realm_host))
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
    let resp = send_get(url, accept, Auth::None).await?;
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
        Some(token) => send_get(url, accept, Auth::Bearer(&token)).await,
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
    // Before attaching credentials, make sure the realm is one we trust — its
    // host must match the upstream, be loopback, be a well-known auth host, or be
    // allowlisted — and, additionally, not cleartext on a non-loopback host. This
    // stops a hostile upstream from naming an attacker realm (over HTTP or HTTPS)
    // in WWW-Authenticate to harvest the credentials.
    if up.username.is_some() {
        if !realm_is_trusted(&realm, &up.url, &up.trusted_realm_hosts) {
            let host = url::Url::parse(&realm)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| "?".to_string());
            return Err(upstream_error(format!(
                "refusing to send upstream credentials to untrusted token-realm host '{host}'; \
                 add it to import.trusted_realm_hosts if it is legitimate"
            )));
        }
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

    // Build the realm URL with its query, then fetch it through the manual,
    // redirect-DNS-guarding path (a token endpoint that redirects can't be
    // steered onto an internal address either).
    let mut realm_url =
        reqwest::Url::parse(&realm).map_err(|_| upstream_error("invalid token realm URL"))?;
    {
        let mut qp = realm_url.query_pairs_mut();
        if let Some(s) = &service {
            qp.append_pair("service", s);
        }
        qp.append_pair("scope", scope);
    }
    let auth = match &up.username {
        Some(user) => Auth::Basic(user, up.password.clone()),
        None => Auth::None,
    };
    let resp = send_get(realm_url.as_str(), None, auth).await?;
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
    use super::{challenge_param, is_loopback_hostish, realm_is_trusted};

    #[test]
    fn realm_trust_allows_expected_and_blocks_attacker() {
        let none: &[String] = &[];
        // Same host as the registry, and well-known Docker Hub auth host.
        assert!(realm_is_trusted(
            "https://registry.example.com/token",
            "https://registry.example.com",
            none
        ));
        assert!(realm_is_trusted(
            "https://auth.docker.io/token",
            "https://registry-1.docker.io",
            none
        ));
        // Loopback (self-hosted / tests).
        assert!(realm_is_trusted(
            "http://127.0.0.1:5000/token",
            "http://127.0.0.1:5000",
            none
        ));
        // A hostile upstream naming an attacker realm is refused (even over HTTPS)…
        assert!(!realm_is_trusted(
            "https://attacker.example/token",
            "https://registry-1.docker.io",
            none
        ));
        // …unless the admin explicitly allowlists that realm host.
        let allow = vec!["attacker.example".to_string()];
        assert!(realm_is_trusted(
            "https://attacker.example/token",
            "https://registry-1.docker.io",
            &allow
        ));
    }

    #[tokio::test]
    async fn guard_refuses_metadata_and_unspecified_targets() {
        use super::guard_fetch_target;
        // The redirect/realm/pagination guard: link-local (cloud metadata) and
        // unspecified addresses are refused (IP literals resolve to themselves,
        // so this is hermetic — no real DNS needed).
        assert!(
            guard_fetch_target("http://169.254.169.254/latest/meta-data/")
                .await
                .is_err()
        );
        assert!(guard_fetch_target("http://0.0.0.0/").await.is_err());
        // Loopback (self-hosted / test upstreams) is allowed.
        assert!(guard_fetch_target("http://127.0.0.1:5000/").await.is_ok());
    }

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
