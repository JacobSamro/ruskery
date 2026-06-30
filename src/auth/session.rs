//! Dashboard session cookies.

use axum::http::HeaderValue;
use axum_extra::extract::cookie::{Cookie, SameSite};
use time::Duration;

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "ruskery_session";

/// Build the session cookie. `secure` should be true behind HTTPS.
pub fn build_cookie(session_id: String, ttl_secs: i64, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE_NAME, session_id);
    c.set_http_only(true);
    c.set_secure(secure);
    c.set_same_site(SameSite::Strict);
    c.set_path("/");
    c.set_max_age(Duration::seconds(ttl_secs));
    c
}

/// Build an expired cookie used to clear the session on logout.
pub fn clear_cookie(secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE_NAME, "");
    c.set_http_only(true);
    c.set_secure(secure);
    c.set_same_site(SameSite::Strict);
    c.set_path("/");
    c.set_max_age(Duration::seconds(0));
    c
}

/// Header value for setting a cookie.
pub fn set_cookie_header(c: &Cookie<'static>) -> HeaderValue {
    HeaderValue::from_str(&c.to_string()).expect("valid cookie header")
}
