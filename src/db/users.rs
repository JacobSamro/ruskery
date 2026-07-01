//! Queries for users, dashboard sessions, and personal access tokens.

use crate::auth::pat;
use crate::error::Result;
use crate::models::{Permission, TokenScope, User};
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
/// Create the very first (super-admin) user, but only if no user exists yet.
/// Returns `None` if someone else already completed setup.
///
/// The insert is guarded by `WHERE (SELECT COUNT(*) FROM users) = 0` in the same
/// statement, so it's atomic against a concurrent first-run request: SQLite
/// serializes writers, so at most one such insert can affect a row — the loser
/// inserts zero rows and gets `None`. This closes the check-then-create race
/// where two racing setup requests could both create super-admins.
pub async fn create_first_admin(
    db: &Db,
    email: &str,
    username: &str,
    password_hash: &str,
) -> Result<Option<User>> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let res = sqlx::query(
        "INSERT INTO users (id, email, username, password_hash, is_admin, created_at)
         SELECT ?, ?, ?, ?, 1, ?
         WHERE (SELECT COUNT(*) FROM users) = 0",
    )
    .bind(&id)
    .bind(email)
    .bind(username)
    .bind(password_hash)
    .bind(&now)
    .execute(db)
    .await?;
    if res.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(User {
        id,
        email: email.into(),
        username: username.into(),
        password_hash: password_hash.into(),
        is_admin: true,
        created_at: now,
    }))
}

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

/// Create a PAT for a user and return the one-time plaintext. `scope_org_id` /
/// `scope_repo_id` narrow the token; pass `None`/`"all"` for full access.
#[allow(clippy::too_many_arguments)]
pub async fn create_pat(
    db: &Db,
    user_id: &str,
    name: &str,
    scope_kind: &str,
    scope_org_id: Option<&str>,
    scope_repo_id: Option<&str>,
    max_perm: &str,
) -> Result<String> {
    let token = pat::generate();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO personal_access_tokens
            (id, user_id, name, token_prefix, token_hash, created_at,
             scope_kind, scope_org_id, scope_repo_id, max_perm)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(&token.display_prefix)
    .bind(&token.hash)
    .bind(now_rfc3339())
    .bind(scope_kind)
    .bind(scope_org_id)
    .bind(scope_repo_id)
    .bind(max_perm)
    .execute(db)
    .await?;
    Ok(token.plaintext)
}

#[derive(serde::Serialize)]
pub struct PatRow {
    pub id: String,
    pub name: String,
    pub token_prefix: String,
    pub last_used_at: Option<String>,
    pub created_at: String,
    /// Human-readable scope: "all", "<org>", or "<org>/<repo>".
    pub scope: String,
    /// Permission cap: "pull", "push", or "admin".
    pub max_perm: String,
}

pub async fn list_pats(db: &Db, user_id: &str) -> Result<Vec<PatRow>> {
    type Row = (
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    let rows = sqlx::query_as::<_, Row>(
        "SELECT p.id, p.name, p.token_prefix, p.last_used_at, p.created_at,
                p.scope_kind, o.slug, r.name, ro.slug, p.max_perm
         FROM personal_access_tokens p
         LEFT JOIN orgs o ON o.id = p.scope_org_id
         LEFT JOIN repositories r ON r.id = p.scope_repo_id
         LEFT JOIN orgs ro ON ro.id = r.org_id
         WHERE p.user_id = ? ORDER BY p.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                name,
                token_prefix,
                last_used_at,
                created_at,
                kind,
                org_slug,
                repo_name,
                repo_org_slug,
                max_perm,
            )| {
                let scope = match kind.as_str() {
                    "org" => org_slug.unwrap_or_else(|| "all".into()),
                    "repo" => match (repo_org_slug, repo_name) {
                        (Some(o), Some(r)) => format!("{o}/{r}"),
                        _ => "all".into(),
                    },
                    _ => "all".into(),
                };
                PatRow {
                    id,
                    name,
                    token_prefix,
                    last_used_at,
                    created_at,
                    scope,
                    max_perm,
                }
            },
        )
        .collect())
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

/// Resolve a PAT plaintext to its owning user, resource scope, and permission
/// cap, updating last-used and honoring expiry.
pub async fn user_for_pat(
    db: &Db,
    plaintext: &str,
) -> Result<Option<(User, TokenScope, Permission)>> {
    let h = pat::hash(plaintext);
    let now = now_rfc3339();
    type Row = (
        String,         // u.id
        String,         // u.email
        String,         // u.username
        String,         // u.password_hash
        bool,           // u.is_admin
        String,         // u.created_at
        String,         // p.scope_kind
        Option<String>, // p.scope_org_id
        Option<String>, // p.scope_repo_id
        String,         // p.max_perm
    );
    let row = sqlx::query_as::<_, Row>(
        "SELECT u.id, u.email, u.username, u.password_hash, u.is_admin, u.created_at,
                p.scope_kind, p.scope_org_id, p.scope_repo_id, p.max_perm
         FROM users u
         JOIN personal_access_tokens p ON p.user_id = u.id
         WHERE p.token_hash = ? AND (p.expires_at IS NULL OR p.expires_at > ?)",
    )
    .bind(&h)
    .bind(&now)
    .fetch_optional(db)
    .await?;

    let Some((
        id,
        email,
        username,
        password_hash,
        is_admin,
        created_at,
        kind,
        org_id,
        repo_id,
        max_perm,
    )) = row
    else {
        return Ok(None);
    };
    let user = User {
        id,
        email,
        username,
        password_hash,
        is_admin,
        created_at,
    };
    sqlx::query("UPDATE personal_access_tokens SET last_used_at = ? WHERE token_hash = ?")
        .bind(&now)
        .bind(&h)
        .execute(db)
        .await?;

    let scope = match kind.as_str() {
        "org" => org_id.map(TokenScope::Org).unwrap_or(TokenScope::All),
        "repo" => repo_id.map(TokenScope::Repo).unwrap_or(TokenScope::All),
        _ => TokenScope::All,
    };
    let cap = Permission::parse(&max_perm).unwrap_or(Permission::Admin);
    Ok(Some((user, scope, cap)))
}
