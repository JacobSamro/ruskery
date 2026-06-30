//! Garbage collection: delete blobs no longer referenced by any manifest.
//!
//! Manifests record their referenced blobs in `manifest_blobs`, so a blob is
//! collectable once no manifest points at it (within its org). Deletion removes
//! the object from Tigris and the row from the database.

use crate::db;
use crate::state::AppState;
use crate::storage::Storage;

/// Run one GC sweep, returning the number of blobs collected.
pub async fn run(state: &AppState) -> anyhow::Result<usize> {
    let cutoff = crate::util::rfc3339_in(-state.config().gc.grace_secs);
    let unreferenced = db::content::unreferenced_blobs(state.db(), &cutoff).await?;
    let mut collected = 0;
    tracing::debug!(count = unreferenced.len(), "gc: unreferenced blobs found");
    for (org_id, digest) in unreferenced {
        let key = Storage::blob_key(&org_id, &digest);
        tracing::debug!(%org_id, %digest, "gc: collecting blob");
        if let Err(e) = state.storage().delete(&key).await {
            tracing::warn!(error = %e, key, "gc: failed to delete object; skipping db row");
            continue;
        }
        db::content::delete_blob(state.db(), &org_id, &digest).await?;
        collected += 1;
    }
    if collected > 0 {
        tracing::info!(collected, "gc sweep complete");
    }
    Ok(collected)
}

/// Background GC loop, runs every `interval_secs` (0 disables it).
pub async fn background(state: AppState, interval_secs: u64) {
    if interval_secs == 0 {
        return;
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    loop {
        tick.tick().await;
        if let Err(e) = run(&state).await {
            tracing::error!(error = %e, "background gc failed");
        }
    }
}
