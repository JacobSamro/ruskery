//! Upstream repository *enumeration* adapters.
//!
//! The OCI `/v2/_catalog` endpoint is optional and not part of the distribution
//! spec, so hosted registries treat it inconsistently: Docker Hub omits it, GHCR
//! gates it, and DigitalOcean under-reports it for multi-registry accounts
//! (returning one registry's repos, not all). Relying on it alone means a bulk
//! import silently finds nothing. For those hosts we enumerate repositories via
//! the provider's own management API instead.
//!
//! Only the *listing* differs. Once we have repository names, the standard OCI
//! per-repo endpoints (`tags/list`, `manifests`, `blobs`) on the registry host
//! drive the actual copy — see [`crate::import`] and [`crate::proxy`].

use std::sync::LazyLock;

use serde::Serialize;

use crate::db::orgs::OrgUpstream;
use crate::error::{Error, Result};
use crate::proxy;

/// Shared HTTP client for provider management APIs (api.digitalocean.com,
/// api.github.com). Separate from the OCI-registry client in [`crate::proxy`].
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("ruskery/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("build provider http client")
});

/// Management-API and registry base URLs per provider. Overridable via env so a
/// GitHub Enterprise Server / proxied endpoint can be targeted (and so tests can
/// point them at a local stub); default to the public hosts.
fn do_api() -> String {
    std::env::var("RUSKERY_DO_API_BASE").unwrap_or_else(|_| "https://api.digitalocean.com".into())
}
fn gh_api() -> String {
    std::env::var("RUSKERY_GH_API_BASE").unwrap_or_else(|_| "https://api.github.com".into())
}

/// Which upstream a bulk import targets. Selected in the import dialog; decides
/// how the catalog is enumerated and (for the API-backed providers) which
/// registry host the copy runs against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// Any OCI registry that exposes `/v2/_catalog` (registry:2, Harbor, …).
    Generic,
    /// DigitalOcean Container Registry (`registry.digitalocean.com`).
    DigitalOcean,
    /// GitHub Container Registry (`ghcr.io`).
    Github,
}

impl Provider {
    /// Parse the wire value from the import request (defaults to generic).
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("generic").trim().to_ascii_lowercase().as_str() {
            "digitalocean" | "do" | "docr" => Provider::DigitalOcean,
            "github" | "ghcr" => Provider::Github,
            _ => Provider::Generic,
        }
    }

    /// The fixed OCI registry host for API-backed providers; `None` means the
    /// user-supplied host is used (generic). Overridable via env (enterprise /
    /// tests) — see [`do_api`]/[`gh_api`] for the paired management-API bases.
    pub fn registry_url(self) -> Option<String> {
        match self {
            Provider::Generic => None,
            Provider::DigitalOcean => Some(
                std::env::var("RUSKERY_DO_REGISTRY_BASE")
                    .unwrap_or_else(|_| "https://registry.digitalocean.com".into()),
            ),
            Provider::Github => Some(
                std::env::var("RUSKERY_GH_REGISTRY_BASE")
                    .unwrap_or_else(|_| "https://ghcr.io".into()),
            ),
        }
    }
}

/// A selectable namespace for the import dialog's dropdown: a DigitalOcean
/// registry or a GitHub owner (the authenticated user or one of their orgs).
#[derive(Debug, Serialize)]
pub struct Namespace {
    pub name: String,
    /// Repository count when cheap to determine, else `None`.
    pub repo_count: Option<i64>,
}

/// The credentials token an API-backed provider authenticates with. Both the DO
/// API token and a GitHub PAT are entered in the password (or username) field.
fn api_token(up: &OrgUpstream) -> Result<&str> {
    up.password
        .as_deref()
        .or(up.username.as_deref())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| Error::bad_request("an API token is required for this provider"))
}

fn provider_error(msg: impl Into<String>) -> Error {
    Error::oci(
        axum::http::StatusCode::BAD_GATEWAY,
        "UPSTREAM_UNAVAILABLE",
        msg.into(),
    )
}

// ── discovery (populates the dialog's namespace dropdown) ──────────────

/// List the namespaces a user can pick from for `provider`. Generic has none
/// (it uses a free-text prefix filter instead).
pub async fn discover(provider: Provider, up: &OrgUpstream) -> Result<Vec<Namespace>> {
    match provider {
        Provider::Generic => Ok(Vec::new()),
        Provider::DigitalOcean => do_registries(up).await,
        Provider::Github => gh_owners(up).await,
    }
}

// ── enumeration (repositories to copy) ─────────────────────────────────

/// Enumerate the repositories to import for `provider`. `namespace` is the DO
/// registry name / GitHub owner (required for those); for generic it's an
/// optional repository-prefix filter over `/v2/_catalog`.
pub async fn enumerate_repos(
    provider: Provider,
    up: &OrgUpstream,
    namespace: Option<&str>,
) -> Result<Vec<String>> {
    match provider {
        Provider::Generic => {
            let mut repos = proxy::list_catalog(up).await?;
            if let Some(prefix) = namespace {
                repos.retain(|repo| crate::import::repo_under_prefix(repo, prefix));
            }
            Ok(repos)
        }
        Provider::DigitalOcean => {
            let name = require_namespace(namespace, "a DigitalOcean registry")?;
            do_repositories(up, name).await
        }
        Provider::Github => {
            let owner = require_namespace(namespace, "a GitHub owner")?;
            gh_packages(up, owner).await
        }
    }
}

fn require_namespace<'a>(namespace: Option<&'a str>, what: &str) -> Result<&'a str> {
    namespace
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| Error::bad_request(format!("{what} name is required")))
}

// ── DigitalOcean ───────────────────────────────────────────────────────

/// GET a DigitalOcean API URL with the token as a bearer, decoding JSON.
async fn do_get(up: &OrgUpstream, url: &str) -> Result<serde_json::Value> {
    let resp = CLIENT
        .get(url)
        .bearer_auth(api_token(up)?)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| provider_error(format!("DigitalOcean API request failed: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(provider_error(
            "DigitalOcean rejected the API token (401) — needs a token with registry read access",
        ));
    }
    if !status.is_success() {
        return Err(provider_error(format!("DigitalOcean API status {status}")));
    }
    resp.json()
        .await
        .map_err(|e| provider_error(format!("reading DigitalOcean API response: {e}")))
}

/// List the account's container registries (`/v2/registries`), with a cheap
/// per-registry repository count for the dropdown.
async fn do_registries(up: &OrgUpstream) -> Result<Vec<Namespace>> {
    let body = do_get(up, &format!("{}/v2/registries", do_api())).await?;
    let names: Vec<String> = body
        .get("registries")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        // One-row page just to read `meta.total` (cheap; a few registries).
        let count = do_get(
            up,
            &format!(
                "{}/v2/registries/{name}/repositoriesV2?page=1&per_page=1",
                do_api()
            ),
        )
        .await
        .ok()
        .and_then(|b| {
            b.get("meta")
                .and_then(|m| m.get("total"))
                .and_then(|t| t.as_i64())
        });
        out.push(Namespace {
            name,
            repo_count: count,
        });
    }
    Ok(out)
}

/// List every repository in a DigitalOcean registry, returning OCI repo names
/// (`<registry>/<repo>`). Follows the API's page links.
async fn do_repositories(up: &OrgUpstream, registry: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut next = Some(format!(
        "{}/v2/registries/{registry}/repositoriesV2?per_page=200",
        do_api()
    ));
    while let Some(url) = next {
        let body = do_get(up, &url).await?;
        if let Some(repos) = body.get("repositories").and_then(|r| r.as_array()) {
            for r in repos {
                if let Some(short) = r.get("name").and_then(|n| n.as_str()) {
                    out.push(format!("{registry}/{short}"));
                }
            }
        }
        next = body
            .get("links")
            .and_then(|l| l.get("pages"))
            .and_then(|p| p.get("next"))
            .and_then(|n| n.as_str())
            .map(String::from);
    }
    Ok(out)
}

// ── GitHub (GHCR) ──────────────────────────────────────────────────────

/// GET a GitHub API URL with the PAT as a bearer. Returns the response so the
/// caller can read both the JSON body and the `Link` pagination header.
async fn gh_get(up: &OrgUpstream, url: &str) -> Result<reqwest::Response> {
    let resp = CLIENT
        .get(url)
        .bearer_auth(api_token(up)?)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| provider_error(format!("GitHub API request failed: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(provider_error(
            "GitHub rejected the token (401) — needs a PAT with the read:packages scope",
        ));
    }
    if !status.is_success() {
        return Err(provider_error(format!("GitHub API status {status}")));
    }
    Ok(resp)
}

/// The authenticated user's login, used to decide the packages endpoint and to
/// offer "your own packages" as a namespace.
async fn gh_login(up: &OrgUpstream) -> Result<String> {
    let body: serde_json::Value = gh_get(up, &format!("{}/user", gh_api()))
        .await?
        .json()
        .await
        .map_err(|e| provider_error(format!("reading GitHub user: {e}")))?;
    body.get("login")
        .and_then(|l| l.as_str())
        .map(String::from)
        .ok_or_else(|| provider_error("GitHub user response had no login"))
}

/// Owners the token can enumerate: the authenticated user plus its orgs.
async fn gh_owners(up: &OrgUpstream) -> Result<Vec<Namespace>> {
    let mut out = vec![Namespace {
        name: gh_login(up).await?,
        repo_count: None,
    }];
    let orgs: serde_json::Value = gh_get(up, &format!("{}/user/orgs?per_page=100", gh_api()))
        .await?
        .json()
        .await
        .map_err(|e| provider_error(format!("reading GitHub orgs: {e}")))?;
    if let Some(arr) = orgs.as_array() {
        for o in arr {
            if let Some(login) = o.get("login").and_then(|l| l.as_str()) {
                out.push(Namespace {
                    name: login.to_string(),
                    repo_count: None,
                });
            }
        }
    }
    Ok(out)
}

/// List an owner's container packages as OCI repo names (`<owner>/<package>`).
/// Uses `/user/packages` for the authenticated user, `/orgs/<owner>/packages`
/// otherwise; paginates via the `Link: rel="next"` header.
async fn gh_packages(up: &OrgUpstream, owner: &str) -> Result<Vec<String>> {
    let login = gh_login(up).await?;
    let base = if owner.eq_ignore_ascii_case(&login) {
        format!(
            "{}/user/packages?package_type=container&per_page=100",
            gh_api()
        )
    } else {
        format!(
            "{}/orgs/{owner}/packages?package_type=container&per_page=100",
            gh_api()
        )
    };

    let mut out = Vec::new();
    let mut next = Some(base);
    while let Some(url) = next {
        let resp = gh_get(up, &url).await?;
        next = link_next(resp.headers());
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| provider_error(format!("reading GitHub packages: {e}")))?;
        if let Some(arr) = body.as_array() {
            for pkg in arr {
                if let Some(name) = pkg.get("name").and_then(|n| n.as_str()) {
                    out.push(format!("{owner}/{name}"));
                }
            }
        }
    }
    Ok(out)
}

/// Parse an absolute `Link: <url>; rel="next"` header (GitHub pagination).
fn link_next(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        if part.contains("rel=\"next\"") {
            let start = part.find('<')? + 1;
            let end = part[start..].find('>')? + start;
            return Some(part[start..end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::Provider;

    #[test]
    fn parses_provider_aliases() {
        assert_eq!(
            Provider::parse(Some("digitalocean")),
            Provider::DigitalOcean
        );
        assert_eq!(Provider::parse(Some("DOCR")), Provider::DigitalOcean);
        assert_eq!(Provider::parse(Some("ghcr")), Provider::Github);
        assert_eq!(Provider::parse(Some("github")), Provider::Github);
        assert_eq!(Provider::parse(Some("")), Provider::Generic);
        assert_eq!(Provider::parse(None), Provider::Generic);
        assert_eq!(Provider::parse(Some("harbor")), Provider::Generic);
    }

    #[test]
    fn registry_url_is_fixed_for_api_providers() {
        assert_eq!(Provider::Generic.registry_url(), None);
        assert_eq!(
            Provider::DigitalOcean.registry_url().as_deref(),
            Some("https://registry.digitalocean.com")
        );
        assert_eq!(
            Provider::Github.registry_url().as_deref(),
            Some("https://ghcr.io")
        );
    }
}
