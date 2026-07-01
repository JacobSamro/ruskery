//! Queries for orgs, teams, memberships, repositories, and RBAC resolution.

use serde::Serialize;

use crate::error::Result;
use crate::models::{OrgRole, Permission};
use crate::util::now_rfc3339;

use super::Db;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Org {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Repository {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Team {
    pub id: String,
    pub org_id: String,
    pub slug: String,
    pub name: String,
    pub created_at: String,
}

pub async fn find_org_by_slug(db: &Db, slug: &str) -> Result<Option<Org>> {
    let o = sqlx::query_as::<_, Org>("SELECT * FROM orgs WHERE lower(slug) = lower(?)")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    Ok(o)
}

pub async fn create_org(db: &Db, slug: &str, name: &str) -> Result<Org> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    sqlx::query("INSERT INTO orgs (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(slug)
        .bind(name)
        .bind(&now)
        .execute(db)
        .await?;
    Ok(Org {
        id,
        slug: slug.into(),
        name: name.into(),
        created_at: now,
    })
}

/// An org's upstream-mirror configuration (set when it is a pull-through cache).
#[derive(Debug, Clone)]
pub struct OrgUpstream {
    /// Base URL of the upstream registry, e.g. `https://registry-1.docker.io`.
    pub url: String,
    /// Optional credentials for a private upstream.
    pub username: Option<String>,
    pub password: Option<String>,
}

/// The org's upstream-mirror config, or `None` if it is a normal (writable) org.
pub async fn org_upstream(db: &Db, org_id: &str) -> Result<Option<OrgUpstream>> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT upstream_url, upstream_username, upstream_password FROM orgs WHERE id = ?",
    )
    .bind(org_id)
    .fetch_optional(db)
    .await?;
    let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
    Ok(row.and_then(|(url, username, password)| {
        nonempty(url).map(|url| OrgUpstream {
            url: url.trim_end_matches('/').to_string(),
            username: nonempty(username),
            password: nonempty(password),
        })
    }))
}

/// Whether an org is a pull-through cache (and therefore read-only). Cheaper
/// than [`org_upstream`] when only the boolean is needed (e.g. a push guard).
pub async fn org_is_proxy(db: &Db, org_id: &str) -> Result<bool> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT upstream_url FROM orgs WHERE id = ?")
            .bind(org_id)
            .fetch_optional(db)
            .await?;
    Ok(row
        .and_then(|r| r.0)
        .map(|u| !u.is_empty())
        .unwrap_or(false))
}

/// Set (or clear, with `None` url) an org's upstream-mirror configuration.
pub async fn set_org_upstream(
    db: &Db,
    org_id: &str,
    url: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE orgs SET upstream_url = ?, upstream_username = ?, upstream_password = ? WHERE id = ?",
    )
    .bind(url)
    .bind(username)
    .bind(password)
    .bind(org_id)
    .execute(db)
    .await?;
    Ok(())
}

/// An org's storage-quota override in bytes, if one is set. `None` means the
/// org has no override and the instance default ([quota] default_storage_bytes)
/// applies; `Some(0)` means explicitly unlimited for this org.
pub async fn org_quota_bytes(db: &Db, org_id: &str) -> Result<Option<i64>> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT storage_quota_bytes FROM orgs WHERE id = ?")
            .bind(org_id)
            .fetch_optional(db)
            .await?;
    Ok(row.and_then(|r| r.0))
}

/// Set (`Some`) or clear (`None`, falling back to the instance default) an org's
/// storage-quota override.
pub async fn set_org_quota(db: &Db, org_id: &str, bytes: Option<i64>) -> Result<()> {
    sqlx::query("UPDATE orgs SET storage_quota_bytes = ? WHERE id = ?")
        .bind(bytes)
        .bind(org_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn add_org_member(db: &Db, org_id: &str, user_id: &str, role: OrgRole) -> Result<()> {
    sqlx::query(
        "INSERT INTO org_members (org_id, user_id, role) VALUES (?, ?, ?)
         ON CONFLICT(org_id, user_id) DO UPDATE SET role = excluded.role",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(role.as_str())
    .execute(db)
    .await?;
    Ok(())
}

/// The user's org-level role, if they are a member.
pub async fn org_role(db: &Db, org_id: &str, user_id: &str) -> Result<Option<OrgRole>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM org_members WHERE org_id = ? AND user_id = ?")
            .bind(org_id)
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    Ok(row.and_then(|r| OrgRole::parse(&r.0)))
}

pub async fn find_repo(db: &Db, org_id: &str, name: &str) -> Result<Option<Repository>> {
    let r =
        sqlx::query_as::<_, Repository>("SELECT * FROM repositories WHERE org_id = ? AND name = ?")
            .bind(org_id)
            .bind(name)
            .fetch_optional(db)
            .await?;
    Ok(r)
}

/// Create a repository, or return the existing one if it already exists.
/// Idempotent (`ON CONFLICT DO NOTHING` on the `(org_id, name)` unique index) so
/// two racing creates — whether two dashboard clicks or two concurrent first
/// pushes — can't collide on a duplicate-key insert.
pub async fn create_repo(db: &Db, org_id: &str, name: &str) -> Result<Repository> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let res = sqlx::query(
        "INSERT INTO repositories (id, org_id, name, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(org_id, name) DO NOTHING",
    )
    .bind(&id)
    .bind(org_id)
    .bind(name)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;

    if res.rows_affected() == 1 {
        Ok(Repository {
            id,
            org_id: org_id.into(),
            name: name.into(),
            created_at: now.clone(),
            updated_at: now,
        })
    } else {
        // The row already existed (a concurrent create won the race) — return it.
        find_repo(db, org_id, name)
            .await?
            .ok_or(crate::error::Error::NotFound)
    }
}

/// Fully-qualified repository names (`<org-slug>/<repo>`) visible to a user,
/// i.e. across every org they belong to. Used by `/v2/_catalog`.
pub async fn catalog_for_user(db: &Db, user_id: &str) -> Result<Vec<String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT o.slug, r.name
         FROM repositories r
         JOIN orgs o ON o.id = r.org_id
         JOIN org_members m ON m.org_id = o.id
         WHERE m.user_id = ?
         ORDER BY o.slug, r.name",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(slug, name)| format!("{slug}/{name}"))
        .collect())
}

/// The highest permission `user_id` has on an existing repository, combining the
/// org-level role (owner/admin ⇒ admin) with any team grants on that repo.
pub async fn repo_permission(
    db: &Db,
    repo_id: &str,
    org_id: &str,
    user_id: &str,
) -> Result<Permission> {
    if let Some(role) = org_role(db, org_id, user_id).await? {
        if role >= OrgRole::Admin {
            return Ok(Permission::Admin);
        }
    } else {
        // Not a member of the org → no access at all.
        return Ok(Permission::None);
    }

    // Member: take the max team grant on this repo.
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT trp.permission
         FROM team_repo_perms trp
         JOIN team_members tm ON tm.team_id = trp.team_id
         WHERE trp.repo_id = ? AND tm.user_id = ?",
    )
    .bind(repo_id)
    .bind(user_id)
    .fetch_all(db)
    .await?;

    let perm = rows
        .iter()
        .filter_map(|(p,)| Permission::parse(p))
        .max()
        .unwrap_or(Permission::None);
    Ok(perm)
}

// ───────────────────────── dashboard queries ─────────────────────────

/// Orgs a user belongs to, paired with their role, most-recent first.
pub async fn orgs_for_user(db: &Db, user_id: &str) -> Result<Vec<(Org, OrgRole)>> {
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT o.id, o.slug, o.name, o.created_at, m.role
         FROM orgs o JOIN org_members m ON m.org_id = o.id
         WHERE m.user_id = ? ORDER BY o.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, slug, name, created_at, role)| {
            (
                Org {
                    id,
                    slug,
                    name,
                    created_at,
                },
                OrgRole::parse(&role).unwrap_or(OrgRole::Member),
            )
        })
        .collect())
}

#[derive(Serialize)]
pub struct OrgSummary {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub created_at: String,
    pub member_count: i64,
    pub repo_count: i64,
}

/// Every organization with member + repository counts — the super-admin view.
pub async fn list_all_with_counts(db: &Db) -> Result<Vec<OrgSummary>> {
    let rows: Vec<(String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT o.id, o.slug, o.name, o.created_at,
                (SELECT COUNT(*) FROM org_members m WHERE m.org_id = o.id),
                (SELECT COUNT(*) FROM repositories r WHERE r.org_id = o.id)
         FROM orgs o ORDER BY o.created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, slug, name, created_at, member_count, repo_count)| OrgSummary {
                id,
                slug,
                name,
                created_at,
                member_count,
                repo_count,
            },
        )
        .collect())
}

#[derive(Serialize)]
pub struct MemberRow {
    pub user_id: String,
    pub username: String,
    pub email: String,
    pub role: String,
}

pub async fn list_members(db: &Db, org_id: &str) -> Result<Vec<MemberRow>> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT u.id, u.username, u.email, m.role
         FROM org_members m JOIN users u ON u.id = m.user_id
         WHERE m.org_id = ? ORDER BY u.username",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(user_id, username, email, role)| MemberRow {
            user_id,
            username,
            email,
            role,
        })
        .collect())
}

pub async fn set_member_role(db: &Db, org_id: &str, user_id: &str, role: OrgRole) -> Result<()> {
    add_org_member(db, org_id, user_id, role).await
}

pub async fn remove_member(db: &Db, org_id: &str, user_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM org_members WHERE org_id = ? AND user_id = ?")
        .bind(org_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

// ── teams ──

pub async fn list_teams(db: &Db, org_id: &str) -> Result<Vec<Team>> {
    let teams = sqlx::query_as::<_, Team>("SELECT * FROM teams WHERE org_id = ? ORDER BY name")
        .bind(org_id)
        .fetch_all(db)
        .await?;
    Ok(teams)
}

pub async fn find_team(db: &Db, org_id: &str, slug: &str) -> Result<Option<Team>> {
    let t = sqlx::query_as::<_, Team>(
        "SELECT * FROM teams WHERE org_id = ? AND lower(slug) = lower(?)",
    )
    .bind(org_id)
    .bind(slug)
    .fetch_optional(db)
    .await?;
    Ok(t)
}

pub async fn create_team(db: &Db, org_id: &str, slug: &str, name: &str) -> Result<Team> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    sqlx::query("INSERT INTO teams (id, org_id, slug, name, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(org_id)
        .bind(slug)
        .bind(name)
        .bind(&now)
        .execute(db)
        .await?;
    Ok(Team {
        id,
        org_id: org_id.into(),
        slug: slug.into(),
        name: name.into(),
        created_at: now,
    })
}

pub async fn add_team_member(db: &Db, team_id: &str, user_id: &str, role: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES (?, ?, ?)
         ON CONFLICT(team_id, user_id) DO UPDATE SET role = excluded.role",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(role)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn remove_team_member(db: &Db, team_id: &str, user_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM team_members WHERE team_id = ? AND user_id = ?")
        .bind(team_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn list_team_members(db: &Db, team_id: &str) -> Result<Vec<MemberRow>> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT u.id, u.username, u.email, tm.role
         FROM team_members tm JOIN users u ON u.id = tm.user_id
         WHERE tm.team_id = ? ORDER BY u.username",
    )
    .bind(team_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(user_id, username, email, role)| MemberRow {
            user_id,
            username,
            email,
            role,
        })
        .collect())
}

#[derive(Serialize)]
pub struct TeamPermRow {
    pub repo: String,
    pub permission: String,
}

pub async fn set_team_repo_perm(
    db: &Db,
    team_id: &str,
    repo_id: &str,
    permission: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO team_repo_perms (team_id, repo_id, permission) VALUES (?, ?, ?)
         ON CONFLICT(team_id, repo_id) DO UPDATE SET permission = excluded.permission",
    )
    .bind(team_id)
    .bind(repo_id)
    .bind(permission)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_team_perms(db: &Db, team_id: &str) -> Result<Vec<TeamPermRow>> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT r.name, p.permission
         FROM team_repo_perms p JOIN repositories r ON r.id = p.repo_id
         WHERE p.team_id = ? ORDER BY r.name",
    )
    .bind(team_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(repo, permission)| TeamPermRow { repo, permission })
        .collect())
}

// ── repositories (dashboard views) ──

#[derive(Serialize)]
pub struct RepoSummary {
    pub name: String,
    pub tag_count: i64,
    pub updated_at: String,
}

pub async fn list_repos(db: &Db, org_id: &str) -> Result<Vec<RepoSummary>> {
    let rows = sqlx::query_as::<_, (String, i64, String)>(
        "SELECT r.name,
                (SELECT COUNT(*) FROM tags t WHERE t.repo_id = r.id) AS tag_count,
                r.updated_at
         FROM repositories r WHERE r.org_id = ? ORDER BY r.updated_at DESC",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, tag_count, updated_at)| RepoSummary {
            name,
            tag_count,
            updated_at,
        })
        .collect())
}

#[derive(Serialize)]
pub struct TagDetail {
    pub tag: String,
    pub digest: String,
    pub size: i64,
    pub updated_at: String,
    /// Number of times this image (digest) has been pulled. Populated by the
    /// caller from `image_pull_counts`; 0 until then.
    #[serde(default)]
    pub pull_count: i64,
}

/// Tags for a repo with the manifest digest, total image size (manifest +
/// referenced blob sizes), and update time.
pub async fn repo_tag_details(db: &Db, repo_id: &str) -> Result<Vec<TagDetail>> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT name, manifest_digest, updated_at FROM tags WHERE repo_id = ? ORDER BY name",
    )
    .bind(repo_id)
    .fetch_all(db)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (tag, digest, updated_at) in rows {
        let (size,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(
                (SELECT size FROM manifests WHERE repo_id = ?1 AND digest = ?2), 0)
              + COALESCE((SELECT SUM(b.size)
                 FROM manifest_blobs mb
                 JOIN repositories r ON r.id = mb.repo_id
                 JOIN blobs b ON b.org_id = r.org_id AND b.digest = mb.blob_digest
                 WHERE mb.repo_id = ?1 AND mb.manifest_digest = ?2), 0)",
        )
        .bind(repo_id)
        .bind(&digest)
        .fetch_one(db)
        .await?;
        out.push(TagDetail {
            tag,
            digest,
            size,
            updated_at,
            pull_count: 0,
        });
    }
    Ok(out)
}

/// Increment the pull counter for one image (manifest digest). Best-effort: run
/// off the request path. Keyed by org + repo name + digest so the pull hot path
/// needs no id lookup.
pub async fn bump_image_pull(db: &Db, org_id: &str, repo: &str, digest: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO image_pulls (org_id, repo, digest, count) VALUES (?, ?, ?, 1)
         ON CONFLICT(org_id, repo, digest) DO UPDATE SET count = count + 1",
    )
    .bind(org_id)
    .bind(repo)
    .bind(digest)
    .execute(db)
    .await?;
    Ok(())
}

/// Pull counts for every image in a repo, as `(digest, count)`.
pub async fn image_pull_counts(db: &Db, org_id: &str, repo: &str) -> Result<Vec<(String, i64)>> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT digest, count FROM image_pulls WHERE org_id = ? AND repo = ?",
    )
    .bind(org_id)
    .bind(repo)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn delete_repo(db: &Db, org_id: &str, name: &str) -> Result<()> {
    sqlx::query("DELETE FROM image_pulls WHERE org_id = ? AND repo = ?")
        .bind(org_id)
        .bind(name)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM repositories WHERE org_id = ? AND name = ?")
        .bind(org_id)
        .bind(name)
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Serialize)]
pub struct OrgStats {
    pub repos: i64,
    pub members: i64,
    pub teams: i64,
}

pub async fn org_stats(db: &Db, org_id: &str) -> Result<OrgStats> {
    let (repos,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM repositories WHERE org_id = ?")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    let (members,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM org_members WHERE org_id = ?")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    let (teams,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM teams WHERE org_id = ?")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    Ok(OrgStats {
        repos,
        members,
        teams,
    })
}
