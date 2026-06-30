//! Authentication: password hashing, PATs, registry JWTs, RBAC, sessions, and
//! the request extractors that enforce them.

pub mod oauth;
pub mod password;
pub mod pat;
pub mod rbac;
pub mod session;
pub mod token;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use base64::Engine;

use crate::db;
use crate::error::Error;
use crate::models::User;
use crate::state::AppState;

/// Decoded HTTP Basic credentials.
pub struct BasicCredentials {
    pub username: String,
    pub password: String,
}

/// Parse an `Authorization: Basic <base64>` header value.
pub fn parse_basic(header: &str) -> Option<BasicCredentials> {
    let b64 = header
        .strip_prefix("Basic ")
        .or_else(|| header.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let s = String::from_utf8(decoded).ok()?;
    let (username, password) = s.split_once(':')?;
    Some(BasicCredentials {
        username: username.to_string(),
        password: password.to_string(),
    })
}

/// Extract the `Bearer <token>` value from an Authorization header.
pub fn parse_bearer(header: &str) -> Option<&str> {
    header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .map(|s| s.trim())
}

/// An authenticated dashboard user, resolved from the session cookie.
pub struct SessionUser(pub User);

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let sid = jar
            .get(session::COOKIE_NAME)
            .map(|c| c.value().to_string())
            .ok_or(Error::Unauthorized)?;
        let user = db::users::user_for_session(state.db(), &sid)
            .await?
            .ok_or(Error::Unauthorized)?;
        Ok(SessionUser(user))
    }
}
