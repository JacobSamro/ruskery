//! "Sign in with Google" (OAuth 2.0 / OIDC). Client id/secret are configured at
//! runtime from the dashboard (Settings → Sign-in) and stored in the DB.

use serde::Deserialize;

use crate::db::{self, Db};
use crate::error::{Error, Result};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// Path appended to the public base URL to form the OAuth redirect URI.
pub const CALLBACK_PATH: &str = "/api/v1/auth/google/callback";

#[derive(Debug, Clone)]
pub struct GoogleConfig {
    pub enabled: bool,
    pub client_id: String,
    pub client_secret: String,
    /// If set, only verified emails in this hosted domain may sign in (and are
    /// auto-provisioned). Empty → Google sign-in only links to existing users.
    pub allowed_domain: String,
}

impl GoogleConfig {
    /// Usable when both enabled and a client id is present.
    pub fn is_active(&self) -> bool {
        self.enabled && !self.client_id.is_empty()
    }
}

/// Load the Google OAuth config from the settings table.
pub async fn load(db: &Db) -> Result<GoogleConfig> {
    let g = |k: &'static str| db::settings::get(db, k);
    Ok(GoogleConfig {
        enabled: g("oauth_google_enabled").await?.as_deref() == Some("true"),
        client_id: g("oauth_google_client_id").await?.unwrap_or_default(),
        client_secret: g("oauth_google_client_secret").await?.unwrap_or_default(),
        allowed_domain: g("oauth_google_allowed_domain")
            .await?
            .unwrap_or_default()
            .to_ascii_lowercase(),
    })
}

/// Build the Google authorization URL to redirect the browser to.
pub fn authorize_url(cfg: &GoogleConfig, redirect_uri: &str, state: &str) -> String {
    let mut url = reqwest::Url::parse(AUTH_URL).expect("valid auth url");
    url.query_pairs_mut()
        .append_pair("client_id", &cfg.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", state)
        .append_pair("access_type", "online")
        .append_pair("prompt", "select_account");
    url.to_string()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Profile fields returned by Google's userinfo endpoint.
#[derive(Debug, Deserialize)]
pub struct GoogleUser {
    pub email: String,
    #[serde(default)]
    pub email_verified: bool,
    #[serde(default)]
    pub name: String,
    /// Google Workspace hosted domain, when applicable.
    #[serde(default)]
    pub hd: String,
}

fn oauth_err<E: std::fmt::Display>(ctx: &str, e: E) -> Error {
    Error::Other(anyhow::anyhow!("google oauth {ctx}: {e}"))
}

/// Exchange an authorization code for the user's verified profile.
pub async fn exchange_code(
    cfg: &GoogleConfig,
    redirect_uri: &str,
    code: &str,
) -> Result<GoogleUser> {
    let client = reqwest::Client::new();

    let token: TokenResponse = client
        .post(TOKEN_URL)
        .form(&[
            ("code", code),
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| oauth_err("token request", e))?
        .error_for_status()
        .map_err(|e| oauth_err("token exchange rejected", e))?
        .json()
        .await
        .map_err(|e| oauth_err("token decode", e))?;

    let user: GoogleUser = client
        .get(USERINFO_URL)
        .bearer_auth(&token.access_token)
        .send()
        .await
        .map_err(|e| oauth_err("userinfo request", e))?
        .error_for_status()
        .map_err(|e| oauth_err("userinfo rejected", e))?
        .json()
        .await
        .map_err(|e| oauth_err("userinfo decode", e))?;

    Ok(user)
}
