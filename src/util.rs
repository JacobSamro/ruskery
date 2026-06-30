//! Small shared helpers.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Current UTC time as an RFC3339 string (the canonical timestamp format in the DB).
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// An RFC3339 timestamp `secs` seconds in the future.
pub fn rfc3339_in(secs: i64) -> String {
    (OffsetDateTime::now_utc() + time::Duration::seconds(secs))
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// UTC calendar day (`YYYY-MM-DD`) `days` before today — the lower bound for
/// analytics range queries.
pub fn utc_day_offset(days: i64) -> String {
    let d = (OffsetDateTime::now_utc() - time::Duration::days(days)).date();
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// Generate a random url-safe id (used for session ids and similar).
pub fn random_id() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
