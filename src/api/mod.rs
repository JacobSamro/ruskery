//! Dashboard REST API (`/api/v1`), authenticated by the session cookie.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{password, session, SessionUser};
use crate::db;
use crate::error::{Error, Result};
use crate::models::OrgRole;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup", post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/orgs", get(list_orgs).post(create_org))
        .route("/api/v1/orgs/{slug}", get(get_org))
        .route("/api/v1/orgs/{slug}/repos", get(list_repos))
        .route(
            "/api/v1/orgs/{slug}/repos/{*name}",
            get(get_repo).delete(delete_repo),
        )
        .route(
            "/api/v1/orgs/{slug}/members",
            get(list_members).post(add_member),
        )
        .route(
            "/api/v1/orgs/{slug}/members/{user_id}",
            post(set_role).delete(remove_member),
        )
        .route(
            "/api/v1/orgs/{slug}/teams",
            get(list_teams).post(create_team),
        )
        .route(
            "/api/v1/orgs/{slug}/teams/{team}/members",
            get(team_members).post(add_team_member),
        )
        .route(
            "/api/v1/orgs/{slug}/teams/{team}/members/{user_id}",
            axum::routing::delete(remove_team_member),
        )
        .route(
            "/api/v1/orgs/{slug}/teams/{team}/perms",
            get(team_perms).post(set_team_perm),
        )
        .route("/api/v1/tokens", get(list_tokens).post(create_token))
        .route("/api/v1/tokens/{id}", axum::routing::delete(delete_token))
        .route("/api/v1/domains", get(list_domains).post(add_domain))
        .route(
            "/api/v1/domains/{domain}",
            axum::routing::delete(delete_domain),
        )
        .route("/api/v1/domains/{domain}/primary", post(set_primary_domain))
        .route("/api/v1/orgs/{slug}/audit", get(list_audit))
        .route(
            "/api/v1/settings/storage",
            get(get_storage_settings).put(update_storage_settings),
        )
}

// ───────────────────────── helpers ─────────────────────────

fn json_ok<T: serde::Serialize>(v: T) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s != "_catalog"
}

/// Resolve an org the user belongs to, returning it with their role.
async fn member_of(
    state: &AppState,
    user_id: &str,
    slug: &str,
) -> Result<(db::orgs::Org, OrgRole)> {
    let org = db::orgs::find_org_by_slug(state.db(), slug)
        .await?
        .ok_or(Error::NotFound)?;
    let role = db::orgs::org_role(state.db(), &org.id, user_id)
        .await?
        .ok_or(Error::Forbidden)?;
    Ok((org, role))
}

/// Like [`member_of`] but requires an admin/owner role.
async fn admin_of(state: &AppState, user_id: &str, slug: &str) -> Result<(db::orgs::Org, OrgRole)> {
    let (org, role) = member_of(state, user_id, slug).await?;
    if role < OrgRole::Admin {
        return Err(Error::Forbidden);
    }
    Ok((org, role))
}

// ───────────────────────── setup / auth ─────────────────────────

async fn setup_status(State(state): State<AppState>) -> Result<Response> {
    let needs = db::users::count(state.db()).await? == 0;
    Ok(json_ok(json!({ "needs_setup": needs })))
}

#[derive(Deserialize)]
struct SetupReq {
    email: String,
    username: String,
    password: String,
    org_slug: String,
    org_name: String,
}

async fn setup(State(state): State<AppState>, Json(req): Json<SetupReq>) -> Result<Response> {
    if db::users::count(state.db()).await? != 0 {
        return Err(Error::conflict("setup already completed"));
    }
    if !valid_slug(&req.org_slug) {
        return Err(Error::bad_request("invalid org slug"));
    }
    if req.password.len() < 8 {
        return Err(Error::bad_request("password must be at least 8 characters"));
    }
    let hash = password::hash_password(&req.password)?;
    let user = db::users::create(state.db(), &req.email, &req.username, &hash, true).await?;
    let org = db::orgs::create_org(state.db(), &req.org_slug, &req.org_name).await?;
    db::orgs::add_org_member(state.db(), &org.id, &user.id, OrgRole::Owner).await?;
    db::settings::set(state.db(), "setup_complete", "1")
        .await
        .ok();

    let sid = db::users::create_session(
        state.db(),
        &user.id,
        state.config().auth.session_ttl_secs as i64,
    )
    .await?;
    let cookie = session::build_cookie(
        sid,
        state.config().auth.session_ttl_secs as i64,
        state.cookie_secure(),
    );
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session::set_cookie_header(&cookie))],
        Json(json!({ "user": user, "org": org })),
    )
        .into_response())
}

#[derive(Deserialize)]
struct LoginReq {
    login: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> Result<Response> {
    let user = db::users::find_by_login(state.db(), &req.login)
        .await?
        .filter(|u| password::verify_password(&req.password, &u.password_hash))
        .ok_or(Error::Unauthorized)?;

    let sid = db::users::create_session(
        state.db(),
        &user.id,
        state.config().auth.session_ttl_secs as i64,
    )
    .await?;
    let cookie = session::build_cookie(
        sid,
        state.config().auth.session_ttl_secs as i64,
        state.cookie_secure(),
    );
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session::set_cookie_header(&cookie))],
        Json(json!({ "user": user })),
    )
        .into_response())
}

async fn logout(
    State(state): State<AppState>,
    jar: axum_extra::extract::CookieJar,
) -> Result<Response> {
    if let Some(c) = jar.get(session::COOKIE_NAME) {
        db::users::delete_session(state.db(), c.value()).await.ok();
    }
    let cookie = session::clear_cookie(state.cookie_secure());
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session::set_cookie_header(&cookie))],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

async fn me(State(state): State<AppState>, SessionUser(user): SessionUser) -> Result<Response> {
    let orgs = db::orgs::orgs_for_user(state.db(), &user.id).await?;
    let orgs: Vec<_> = orgs
        .into_iter()
        .map(|(o, role)| json!({ "id": o.id, "slug": o.slug, "name": o.name, "role": role }))
        .collect();
    Ok(json_ok(json!({ "user": user, "orgs": orgs })))
}

// ───────────────────────── users (instance admin) ─────────────────────────

async fn list_users(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    let users = db::users::list_all(state.db()).await?;
    Ok(json_ok(json!({ "users": users })))
}

#[derive(Deserialize)]
struct CreateUserReq {
    email: String,
    username: String,
    password: String,
    #[serde(default)]
    is_admin: bool,
}

async fn create_user(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Json(req): Json<CreateUserReq>,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    if req.password.len() < 8 {
        return Err(Error::bad_request("password must be at least 8 characters"));
    }
    let hash = password::hash_password(&req.password)?;
    let created =
        db::users::create(state.db(), &req.email, &req.username, &hash, req.is_admin).await?;
    Ok(json_ok(json!({ "user": created })))
}

// ───────────────────────── orgs / repos ─────────────────────────

async fn list_orgs(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Response> {
    let orgs = db::orgs::orgs_for_user(state.db(), &user.id).await?;
    let orgs: Vec<_> = orgs
        .into_iter()
        .map(|(o, role)| json!({ "id": o.id, "slug": o.slug, "name": o.name, "role": role }))
        .collect();
    Ok(json_ok(json!({ "orgs": orgs })))
}

#[derive(Deserialize)]
struct CreateOrgReq {
    slug: String,
    name: String,
}

async fn create_org(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Json(req): Json<CreateOrgReq>,
) -> Result<Response> {
    if !valid_slug(&req.slug) {
        return Err(Error::bad_request(
            "invalid slug (lowercase letters, digits, hyphens)",
        ));
    }
    if db::orgs::find_org_by_slug(state.db(), &req.slug)
        .await?
        .is_some()
    {
        return Err(Error::conflict("org slug already taken"));
    }
    let org = db::orgs::create_org(state.db(), &req.slug, &req.name).await?;
    db::orgs::add_org_member(state.db(), &org.id, &user.id, OrgRole::Owner).await?;
    Ok(json_ok(json!({ "org": org })))
}

async fn get_org(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
) -> Result<Response> {
    let (org, role) = member_of(&state, &user.id, &slug).await?;
    let stats = db::orgs::org_stats(state.db(), &org.id).await?;
    Ok(json_ok(json!({ "org": org, "role": role, "stats": stats })))
}

async fn list_repos(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
) -> Result<Response> {
    let (org, _) = member_of(&state, &user.id, &slug).await?;
    let repos = db::orgs::list_repos(state.db(), &org.id).await?;
    Ok(json_ok(json!({ "repositories": repos })))
}

async fn get_repo(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, name)): Path<(String, String)>,
) -> Result<Response> {
    let (org, _) = member_of(&state, &user.id, &slug).await?;
    let repo = db::orgs::find_repo(state.db(), &org.id, &name)
        .await?
        .ok_or(Error::NotFound)?;
    let tags = db::orgs::repo_tag_details(state.db(), &repo.id).await?;
    let pull = format!("docker pull {}/{}/{}", pull_host(&state), slug, name);
    Ok(json_ok(
        json!({ "repository": repo, "tags": tags, "pull_prefix": pull }),
    ))
}

async fn delete_repo(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, name)): Path<(String, String)>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    db::orgs::delete_repo(state.db(), &org.id, &name).await?;
    db::audit::record(
        state.db(),
        Some(&user.id),
        Some(&org.id),
        "repo.delete",
        Some(&name),
        None,
    )
    .await
    .ok();
    Ok(json_ok(json!({ "ok": true })))
}

fn pull_host(state: &AppState) -> String {
    let url = &state.config().server.public_url;
    if url.is_empty() {
        "registry.example.com".to_string()
    } else {
        url.split("://")
            .nth(1)
            .unwrap_or(url)
            .trim_end_matches('/')
            .to_string()
    }
}

// ───────────────────────── members ─────────────────────────

async fn list_members(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
) -> Result<Response> {
    let (org, _) = member_of(&state, &user.id, &slug).await?;
    let members = db::orgs::list_members(state.db(), &org.id).await?;
    Ok(json_ok(json!({ "members": members })))
}

#[derive(Deserialize)]
struct AddMemberReq {
    login: String,
    #[serde(default = "default_role")]
    role: String,
}
fn default_role() -> String {
    "member".to_string()
}

async fn add_member(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
    Json(req): Json<AddMemberReq>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    let role = OrgRole::parse(&req.role).ok_or_else(|| Error::bad_request("invalid role"))?;
    let target = db::users::find_by_login(state.db(), &req.login)
        .await?
        .ok_or_else(|| Error::bad_request("no such user"))?;
    db::orgs::add_org_member(state.db(), &org.id, &target.id, role).await?;
    db::audit::record(
        state.db(),
        Some(&user.id),
        Some(&org.id),
        "member.add",
        Some(&target.username),
        Some(req.role.as_str()),
    )
    .await
    .ok();
    Ok(json_ok(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SetRoleReq {
    role: String,
}

async fn set_role(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, user_id)): Path<(String, String)>,
    Json(req): Json<SetRoleReq>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    let role = OrgRole::parse(&req.role).ok_or_else(|| Error::bad_request("invalid role"))?;
    db::orgs::set_member_role(state.db(), &org.id, &user_id, role).await?;
    Ok(json_ok(json!({ "ok": true })))
}

async fn remove_member(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, user_id)): Path<(String, String)>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    db::orgs::remove_member(state.db(), &org.id, &user_id).await?;
    db::audit::record(
        state.db(),
        Some(&user.id),
        Some(&org.id),
        "member.remove",
        Some(&user_id),
        None,
    )
    .await
    .ok();
    Ok(json_ok(json!({ "ok": true })))
}

// ───────────────────────── teams ─────────────────────────

async fn list_teams(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
) -> Result<Response> {
    let (org, _) = member_of(&state, &user.id, &slug).await?;
    let teams = db::orgs::list_teams(state.db(), &org.id).await?;
    Ok(json_ok(json!({ "teams": teams })))
}

#[derive(Deserialize)]
struct CreateTeamReq {
    slug: String,
    name: String,
}

async fn create_team(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
    Json(req): Json<CreateTeamReq>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    if !valid_slug(&req.slug) {
        return Err(Error::bad_request("invalid team slug"));
    }
    let team = db::orgs::create_team(state.db(), &org.id, &req.slug, &req.name).await?;
    Ok(json_ok(json!({ "team": team })))
}

async fn team_members(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, team)): Path<(String, String)>,
) -> Result<Response> {
    let (org, _) = member_of(&state, &user.id, &slug).await?;
    let team = db::orgs::find_team(state.db(), &org.id, &team)
        .await?
        .ok_or(Error::NotFound)?;
    let members = db::orgs::list_team_members(state.db(), &team.id).await?;
    Ok(json_ok(json!({ "members": members })))
}

#[derive(Deserialize)]
struct AddTeamMemberReq {
    login: String,
    #[serde(default = "default_role")]
    role: String,
}

async fn add_team_member(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, team)): Path<(String, String)>,
    Json(req): Json<AddTeamMemberReq>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    let team = db::orgs::find_team(state.db(), &org.id, &team)
        .await?
        .ok_or(Error::NotFound)?;
    let target = db::users::find_by_login(state.db(), &req.login)
        .await?
        .ok_or_else(|| Error::bad_request("no such user"))?;
    db::orgs::add_team_member(state.db(), &team.id, &target.id, &req.role).await?;
    Ok(json_ok(json!({ "ok": true })))
}

async fn remove_team_member(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, team, user_id)): Path<(String, String, String)>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    let team = db::orgs::find_team(state.db(), &org.id, &team)
        .await?
        .ok_or(Error::NotFound)?;
    db::orgs::remove_team_member(state.db(), &team.id, &user_id).await?;
    Ok(json_ok(json!({ "ok": true })))
}

async fn team_perms(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, team)): Path<(String, String)>,
) -> Result<Response> {
    let (org, _) = member_of(&state, &user.id, &slug).await?;
    let team = db::orgs::find_team(state.db(), &org.id, &team)
        .await?
        .ok_or(Error::NotFound)?;
    let perms = db::orgs::list_team_perms(state.db(), &team.id).await?;
    Ok(json_ok(json!({ "permissions": perms })))
}

#[derive(Deserialize)]
struct SetTeamPermReq {
    repo: String,
    permission: String,
}

async fn set_team_perm(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path((slug, team)): Path<(String, String)>,
    Json(req): Json<SetTeamPermReq>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    let team = db::orgs::find_team(state.db(), &org.id, &team)
        .await?
        .ok_or(Error::NotFound)?;
    if crate::models::Permission::parse(&req.permission).is_none() {
        return Err(Error::bad_request("invalid permission"));
    }
    let repo = db::orgs::find_repo(state.db(), &org.id, &req.repo)
        .await?
        .ok_or_else(|| Error::bad_request("no such repository"))?;
    db::orgs::set_team_repo_perm(state.db(), &team.id, &repo.id, &req.permission).await?;
    Ok(json_ok(json!({ "ok": true })))
}

// ───────────────────────── personal access tokens ─────────────────────────

async fn list_tokens(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Response> {
    let tokens = db::users::list_pats(state.db(), &user.id).await?;
    Ok(json_ok(json!({ "tokens": tokens })))
}

#[derive(Deserialize)]
struct CreateTokenReq {
    name: String,
    /// Optional org slug to scope the token to.
    #[serde(default)]
    org: Option<String>,
    /// Optional repo name (within `org`) to scope the token to a single repo.
    #[serde(default)]
    repo: Option<String>,
    /// Optional permission cap: "pull", "push", or "admin" (default).
    #[serde(default)]
    permission: Option<String>,
}

async fn create_token(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Json(req): Json<CreateTokenReq>,
) -> Result<Response> {
    // Resolve the requested scope. The user must belong to any org they scope
    // to; the resulting token is still bounded by their RBAC at issue time.
    let (kind, org_id, repo_id) = match (req.org.as_deref(), req.repo.as_deref()) {
        (Some(slug), Some(repo_name)) => {
            let (org, _) = member_of(&state, &user.id, slug).await?;
            let repo = db::orgs::find_repo(state.db(), &org.id, repo_name)
                .await?
                .ok_or_else(|| Error::bad_request("no such repository"))?;
            ("repo".to_string(), None, Some(repo.id))
        }
        (Some(slug), None) => {
            let (org, _) = member_of(&state, &user.id, slug).await?;
            ("org".to_string(), Some(org.id), None)
        }
        _ => ("all".to_string(), None, None),
    };

    let max_perm = match req.permission.as_deref() {
        None | Some("admin") => "admin",
        Some("push") => "push",
        Some("pull") => "pull",
        Some(_) => return Err(Error::bad_request("invalid permission (pull|push|admin)")),
    };

    let plaintext = db::users::create_pat(
        state.db(),
        &user.id,
        &req.name,
        &kind,
        org_id.as_deref(),
        repo_id.as_deref(),
        max_perm,
    )
    .await?;
    Ok(json_ok(
        json!({ "token": plaintext, "username": user.username }),
    ))
}

async fn delete_token(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(id): Path<String>,
) -> Result<Response> {
    db::users::delete_pat(state.db(), &user.id, &id).await?;
    Ok(json_ok(json!({ "ok": true })))
}

// ───────────────────────── storage settings (instance admin) ─────────────────────────

async fn get_storage_settings(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    let cfg = db::settings::effective_storage(state.db(), &state.config().storage).await?;
    // The secret is never returned — only whether one is set.
    Ok(json_ok(json!({
        "endpoint": cfg.endpoint,
        "bucket": cfg.bucket,
        "region": cfg.region,
        "access_key_id": cfg.access_key_id,
        "secret_set": !cfg.secret_access_key.is_empty(),
        "cdn_url": cfg.cdn_url,
        "force_path_style": cfg.force_path_style,
        "presign_ttl_secs": cfg.presign_ttl_secs,
    })))
}

#[derive(Deserialize)]
struct StorageUpdate {
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    bucket: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    access_key_id: Option<String>,
    /// Empty/absent keeps the existing secret.
    #[serde(default)]
    secret_access_key: Option<String>,
    #[serde(default)]
    cdn_url: Option<String>,
    #[serde(default)]
    force_path_style: Option<bool>,
    #[serde(default)]
    presign_ttl_secs: Option<u64>,
}

async fn update_storage_settings(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Json(req): Json<StorageUpdate>,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    let db = state.db();
    if let Some(v) = req.endpoint {
        db::settings::set(db, "storage_endpoint", v.trim()).await?;
    }
    if let Some(v) = req.bucket {
        db::settings::set(db, "storage_bucket", v.trim()).await?;
    }
    if let Some(v) = req.region {
        db::settings::set(db, "storage_region", v.trim()).await?;
    }
    if let Some(v) = req.access_key_id {
        db::settings::set(db, "storage_access_key_id", v.trim()).await?;
    }
    if let Some(v) = req.secret_access_key {
        // Only overwrite the secret when a non-empty value is provided.
        if !v.is_empty() {
            db::settings::set(db, "storage_secret_access_key", &v).await?;
        }
    }
    if let Some(v) = req.cdn_url {
        db::settings::set(db, "storage_cdn_url", v.trim()).await?;
    }
    if let Some(v) = req.force_path_style {
        db::settings::set(
            db,
            "storage_force_path_style",
            if v { "true" } else { "false" },
        )
        .await?;
    }
    if let Some(v) = req.presign_ttl_secs {
        db::settings::set(db, "storage_presign_ttl_secs", &v.to_string()).await?;
    }

    // Rebuild + hot-swap the storage client so changes take effect immediately.
    let cfg = db::settings::effective_storage(db, &state.config().storage).await?;
    let storage = crate::storage::Storage::new(&cfg)
        .await
        .map_err(|e| Error::bad_request(format!("invalid storage settings: {e}")))?;
    state.set_storage(storage);

    db::audit::record(db, Some(&user.id), None, "storage.update", None, None)
        .await
        .ok();
    Ok(json_ok(json!({ "ok": true })))
}

// ───────────────────────── audit ─────────────────────────

async fn list_audit(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(slug): Path<String>,
) -> Result<Response> {
    let (org, _) = admin_of(&state, &user.id, &slug).await?;
    let entries = db::audit::list(state.db(), &org.id, 200).await?;
    Ok(json_ok(json!({ "entries": entries })))
}

// ───────────────────────── domains / TLS (instance admin) ─────────────────────────

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && !d.starts_with('.')
        && !d.ends_with('.')
        && d.contains('.')
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

async fn list_domains(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    let domains = db::domains::list(state.db()).await?;
    let tls_enabled = state.config().tls.enabled;
    Ok(json_ok(
        json!({ "domains": domains, "tls_enabled": tls_enabled }),
    ))
}

#[derive(Deserialize)]
struct AddDomainReq {
    domain: String,
}

async fn add_domain(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Json(req): Json<AddDomainReq>,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    let domain = req.domain.trim().to_ascii_lowercase();
    if !valid_domain(&domain) {
        return Err(Error::bad_request("invalid domain name"));
    }
    db::domains::add(state.db(), &domain).await?;
    db::audit::record(
        state.db(),
        Some(&user.id),
        None,
        "domain.add",
        Some(&domain),
        None,
    )
    .await
    .ok();
    state.notify_domains_changed();
    Ok(json_ok(json!({ "ok": true })))
}

async fn delete_domain(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(domain): Path<String>,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    db::domains::delete(state.db(), &domain).await?;
    db::audit::record(
        state.db(),
        Some(&user.id),
        None,
        "domain.remove",
        Some(&domain),
        None,
    )
    .await
    .ok();
    state.notify_domains_changed();
    Ok(json_ok(json!({ "ok": true })))
}

async fn set_primary_domain(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(domain): Path<String>,
) -> Result<Response> {
    if !user.is_admin {
        return Err(Error::Forbidden);
    }
    db::domains::set_primary(state.db(), &domain).await?;
    Ok(json_ok(json!({ "ok": true })))
}
