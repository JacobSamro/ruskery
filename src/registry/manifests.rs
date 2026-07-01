//! Manifest handling. Manifest bytes are stored in SQLite for instant serving
//! and tag resolution; their referenced blobs live in Tigris.

use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::cache::CachedManifest;
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

pub(crate) fn is_digest(reference: &str) -> bool {
    reference.contains(':')
}

/// A syntactically valid digest: `<algo>:<hex>`, non-empty algorithm and an
/// all-hex digest (we only emit sha256 but accept any algorithm name).
fn is_valid_digest(d: &str) -> bool {
    match d.split_once(':') {
        Some((algo, hex)) => {
            !algo.is_empty() && !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => false,
    }
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
    let child_refs = parse_child_refs(&doc);
    let subject = parse_subject(&doc);

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

    let links = db::content::ManifestLinks {
        blobs: &blob_refs,
        children: &child_refs,
        subject: subject.as_ref().map(|(d, at)| (d.as_str(), at.clone())),
    };
    db::content::put_manifest(state.db(), &repo.id, &digest, &media_type, &body, &links).await?;

    if !is_digest(reference) {
        db::content::upsert_tag(state.db(), &repo.id, reference, &digest).await?;
    }

    // Keep the read cache consistent with the write: drop any stale bytes for
    // this digest (media type could differ on re-push) and refresh the tag.
    state.cache().invalidate_manifest(&repo.id, &digest);
    if !is_digest(reference) {
        state.cache().put_tag(&repo.id, reference, &digest);
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

    let mut resp = (
        StatusCode::CREATED,
        [
            (header::LOCATION, format!("/v2/{name}/manifests/{digest}")),
            (
                header::HeaderName::from_static("docker-content-digest"),
                digest.clone(),
            ),
        ],
    )
        .into_response();
    // OCI 1.1: echo the subject digest so clients know referrers are supported.
    if let Some((subject_digest, _)) = &subject {
        if let Ok(v) = header::HeaderValue::from_str(subject_digest) {
            resp.headers_mut()
                .insert(header::HeaderName::from_static("oci-subject"), v);
        }
    }
    Ok(resp)
}

/// `GET`/`HEAD /v2/<name>/manifests/<reference>`. Serves from local storage;
/// on a miss, if the org is a pull-through cache, fetches + caches from the
/// upstream and serves that.
pub async fn get(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    reference: &str,
    head_only: bool,
) -> Result<Response> {
    if let Some(resp) = serve_local(state, org, repo_name, reference, head_only).await? {
        return Ok(resp);
    }

    // Local miss: if this org mirrors an upstream, fetch + cache the manifest,
    // then serve the freshly-cached copy.
    if let Some(mut up) = db::orgs::org_upstream(state.db(), &org.id).await? {
        up.trusted_realm_hosts = state.config().import.trusted_realm_hosts.clone();
        crate::proxy::cache_manifest(state, org, repo_name, reference, &up).await?;
        if let Some(resp) = serve_local(state, org, repo_name, reference, head_only).await? {
            return Ok(resp);
        }
    }

    Err(manifest_unknown())
}

/// Serve a manifest from local storage (DB + read cache). Returns `Ok(None)` —
/// not an error — when the repo, tag, or manifest isn't present locally, so the
/// caller can decide whether to consult an upstream.
async fn serve_local(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    reference: &str,
    head_only: bool,
) -> Result<Option<Response>> {
    let Some(repo) = db::orgs::find_repo(state.db(), &org.id, repo_name).await? else {
        return Ok(None);
    };

    // Snapshot the cache generation before any DB read so a populate that races
    // a concurrent delete/re-push is dropped instead of caching stale content.
    let gen = state.cache().generation();

    // Resolve the reference to a digest. Tag→digest is mutable, but the push and
    // delete paths invalidate it, so a cache hit is authoritative.
    let digest = if is_digest(reference) {
        reference.to_string()
    } else if let Some(d) = state.cache().get_tag(&repo.id, reference) {
        d
    } else {
        match db::content::tag_digest(state.db(), &repo.id, reference).await? {
            Some(d) => {
                state
                    .cache()
                    .put_tag_if_current(&repo.id, reference, &d, gen);
                d
            }
            None => return Ok(None),
        }
    };

    // Manifest bytes are content-addressed: a cached `(repo, digest)` entry is
    // immutable, so the read can be served straight from memory.
    let manifest = if let Some(m) = state.cache().get_manifest(&repo.id, &digest) {
        m
    } else {
        match db::content::get_manifest_by_digest(state.db(), &repo.id, &digest).await? {
            Some(m) => {
                let cached = Arc::new(CachedManifest {
                    media_type: m.media_type,
                    size: m.size,
                    content: Bytes::from(m.content),
                });
                state
                    .cache()
                    .put_manifest_if_current(&repo.id, &digest, cached.clone(), gen);
                cached
            }
            None => return Ok(None),
        }
    };

    let common = [
        (header::CONTENT_TYPE, manifest.media_type.clone()),
        (header::CONTENT_LENGTH, manifest.size.to_string()),
        (
            header::HeaderName::from_static("docker-content-digest"),
            digest,
        ),
    ];

    Ok(Some(if head_only {
        (StatusCode::OK, common).into_response()
    } else {
        (StatusCode::OK, common, manifest.content.clone()).into_response()
    }))
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

    // Drop the deleted bytes and every tag resolution for this repo (a deleted
    // digest may have had several tags pointing at it), so a follow-up pull
    // re-resolves from SQLite and correctly 404s.
    state.cache().invalidate_manifest(&repo.id, &digest);
    state.cache().invalidate_repo_tags(&repo.id);

    Ok(StatusCode::ACCEPTED.into_response())
}

/// `GET /v2/<name>/referrers/<digest>` (OCI 1.1) — an image index of the
/// manifests whose `subject` is `<digest>`. Returns an empty index (200) when
/// the repo or subject is unknown, per the spec. `artifact_type` filters the
/// list and, when set, adds the `OCI-Filters-Applied` header.
pub async fn referrers(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    subject_digest: &str,
    artifact_type: Option<&str>,
) -> Result<Response> {
    if !is_valid_digest(subject_digest) {
        return Err(Error::oci(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            "invalid digest",
        ));
    }

    let refs = match db::orgs::find_repo(state.db(), &org.id, repo_name).await? {
        Some(repo) => {
            let mut refs =
                db::content::list_referrers(state.db(), &repo.id, subject_digest).await?;
            if let Some(at) = artifact_type {
                refs.retain(|r| r.artifact_type == at);
            }
            refs
        }
        None => Vec::new(),
    };

    let manifests: Vec<serde_json::Value> = refs
        .iter()
        .map(|r| {
            let mut entry = serde_json::json!({
                "mediaType": r.media_type,
                "digest": r.digest,
                "size": r.size,
            });
            if !r.artifact_type.is_empty() {
                entry["artifactType"] = serde_json::Value::String(r.artifact_type.clone());
            }
            // Surface the referrer's annotations in its descriptor (OCI).
            if let Some(ann) = serde_json::from_slice::<serde_json::Value>(&r.content)
                .ok()
                .and_then(|m| m.get("annotations").cloned())
                .filter(|a| a.is_object())
            {
                entry["annotations"] = ann;
            }
            entry
        })
        .collect();
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": manifests,
    });
    let body = serde_json::to_vec(&index).unwrap_or_default();

    let mut resp = (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/vnd.oci.image.index.v1+json",
        )],
        body,
    )
        .into_response();
    if artifact_type.is_some() {
        resp.headers_mut().insert(
            header::HeaderName::from_static("oci-filters-applied"),
            header::HeaderValue::from_static("artifactType"),
        );
    }
    Ok(resp)
}

/// Collect the blob digests (config + layers) a manifest references. Manifest
/// indexes (which reference other manifests, not blobs) yield no blob refs here.
pub(crate) fn parse_blob_refs_value(v: &serde_json::Value) -> Vec<String> {
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

/// Child manifest digests referenced by an image index / manifest list.
pub(crate) fn parse_child_refs(v: &serde_json::Value) -> Vec<String> {
    v.get("manifests")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("digest").and_then(|d| d.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The `subject` a manifest refers to (OCI 1.1), with its artifact type. The
/// artifact type is `artifactType` when present, else the config media type
/// (the OCI fallback for image manifests used as artifacts).
pub(crate) fn parse_subject(v: &serde_json::Value) -> Option<(String, String)> {
    let subject = v.get("subject")?.get("digest")?.as_str()?.to_string();
    let artifact_type = v
        .get("artifactType")
        .and_then(|a| a.as_str())
        .or_else(|| {
            v.get("config")
                .and_then(|c| c.get("mediaType"))
                .and_then(|m| m.as_str())
        })
        .unwrap_or("")
        .to_string();
    Some((subject, artifact_type))
}
