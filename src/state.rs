//! Shared application state passed to every handler.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::analytics::UsageCollector;
use crate::cache::ManifestCache;
use crate::config::Config;
use crate::db::Db;
use crate::registry::uploads::UploadRegistry;
use crate::storage::Storage;

/// Cheaply-cloneable handle to shared server state.
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub config: Config,
    pub db: Db,
    /// Effective public base URL (`scheme://host`, no trailing slash). Explicit
    /// `server.public_url` config wins; otherwise it's derived from the primary
    /// custom domain and refreshed whenever the domain set changes. Empty during
    /// the IP-only bootstrap phase, in which case handlers fall back to the
    /// request's `Host` header.
    pub public_url: ArcSwap<String>,
    /// Hot-swappable storage client (rebuilt when admins change settings).
    pub storage: ArcSwap<Storage>,
    /// In-memory registry of in-progress blob uploads (single-process).
    pub uploads: UploadRegistry,
    /// Secret used to sign session cookies and registry JWTs.
    pub secret_key: Vec<u8>,
    /// Notified when the custom-domain set changes, so the TLS task reloads.
    pub domains_changed: tokio::sync::Notify,
    /// In-memory usage counters, flushed to the analytics rollup tables.
    pub usage: UsageCollector,
    /// Bounded in-memory manifest read cache for the pull hot path.
    pub cache: ManifestCache,
}

impl AppState {
    pub fn new(config: Config, db: Db, storage: Storage, secret_key: Vec<u8>) -> Self {
        let usage = UsageCollector::new(config.analytics.enabled);
        let cache = ManifestCache::new(&config.cache);
        let public_url = config.server.public_url.trim_end_matches('/').to_string();
        AppState(Arc::new(Inner {
            config,
            db,
            public_url: ArcSwap::from_pointee(public_url),
            storage: ArcSwap::from_pointee(storage),
            uploads: UploadRegistry::new(),
            secret_key,
            domains_changed: tokio::sync::Notify::new(),
            usage,
            cache,
        }))
    }

    /// Usage analytics collector (in-memory; flushed by the background task).
    pub fn usage(&self) -> &UsageCollector {
        &self.0.usage
    }

    /// Bounded in-memory manifest read cache.
    pub fn cache(&self) -> &ManifestCache {
        &self.0.cache
    }

    /// Wake the TLS task to reload its certificate domain set.
    pub fn notify_domains_changed(&self) {
        self.0.domains_changed.notify_one();
    }

    /// Await the next domain-set change.
    pub async fn domains_changed(&self) {
        self.0.domains_changed.notified().await;
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    /// Current effective public base URL (`scheme://host`, no trailing slash).
    /// Empty during the IP-only bootstrap phase.
    pub fn public_url(&self) -> Arc<String> {
        self.0.public_url.load_full()
    }

    /// Recompute the effective public URL and store it. Explicit config always
    /// wins; otherwise derive `https://<primary-domain>`, or empty if no domain
    /// is configured yet. Called at startup and whenever the domain set changes,
    /// so adding a domain in the dashboard updates the registry realm/audience
    /// without a restart.
    pub async fn refresh_public_url(&self) {
        let configured = self.0.config.server.public_url.trim_end_matches('/');
        let effective = if !configured.is_empty() {
            configured.to_string()
        } else {
            match crate::db::domains::primary(self.db()).await {
                Ok(Some(domain)) => format!("https://{domain}"),
                _ => String::new(),
            }
        };
        self.0.public_url.store(Arc::new(effective));
    }

    pub fn db(&self) -> &Db {
        &self.0.db
    }

    /// Current storage client (cheap Arc load; safe to hold across awaits).
    pub fn storage(&self) -> Arc<Storage> {
        self.0.storage.load_full()
    }

    /// Replace the storage client after a settings change.
    pub fn set_storage(&self, storage: Storage) {
        self.0.storage.store(Arc::new(storage));
    }

    pub fn uploads(&self) -> &UploadRegistry {
        &self.0.uploads
    }

    pub fn secret_key(&self) -> &[u8] {
        &self.0.secret_key
    }

    /// Whether session cookies should carry the `Secure` attribute (only when
    /// the public URL is HTTPS; lets dashboard login work over plain HTTP in dev).
    pub fn cookie_secure(&self) -> bool {
        self.0.config.server.public_url.starts_with("https://")
    }
}
