//! Queries for OCI content: blobs (metadata), manifests (bytes + refs), tags.

use crate::error::Result;
use crate::util::now_rfc3339;

use super::Db;

// ── blobs ──────────────────────────────────────────────────────────

pub async fn blob_exists(db: &Db, org_id: &str, digest: &str) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM blobs WHERE org_id = ? AND digest = ?")
        .bind(org_id)
        .bind(digest)
        .fetch_optional(db)
        .await?;
    Ok(row.is_some())
}

pub async fn blob_size(db: &Db, org_id: &str, digest: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT size FROM blobs WHERE org_id = ? AND digest = ?")
            .bind(org_id)
            .bind(digest)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0))
}

pub async fn record_blob(db: &Db, org_id: &str, digest: &str, size: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO blobs (org_id, digest, size, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(org_id, digest) DO NOTHING",
    )
    .bind(org_id)
    .bind(digest)
    .bind(size)
    .bind(now_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete_blob(db: &Db, org_id: &str, digest: &str) -> Result<()> {
    sqlx::query("DELETE FROM blobs WHERE org_id = ? AND digest = ?")
        .bind(org_id)
        .bind(digest)
        .execute(db)
        .await?;
    Ok(())
}

/// Blobs not referenced by any manifest within their org and older than
/// `created_before` (RFC3339). The age cutoff is a grace window so a blob that
/// was just uploaded but not yet referenced by a manifest isn't collected out
/// from under an in-flight push.
pub async fn unreferenced_blobs(db: &Db, created_before: &str) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT b.org_id, b.digest FROM blobs b
         WHERE b.created_at < ?
           AND NOT EXISTS (
            SELECT 1 FROM manifest_blobs mb
            JOIN repositories r ON r.id = mb.repo_id
            WHERE r.org_id = b.org_id AND mb.blob_digest = b.digest
         )",
    )
    .bind(created_before)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

// ── manifests ──────────────────────────────────────────────────────

pub struct StoredManifest {
    pub media_type: String,
    pub size: i64,
    pub content: Vec<u8>,
}

/// Everything a manifest references, recorded for GC, multi-arch index integrity
/// and the referrers API.
#[derive(Default)]
pub struct ManifestLinks<'a> {
    /// Config + layer blob digests (image manifests).
    pub blobs: &'a [String],
    /// Child manifest digests (image indexes / manifest lists).
    pub children: &'a [String],
    /// `(subject_digest, artifact_type)` when the manifest carries a `subject`.
    pub subject: Option<(&'a str, String)>,
}

/// Store (or replace) a manifest and everything it references, atomically.
pub async fn put_manifest(
    db: &Db,
    repo_id: &str,
    digest: &str,
    media_type: &str,
    content: &[u8],
    links: &ManifestLinks<'_>,
) -> Result<()> {
    let now = now_rfc3339();
    let mut tx = db.begin().await?;

    sqlx::query(
        "INSERT INTO manifests (repo_id, digest, media_type, size, content, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(repo_id, digest) DO UPDATE SET
            media_type = excluded.media_type,
            size = excluded.size,
            content = excluded.content",
    )
    .bind(repo_id)
    .bind(digest)
    .bind(media_type)
    .bind(content.len() as i64)
    .bind(content)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    // Re-record blob references.
    sqlx::query("DELETE FROM manifest_blobs WHERE repo_id = ? AND manifest_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    for blob in links.blobs {
        sqlx::query(
            "INSERT INTO manifest_blobs (repo_id, manifest_digest, blob_digest)
             VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(repo_id)
        .bind(digest)
        .bind(blob)
        .execute(&mut *tx)
        .await?;
    }

    // Re-record child-manifest references (image index).
    sqlx::query("DELETE FROM manifest_manifests WHERE repo_id = ? AND manifest_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    for child in links.children {
        sqlx::query(
            "INSERT INTO manifest_manifests (repo_id, manifest_digest, child_digest)
             VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(repo_id)
        .bind(digest)
        .bind(child)
        .execute(&mut *tx)
        .await?;
    }

    // Re-record this manifest's referrer link (its `subject`, if any).
    sqlx::query("DELETE FROM manifest_referrers WHERE repo_id = ? AND referrer_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    if let Some((subject, artifact_type)) = &links.subject {
        sqlx::query(
            "INSERT INTO manifest_referrers (repo_id, subject_digest, referrer_digest, artifact_type)
             VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(repo_id)
        .bind(subject)
        .bind(digest)
        .bind(artifact_type)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// A manifest that refers to a subject (for the referrers API).
pub struct Referrer {
    pub digest: String,
    pub media_type: String,
    pub size: i64,
    pub artifact_type: String,
    /// The referrer manifest bytes, so its `annotations` can be surfaced in the
    /// referrers descriptor (per the OCI spec).
    pub content: Vec<u8>,
}

/// Manifests in `repo_id` whose `subject` is `subject_digest`.
pub async fn list_referrers(db: &Db, repo_id: &str, subject_digest: &str) -> Result<Vec<Referrer>> {
    let rows: Vec<(String, String, i64, String, Vec<u8>)> = sqlx::query_as(
        "SELECT m.digest, m.media_type, m.size, r.artifact_type, m.content
         FROM manifest_referrers r
         JOIN manifests m ON m.repo_id = r.repo_id AND m.digest = r.referrer_digest
         WHERE r.repo_id = ? AND r.subject_digest = ?
         ORDER BY m.created_at",
    )
    .bind(repo_id)
    .bind(subject_digest)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(digest, media_type, size, artifact_type, content)| Referrer {
                digest,
                media_type,
                size,
                artifact_type,
                content,
            },
        )
        .collect())
}

pub async fn get_manifest_by_digest(
    db: &Db,
    repo_id: &str,
    digest: &str,
) -> Result<Option<StoredManifest>> {
    let row: Option<(String, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT media_type, size, content FROM manifests WHERE repo_id = ? AND digest = ?",
    )
    .bind(repo_id)
    .bind(digest)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(media_type, size, content)| StoredManifest {
        media_type,
        size,
        content,
    }))
}

/// Resolve a tag to its manifest digest.
pub async fn tag_digest(db: &Db, repo_id: &str, tag: &str) -> Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT manifest_digest FROM tags WHERE repo_id = ? AND name = ?")
            .bind(repo_id)
            .bind(tag)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0))
}

pub async fn upsert_tag(db: &Db, repo_id: &str, tag: &str, digest: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO tags (repo_id, name, manifest_digest, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(repo_id, name) DO UPDATE SET
            manifest_digest = excluded.manifest_digest,
            updated_at = excluded.updated_at",
    )
    .bind(repo_id)
    .bind(tag)
    .bind(digest)
    .bind(now_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_tags(db: &Db, repo_id: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM tags WHERE repo_id = ? ORDER BY name")
            .bind(repo_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn delete_manifest(db: &Db, repo_id: &str, digest: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM tags WHERE repo_id = ? AND manifest_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM manifest_blobs WHERE repo_id = ? AND manifest_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM manifest_manifests WHERE repo_id = ? AND manifest_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM manifest_referrers WHERE repo_id = ? AND referrer_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM manifests WHERE repo_id = ? AND digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
