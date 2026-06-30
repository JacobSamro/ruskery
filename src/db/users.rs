//! Queries for users, dashboard sessions, and personal access tokens.

use crate::auth::pat;
use crate::error::Result;
use crate::models::User;
use crate::util::{now_rfc3339, random_id, rfc3339_in};

use super::Db;

/// Total number of user accounts (used to detect first-run setup state).
pub async fn count(db: &Db) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(db)
        .await?;
    Ok(n)
}

/// All users (instance-admin view for the dashboard).
pub async fn list_all(db: &Db) -> Result<Vec<User>> {
    let users = sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY username")
        .fetch_all(db)
        .await?;
    Ok(users)
}

#[allow(dead_code)]
pub async fn find_by_id(db: &Db, id: &str) -> Result<Option<User>> {
    let u = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(u)
}

/// Look up by email or username, case-insensitively.
pub async fn find_by_login(db: &Db, login: &str) -> Result<Option<User>> {
    let u = sqlx::query_as::<_, User>(
        "SELECT * FROM users WHERE lower(email) = lower(?1) OR lower(username) = lower(?1)",
    )
    .bind(login)
    .fetch_optional(db)
    .await?;
    Ok(u)
}

/// Create a user. `password_hash` must already be an Argon2id PHC string.
pub async fn create(
    db: &Db,
    email: &str,
    username: &str,
    password_hash: &str,
    is_admin: bool,
) -> Result<User> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    sqlx::query(
        "INSERT INTO users (id, email, username, password_hash, is_admin, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(email)
    .bind(username)
    .bind(password_hash)
    .bind(is_admin as i64)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(User {
        id,
        email: email.into(),
        username: username.into(),
        password_hash: password_hash.into(),
        is_admin,
        created_at: now,
    })
}

// ───────────────────────── sessions ─────────────────────────

/// Create a dashboard session and return its id (the cookie value references it).
pub async fn create_session(db: &Db, user_id: &str, ttl_secs: i64) -> Result<String> {
    let id = random_id();
    sqlx::query("INSERT INTO sessions (id, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(user_id)
        .bind(now_rfc3339())
        .bind(rfc3339_in(ttl_secs))
        .execute(db)
        .await?;
    Ok(id)
}

/// Resolve a session id to its user, if the session exists and is unexpired.
pub async fn user_for_session(db: &Db, session_id: &str) -> Result<Option<User>> {
    let u = sqlx::query_as::<_, User>(
        "SELECT u.* FROM users u
         JOIN sessions s ON s.user_id = u.id
         WHERE s.id = ? AND s.expires_at > ?",
    )
    .bind(session_id)
    .bind(now_rfc3339())
    .fetch_optional(db)
    .await?;
    Ok(u)
}

pub async fn delete_session(db: &Db, session_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

// ───────────────────────── personal access tokens ─────────────────────────

/// Create a PAT for a user and return the one-time plaintext.
pub async fn create_pat(db: &Db, user_id: &str, name: &str) -> Result<String> {
    let token = pat::generate();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO personal_access_tokens (id, user_id, name, token_prefix, token_hash, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(&token.display_prefix)
    .bind(&token.hash)
    .bind(now_rfc3339())
    .execute(db)
    .await?;
    Ok(token.plaintext)
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct PatRow {
    pub id: String,
    pub name: String,
    pub token_prefix: String,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

pub async fn list_pats(db: &Db, user_id: &str) -> Result<Vec<PatRow>> {
    let rows = sqlx::query_as::<_, PatRow>(
        "SELECT id, name, token_prefix, last_used_at, created_at
         FROM personal_access_tokens WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Delete a PAT, scoped to its owner so users can't revoke others' tokens.
pub async fn delete_pat(db: &Db, user_id: &str, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM personal_access_tokens WHERE user_id = ? AND id = ?")
        .bind(user_id)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Resolve a PAT plaintext to its owning user, updating last-used and honoring expiry.
pub async fn user_for_pat(db: &Db, plaintext: &str) -> Result<Option<User>> {
    let h = pat::hash(plaintext);
    let now = now_rfc3339();
    let user = sqlx::query_as::<_, User>(
        "SELECT u.* FROM users u
         JOIN personal_access_tokens p ON p.user_id = u.id
         WHERE p.token_hash = ? AND (p.expires_at IS NULL OR p.expires_at > ?)",
    )
    .bind(&h)
    .bind(&now)
    .fetch_optional(db)
    .await?;
    if user.is_some() {
        sqlx::query("UPDATE personal_access_tokens SET last_used_at = ? WHERE token_hash = ?")
            .bind(&now)
            .bind(&h)
            .execute(db)
            .await?;
    }
    Ok(user)
}
