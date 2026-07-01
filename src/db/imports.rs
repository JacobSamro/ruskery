//! Bulk registry-import jobs: create, progress bookkeeping, and listing. The
//! actual copy work runs in a background task (see `crate::import`); this module
//! only owns the persisted job rows and their progress counters.

use serde::Serialize;

use crate::error::Result;
use crate::util::now_rfc3339;

use super::Db;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Import {
    pub id: String,
    pub org_id: String,
    pub upstream: String,
    pub status: String, // running | completed | failed
    pub repos_total: i64,
    pub repos_done: i64,
    pub tags_total: i64,
    pub tags_done: i64,
    pub blobs_done: i64,
    pub bytes_done: i64,
    pub error: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn create(
    db: &Db,
    id: &str,
    org_id: &str,
    upstream: &str,
    created_by: &str,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "INSERT INTO imports (id, org_id, upstream, status, created_by, created_at, updated_at)
         VALUES (?, ?, ?, 'running', ?, ?, ?)",
    )
    .bind(id)
    .bind(org_id)
    .bind(upstream)
    .bind(created_by)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(())
}

/// Set the total repository count once the upstream catalog is known.
pub async fn set_repos_total(db: &Db, id: &str, total: i64) -> Result<()> {
    sqlx::query("UPDATE imports SET repos_total = ?, updated_at = ? WHERE id = ?")
        .bind(total)
        .bind(now_rfc3339())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Add to the total tag count as each repo's tag list is discovered.
pub async fn add_tags_total(db: &Db, id: &str, delta: i64) -> Result<()> {
    sqlx::query("UPDATE imports SET tags_total = tags_total + ?, updated_at = ? WHERE id = ?")
        .bind(delta)
        .bind(now_rfc3339())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record progress after a tag (and its blobs) finish copying. Deltas are added
/// atomically so the counters stay correct without reading them back first.
pub async fn advance(
    db: &Db,
    id: &str,
    repos_done: i64,
    tags_done: i64,
    blobs_done: i64,
    bytes_done: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE imports SET
            repos_done = repos_done + ?,
            tags_done  = tags_done  + ?,
            blobs_done = blobs_done + ?,
            bytes_done = bytes_done + ?,
            updated_at = ?
         WHERE id = ?",
    )
    .bind(repos_done)
    .bind(tags_done)
    .bind(blobs_done)
    .bind(bytes_done)
    .bind(now_rfc3339())
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Terminal update: `completed` (error None) or `failed` (error Some).
pub async fn finish(db: &Db, id: &str, status: &str, error: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE imports SET status = ?, error = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(error)
        .bind(now_rfc3339())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn list(db: &Db, org_id: &str) -> Result<Vec<Import>> {
    let rows = sqlx::query_as::<_, Import>(
        "SELECT * FROM imports WHERE org_id = ? ORDER BY created_at DESC LIMIT 50",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// On startup, no import task can still be running (the process just started), so
/// any row left `running` was interrupted by a restart — mark it failed.
pub async fn fail_interrupted(db: &Db) -> Result<()> {
    sqlx::query(
        "UPDATE imports SET status = 'failed', error = 'interrupted by a server restart', updated_at = ?
         WHERE status = 'running'",
    )
    .bind(now_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}
