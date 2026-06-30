//! SQLite connection pool (WAL mode) and migration runner.

pub mod analytics;
pub mod audit;
pub mod content;
pub mod domains;
pub mod orgs;
pub mod settings;
pub mod users;

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

/// Shared database handle.
pub type Db = SqlitePool;

/// Open (creating if needed) the SQLite database with performance-oriented
/// pragmas: WAL journaling, `NORMAL` synchronous, foreign keys on, and a busy
/// timeout so concurrent writers retry instead of erroring.
pub async fn connect(path: &Path) -> anyhow::Result<Db> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let url = format!("sqlite://{}", path.display());
    let opts = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(10))
        .pragma("temp_store", "memory")
        .pragma("mmap_size", "268435456"); // 256 MiB

    // SQLite writes are serialized; a small pool with one dedicated writer
    // connection is the standard high-throughput setup. Readers use WAL.
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    Ok(pool)
}

/// Apply all pending migrations from the embedded `migrations/` directory.
pub async fn migrate(pool: &Db) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
