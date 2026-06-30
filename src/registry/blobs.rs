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
pub async fn head(state: &AppState, org_id: &str, digest: &str) -> Result<Response> {
    let size = db::content::blob_size(state.db(), org_id, digest)
        .await?
        .ok_or_else(blob_unknown)?;
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
pub async fn get(state: &AppState, org_id: &str, digest: &str) -> Result<Response> {
    if !db::content::blob_exists(state.db(), org_id, digest).await? {
        return Err(blob_unknown());
    }
    let key = Storage::blob_key(org_id, digest);
    let url = state.storage().presign_get(&key).await?;
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

/// `DELETE /v2/<name>/blobs/<digest>`
pub async fn delete(state: &AppState, org_id: &str, digest: &str) -> Result<Response> {
    if !db::content::blob_exists(state.db(), org_id, digest).await? {
        return Err(blob_unknown());
    }
    let key = Storage::blob_key(org_id, digest);
    let _ = state.storage().delete(&key).await;
    db::content::delete_blob(state.db(), org_id, digest).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}
