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
