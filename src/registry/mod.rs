//! OCI Distribution v2 surface: version check, token endpoint, and the data
//! plane (blobs, uploads, manifests, tags, catalog).
//!
//! Repository names may contain slashes, so the data-plane routes are matched by
//! a single wildcard handler that parses the operation from the path suffix.

pub mod auth;
pub mod blobs;
pub mod manifests;
pub mod tags;
pub mod uploads;

use axum::{
    body::Body,
    extract::{RawQuery, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use serde_json::json;

use crate::auth::{parse_basic, password, rbac, token};
use crate::db;
use crate::db::orgs::Org;
use crate::error::Error;
use crate::models::Permission;
use crate::state::AppState;

/// Registry routes mounted under the application root.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v2/", get(v2_base))
        .route("/v2", get(v2_base))
        .route("/v2/token", get(token_endpoint))
        .route("/v2/{*rest}", any(dispatch))
}

// ───────────────────────── version check ─────────────────────────

async fn v2_base(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if auth::verify_bearer(&state, &headers).is_some() {
        return (
            StatusCode::OK,
            [(
                header::HeaderName::from_static("docker-distribution-api-version"),
                header::HeaderValue::from_static("registry/2.0"),
            )],
        )
            .into_response();
    }
    auth::challenge(&state, &headers, None)
}

// ───────────────────────── data-plane dispatch ─────────────────────────

enum Op {
    TagsList,
    Manifest(String),
    Blob(String),
    UploadStart,
    UploadSession(String),
}

struct Parsed {
    name: String,
    op: Op,
}

/// A registry name is `<org>/<repo...>`: at least two path components, each
/// non-empty and drawn from the OCI repository-name character set.
fn valid_repo_name(name: &str) -> bool {
    let segs: Vec<&str> = name.split('/').collect();
    if segs.len() < 2 {
        return false;
    }
    segs.iter().all(|s| {
        !s.is_empty()
            && s.len() <= 255
            && s.bytes().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
            })
    })
}

/// Parse the path after `/v2/` into a repository name + operation.
fn parse_route(rest: &str) -> Option<Parsed> {
    if let Some(name) = rest.strip_suffix("/tags/list") {
        return Some(Parsed {
            name: name.to_string(),
            op: Op::TagsList,
        });
    }
    if let Some(i) = rest.rfind("/manifests/") {
        let name = &rest[..i];
        let reference = &rest[i + "/manifests/".len()..];
        if !name.is_empty() && !reference.is_empty() {
            return Some(Parsed {
                name: name.to_string(),
                op: Op::Manifest(reference.to_string()),
            });
        }
    }
    for suffix in ["/blobs/uploads/", "/blobs/uploads"] {
        if let Some(name) = rest.strip_suffix(suffix) {
            if !name.is_empty() {
                return Some(Parsed {
                    name: name.to_string(),
                    op: Op::UploadStart,
                });
            }
        }
    }
    if let Some(i) = rest.rfind("/blobs/uploads/") {
        let name = &rest[..i];
        let uuid = &rest[i + "/blobs/uploads/".len()..];
        if !name.is_empty() && !uuid.is_empty() {
            return Some(Parsed {
                name: name.to_string(),
                op: Op::UploadSession(uuid.to_string()),
            });
        }
    }
    if let Some(i) = rest.rfind("/blobs/") {
        let name = &rest[..i];
        let digest = &rest[i + "/blobs/".len()..];
        if !name.is_empty() && !digest.is_empty() {
            return Some(Parsed {
                name: name.to_string(),
                op: Op::Blob(digest.to_string()),
            });
        }
    }
    None
}

async fn dispatch(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Body,
) -> Response {
    let rest = uri.path().strip_prefix("/v2/").unwrap_or("");
    let query = uri.query().unwrap_or("").to_string();

    if rest == "_catalog" {
        return catalog(&state, &headers).await;
    }

    let Some(parsed) = parse_route(rest) else {
        return Error::oci(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "unknown route").into_response();
    };
    let name = parsed.name;

    // Reject empty/malformed repository names (e.g. `org//repo`) before any
    // auth or storage decision.
    if !valid_repo_name(&name) {
        return Error::oci(
            StatusCode::BAD_REQUEST,
            "NAME_INVALID",
            "invalid repository name",
        )
        .into_response();
    }

    // The action a request needs depends on the operation + method.
    let required = match (&parsed.op, &method) {
        (Op::TagsList, _) => Permission::Pull,
        (Op::Manifest(_), &Method::GET | &Method::HEAD) => Permission::Pull,
        (Op::Manifest(_), &Method::PUT) => Permission::Push,
        (Op::Manifest(_), &Method::DELETE) => Permission::Admin,
        (Op::Blob(_), &Method::GET | &Method::HEAD) => Permission::Pull,
        (Op::Blob(_), &Method::DELETE) => Permission::Admin,
        (Op::UploadStart, _) | (Op::UploadSession(_), _) => Permission::Push,
        _ => {
            return Error::oci(
                StatusCode::METHOD_NOT_ALLOWED,
                "UNSUPPORTED",
                "method not allowed",
            )
            .into_response()
        }
    };

    // Enforce the scope.
    let action = match required {
        Permission::Pull => "pull",
        Permission::Push => "push",
        _ => "delete",
    };
    let claims = match auth::require(&state, &headers, &name, action) {
        Ok(c) => c,
        Err(challenge) => return challenge,
    };

    // Resolve org + repo-within-org (org is guaranteed to exist post-auth).
    let Some((org, repo_name)) = resolve(&state, &name).await else {
        return Error::oci(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository unknown")
            .into_response();
    };

    let result = match parsed.op {
        Op::TagsList => tags::list(&state, &org, &repo_name, &name).await,
        Op::Manifest(reference) => match method {
            Method::GET => manifests::get(&state, &org, &repo_name, &reference, false).await,
            Method::HEAD => manifests::get(&state, &org, &repo_name, &reference, true).await,
            Method::PUT => {
                let max = state.config().server.max_body_bytes;
                match axum::body::to_bytes(body, max).await {
                    Ok(bytes) => {
                        manifests::put(
                            &state,
                            &org,
                            &repo_name,
                            &name,
                            &reference,
                            &headers,
                            bytes,
                            &claims.sub,
                        )
                        .await
                    }
                    Err(_) => Err(Error::oci(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "SIZE_INVALID",
                        "manifest too large",
                    )),
                }
            }
            Method::DELETE => manifests::delete(&state, &org, &repo_name, &reference).await,
            _ => unsupported(),
        },
        Op::Blob(digest) => match method {
            Method::GET => blobs::get(&state, &org.id, &digest).await,
            Method::HEAD => blobs::head(&state, &org.id, &digest).await,
            Method::DELETE => blobs::delete(&state, &org.id, &digest).await,
            _ => unsupported(),
        },
        Op::UploadStart => match method {
            Method::POST => {
                let mount = uploads::query_param(&query, "mount").map(percent_decode);
                // A monolithic POST may carry ?digest= with the whole body.
                let digest = uploads::query_param(&query, "digest").map(percent_decode);
                match digest {
                    Some(d) => start_then_finish(&state, &org.id, &name, body, &d).await,
                    None => uploads::start(&state, &org.id, &name, mount.as_deref()).await,
                }
            }
            _ => unsupported(),
        },
        Op::UploadSession(uuid) => match method {
            Method::PATCH => uploads::patch(&state, &org.id, &name, &uuid, body).await,
            Method::PUT => {
                let digest = uploads::query_param(&query, "digest").map(percent_decode);
                match digest {
                    Some(d) => uploads::finish(&state, &org.id, &name, &uuid, &d, body).await,
                    None => Err(Error::oci(
                        StatusCode::BAD_REQUEST,
                        "DIGEST_INVALID",
                        "missing digest on upload finalize",
                    )),
                }
            }
            Method::GET => uploads::status(&state, &org.id, &name, &uuid).await,
            Method::DELETE => uploads::cancel(&state, &org.id, &uuid).await,
            _ => unsupported(),
        },
    };

    result.unwrap_or_else(|e| e.into_response())
}

fn unsupported() -> crate::error::Result<Response> {
    Err(Error::oci(
        StatusCode::METHOD_NOT_ALLOWED,
        "UNSUPPORTED",
        "method not allowed",
    ))
}

/// Handle a monolithic upload (`POST ...?digest=`) as start + immediate finish.
async fn start_then_finish(
    state: &AppState,
    org_id: &str,
    name: &str,
    body: Body,
    digest: &str,
) -> crate::error::Result<Response> {
    // Create a session, then drive the finalize path with the request body.
    let start = uploads::start(state, org_id, name, None).await?;
    // Extract the upload id from the Location header of the start response.
    let upload_id = start
        .headers()
        .get(header::HeaderName::from_static("docker-upload-uuid"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("upload id missing")))?;
    uploads::finish(state, org_id, name, &upload_id, digest, body).await
}

/// Split a full repository name into its org + repo-within-org, resolving the org.
async fn resolve(state: &AppState, name: &str) -> Option<(Org, String)> {
    let (slug, repo_name) = name.split_once('/')?;
    let org = db::orgs::find_org_by_slug(state.db(), slug).await.ok()??;
    Some((org, repo_name.to_string()))
}

// ───────────────────────── catalog ─────────────────────────

async fn catalog(state: &AppState, headers: &HeaderMap) -> Response {
    let Some(claims) = auth::verify_bearer(state, headers) else {
        return auth::challenge(state, headers, Some("registry:catalog:*"));
    };
    match db::orgs::catalog_for_user(state.db(), &claims.sub).await {
        Ok(repos) => (StatusCode::OK, Json(json!({ "repositories": repos }))).into_response(),
        Err(e) => e.into_response(),
    }
}

// ───────────────────────── token endpoint ─────────────────────────

async fn token_endpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let creds = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_basic);

    let Some(creds) = creds else {
        return auth::challenge(&state, &headers, None);
    };

    let (user, token_scope, cap) =
        match resolve_user(&state, &creds.username, &creds.password).await {
            Some(u) => u,
            None => return auth::challenge(&state, &headers, None),
        };

    let requested = parse_scopes(query.as_deref().unwrap_or(""));
    let service = auth::service_name(&state, &headers);

    let mut access = Vec::new();
    for scope in &requested {
        if scope.kind != "repository" {
            continue;
        }
        match rbac::grant_repository(
            state.db(),
            &user.id,
            &scope.name,
            &scope.actions,
            &token_scope,
            cap,
        )
        .await
        {
            Ok(granted) if !granted.is_empty() => access.push(token::AccessEntry {
                kind: "repository".into(),
                name: scope.name.clone(),
                actions: granted,
            }),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "scope resolution failed"),
        }
    }

    let ttl = state.config().auth.token_ttl_secs as i64;
    match token::issue(state.secret_key(), &user.id, &service, access, ttl) {
        Ok(jwt) => {
            let body = json!({
                "token": jwt,
                "access_token": jwt,
                "expires_in": ttl,
                "issued_at": crate::util::now_rfc3339(),
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// Authenticate by account password (full scope) or, failing that, as a PAT
/// (carrying that token's scope).
async fn resolve_user(
    state: &AppState,
    username: &str,
    secret: &str,
) -> Option<(crate::models::User, crate::models::TokenScope, Permission)> {
    if !username.is_empty() {
        if let Ok(Some(u)) = db::users::find_by_login(state.db(), username).await {
            if password::verify_password(secret, &u.password_hash) {
                // An account-password login is unscoped and uncapped.
                return Some((u, crate::models::TokenScope::All, Permission::Admin));
            }
        }
    }
    db::users::user_for_pat(state.db(), secret)
        .await
        .ok()
        .flatten()
}

fn parse_scopes(query: &str) -> Vec<rbac::Scope> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == "scope")
                .then(|| percent_decode(v))
                .and_then(|s| rbac::Scope::parse(&s))
        })
        .collect()
}

// ───────────────────────── shared helpers ─────────────────────────

/// Minimal `application/x-www-form-urlencoded` value decoding (`+` and `%XX`).
pub(crate) fn percent_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Compare two digests strictly: both must be a well-formed `<algo>:<hex>` with
/// the same algorithm and hex (case-insensitive). A missing prefix, unknown
/// algorithm, or wrong-length/non-hex digest never matches.
pub(crate) fn digests_equal(a: &str, b: &str) -> bool {
    fn parsed(d: &str) -> Option<(String, String)> {
        let (algo, hex) = d.split_once(':')?;
        let algo = algo.to_ascii_lowercase();
        let hex = hex.to_ascii_lowercase();
        let want_len = match algo.as_str() {
            "sha256" => 64,
            "sha512" => 128,
            _ => return None,
        };
        if hex.len() != want_len || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some((algo, hex))
    }
    match (parsed(a), parsed(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}
