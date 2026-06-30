//! ruskery — a high-performance, S3-backed (Tigris) Docker/OCI registry.

mod api;
mod auth;
mod config;
mod db;
mod error;
mod gc;
mod models;
mod rate_limit;
mod registry;
mod server;
mod state;
mod storage;
mod tls;
mod util;
mod web;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::{prelude::*, EnvFilter};

use models::OrgRole;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "ruskery", version, about, long_about = None)]
struct Cli {
    /// Path to the configuration file.
    #[arg(
        short,
        long,
        env = "RUSKERY_CONFIG",
        default_value = "/etc/ruskery/config.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run database migrations and start the server (default).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Run a one-off garbage-collection sweep (delete unreferenced blobs).
    Gc,
    /// Administrative commands for bootstrapping (orgs, users, tokens).
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Create a user account.
    CreateUser {
        #[arg(long)]
        email: String,
        #[arg(long)]
        username: String,
        #[arg(long)]
        password: String,
        /// Grant instance super-admin.
        #[arg(long, default_value_t = false)]
        admin: bool,
    },
    /// Create an organization.
    CreateOrg {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: String,
    },
    /// Add a user to an org with a role (owner|admin|member).
    AddMember {
        #[arg(long)]
        org: String,
        #[arg(long)]
        username: String,
        #[arg(long, default_value = "member")]
        role: String,
    },
    /// Create a personal access token for a user (printed once).
    CreateToken {
        #[arg(long)]
        username: String,
        #[arg(long, default_value = "cli")]
        name: String,
        /// Scope the token to an org slug.
        #[arg(long)]
        org: Option<String>,
        /// Scope the token to a repo (requires --org).
        #[arg(long)]
        repo: Option<String>,
        /// Permission cap: pull | push | admin.
        #[arg(long, default_value = "admin")]
        permission: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config = config::Config::load(Some(&cli.config))?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Migrate => {
            let pool = db::connect(&config.database.path).await?;
            db::migrate(&pool).await?;
            tracing::info!("migrations applied");
        }
        Command::Gc => {
            let pool = db::connect(&config.database.path).await?;
            db::migrate(&pool).await?;
            let secret_key =
                db::settings::ensure_secret_key(&pool, &config.auth.secret_key).await?;
            let storage = storage::Storage::new(&config.storage).await?;
            let state = state::AppState::new(config, pool, storage, secret_key);
            let n = gc::run(&state).await?;
            println!("garbage collected {n} blob(s)");
        }
        Command::Admin(cmd) => {
            let pool = db::connect(&config.database.path).await?;
            db::migrate(&pool).await?;
            run_admin(&pool, cmd).await?;
        }
        Command::Serve => {
            let pool = db::connect(&config.database.path).await?;
            db::migrate(&pool).await?;

            let secret_key =
                db::settings::ensure_secret_key(&pool, &config.auth.secret_key).await?;

            let storage = storage::Storage::new(&config.storage).await?;

            let tls_enabled = config.tls.enabled;
            let http_addr = config.server.http_addr.clone();
            let gc_interval = config.gc.interval_secs;
            let state = state::AppState::new(config, pool, storage, secret_key);
            let app = server::router(state.clone());

            tokio::spawn(gc::background(state.clone(), gc_interval));

            if tls_enabled {
                tls::serve(state, app).await?;
            } else {
                server::serve_http(&http_addr, app).await?;
            }
        }
    }

    Ok(())
}

async fn run_admin(pool: &db::Db, cmd: AdminCommand) -> anyhow::Result<()> {
    match cmd {
        AdminCommand::CreateUser {
            email,
            username,
            password,
            admin,
        } => {
            let hash = auth::password::hash_password(&password)?;
            let user = db::users::create(pool, &email, &username, &hash, admin).await?;
            println!("created user {} ({})", user.username, user.id);
        }
        AdminCommand::CreateOrg { slug, name } => {
            let org = db::orgs::create_org(pool, &slug, &name).await?;
            println!("created org {} ({})", org.slug, org.id);
        }
        AdminCommand::AddMember {
            org,
            username,
            role,
        } => {
            let org = db::orgs::find_org_by_slug(pool, &org)
                .await?
                .ok_or_else(|| anyhow::anyhow!("org not found"))?;
            let user = db::users::find_by_login(pool, &username)
                .await?
                .ok_or_else(|| anyhow::anyhow!("user not found"))?;
            let role = OrgRole::parse(&role).ok_or_else(|| anyhow::anyhow!("invalid role"))?;
            db::orgs::add_org_member(pool, &org.id, &user.id, role).await?;
            println!(
                "added {} to {} as {}",
                user.username,
                org.slug,
                role.as_str()
            );
        }
        AdminCommand::CreateToken {
            username,
            name,
            org,
            repo,
            permission,
        } => {
            if !matches!(permission.as_str(), "pull" | "push" | "admin") {
                return Err(anyhow::anyhow!("permission must be pull|push|admin"));
            }
            let user = db::users::find_by_login(pool, &username)
                .await?
                .ok_or_else(|| anyhow::anyhow!("user not found"))?;
            let (kind, org_id, repo_id) = match (org.as_deref(), repo.as_deref()) {
                (Some(slug), Some(repo_name)) => {
                    let org = db::orgs::find_org_by_slug(pool, slug)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("org not found"))?;
                    let r = db::orgs::find_repo(pool, &org.id, repo_name)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("repo not found"))?;
                    ("repo", None, Some(r.id))
                }
                (Some(slug), None) => {
                    let org = db::orgs::find_org_by_slug(pool, slug)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("org not found"))?;
                    ("org", Some(org.id), None)
                }
                _ => ("all", None, None),
            };
            let token = db::users::create_pat(
                pool,
                &user.id,
                &name,
                kind,
                org_id.as_deref(),
                repo_id.as_deref(),
                &permission,
            )
            .await?;
            println!("{token}");
        }
    }
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ruskery=debug,tower_http=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}
