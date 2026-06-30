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

/// Blobs not referenced by any manifest within their org — candidates for GC.
pub async fn unreferenced_blobs(db: &Db) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT b.org_id, b.digest FROM blobs b
         WHERE NOT EXISTS (
            SELECT 1 FROM manifest_blobs mb
            JOIN repositories r ON r.id = mb.repo_id
            WHERE r.org_id = b.org_id AND mb.blob_digest = b.digest
         )",
    )
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

/// Store (or replace) a manifest and the set of blobs it references, atomically.
pub async fn put_manifest(
    db: &Db,
    repo_id: &str,
    digest: &str,
    media_type: &str,
    content: &[u8],
    blob_refs: &[String],
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

    sqlx::query("DELETE FROM manifest_blobs WHERE repo_id = ? AND manifest_digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;

    for blob in blob_refs {
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

    tx.commit().await?;
    Ok(())
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
    sqlx::query("DELETE FROM manifests WHERE repo_id = ? AND digest = ?")
        .bind(repo_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
