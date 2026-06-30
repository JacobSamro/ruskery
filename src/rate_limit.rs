//! Per-IP rate limiting for sensitive endpoints (login, setup, token issuance),
//! to blunt credential-stuffing and brute-force attempts.

use std::num::NonZeroU32;
use std::sync::LazyLock;

use axum::extract::{ConnectInfo, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use governor::{clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota, RateLimiter};
use serde_json::json;

type Limiter = RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock>;

/// Strict limit for the human-facing credential endpoints (dashboard login and
/// first-run setup): 10 requests/second per IP with a burst of 20. Generous for
/// a person, hostile to a brute-force script.
static LOGIN_LIMITER: LazyLock<Limiter> = LazyLock::new(|| {
    let quota =
        Quota::per_second(NonZeroU32::new(10).unwrap()).allow_burst(NonZeroU32::new(20).unwrap());
    RateLimiter::keyed(quota)
});

/// Generous limit for the registry token endpoint: 50 requests/second per IP
/// with a burst of 500. Registry clients (docker, CI runners, the OCI
/// conformance suite) fetch many short-lived, per-scope tokens in tight bursts,
/// frequently from a *shared* egress IP (corporate NAT, CI providers), so the
/// strict login cap would break legitimate use. This still bounds a password
/// brute-forcer to a sane ceiling, on top of the slow Argon2 verify and the
/// high entropy of PATs.
static TOKEN_LIMITER: LazyLock<Limiter> = LazyLock::new(|| {
    let quota =
        Quota::per_second(NonZeroU32::new(50).unwrap()).allow_burst(NonZeroU32::new(500).unwrap());
    RateLimiter::keyed(quota)
});

/// Which limiter (if any) guards a path.
enum Guard {
    /// Strict human-credential limiter; rejects with the dashboard JSON schema.
    Login,
    /// Generous registry-token limiter; rejects with the OCI error schema so a
    /// throttled docker/OCI client can still parse the body.
    Token,
    None,
}

fn guard_for(path: &str) -> Guard {
    match path {
        "/v2/token" => Guard::Token,
        "/api/v1/auth/login" | "/api/v1/setup" => Guard::Login,
        _ => Guard::None,
    }
}

/// Client IP for rate-limit keying. Forwarded headers are honored ONLY when
/// `trust_proxy` is set (i.e. a trusted proxy populates them); otherwise the
/// real peer socket address is used so a client can't spoof a fresh key.
fn client_ip(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        let h = req.headers();
        if let Some(xff) = h.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            // Take the first non-empty hop; an empty/whitespace header must not
            // collapse every client onto one degenerate rate-limit key.
            if let Some(first) = xff.split(',').map(str::trim).find(|s| !s.is_empty()) {
                return first.to_string();
            }
        }
        if let Some(rip) = h.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            let rip = rip.trim();
            if !rip.is_empty() {
                return rip.to_string();
            }
        }
    }
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `429` carrying the OCI distribution error schema, for the registry token
/// endpoint (so a throttled docker/conformance client parses it cleanly).
fn too_many_oci() -> Response {
    let body = json!({ "errors": [ {
        "code": "TOOMANYREQUESTS",
        "message": "rate limit exceeded"
    } ] });
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "1")],
        Json(body),
    )
        .into_response()
}

/// `429` carrying the dashboard JSON error schema, for login/setup.
fn too_many_login() -> Response {
    let body = json!({ "error": { "code": "rate_limited", "message": "rate limit exceeded" } });
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "1")],
        Json(body),
    )
        .into_response()
}

pub async fn middleware(trust_proxy: bool, req: Request, next: Next) -> Response {
    match guard_for(req.uri().path()) {
        Guard::Token => {
            let ip = client_ip(&req, trust_proxy);
            if TOKEN_LIMITER.check_key(&ip).is_err() {
                return too_many_oci();
            }
        }
        Guard::Login => {
            let ip = client_ip(&req, trust_proxy);
            if LOGIN_LIMITER.check_key(&ip).is_err() {
                return too_many_login();
            }
        }
        Guard::None => {}
    }
    next.run(req).await
}
