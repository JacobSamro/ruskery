//! Custom domain management for the instance (used by automatic TLS / ACME).

use serde::Serialize;

use crate::error::Result;
use crate::util::now_rfc3339;

use super::Db;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Domain {
    pub domain: String,
    pub status: String, // pending | active | failed
    pub is_primary: bool,
    pub detail: Option<String>,
    pub created_at: String,
}

pub async fn list(db: &Db) -> Result<Vec<Domain>> {
    let rows = sqlx::query_as::<_, Domain>(
        "SELECT domain, status, is_primary, detail, created_at FROM domains ORDER BY is_primary DESC, created_at",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The set of domain names eligible for certificate issuance (everything we know
/// about — ACME will only succeed for those whose DNS actually points here).
pub async fn allowlist(db: &Db) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT domain FROM domains")
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn add(db: &Db, domain: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO domains (domain, status, is_primary, created_at) VALUES (?, 'pending', 0, ?)
         ON CONFLICT(domain) DO NOTHING",
    )
    .bind(domain)
    .bind(now_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete(db: &Db, domain: &str) -> Result<()> {
    sqlx::query("DELETE FROM domains WHERE domain = ?")
        .bind(domain)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_primary(db: &Db, domain: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE domains SET is_primary = 0")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE domains SET is_primary = 1 WHERE domain = ?")
        .bind(domain)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[allow(dead_code)] // used by the ACME event handler to surface cert status
pub async fn set_status(db: &Db, domain: &str, status: &str, detail: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE domains SET status = ?, detail = ? WHERE domain = ?")
        .bind(status)
        .bind(detail)
        .bind(domain)
        .execute(db)
        .await?;
    Ok(())
}
