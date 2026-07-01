//! Registry bearer tokens (JWT, HS256). ruskery is both the auth server and the
//! resource server, so the same secret signs and verifies. Tokens carry the
//! exact set of granted repository scopes (`access`), which the registry
//! middleware checks against each request.

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A single granted scope, mirroring the Docker token "access" entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessEntry {
    #[serde(rename = "type")]
    pub kind: String, // always "repository" for now
    pub name: String,         // "<org>/<repo>"
    pub actions: Vec<String>, // subset of ["pull","push","delete"]
}

/// The bearer's PAT scope, carried so metadata endpoints (`_catalog`) can
/// confine results to what the token may reach — an `access` list only covers
/// the repositories the client explicitly requested, which catalog doesn't.
/// `kind` is `all` | `org` | `repo`; `id` is the org/repo id for the latter two.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeClaim {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl Default for ScopeClaim {
    /// Absent on older tokens (and account-password logins) → full scope.
    fn default() -> Self {
        Self {
            kind: "all".to_string(),
            id: None,
        }
    }
}

impl ScopeClaim {
    pub fn all() -> Self {
        Self::default()
    }
    pub fn org(id: &str) -> Self {
        Self {
            kind: "org".to_string(),
            id: Some(id.to_string()),
        }
    }
    pub fn repo(id: &str) -> Self {
        Self {
            kind: "repo".to_string(),
            id: Some(id.to_string()),
        }
    }
}

/// JWT claims for a registry bearer token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String, // user id
    pub iss: String, // "ruskery"
    pub aud: String, // service (registry host)
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    #[serde(default)]
    pub access: Vec<AccessEntry>,
    #[serde(default)]
    pub scope: ScopeClaim,
}

impl Claims {
    /// Does this token grant `action` on repository `name`?
    pub fn grants(&self, name: &str, action: &str) -> bool {
        self.access.iter().any(|a| {
            a.kind == "repository" && a.name == name && a.actions.iter().any(|x| x == action)
        })
    }
}

/// Issue a signed bearer token.
pub fn issue(
    secret: &[u8],
    user_id: &str,
    service: &str,
    access: Vec<AccessEntry>,
    scope: ScopeClaim,
    ttl_secs: i64,
) -> Result<String> {
    let now = now_unix();
    let claims = Claims {
        sub: user_id.to_string(),
        iss: "ruskery".to_string(),
        aud: service.to_string(),
        iat: now,
        exp: now + ttl_secs,
        jti: uuid::Uuid::new_v4().to_string(),
        access,
        scope,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("token encode failed: {e}")))
}

/// Verify a bearer token's signature and expiry, returning its claims.
pub fn verify(secret: &[u8], token: &str, service: &str) -> Result<Claims> {
    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_audience(&[service]);
    validation.set_issuer(&["ruskery"]);
    let data = decode::<Claims>(token, &DecodingKey::from_secret(secret), &validation)
        .map_err(|_| Error::Unauthorized)?;
    Ok(data.claims)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
