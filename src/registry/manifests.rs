//! Manifest handling. Manifest bytes are stored in SQLite for instant serving
//! and tag resolution; their referenced blobs live in Tigris.

use axum::body::Bytes;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::db;
use crate::db::orgs::Org;
use crate::error::{Error, Result};
use crate::state::AppState;

fn manifest_unknown() -> Error {
    Error::oci(
        StatusCode::NOT_FOUND,
        "MANIFEST_UNKNOWN",
        "manifest unknown",
    )
}

fn is_digest(reference: &str) -> bool {
    reference.contains(':')
}

/// `PUT /v2/<name>/manifests/<reference>` — store a manifest, tagging it if the
/// reference is a tag.
#[allow(clippy::too_many_arguments)]
pub async fn put(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    name: &str,
    reference: &str,
    headers: &HeaderMap,
    body: Bytes,
    actor_user_id: &str,
) -> Result<Response> {
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
    if is_digest(reference) && !crate::registry::digests_equal(reference, &digest) {
        return Err(Error::oci(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            "provided digest does not match content",
        ));
    }

    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/vnd.oci.image.manifest.v1+json")
        .to_string();

    // All OCI manifests/indexes are JSON; reject anything that isn't, so we
    // never store junk that the pull/GC paths would treat as a real manifest.
    let doc: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        Error::oci(
            StatusCode::BAD_REQUEST,
            "MANIFEST_INVALID",
            "manifest is not valid JSON",
        )
    })?;

    let blob_refs = parse_blob_refs_value(&doc);

    // Ensure every referenced blob has actually been uploaded to this org.
    for r in &blob_refs {
        if !db::content::blob_exists(state.db(), &org.id, r).await? {
            return Err(Error::oci(
                StatusCode::BAD_REQUEST,
                "MANIFEST_BLOB_UNKNOWN",
                format!("referenced blob {r} is missing"),
            ));
        }
    }

    // Create the repository on first push.
    let repo = match db::orgs::find_repo(state.db(), &org.id, repo_name).await? {
        Some(r) => r,
        None => db::orgs::create_repo(state.db(), &org.id, repo_name).await?,
    };

    db::content::put_manifest(
        state.db(),
        &repo.id,
        &digest,
        &media_type,
        &body,
        &blob_refs,
    )
    .await?;

    if !is_digest(reference) {
        db::content::upsert_tag(state.db(), &repo.id, reference, &digest).await?;
    }

    db::audit::record(
        state.db(),
        Some(actor_user_id),
        Some(&org.id),
        "image.push",
        Some(&format!("{name}:{reference}")),
        Some(&digest),
    )
    .await
    .ok();

    Ok((
        StatusCode::CREATED,
        [
            (header::LOCATION, format!("/v2/{name}/manifests/{digest}")),
            (
                header::HeaderName::from_static("docker-content-digest"),
                digest,
            ),
        ],
    )
        .into_response())
}

/// `GET`/`HEAD /v2/<name>/manifests/<reference>`
pub async fn get(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    reference: &str,
    head_only: bool,
) -> Result<Response> {
    let repo = db::orgs::find_repo(state.db(), &org.id, repo_name)
        .await?
        .ok_or_else(manifest_unknown)?;

    let digest = if is_digest(reference) {
        reference.to_string()
    } else {
        db::content::tag_digest(state.db(), &repo.id, reference)
            .await?
            .ok_or_else(manifest_unknown)?
    };

    let m = db::content::get_manifest_by_digest(state.db(), &repo.id, &digest)
        .await?
        .ok_or_else(manifest_unknown)?;

    let common = [
        (header::CONTENT_TYPE, m.media_type),
        (header::CONTENT_LENGTH, m.size.to_string()),
        (
            header::HeaderName::from_static("docker-content-digest"),
            digest,
        ),
    ];

    if head_only {
        Ok((StatusCode::OK, common).into_response())
    } else {
        Ok((StatusCode::OK, common, m.content).into_response())
    }
}

/// `DELETE /v2/<name>/manifests/<reference>`
pub async fn delete(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    reference: &str,
) -> Result<Response> {
    let repo = db::orgs::find_repo(state.db(), &org.id, repo_name)
        .await?
        .ok_or_else(manifest_unknown)?;

    let digest = if is_digest(reference) {
        reference.to_string()
    } else {
        db::content::tag_digest(state.db(), &repo.id, reference)
            .await?
            .ok_or_else(manifest_unknown)?
    };

    db::content::delete_manifest(state.db(), &repo.id, &digest).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

/// Collect the blob digests (config + layers) a manifest references. Manifest
/// indexes (which reference other manifests, not blobs) yield no blob refs here.
fn parse_blob_refs_value(v: &serde_json::Value) -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(d) = v
        .get("config")
        .and_then(|c| c.get("digest"))
        .and_then(|d| d.as_str())
    {
        refs.push(d.to_string());
    }
    if let Some(layers) = v.get("layers").and_then(|l| l.as_array()) {
        for layer in layers {
            if let Some(d) = layer.get("digest").and_then(|d| d.as_str()) {
                refs.push(d.to_string());
            }
        }
    }
    refs
}
