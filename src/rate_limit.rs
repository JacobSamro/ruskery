//! Per-IP rate limiting for sensitive endpoints (login, setup, token issuance),
//! to blunt credential-stuffing and brute-force attempts.

use std::num::NonZeroU32;
use std::sync::LazyLock;

use axum::extract::{ConnectInfo, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::{clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota, RateLimiter};

type Limiter = RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock>;

/// 10 requests/second per IP with a burst of 20 — generous for humans, hostile
/// to brute force. Applied only to authentication paths.
static AUTH_LIMITER: LazyLock<Limiter> = LazyLock::new(|| {
    let quota =
        Quota::per_second(NonZeroU32::new(10).unwrap()).allow_burst(NonZeroU32::new(20).unwrap());
    RateLimiter::keyed(quota)
});

fn is_sensitive(path: &str) -> bool {
    path == "/v2/token" || path == "/api/v1/auth/login" || path == "/api/v1/setup"
}

/// Client IP for rate-limit keying. Forwarded headers are honored ONLY when
/// `trust_proxy` is set (i.e. a trusted proxy populates them); otherwise the
/// real peer socket address is used so a client can't spoof a fresh key.
fn client_ip(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        let h = req.headers();
        if let Some(xff) = h.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                return first.trim().to_string();
            }
        }
        if let Some(rip) = h.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            return rip.trim().to_string();
        }
    }
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub async fn middleware(trust_proxy: bool, req: Request, next: Next) -> Response {
    if is_sensitive(req.uri().path()) {
        let ip = client_ip(&req, trust_proxy);
        if AUTH_LIMITER.check_key(&ip).is_err() {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "1")],
                "rate limit exceeded",
            )
                .into_response();
        }
    }
    next.run(req).await
}
