//! Bulk registry-to-registry import: copy every repository (all tags, all
//! architectures) from an upstream OCI registry into a target org, in the
//! background. Essentially a server-side `skopeo sync` of a whole catalog.
//!
//! It reuses the pull-through cache machinery in [`crate::proxy`] — the upstream
//! bearer-token dance, digest-verified blob streaming into object storage, and
//! content-addressed manifest recording — but drives it *eagerly*: enumerate the
//! catalog (`/v2/_catalog`), then for every tag copy the manifest, recursing
//! into manifest-list children so multi-arch images come across whole. Blobs
//! already present under the org are skipped (dedup), so re-running is cheap.
//!
//! Credentials live only in the running task; only the upstream URL and progress
//! counters are persisted (see `db::imports`).

use std::collections::HashSet;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db;
use crate::db::orgs::{Org, OrgUpstream};
use crate::error::{Error, Result};
use crate::proxy;
use crate::registry::manifests;
use crate::state::AppState;

/// Guard against a malicious upstream serving a self-referential or absurdly
/// nested index. Real image indexes are one level deep (index → platform
/// manifests); nested indexes are rare and shallow.
const MAX_MANIFEST_DEPTH: usize = 8;

/// What a single manifest (and its children) copied, for progress accounting.
#[derive(Default)]
struct Copied {
    blobs: i64,
    bytes: i64,
}

/// Kick off an import in the background. The task owns `up` (with credentials)
/// for its lifetime and updates the `imports` row as it goes.
pub fn spawn(state: AppState, id: String, org: Org, up: OrgUpstream) {
    tokio::spawn(async move {
        tracing::info!(import = %id, upstream = %up.url, org = %org.slug, "import started");
        let outcome = run(&state, &id, &org, &up).await;
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

/// Enumerate the catalog and copy each repository's tags. A failure listing or
/// copying a single repo/tag is logged and skipped (progress still advances) so
/// one bad image can't abort a large migration; only a catalog-level failure
/// (unreachable/unauthorized) fails the whole job.
async fn run(state: &AppState, id: &str, org: &Org, up: &OrgUpstream) -> Result<()> {
    let repos = proxy::list_catalog(up).await?;
    db::imports::set_repos_total(state.db(), id, repos.len() as i64).await?;

    for repo in &repos {
        let tags = match proxy::list_tags(up, repo).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(%repo, error = %e, "import: listing tags failed; skipping repo");
                let _ = db::imports::advance(state.db(), id, 1, 0, 0, 0).await;
                continue;
            }
        };
        db::imports::add_tags_total(state.db(), id, tags.len() as i64).await?;

        for tag in &tags {
            // Fresh visited-set per tag: breaks cycles and skips re-copying a
            // manifest already handled while resolving this tag's index.
            let mut visited = HashSet::new();
            match copy_reference(state, up, org, repo, tag, 0, &mut visited).await {
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
    Ok(())
}

/// Copy one manifest reference (tag or digest) and everything it points at into
/// the org's repo. For a manifest list / image index, recurse into each child
/// manifest first (so all platforms come over); for an image manifest, copy the
/// config + layer blobs (skipping any already present) then store the manifest.
async fn copy_reference(
    state: &AppState,
    up: &OrgUpstream,
    org: &Org,
    repo: &str,
    reference: &str,
    depth: usize,
    visited: &mut HashSet<String>,
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
        // Image manifest: copy config + layers (content-addressed, dedup'd).
        for (digest, size) in blob_descriptors(&doc) {
            if !db::content::blob_exists(state.db(), &org.id, &digest).await? {
                proxy::cache_blob(state, &org.id, repo, &digest, up).await?;
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
