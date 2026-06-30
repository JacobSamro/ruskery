//! In-memory caches for the pull hot path.
//!
//! Manifests are content-addressed, so a `(repo_id, digest) → bytes` entry is
//! immutable; only tag resolutions (and a digest's `media_type`, which comes
//! from the push's `Content-Type`) are mutable, and the write paths invalidate
//! them. ruskery is single-process, so the cache is always consistent with
//! SQLite — there is no second node to skew it.
//!
//! ## Coherency under concurrency
//!
//! A read that misses does `DB read → populate`, which races writers: a delete
//! or re-push can commit and invalidate *between* the reader's DB read and its
//! cache insert, leaving the reader about to cache a stale entry. Left
//! unguarded, a pull-by-digest could then serve a deleted manifest until LRU
//! eviction. We close this with a **generation counter**: every invalidation
//! bumps it, and a read-path populate carries the generation snapshotted
//! *before* its DB read, inserting only if the generation is still current
//! (`put_*_if_current`). Authoritative writes (a push setting `tag → digest`)
//! insert unconditionally, but the push bumps the generation first (via
//! `invalidate_manifest`), so a racing stale populate is cancelled. Both maps
//! and the counter live under one mutex, so the generation check and the insert
//! are atomic with respect to invalidation.
//!
//! Keys are composite strings (`<repo_id>|<value>`). A repo id is a UUID and a
//! digest is `<algo>:<hex>`; neither contains `|`, and tag names are limited to
//! the OCI repository-name charset (also no `|`), so the separator is
//! unambiguous and `<repo_id>|` is a safe prefix for per-repo invalidation.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use lru::LruCache;

use crate::config::CacheConfig;

/// A manifest ready to serve: media type, byte length, and the bytes. `content`
/// is `Bytes`, so cloning a cached entry into a response is an `Arc` bump rather
/// than a copy.
#[derive(Clone)]
pub struct CachedManifest {
    pub media_type: String,
    pub size: i64,
    pub content: Bytes,
}

struct Inner {
    /// Bumped on every invalidation; gates optimistic read-path populates.
    generation: u64,
    manifests: LruCache<String, Arc<CachedManifest>>,
    tags: LruCache<String, String>,
}

/// Bounded LRU caches for manifest reads and tag→digest resolutions, plus the
/// invalidation generation, all behind a single mutex.
pub struct ManifestCache {
    enabled: bool,
    inner: Mutex<Inner>,
}

fn key(repo_id: &str, value: &str) -> String {
    format!("{repo_id}|{value}")
}

impl ManifestCache {
    pub fn new(cfg: &CacheConfig) -> Self {
        // `.max(1)` keeps `NonZeroUsize` happy if a config sets 0; `enabled`
        // gates every access, so a disabled cache never stores an entry.
        let mcap = NonZeroUsize::new(cfg.manifest_capacity.max(1)).unwrap();
        let tcap = NonZeroUsize::new(cfg.tag_capacity.max(1)).unwrap();
        Self {
            enabled: cfg.enabled,
            inner: Mutex::new(Inner {
                generation: 0,
                manifests: LruCache::new(mcap),
                tags: LruCache::new(tcap),
            }),
        }
    }

    // The lock is only ever held for a single map op (no `.await` inside), and
    // those ops don't panic, so poisoning is effectively impossible; recover
    // from it anyway rather than cascade a panic.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current invalidation generation. Snapshot this *before* a DB read,
    /// then pass it to a `put_*_if_current` so a racing invalidation cancels the
    /// stale fill. Returns 0 when the cache is disabled (never matched, since
    /// `put_*_if_current` is also a no-op then).
    pub fn generation(&self) -> u64 {
        if !self.enabled {
            return 0;
        }
        self.lock().generation
    }

    /// Cached manifest bytes for `(repo_id, digest)`, if present. A hit is always
    /// fresh: any invalidation since the insert would have evicted it under this
    /// same lock.
    pub fn get_manifest(&self, repo_id: &str, digest: &str) -> Option<Arc<CachedManifest>> {
        if !self.enabled {
            return None;
        }
        self.lock().manifests.get(&key(repo_id, digest)).cloned()
    }

    /// Cache manifest bytes under `(repo_id, digest)` from a read-path populate,
    /// but only if no invalidation has happened since `gen` was snapshotted.
    pub fn put_manifest_if_current(
        &self,
        repo_id: &str,
        digest: &str,
        m: Arc<CachedManifest>,
        gen: u64,
    ) {
        if !self.enabled {
            return;
        }
        let mut inner = self.lock();
        if inner.generation == gen {
            inner.manifests.put(key(repo_id, digest), m);
        }
    }

    /// Drop the cached bytes for `(repo_id, digest)` (manifest deleted/replaced)
    /// and bump the generation so any in-flight populate is cancelled.
    pub fn invalidate_manifest(&self, repo_id: &str, digest: &str) {
        if !self.enabled {
            return;
        }
        let mut inner = self.lock();
        inner.generation = inner.generation.wrapping_add(1);
        inner.manifests.pop(&key(repo_id, digest));
    }

    /// Cached digest a tag resolves to, if present. Fresh for the same reason as
    /// `get_manifest`.
    pub fn get_tag(&self, repo_id: &str, tag: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.lock().tags.get(&key(repo_id, tag)).cloned()
    }

    /// Authoritative `tag → digest` write (a push). Unconditional: the caller
    /// has already bumped the generation (via `invalidate_manifest`), so a
    /// racing stale read-populate is cancelled rather than overwriting this.
    pub fn put_tag(&self, repo_id: &str, tag: &str, digest: &str) {
        if !self.enabled {
            return;
        }
        self.lock().tags.put(key(repo_id, tag), digest.to_string());
    }

    /// Cache `tag → digest` from a read-path populate, only if current.
    pub fn put_tag_if_current(&self, repo_id: &str, tag: &str, digest: &str, gen: u64) {
        if !self.enabled {
            return;
        }
        let mut inner = self.lock();
        if inner.generation == gen {
            inner.tags.put(key(repo_id, tag), digest.to_string());
        }
    }

    /// Drop every cached tag resolution for a repository and bump the
    /// generation. Used on manifest delete: a deleted digest may have had
    /// several tags pointing at it and we can't enumerate them here, so the
    /// whole repo's tag set is re-resolved from SQLite on the next pull (cheap;
    /// manifest deletes are rare).
    pub fn invalidate_repo_tags(&self, repo_id: &str) {
        if !self.enabled {
            return;
        }
        let prefix = format!("{repo_id}|");
        let mut inner = self.lock();
        inner.generation = inner.generation.wrapping_add(1);
        let stale: Vec<String> = inner
            .tags
            .iter()
            .map(|(k, _)| k.clone())
            .filter(|k| k.starts_with(&prefix))
            .collect();
        for k in stale {
            inner.tags.pop(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(enabled: bool) -> ManifestCache {
        ManifestCache::new(&CacheConfig {
            enabled,
            manifest_capacity: 8,
            tag_capacity: 8,
        })
    }

    fn manifest(body: &str) -> Arc<CachedManifest> {
        Arc::new(CachedManifest {
            media_type: "application/vnd.oci.image.manifest.v1+json".into(),
            size: body.len() as i64,
            content: Bytes::from(body.to_string()),
        })
    }

    #[test]
    fn round_trips_manifest_and_tag() {
        let c = cache(true);
        let gen = c.generation();
        assert!(c.get_manifest("org/repo", "sha256:aa").is_none());
        c.put_manifest_if_current("org/repo", "sha256:aa", manifest("hello"), gen);
        let got = c.get_manifest("org/repo", "sha256:aa").unwrap();
        assert_eq!(&got.content[..], b"hello");

        assert!(c.get_tag("org/repo", "latest").is_none());
        c.put_tag_if_current("org/repo", "latest", "sha256:aa", gen);
        assert_eq!(
            c.get_tag("org/repo", "latest").as_deref(),
            Some("sha256:aa")
        );
    }

    #[test]
    fn invalidate_manifest_drops_only_that_entry() {
        let c = cache(true);
        let gen = c.generation();
        c.put_manifest_if_current("r", "sha256:aa", manifest("a"), gen);
        c.put_manifest_if_current("r", "sha256:bb", manifest("b"), gen);
        c.invalidate_manifest("r", "sha256:aa");
        assert!(c.get_manifest("r", "sha256:aa").is_none());
        assert!(c.get_manifest("r", "sha256:bb").is_some());
    }

    #[test]
    fn invalidate_repo_tags_clears_only_that_repo() {
        let c = cache(true);
        let gen = c.generation();
        c.put_tag_if_current("r1", "latest", "sha256:aa", gen);
        c.put_tag_if_current("r1", "v1", "sha256:aa", gen);
        c.put_tag_if_current("r2", "latest", "sha256:cc", gen);
        c.invalidate_repo_tags("r1");
        assert!(c.get_tag("r1", "latest").is_none());
        assert!(c.get_tag("r1", "v1").is_none());
        assert_eq!(c.get_tag("r2", "latest").as_deref(), Some("sha256:cc"));
    }

    #[test]
    fn repo_prefix_is_not_confused_by_a_shared_prefix() {
        // "r1" must not match keys for a repo whose id starts with "r1".
        let c = cache(true);
        let gen = c.generation();
        c.put_tag_if_current("r1", "latest", "sha256:aa", gen);
        c.put_tag_if_current("r10", "latest", "sha256:bb", gen);
        c.invalidate_repo_tags("r1");
        assert!(c.get_tag("r1", "latest").is_none());
        assert_eq!(c.get_tag("r10", "latest").as_deref(), Some("sha256:bb"));
    }

    #[test]
    fn stale_populate_is_rejected_after_invalidation() {
        // Models a read that snapshots the generation, then a concurrent delete
        // bumps it, then the read tries to populate: the stale fill must be
        // dropped, not cached.
        let c = cache(true);
        let gen = c.generation();
        c.invalidate_manifest("r", "sha256:aa"); // a racing delete bumps the gen
        c.put_manifest_if_current("r", "sha256:aa", manifest("stale"), gen);
        assert!(c.get_manifest("r", "sha256:aa").is_none());

        c.put_tag_if_current("r", "latest", "sha256:aa", gen);
        assert!(c.get_tag("r", "latest").is_none());

        // A fresh snapshot populates normally.
        let gen2 = c.generation();
        c.put_manifest_if_current("r", "sha256:aa", manifest("fresh"), gen2);
        assert!(c.get_manifest("r", "sha256:aa").is_some());
    }

    #[test]
    fn authoritative_tag_write_is_unconditional() {
        // A push refreshes the tag regardless of generation churn.
        let c = cache(true);
        c.invalidate_manifest("r", "sha256:new");
        c.put_tag("r", "latest", "sha256:new");
        assert_eq!(c.get_tag("r", "latest").as_deref(), Some("sha256:new"));
    }

    #[test]
    fn disabled_cache_stores_nothing() {
        let c = cache(false);
        let gen = c.generation();
        c.put_manifest_if_current("r", "sha256:aa", manifest("a"), gen);
        c.put_tag("r", "latest", "sha256:aa");
        assert!(c.get_manifest("r", "sha256:aa").is_none());
        assert!(c.get_tag("r", "latest").is_none());
    }
}
