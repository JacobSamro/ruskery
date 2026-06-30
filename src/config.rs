//! Layered configuration: defaults < `config.toml` < `RUSKERY_*` environment variables.

use std::path::PathBuf;

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

/// Top-level runtime configuration for the ruskery server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// HTTP/HTTPS server settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// SQLite database settings.
    #[serde(default)]
    pub database: DatabaseConfig,
    /// Tigris (S3-compatible) object storage settings.
    #[serde(default)]
    pub storage: StorageConfig,
    /// Authentication / token settings.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Automatic TLS (Let's Encrypt) settings.
    #[serde(default)]
    pub tls: TlsConfig,
    /// Garbage collection settings.
    #[serde(default)]
    pub gc: GcConfig,
    /// Usage analytics settings.
    #[serde(default)]
    pub analytics: AnalyticsConfig,
    /// In-memory manifest read cache settings.
    #[serde(default)]
    pub cache: CacheConfig,
    /// Storage quota / upload-size limits.
    #[serde(default)]
    pub quota: QuotaConfig,
}

/// Storage quota / upload-size limits. Both default to `0` (unlimited) — quotas
/// are opt-in per deployment.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuotaConfig {
    /// Default per-org storage cap in bytes (deduplicated total of an org's blob
    /// sizes). `0` = unlimited. A per-org override in the database
    /// (`orgs.storage_quota_bytes`) takes precedence over this default.
    pub default_storage_bytes: u64,
    /// Maximum size in bytes of a single uploaded blob. `0` = unlimited.
    /// Enforced while the upload streams, so an over-size blob is rejected
    /// without being fully written to object storage.
    pub max_blob_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Cache manifest bytes and tag→digest resolutions in memory on the pull
    /// hot path, so a repeated pull skips the SQLite read. Safe because
    /// manifests are content-addressed (immutable per digest) and the push /
    /// delete paths invalidate tag resolutions.
    pub enabled: bool,
    /// Max number of manifest entries (keyed by digest) held in memory.
    pub manifest_capacity: usize,
    /// Max number of tag→digest resolutions held in memory.
    pub tag_capacity: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            manifest_capacity: 1024,
            tag_capacity: 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsConfig {
    /// Capture per-org usage (push/pull/storage). In-memory + daily rollups.
    pub enabled: bool,
    /// How often (seconds) to flush in-memory counters to the rollup tables and
    /// take the daily storage snapshot.
    pub rollup_secs: u64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rollup_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcConfig {
    /// Interval (seconds) for the background blob GC sweep. 0 disables it.
    pub interval_secs: u64,
    /// Blobs younger than this (seconds) are never collected — a grace window
    /// protecting freshly-uploaded blobs whose manifest isn't committed yet.
    pub grace_secs: i64,
}

impl Default for GcConfig {
    fn default() -> Self {
        // Off by default; run `ruskery gc` manually or set an interval.
        Self {
            interval_secs: 0,
            grace_secs: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Address for the plain-HTTP listener (also serves ACME HTTP-01 + redirect).
    pub http_addr: String,
    /// Address for the HTTPS listener. Only bound when TLS is enabled.
    pub https_addr: String,
    /// Public base URL (e.g. `https://registry.example.com`) used to build
    /// the registry realm and `docker pull` hints. May be empty before setup.
    #[serde(default)]
    pub public_url: String,
    /// Maximum size (bytes) accepted for a single upload PATCH chunk / manifest.
    pub max_body_bytes: usize,
    /// Trust `X-Forwarded-For` / `X-Real-IP` for the client IP (rate limiting).
    /// Enable only when behind a trusted reverse proxy / load balancer that
    /// sets these; otherwise the peer socket address is used.
    #[serde(default)]
    pub trust_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Path to the SQLite database file.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// S3 endpoint URL. For Tigris: `https://t3.storage.dev` (or fly.io variant).
    pub endpoint: String,
    /// Bucket name that backs the registry.
    pub bucket: String,
    /// Region label (Tigris accepts `auto`).
    pub region: String,
    /// Access key id. Prefer setting via `RUSKERY_STORAGE__ACCESS_KEY_ID`.
    #[serde(default)]
    pub access_key_id: String,
    /// Secret access key. Prefer setting via env, not the config file.
    #[serde(default)]
    pub secret_access_key: String,
    /// TTL (seconds) for presigned GET URLs handed to docker clients on pull.
    pub presign_ttl_secs: u64,
    /// Use path-style addressing (`endpoint/bucket/key`) instead of virtual-hosted.
    pub force_path_style: bool,
    /// Optional CDN / custom-domain base URL used to sign + serve pull
    /// redirects (e.g. a Tigris custom domain). Empty → use `endpoint`.
    #[serde(default)]
    pub cdn_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Secret used to sign session cookies and registry JWTs. Auto-generated at
    /// first run if empty and persisted to the database.
    #[serde(default)]
    pub secret_key: String,
    /// Lifetime (seconds) of a registry bearer token.
    pub token_ttl_secs: u64,
    /// Lifetime (seconds) of a dashboard session.
    pub session_ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Master switch for automatic Let's Encrypt TLS on the HTTPS listener.
    pub enabled: bool,
    /// Contact email registered with the ACME provider.
    #[serde(default)]
    pub contact_email: String,
    /// Use the Let's Encrypt staging environment (for testing, avoids rate limits).
    pub staging: bool,
    /// Directory where issued certificates / ACME account data are cached.
    pub cache_dir: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_addr: "0.0.0.0:80".into(),
            https_addr: "0.0.0.0:443".into(),
            public_url: String::new(),
            // 0 means "fall back to a large streaming-friendly default"; the
            // registry streams uploads, so this only bounds non-stream bodies.
            max_body_bytes: 32 * 1024 * 1024,
            trust_proxy: false,
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("/var/lib/ruskery/ruskery.db"),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://t3.storage.dev".into(),
            bucket: String::new(),
            region: "auto".into(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            presign_ttl_secs: 900,
            force_path_style: false,
            cdn_url: String::new(),
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret_key: String::new(),
            token_ttl_secs: 300,
            session_ttl_secs: 7 * 24 * 3600,
        }
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            // On by default: as soon as a domain is added in the dashboard, a
            // Let's Encrypt cert is provisioned on :443 with no restart. With no
            // domain yet the server stays on plain HTTP (reachable by IP for
            // first-run setup) — see `crate::tls::serve`.
            enabled: true,
            contact_email: String::new(),
            staging: false,
            cache_dir: PathBuf::from("/var/lib/ruskery/acme"),
        }
    }
}

#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            storage: StorageConfig::default(),
            auth: AuthConfig::default(),
            tls: TlsConfig::default(),
            gc: GcConfig::default(),
            analytics: AnalyticsConfig::default(),
            cache: CacheConfig::default(),
            quota: QuotaConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration, merging (in increasing priority): built-in defaults,
    /// the TOML file at `path` (if present), then `RUSKERY_*` environment
    /// variables. Nested keys use `__` as the separator, e.g.
    /// `RUSKERY_STORAGE__BUCKET=my-bucket`.
    pub fn load(path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        let mut fig = Figment::from(Serialized::defaults(Config::default()));
        if let Some(p) = path {
            if p.exists() {
                fig = fig.merge(Toml::file(p));
            }
        }
        let cfg: Config = fig.merge(Env::prefixed("RUSKERY_").split("__")).extract()?;
        Ok(cfg)
    }
}
