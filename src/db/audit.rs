//! Append-only audit log.

use serde::Serialize;

use crate::error::Result;
use crate::util::now_rfc3339;

use super::Db;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AuditRow {
    pub id: i64,
    pub ts: String,
    pub actor_user_id: Option<String>,
    pub org_id: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

/// Record an audit event. Failures are swallowed by callers (auditing must never
/// break the primary operation).
pub async fn record(
    db: &Db,
    actor_user_id: Option<&str>,
    org_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (ts, actor_user_id, org_id, action, target, detail)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(now_rfc3339())
    .bind(actor_user_id)
    .bind(org_id)
    .bind(action)
    .bind(target)
    .bind(detail)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list(db: &Db, org_id: &str, limit: i64) -> Result<Vec<AuditRow>> {
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, ts, actor_user_id, org_id, action, target, detail
         FROM audit_log WHERE org_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(org_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}
