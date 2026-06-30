//! Registry authentication helpers: realm/service derivation, the Bearer
//! challenge, and bearer-token verification for `/v2/*` routes.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::auth::{parse_bearer, token};
use crate::state::AppState;

/// Derive the externally-visible base URL (scheme://host) for building the realm.
/// Prefers the configured `public_url`; otherwise reconstructs it from request
/// headers (honoring `X-Forwarded-Proto` when behind a proxy).
pub fn base_url(state: &AppState, headers: &HeaderMap) -> String {
    let configured = &state.config().server.public_url;
    if !configured.is_empty() {
        return configured.trim_end_matches('/').to_string();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

/// The token "service" identifier (the registry host), used as JWT audience.
pub fn service_name(state: &AppState, headers: &HeaderMap) -> String {
    base_url(state, headers)
        .split("://")
        .nth(1)
        .unwrap_or("localhost")
        .to_string()
}

/// Build a `401` response carrying the Bearer challenge that tells docker where
/// to obtain a token and for what scope.
pub fn challenge(state: &AppState, headers: &HeaderMap, scope: Option<&str>) -> Response {
    let realm = format!("{}/v2/token", base_url(state, headers));
    let service = service_name(state, headers);
    let mut value = format!("Bearer realm=\"{realm}\",service=\"{service}\"");
    if let Some(s) = scope {
        value.push_str(&format!(",scope=\"{s}\""));
    }

    let body = json!({ "errors": [ {
        "code": "UNAUTHORIZED",
        "message": "authentication required"
    } ] });

    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, value)],
        Json(body),
    )
        .into_response()
}

/// Require that the request carries a bearer token granting `action` on
/// repository `name`. On success returns the claims; on failure returns the
/// `401` challenge response (with the needed scope) to send back.
#[allow(clippy::result_large_err)] // Err is a ready-to-send Response, by design
pub fn require(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    action: &str,
) -> std::result::Result<token::Claims, Response> {
    match verify_bearer(state, headers) {
        Some(claims) if claims.grants(name, action) => Ok(claims),
        // Authenticated but lacking the grant → 403 DENIED (a hard deny, not a
        // re-auth prompt). No/invalid credentials → 401 challenge.
        Some(_) => Err(denied(name, action)),
        None => Err(challenge(
            state,
            headers,
            Some(&format!("repository:{name}:{action}")),
        )),
    }
}

/// A `403 DENIED` for a caller who is authenticated but not authorized.
fn denied(name: &str, action: &str) -> Response {
    let body = json!({ "errors": [ {
        "code": "DENIED",
        "message": "requested access to the resource is denied",
        "detail": format!("{action} on {name}")
    } ] });
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}

/// Verify the request's bearer token. Returns the claims on success.
pub fn verify_bearer(state: &AppState, headers: &HeaderMap) -> Option<token::Claims> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())?;
    let jwt = parse_bearer(raw)?;
    let service = service_name(state, headers);
    token::verify(state.secret_key(), jwt, &service).ok()
}
