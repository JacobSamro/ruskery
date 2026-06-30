//! Persistence for usage analytics: batched rollup upserts (written by the
//! in-memory collector's flush task) and the read queries that power the org
//! analytics dashboard. All reads are scoped by `org_id` for tenant isolation.

use serde::Serialize;

use super::Db;
use crate::error::Result;

/// (day, org_id, repo, kind, count, bytes)
pub type RepoRow = (String, String, String, String, i64, i64);
/// (day, org_id, user_id, kind, count, bytes)
pub type UserRow = (String, String, String, String, i64, i64);

/// Apply a drained batch of in-memory counters to the daily rollup tables in a
/// single transaction (one round trip per flush, not per event).
pub async fn flush_batch(db: &Db, repo_rows: &[RepoRow], user_rows: &[UserRow]) -> Result<()> {
    if repo_rows.is_empty() && user_rows.is_empty() {
        return Ok(());
    }
    let mut tx = db.begin().await?;
    for (day, org, repo, kind, count, bytes) in repo_rows {
        sqlx::query(
            "INSERT INTO usage_daily (day, org_id, repo, kind, count, bytes)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(day, org_id, repo, kind)
             DO UPDATE SET count = count + excluded.count, bytes = bytes + excluded.bytes",
        )
        .bind(day)
        .bind(org)
        .bind(repo)
        .bind(kind)
        .bind(count)
        .bind(bytes)
        .execute(&mut *tx)
        .await?;
    }
    for (day, org, user, kind, count, bytes) in user_rows {
        sqlx::query(
            "INSERT INTO usage_user_daily (day, org_id, user_id, kind, count, bytes)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(day, org_id, user_id, kind)
             DO UPDATE SET count = count + excluded.count, bytes = bytes + excluded.bytes",
        )
        .bind(day)
        .bind(org)
        .bind(user)
        .bind(kind)
        .bind(count)
        .bind(bytes)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Take a daily storage snapshot: an org-wide deduplicated total (`repo=''`) and
/// one row per repository (distinct blobs referenced by its manifests).
pub async fn snapshot_storage(db: &Db, day: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    // Org total — the `blobs` PK is (org_id, digest), so it is already dedup'd.
    sqlx::query(
        "INSERT INTO storage_daily (day, org_id, repo, bytes, blob_count)
         SELECT ?, org_id, '', COALESCE(SUM(size), 0), COUNT(*)
         FROM blobs GROUP BY org_id
         ON CONFLICT(day, org_id, repo)
         DO UPDATE SET bytes = excluded.bytes, blob_count = excluded.blob_count",
    )
    .bind(day)
    .execute(&mut *tx)
    .await?;
    // Per-repo — distinct blobs referenced by the repo's manifests.
    sqlx::query(
        "INSERT INTO storage_daily (day, org_id, repo, bytes, blob_count)
         SELECT ?, r.org_id, r.name, COALESCE(SUM(b.size), 0), COUNT(b.digest)
         FROM repositories r
         LEFT JOIN (SELECT DISTINCT repo_id, blob_digest FROM manifest_blobs) mb
                ON mb.repo_id = r.id
         LEFT JOIN blobs b ON b.org_id = r.org_id AND b.digest = mb.blob_digest
         GROUP BY r.id
         ON CONFLICT(day, org_id, repo)
         DO UPDATE SET bytes = excluded.bytes, blob_count = excluded.blob_count",
    )
    .bind(day)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// One-time backfill of push history from the audit log (`image.push` rows).
/// Pull history doesn't exist pre-instrumentation, so it starts at zero.
pub async fn backfill_pushes(db: &Db) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_daily (day, org_id, repo, kind, count, bytes)
         SELECT substr(ts, 1, 10), org_id, COALESCE(target, ''), 'manifest.push', COUNT(*), 0
         FROM audit_log
         WHERE action = 'image.push' AND org_id IS NOT NULL
         GROUP BY substr(ts, 1, 10), org_id, target
         ON CONFLICT(day, org_id, repo, kind) DO NOTHING",
    )
    .execute(db)
    .await?;
    sqlx::query(
        "INSERT INTO usage_user_daily (day, org_id, user_id, kind, count, bytes)
         SELECT substr(ts, 1, 10), org_id, actor_user_id, 'manifest.push', COUNT(*), 0
         FROM audit_log
         WHERE action = 'image.push' AND org_id IS NOT NULL AND actor_user_id IS NOT NULL
         GROUP BY substr(ts, 1, 10), org_id, actor_user_id
         ON CONFLICT(day, org_id, user_id, kind) DO NOTHING",
    )
    .execute(db)
    .await?;
    Ok(())
}

// ───────────────────────── read side (dashboard) ─────────────────────────

#[derive(Debug, Default, Serialize)]
pub struct Overview {
    pub pushes: i64,
    pub pulls: i64,
    pub blob_serves: i64,
    pub bytes_pushed: i64,
    pub bytes_served: i64,
    pub storage_bytes: i64,
    pub storage_blobs: i64,
}

pub async fn overview(db: &Db, org_id: &str, since: &str) -> Result<Overview> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT kind, COALESCE(SUM(count), 0), COALESCE(SUM(bytes), 0)
         FROM usage_daily WHERE org_id = ? AND day >= ? GROUP BY kind",
    )
    .bind(org_id)
    .bind(since)
    .fetch_all(db)
    .await?;
    let mut o = Overview::default();
    for (kind, count, bytes) in rows {
        match kind.as_str() {
            "manifest.push" => o.pushes = count,
            "manifest.pull" => o.pulls = count,
            "blob.upload" => o.bytes_pushed = bytes,
            "blob.serve" => {
                o.blob_serves = count;
                o.bytes_served = bytes;
            }
            _ => {}
        }
    }
    // Current storage is read live from `blobs` so it's accurate even before the
    // first daily snapshot.
    let (bytes, blobs): (i64, i64) =
        sqlx::query_as("SELECT COALESCE(SUM(size), 0), COUNT(*) FROM blobs WHERE org_id = ?")
            .bind(org_id)
            .fetch_one(db)
            .await?;
    o.storage_bytes = bytes;
    o.storage_blobs = blobs;
    Ok(o)
}

#[derive(Debug, Serialize)]
pub struct DayPoint {
    pub day: String,
    pub pushes: i64,
    pub pulls: i64,
}

pub async fn daily_series(db: &Db, org_id: &str, since: &str) -> Result<Vec<DayPoint>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT day,
                COALESCE(SUM(CASE WHEN kind = 'manifest.push' THEN count END), 0),
                COALESCE(SUM(CASE WHEN kind = 'manifest.pull' THEN count END), 0)
         FROM usage_daily WHERE org_id = ? AND day >= ? GROUP BY day ORDER BY day",
    )
    .bind(org_id)
    .bind(since)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(day, pushes, pulls)| DayPoint { day, pushes, pulls })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct StoragePoint {
    pub day: String,
    pub bytes: i64,
}

pub async fn storage_series(db: &Db, org_id: &str, since: &str) -> Result<Vec<StoragePoint>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT day, bytes FROM storage_daily
         WHERE org_id = ? AND repo = '' AND day >= ? ORDER BY day",
    )
    .bind(org_id)
    .bind(since)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(day, bytes)| StoragePoint { day, bytes })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct RepoStat {
    pub repo: String,
    pub pushes: i64,
    pub pulls: i64,
    pub bytes_served: i64,
    pub storage_bytes: i64,
}

pub async fn top_repos(db: &Db, org_id: &str, since: &str, limit: i64) -> Result<Vec<RepoStat>> {
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT repo,
                COALESCE(SUM(CASE WHEN kind = 'manifest.push' THEN count END), 0),
                COALESCE(SUM(CASE WHEN kind = 'manifest.pull' THEN count END), 0),
                COALESCE(SUM(CASE WHEN kind = 'blob.serve' THEN bytes END), 0)
         FROM usage_daily WHERE org_id = ? AND day >= ?
         GROUP BY repo ORDER BY 3 DESC, 2 DESC LIMIT ?",
    )
    .bind(org_id)
    .bind(since)
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (repo, pushes, pulls, bytes_served) in rows {
        // Latest snapshot size for the repo (0 if not snapshotted yet).
        let storage_bytes: i64 = sqlx::query_as::<_, (i64,)>(
            "SELECT bytes FROM storage_daily WHERE org_id = ? AND repo = ?
             ORDER BY day DESC LIMIT 1",
        )
        .bind(org_id)
        .bind(&repo)
        .fetch_optional(db)
        .await?
        .map(|r| r.0)
        .unwrap_or(0);
        out.push(RepoStat {
            repo,
            pushes,
            pulls,
            bytes_served,
            storage_bytes,
        });
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct UserStat {
    pub user_id: String,
    pub username: String,
    pub pushes: i64,
    pub pulls: i64,
}

pub async fn top_users(db: &Db, org_id: &str, since: &str, limit: i64) -> Result<Vec<UserStat>> {
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT u.id, u.username,
                COALESCE(SUM(CASE WHEN ud.kind = 'manifest.push' THEN ud.count END), 0),
                COALESCE(SUM(CASE WHEN ud.kind = 'manifest.pull' THEN ud.count END), 0)
         FROM usage_user_daily ud JOIN users u ON u.id = ud.user_id
         WHERE ud.org_id = ? AND ud.day >= ?
         GROUP BY u.id ORDER BY 3 + 4 DESC LIMIT ?",
    )
    .bind(org_id)
    .bind(since)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(user_id, username, pushes, pulls)| UserStat {
            user_id,
            username,
            pushes,
            pulls,
        })
        .collect())
}
