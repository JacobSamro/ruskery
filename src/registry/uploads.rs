//! Blob upload sessions implementing the OCI chunked/monolithic upload flow.
//!
//! Bytes are streamed straight into a Tigris multipart upload at a temporary
//! key while a running SHA-256 is computed. On finalize we verify the digest,
//! then server-side-copy the object to its content-addressed key. Session state
//! lives in memory (single process); a restart simply asks the client to retry.

use std::sync::Arc;

use aws_sdk_s3::types::CompletedPart;
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::db;
use crate::error::{Error, Result};
use crate::state::AppState;
use crate::storage::Storage;

/// S3 multipart part size. Parts (except the last) must be ≥ 5 MiB.
const PART_SIZE: usize = 8 * 1024 * 1024;

/// In-memory map of active upload sessions.
#[derive(Clone, Default)]
pub struct UploadRegistry {
    inner: Arc<DashMap<String, Arc<Mutex<UploadSession>>>>,
}

impl UploadRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, id: &str) -> Option<Arc<Mutex<UploadSession>>> {
        self.inner.get(id).map(|e| e.clone())
    }

    fn insert(&self, id: String, session: UploadSession) {
        self.inner.insert(id, Arc::new(Mutex::new(session)));
    }

    fn remove(&self, id: &str) {
        self.inner.remove(id);
    }
}

/// Mutable state for one in-progress upload.
pub struct UploadSession {
    pub org_id: String,
    #[allow(dead_code)] // retained for diagnostics/logging
    pub name: String, // full repository name "<org>/<repo>"
    pub temp_key: String,
    pub s3_upload_id: String,
    pub hasher: Sha256,
    pub buffer: Vec<u8>,
    pub parts: Vec<CompletedPart>,
    pub next_part: i32,
    pub total: u64,
}

impl UploadSession {
    /// Append bytes, flushing full multipart parts to storage as they fill.
    async fn write(&mut self, storage: &Storage, chunk: &[u8]) -> Result<()> {
        self.hasher.update(chunk);
        self.total += chunk.len() as u64;
        self.buffer.extend_from_slice(chunk);
        while self.buffer.len() >= PART_SIZE {
            let part: Vec<u8> = self.buffer.drain(..PART_SIZE).collect();
            let completed = storage
                .upload_part(&self.temp_key, &self.s3_upload_id, self.next_part, part)
                .await?;
            self.parts.push(completed);
            self.next_part += 1;
        }
        Ok(())
    }
}

/// `POST /v2/<name>/blobs/uploads/` — start an upload, or cross-repo mount.
pub async fn start(
    state: &AppState,
    org_id: &str,
    name: &str,
    mount: Option<&str>,
) -> Result<Response> {
    // Cross-repo mount: if the blob already exists in this org, link instantly.
    if let Some(digest) = mount {
        if db::content::blob_exists(state.db(), org_id, digest).await? {
            return Ok(mounted_response(name, digest));
        }
        // Mount miss → fall through to a normal upload session (per spec).
    }

    let upload_id = uuid::Uuid::new_v4().to_string();
    let temp_key = Storage::upload_key(org_id, &upload_id);
    let s3_upload_id = state.storage().create_multipart(&temp_key).await?;

    state.uploads().insert(
        upload_id.clone(),
        UploadSession {
            org_id: org_id.to_string(),
            name: name.to_string(),
            temp_key,
            s3_upload_id,
            hasher: Sha256::new(),
            buffer: Vec::new(),
            parts: Vec::new(),
            next_part: 1,
            total: 0,
        },
    );

    Ok(accepted_response(name, &upload_id, 0))
}

/// `PATCH /v2/<name>/blobs/uploads/<uuid>` — stream a chunk.
pub async fn patch(state: &AppState, name: &str, upload_id: &str, body: Body) -> Result<Response> {
    let session = state.uploads().get(upload_id).ok_or_else(|| {
        Error::oci(
            StatusCode::NOT_FOUND,
            "BLOB_UPLOAD_UNKNOWN",
            "unknown upload",
        )
    })?;
    let mut s = session.lock().await;
    let storage = state.storage();

    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::bad_request(format!("body read: {e}")))?;
        s.write(&storage, &chunk).await?;
    }

    let total = s.total;
    drop(s);
    Ok(accepted_response(name, upload_id, total))
}

/// `PUT /v2/<name>/blobs/uploads/<uuid>?digest=...` — append any final bytes,
/// verify the digest, and commit the blob to its content-addressed key.
pub async fn finish(
    state: &AppState,
    name: &str,
    upload_id: &str,
    expected_digest: &str,
    body: Body,
) -> Result<Response> {
    let session = state.uploads().get(upload_id).ok_or_else(|| {
        Error::oci(
            StatusCode::NOT_FOUND,
            "BLOB_UPLOAD_UNKNOWN",
            "unknown upload",
        )
    })?;
    let mut s = session.lock().await;
    let storage = state.storage();

    // Absorb any bytes sent with the final PUT.
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::bad_request(format!("body read: {e}")))?;
        s.write(&storage, &chunk).await?;
    }

    let computed = format!("sha256:{}", hex::encode(s.hasher.clone().finalize()));
    if !super::digests_equal(&computed, expected_digest) {
        let _ = storage.abort_multipart(&s.temp_key, &s.s3_upload_id).await;
        state.uploads().remove(upload_id);
        return Err(Error::oci(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            format!("digest mismatch: got {computed}, expected {expected_digest}"),
        ));
    }

    // Commit the temp object: tiny/empty uploads (no flushed parts) go via a
    // single PutObject; larger ones complete the multipart upload.
    if s.parts.is_empty() {
        let _ = storage.abort_multipart(&s.temp_key, &s.s3_upload_id).await;
        let body = std::mem::take(&mut s.buffer);
        storage.put(&s.temp_key, body).await?;
    } else {
        if !s.buffer.is_empty() {
            let last: Vec<u8> = std::mem::take(&mut s.buffer);
            let part = storage
                .upload_part(&s.temp_key, &s.s3_upload_id, s.next_part, last)
                .await?;
            s.parts.push(part);
        }
        let parts = std::mem::take(&mut s.parts);
        storage
            .complete_multipart(&s.temp_key, &s.s3_upload_id, parts)
            .await?;
    }

    // Move to the final content-addressed key, then drop the temp object.
    let final_key = Storage::blob_key(&s.org_id, &computed);
    storage.copy(&s.temp_key, &final_key).await?;
    let _ = storage.delete(&s.temp_key).await;

    db::content::record_blob(state.db(), &s.org_id, &computed, s.total as i64).await?;

    let total = s.total;
    let org_id = s.org_id.clone();
    drop(s);
    state.uploads().remove(upload_id);
    tracing::debug!(%name, digest = %computed, size = total, org = %org_id, "blob committed");

    Ok((
        StatusCode::CREATED,
        [
            (header::LOCATION, format!("/v2/{name}/blobs/{computed}")),
            (
                header::HeaderName::from_static("docker-content-digest"),
                computed.clone(),
            ),
        ],
    )
        .into_response())
}

/// `GET /v2/<name>/blobs/uploads/<uuid>` — report upload progress.
pub async fn status(state: &AppState, name: &str, upload_id: &str) -> Result<Response> {
    let session = state.uploads().get(upload_id).ok_or_else(|| {
        Error::oci(
            StatusCode::NOT_FOUND,
            "BLOB_UPLOAD_UNKNOWN",
            "unknown upload",
        )
    })?;
    let total = session.lock().await.total;
    Ok(accepted_response(name, upload_id, total))
}

/// `DELETE /v2/<name>/blobs/uploads/<uuid>` — cancel an upload.
pub async fn cancel(state: &AppState, upload_id: &str) -> Result<Response> {
    if let Some(session) = state.uploads().get(upload_id) {
        let s = session.lock().await;
        let _ = state
            .storage()
            .abort_multipart(&s.temp_key, &s.s3_upload_id)
            .await;
        drop(s);
        state.uploads().remove(upload_id);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── response builders ──────────────────────────────────────────────

fn accepted_response(name: &str, upload_id: &str, total: u64) -> Response {
    let range_end = total.saturating_sub(1);
    (
        StatusCode::ACCEPTED,
        [
            (
                header::LOCATION,
                format!("/v2/{name}/blobs/uploads/{upload_id}"),
            ),
            (header::RANGE, format!("0-{range_end}")),
            (
                header::HeaderName::from_static("docker-upload-uuid"),
                upload_id.to_string(),
            ),
            (header::CONTENT_LENGTH, "0".to_string()),
        ],
    )
        .into_response()
}

fn mounted_response(name: &str, digest: &str) -> Response {
    (
        StatusCode::CREATED,
        [
            (header::LOCATION, format!("/v2/{name}/blobs/{digest}")),
            (
                header::HeaderName::from_static("docker-content-digest"),
                digest.to_string(),
            ),
            (header::CONTENT_LENGTH, "0".to_string()),
        ],
    )
        .into_response()
}

/// Read a query parameter's raw (still percent-encoded) value.
pub fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}
