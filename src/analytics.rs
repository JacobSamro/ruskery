//! In-memory usage capture. The hot path (every push/pull) only increments a
//! sharded `DashMap` counter — no per-request DB write. A background task drains
//! the counters into the daily rollup tables every `rollup_secs`, so SQLite sees
//! roughly one batched upsert per flush regardless of pull volume.

use std::sync::Mutex;

use dashmap::DashMap;

use crate::db::{self, Db};
use crate::error::Result;
use crate::state::AppState;
use crate::util::now_rfc3339;

/// The event kinds tracked per repo/user/day.
#[derive(Clone, Copy)]
pub enum Kind {
    ManifestPush,
    ManifestPull,
    BlobUpload,
    BlobServe,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::ManifestPush => "manifest.push",
            Kind::ManifestPull => "manifest.pull",
            Kind::BlobUpload => "blob.upload",
            Kind::BlobServe => "blob.serve",
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Counter {
    count: i64,
    bytes: i64,
}

/// Key: (day, org_id, repo|user_id, kind).
type Key = (String, String, String, String);

pub struct UsageCollector {
    enabled: bool,
    repo: DashMap<Key, Counter>,
    user: DashMap<Key, Counter>,
    last_snapshot_day: Mutex<String>,
}

impl UsageCollector {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            repo: DashMap::new(),
            user: DashMap::new(),
            last_snapshot_day: Mutex::new(String::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Current UTC day, `YYYY-MM-DD` (the RFC3339 timestamp's date prefix).
    fn today() -> String {
        now_rfc3339().chars().take(10).collect()
    }

    /// Record one event. Non-blocking and never fails — analytics must never
    /// interfere with the registry operation that triggered it.
    pub fn record(&self, org_id: &str, repo: &str, user_id: Option<&str>, kind: Kind, bytes: i64) {
        if !self.enabled {
            return;
        }
        let day = Self::today();
        let kind = kind.as_str().to_string();
        bump(
            &self.repo,
            (
                day.clone(),
                org_id.to_string(),
                repo.to_string(),
                kind.clone(),
            ),
            bytes,
        );
        if let Some(uid) = user_id {
            bump(
                &self.user,
                (day, org_id.to_string(), uid.to_string(), kind),
                bytes,
            );
        }
    }

    /// Drain the in-memory counters into the rollup tables.
    pub async fn flush(&self, db: &Db) -> Result<()> {
        let repo_rows = drain(&self.repo);
        let user_rows = drain(&self.user);
        db::analytics::flush_batch(db, &repo_rows, &user_rows).await
    }

    /// Take the daily storage snapshot once per day.
    pub async fn maybe_snapshot(&self, db: &Db) -> Result<()> {
        let day = Self::today();
        {
            let mut last = self.last_snapshot_day.lock().expect("snapshot mutex");
            if *last == day {
                return Ok(());
            }
            *last = day.clone();
        }
        db::analytics::snapshot_storage(db, &day).await
    }
}

fn bump(map: &DashMap<Key, Counter>, key: Key, bytes: i64) {
    map.entry(key)
        .and_modify(|c| {
            c.count += 1;
            c.bytes += bytes;
        })
        .or_insert(Counter { count: 1, bytes });
}

/// Remove and return every counter as a rollup row tuple.
fn drain(map: &DashMap<Key, Counter>) -> Vec<db::analytics::RepoRow> {
    let mut rows = Vec::new();
    map.retain(|k, v| {
        rows.push((
            k.0.clone(),
            k.1.clone(),
            k.2.clone(),
            k.3.clone(),
            v.count,
            v.bytes,
        ));
        false // remove every entry
    });
    rows
}

/// Background flush + daily snapshot loop (spawned by `serve`).
pub async fn flush_loop(state: AppState, interval_secs: u64) {
    if !state.usage().enabled() || interval_secs == 0 {
        return;
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    loop {
        tick.tick().await;
        if let Err(e) = state.usage().flush(state.db()).await {
            tracing::warn!(error = %e, "usage rollup flush failed");
        }
        if let Err(e) = state.usage().maybe_snapshot(state.db()).await {
            tracing::warn!(error = %e, "storage snapshot failed");
        }
    }
}
