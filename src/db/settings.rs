//! Helpers for the `settings` key/value table, including the signing-secret
//! bootstrap used on first run.

use base64::Engine;
use rand::RngCore;

use super::Db;

/// Read a setting value by key.
pub async fn get(db: &Db, key: &str) -> anyhow::Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.0))
}

/// Upsert a setting value.
pub async fn set(db: &Db, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// Overlay any admin-saved storage overrides (from the `settings` table) on top
/// of the bootstrap config from the file/env. DB values win when present.
pub async fn effective_storage(
    db: &Db,
    base: &crate::config::StorageConfig,
) -> anyhow::Result<crate::config::StorageConfig> {
    let mut cfg = base.clone();
    if let Some(v) = get(db, "storage_endpoint").await? {
        cfg.endpoint = v;
    }
    if let Some(v) = get(db, "storage_bucket").await? {
        cfg.bucket = v;
    }
    if let Some(v) = get(db, "storage_region").await? {
        cfg.region = v;
    }
    if let Some(v) = get(db, "storage_access_key_id").await? {
        cfg.access_key_id = v;
    }
    if let Some(v) = get(db, "storage_secret_access_key").await? {
        cfg.secret_access_key = v;
    }
    if let Some(v) = get(db, "storage_cdn_url").await? {
        cfg.cdn_url = v;
    }
    if let Some(v) = get(db, "storage_force_path_style").await? {
        cfg.force_path_style = v == "true" || v == "1";
    }
    if let Some(v) = get(db, "storage_presign_ttl_secs").await? {
        if let Ok(n) = v.parse() {
            cfg.presign_ttl_secs = n;
        }
    }
    Ok(cfg)
}

/// The ACME contact email, preferring the dashboard-saved value over the
/// bootstrap config. Empty string means "none set".
pub async fn effective_contact_email(db: &Db, base: &str) -> anyhow::Result<String> {
    match get(db, "tls_contact_email").await? {
        Some(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Ok(base.trim().to_string()),
    }
}

/// Return the instance signing secret, generating and persisting a fresh 32-byte
/// random key on first run. A key from config (if non-empty) always wins.
pub async fn ensure_secret_key(db: &Db, configured: &str) -> anyhow::Result<Vec<u8>> {
    if !configured.is_empty() {
        return Ok(configured.as_bytes().to_vec());
    }
    if let Some(existing) = get(db, "secret_key").await? {
        let decoded = base64::engine::general_purpose::STANDARD.decode(existing.as_bytes())?;
        return Ok(decoded);
    }
    let mut key = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&key);
    set(db, "secret_key", &encoded).await?;
    Ok(key)
}
