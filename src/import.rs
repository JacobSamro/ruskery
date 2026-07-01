//! Bulk registry-to-registry import: copy every repository (all tags, all
//! architectures) from an upstream OCI registry into a target org, in the
//! background. Essentially a server-side `skopeo sync` of a whole catalog.
//!
//! It reuses the pull-through cache machinery in [`crate::proxy`] — the upstream
//! bearer-token dance, digest-verified blob streaming into object storage, and
//! content-addressed manifest recording — but drives it *eagerly*: enumerate the
//! catalog (`/v2/_catalog`), then for every tag copy the manifest, recursing
//! into manifest-list children so multi-arch images come across whole.
//!
//! Work is parallelized under a single bound (`import.concurrency`): repositories
//! are copied concurrently, and within an image the config + layer blobs download
//! concurrently — all gated by one semaphore so the total number of in-flight
//! upstream downloads stays capped. A per-digest lock dedups shared base layers
//! so two images never download the same blob twice at once. Blobs already
//! present under the org are skipped, so re-running is cheap.
//!
//! Credentials live only in the running task; only the upstream URL and progress
//! counters are persisted (see `db::imports`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};

use futures::future::join_all;
use futures::stream::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use crate::db;
use crate::db::orgs::{Org, OrgUpstream};
use crate::error::{Error, Result};
use crate::providers::{self, Provider};
use crate::proxy;
use crate::registry::manifests;
use crate::state::AppState;

/// Guard against a malicious upstream serving a self-referential or absurdly
/// nested index. Real image indexes are one level deep (index → platform
/// manifests); nested indexes are rare and shallow.
const MAX_MANIFEST_DEPTH: usize = 8;

/// Shared concurrency controls for one import run.
struct Ctx {
    /// Max repositories copied in parallel.
    concurrency: usize,
    /// Caps total in-flight blob downloads across the whole import.
    blob_sem: Semaphore,
    /// Per-digest locks: two tasks copying the same blob serialize here, so a
    /// shared base layer is downloaded once (the second finds it already stored).
    inflight: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

/// What a single manifest (and its children) copied, for progress accounting.
#[derive(Default)]
struct Copied {
    blobs: i64,
    bytes: i64,
}

/// Kick off an import in the background. The task owns `up` (with credentials)
/// for its lifetime and updates the `imports` row as it goes. `provider` selects
/// how the catalog is enumerated; `namespace` is the DigitalOcean registry /
/// GitHub owner (required for those) or an optional prefix filter for generic.
pub fn spawn(
    state: AppState,
    id: String,
    org: Org,
    up: OrgUpstream,
    provider: Provider,
    namespace: Option<String>,
) {
    let concurrency = state.config().import.concurrency.max(1);
    tokio::spawn(async move {
        tracing::info!(
            import = %id,
            upstream = %up.url,
            org = %org.slug,
            concurrency,
            provider = ?provider,
            namespace = namespace.as_deref().unwrap_or(""),
            "import started"
        );
        let ctx = Ctx {
            concurrency,
            blob_sem: Semaphore::new(concurrency),
            inflight: StdMutex::new(HashMap::new()),
        };
        let outcome = run(&state, &id, &org, &up, &ctx, provider, namespace.as_deref()).await;
        match outcome {
            Ok(()) => {
                tracing::info!(import = %id, "import completed");
                let _ = db::imports::finish(state.db(), &id, "completed", None).await;
            }
            Err(e) => {
                tracing::error!(import = %id, error = %e, "import failed");
                let _ = db::imports::finish(state.db(), &id, "failed", Some(&e.to_string())).await;
            }
        }
    });
}

/// Enumerate the catalog and copy each repository concurrently. A failure listing
/// or copying a single repo/tag is logged and skipped (progress still advances)
/// so one bad image can't abort a large migration; only a catalog-level failure
/// (unreachable/unauthorized) fails the whole job.
async fn run(
    state: &AppState,
    id: &str,
    org: &Org,
    up: &OrgUpstream,
    ctx: &Ctx,
    provider: Provider,
    namespace: Option<&str>,
) -> Result<()> {
    let repos = providers::enumerate_repos(provider, up, namespace).await?;
    // A generic upstream that lists nothing almost always means it doesn't expose
    // a usable `/v2/_catalog` (Docker Hub, GHCR, a multi-registry DOCR account) —
    // fail loudly with guidance rather than silently "completing" at 0/0. For an
    // API-backed provider an empty result is a genuinely empty namespace.
    if repos.is_empty() && provider == Provider::Generic {
        return Err(Error::bad_request(
            "no repositories found — this registry may not expose a usable /v2/_catalog; \
             pick a specific provider (DigitalOcean / GitHub) or check the host and credentials",
        ));
    }
    db::imports::set_repos_total(state.db(), id, repos.len() as i64).await?;

    futures::stream::iter(repos.iter())
        .for_each_concurrent(ctx.concurrency, |repo| async move {
            copy_repo(state, id, org, up, ctx, repo).await;
        })
        .await;
    Ok(())
}

/// Copy every tag of one repository (tags sequential; the parallelism is across
/// repos and within each image's blobs).
async fn copy_repo(state: &AppState, id: &str, org: &Org, up: &OrgUpstream, ctx: &Ctx, repo: &str) {
    let tags = match proxy::list_tags(up, repo).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(%repo, error = %e, "import: listing tags failed; skipping repo");
            let _ = db::imports::advance(state.db(), id, 1, 0, 0, 0).await;
            return;
        }
    };
    let _ = db::imports::add_tags_total(state.db(), id, tags.len() as i64).await;

    for tag in &tags {
        // Fresh visited-set per tag: breaks cycles and skips re-copying a
        // manifest already handled while resolving this tag's index.
        let mut visited = HashSet::new();
        match copy_reference(state, up, org, repo, tag, 0, &mut visited, ctx).await {
            Ok(c) => {
                let _ = db::imports::advance(state.db(), id, 0, 1, c.blobs, c.bytes).await;
            }
            Err(e) => {
                tracing::warn!(%repo, %tag, error = %e, "import: tag failed; skipping");
                let _ = db::imports::advance(state.db(), id, 0, 1, 0, 0).await;
            }
        }
    }
    let _ = db::imports::advance(state.db(), id, 1, 0, 0, 0).await;
}

/// Copy one manifest reference (tag or digest) and everything it points at into
/// the org's repo. For a manifest list / image index, recurse into each child
/// manifest first (so all platforms come over); for an image manifest, copy the
/// config + layer blobs concurrently (skipping any already present) then store
/// the manifest.
#[allow(clippy::too_many_arguments)]
async fn copy_reference(
    state: &AppState,
    up: &OrgUpstream,
    org: &Org,
    repo: &str,
    reference: &str,
    depth: usize,
    visited: &mut HashSet<String>,
    ctx: &Ctx,
) -> Result<Copied> {
    if depth > MAX_MANIFEST_DEPTH {
        return Err(Error::Other(anyhow::anyhow!(
            "manifest nesting exceeded {MAX_MANIFEST_DEPTH} levels at {repo}@{reference} (possible cycle)"
        )));
    }
    let Some((bytes, media_type)) = proxy::fetch_manifest(up, repo, reference).await? else {
        // Upstream 404 for a listed tag (raced deletion) — nothing to copy.
        return Ok(Copied::default());
    };

    // The upstream is untrusted: if we asked by digest, its content must hash to
    // that digest. Verify *before* recursing, so a bogus self-referential index
    // can't drive infinite recursion (the visited-set below is a second guard).
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    if manifests::is_digest(reference) && !crate::registry::digests_equal(reference, &digest) {
        return Err(Error::Other(anyhow::anyhow!(
            "upstream returned a manifest whose digest does not match {reference}"
        )));
    }
    if !visited.insert(digest) {
        // Already copied this exact manifest while resolving this tag.
        return Ok(Copied::default());
    }

    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Other(anyhow::anyhow!("upstream manifest is not JSON")))?;

    let mut copied = Copied::default();
    let children = manifests::parse_child_refs(&doc);
    if children.is_empty() {
        // Image manifest: copy config + layers concurrently (content-addressed,
        // dedup'd across the whole import via the per-digest lock). Use
        // `join_all`, not `try_join_all`: on an error we must NOT cancel the
        // sibling copies, or a sibling dropped mid-multipart-upload would skip
        // its abort and leak an in-progress upload. Let every copy finish (each
        // aborts its own multipart on failure), then surface the first error.
        let descs = blob_descriptors(&doc);
        let results = join_all(descs.iter().map(|(digest, size)| async move {
            copy_blob_once(state, &org.id, repo, digest, up, ctx)
                .await
                .map(|did_copy| (did_copy, *size))
        }))
        .await;
        for result in results {
            let (did_copy, size) = result?;
            if did_copy {
                copied.blobs += 1;
                copied.bytes += size;
            }
        }
    } else {
        // Manifest list / index: bring across every referenced platform manifest.
        for child in &children {
            let c = Box::pin(copy_reference(
                state,
                up,
                org,
                repo,
                child,
                depth + 1,
                visited,
                ctx,
            ))
            .await?;
            copied.blobs += c.blobs;
            copied.bytes += c.bytes;
        }
    }

    // Store this manifest (image or index) last, after its dependencies exist.
    proxy::store_manifest(state, org, repo, reference, &bytes, &media_type).await?;
    Ok(copied)
}

/// Copy one blob into the org exactly once, even under concurrency. Returns
/// `true` if this call actually downloaded it (for progress accounting), `false`
/// if it was already present. The per-digest lock plus the re-check means a base
/// layer shared by many images is fetched a single time.
async fn copy_blob_once(
    state: &AppState,
    org_id: &str,
    repo: &str,
    digest: &str,
    up: &OrgUpstream,
    ctx: &Ctx,
) -> Result<bool> {
    if db::content::blob_exists(state.db(), org_id, digest).await? {
        return Ok(false);
    }
    let lock = {
        let mut map = ctx.inflight.lock().unwrap();
        map.entry(digest.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;
    // Another task may have finished this blob while we waited for the lock.
    if db::content::blob_exists(state.db(), org_id, digest).await? {
        return Ok(false);
    }
    let _permit = ctx
        .blob_sem
        .acquire()
        .await
        .map_err(|_| Error::Other(anyhow::anyhow!("import semaphore closed")))?;
    proxy::cache_blob(state, org_id, repo, digest, up).await?;
    Ok(true)
}

/// Whether `repo` sits under the namespace `prefix`: either the repo *is* the
/// prefix, or it's a child path (`prefix/…`). A plain `starts_with` would wrongly
/// match `my-registry-staging` against prefix `my-registry`, so we require a `/`
/// boundary.
pub(crate) fn repo_under_prefix(repo: &str, prefix: &str) -> bool {
    repo == prefix
        || repo
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Collect `(digest, size)` for the config + layer blobs of an image manifest.
fn blob_descriptors(doc: &Value) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let mut push = |desc: &Value| {
        if let Some(d) = desc.get("digest").and_then(|d| d.as_str()) {
            let size = desc.get("size").and_then(|s| s.as_i64()).unwrap_or(0);
            out.push((d.to_string(), size));
        }
    };
    if let Some(config) = doc.get("config") {
        push(config);
    }
    if let Some(layers) = doc.get("layers").and_then(|l| l.as_array()) {
        for layer in layers {
            push(layer);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::repo_under_prefix;

    #[test]
    fn prefix_matches_namespace_not_substring() {
        // Exact match and child paths are in.
        assert!(repo_under_prefix("my-registry", "my-registry"));
        assert!(repo_under_prefix("my-registry/api", "my-registry"));
        assert!(repo_under_prefix("my-registry/team/api", "my-registry"));
        // A shared string prefix that isn't a path boundary is out.
        assert!(!repo_under_prefix("my-registry-staging/api", "my-registry"));
        assert!(!repo_under_prefix("other/api", "my-registry"));
        // Multi-segment prefixes work too.
        assert!(repo_under_prefix("team/sub/app", "team/sub"));
        assert!(!repo_under_prefix("team/subfoo/app", "team/sub"));
    }
}
