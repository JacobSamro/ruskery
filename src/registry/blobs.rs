//! Blob delivery: HEAD (existence), GET (307 redirect to a presigned Tigris
//! URL so bytes stream from the CDN), and DELETE.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::db;
use crate::error::{Error, Result};
use crate::state::AppState;
use crate::storage::Storage;

fn blob_unknown() -> Error {
    Error::oci(
        StatusCode::NOT_FOUND,
        "BLOB_UNKNOWN",
        "blob unknown to registry",
    )
}

/// `HEAD /v2/<name>/blobs/<digest>`
pub async fn head(state: &AppState, org_id: &str, repo: &str, digest: &str) -> Result<Response> {
    let size = match db::content::blob_size(state.db(), org_id, digest).await? {
        Some(s) => s,
        None => ensure_cached(state, org_id, repo, digest).await?,
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_LENGTH, size.to_string()),
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::HeaderName::from_static("docker-content-digest"),
                digest.to_string(),
            ),
        ],
    )
        .into_response())
}

/// `GET /v2/<name>/blobs/<digest>` — redirect to a short-lived presigned URL.
/// A single `blob_size` read serves both the existence check and the analytics
/// `blob.serve` byte attribution (recorded here, not in the dispatch layer, so
/// the hot path does just one DB read).
pub async fn get(
    state: &AppState,
    org_id: &str,
    repo: &str,
    user_id: &str,
    digest: &str,
) -> Result<Response> {
    let size = match db::content::blob_size(state.db(), org_id, digest).await? {
        Some(s) => s,
        None => ensure_cached(state, org_id, repo, digest).await?,
    };
    let key = Storage::blob_key(org_id, digest);
    let url = state.storage().presign_get(&key).await?;
    state.usage().record(
        org_id,
        repo,
        Some(user_id),
        crate::analytics::Kind::BlobServe,
        size,
    );
    Ok((
        StatusCode::TEMPORARY_REDIRECT,
        [
            (header::LOCATION, url),
            (
                header::HeaderName::from_static("docker-content-digest"),
                digest.to_string(),
            ),
        ],
    )
        .into_response())
}

/// Resolve a blob that isn't stored locally: if the org is a pull-through cache,
/// fetch it from the upstream into object storage and return its size; otherwise
/// it's genuinely unknown. Blobs are content-addressed, so a cached copy is
/// digest-verified by [`crate::proxy::cache_blob`] before it's recorded.
async fn ensure_cached(state: &AppState, org_id: &str, repo: &str, digest: &str) -> Result<i64> {
    if let Some(mut up) = db::orgs::org_upstream(state.db(), org_id).await? {
        up.trusted_realm_hosts = state.config().import.trusted_realm_hosts.clone();
        crate::proxy::cache_blob(state, org_id, repo, digest, &up).await?;
        if let Some(size) = db::content::blob_size(state.db(), org_id, digest).await? {
            return Ok(size);
        }
    }
    Err(blob_unknown())
}

/// `DELETE /v2/<name>/blobs/<digest>`
pub async fn delete(state: &AppState, org_id: &str, digest: &str) -> Result<Response> {
    if !db::content::blob_exists(state.db(), org_id, digest).await? {
        return Err(blob_unknown());
    }
    // Blobs are org-scoped and deduplicated across repos, so a blob still
    // referenced by *any* manifest in the org must not be removed — otherwise an
    // admin on one repo could break another repo's images (and even this repo's).
    // Only genuinely unreferenced blobs are deletable here; GC handles the rest.
    if db::content::blob_referenced(state.db(), org_id, digest).await? {
        return Err(Error::oci(
            StatusCode::CONFLICT,
            "DENIED",
            "blob is still referenced by one or more manifests",
        ));
    }
    let key = Storage::blob_key(org_id, digest);
    let _ = state.storage().delete(&key).await;
    db::content::delete_blob(state.db(), org_id, digest).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}
