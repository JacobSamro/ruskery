<div align="center">

# ruskery

**A fast, private container registry — a single Rust binary, backed by Tigris S3 as storage _and_ CDN.**

One‑line install · embedded dashboard · automatic HTTPS · multi‑tenant orgs & teams

[Quick start](#quick-start) · [Why ruskery](#why-ruskery) · [Configuration](#configuration) · [Architecture](#architecture)

</div>

---

ruskery is an [OCI Distribution](https://github.com/opencontainers/distribution-spec)‑compliant
Docker registry designed to run on the smallest box you have. It keeps image layers out of the
data path entirely: pulls are answered with a redirect to a presigned [Tigris](https://www.tigrisdata.com/)
URL, so bytes stream straight from the nearest CDN edge while your droplet only does auth and
metadata. The whole thing — registry, REST API, and a shadcn‑style web dashboard — ships as one
self-contained binary with no runtime services to manage.

## Features

- 🦀 **One self-contained binary.** No Docker, no Postgres, no Redis. Embedded SQLite (WAL) + an embedded web UI.
- ⚡ **Pulls served from the CDN.** `307` redirect to presigned Tigris URLs — layer bytes never touch the server.
- 🔐 **Private & multi‑tenant.** Organizations → teams → users, with per‑repository `pull`/`push`/`admin` grants.
- 🔑 **Real auth.** Argon2id passwords, personal access tokens for `docker login`, short‑lived scoped JWTs for the registry.
- 🔓 **Google sign-in (optional).** Configure an OAuth client right in the dashboard (it shows the exact redirect URI for the Google Cloud console); restrict to an email domain to auto-provision your team.
- 🌐 **Automatic HTTPS.** Add a domain in the dashboard; certificates are issued and renewed via Let's Encrypt (no certbot).
- 📈 **Usage analytics.** Per‑org pushes, pulls, storage, attributed egress, daily trends, and top repos/users — captured in memory and rolled up daily, so it stays light even under heavy pull traffic.
- 🧹 **Garbage collection.** Mark‑and‑sweep of unreferenced blobs, on a schedule or on demand.
- 📊 **Dashboard.** Browse repos & tags, manage members/teams/tokens, connect domains, read the audit log.
- 🛡️ **Hardened by default.** Per‑IP rate limiting on auth, CSP/HSTS/security headers, per‑org storage isolation.

## Quick start

### 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/jacobsamro/ruskery/master/install.sh | sudo sh
```

This drops a self-contained binary at `/usr/local/bin/ruskery`, writes `/etc/ruskery/config.toml`, and
installs + enables a `ruskery` systemd service. (Prefer containers? See [Docker](#run-with-docker).)

**Upgrading is the same command.** Re-running the installer fetches the latest release,
replaces the binary, **keeps your config and data**, applies any new database migrations,
and restarts the service. Pin a version with `RUSKERY_VERSION=v0.2.0` if you'd rather not
track latest.

### 2. Point it at a Tigris bucket

Edit `/etc/ruskery/config.toml` (or use environment variables) with your bucket and keys, then:

```sh
sudo systemctl restart ruskery
```

### 3. Finish setup in the browser

Open `http://your-server` and complete the first‑run wizard — it creates your admin account and
first organization. Then go to **Settings → Domains & TLS**, add your domain (point an `A` record
at the server first), and ruskery provisions a certificate automatically.

### 4. Push an image

```sh
docker login registry.example.com -u <you> -p <access-token>   # token from the dashboard
docker tag my-app registry.example.com/acme/my-app:latest
docker push registry.example.com/acme/my-app:latest
docker pull registry.example.com/acme/my-app:latest
```

> Image names are `<org-slug>/<repo>`. Create access tokens under **Access Tokens** in the dashboard.

## Why ruskery

Most self‑hosted registries put your droplet in the middle of every byte: a `docker pull` of a
multi‑gigabyte image saturates the box's network and CPU. ruskery treats Tigris as both the object
store **and** the CDN — the registry verifies your token, then hands the client a short‑lived
presigned URL and gets out of the way. The result is a registry that stays responsive on a
$6 VPS while serving images from a global edge network.

| | ruskery |
|---|---|
| Footprint | one self-contained binary + one SQLite file |
| Pull path | client → CDN edge (server not in the data path) |
| Push path | streamed straight into Tigris via S3 multipart, digest verified on the fly |
| TLS | automatic Let's Encrypt, managed from the dashboard |
| Dependencies | none at runtime |

## Configuration

Configuration is layered: built‑in defaults < `/etc/ruskery/config.toml` < `RUSKERY_*` environment
variables. Nested keys use `__`, e.g. `RUSKERY_STORAGE__BUCKET=my-bucket`. Keep secrets in the
environment (e.g. a systemd drop‑in) rather than the file.

```toml
[server]
http_addr  = "0.0.0.0:80"
https_addr = "0.0.0.0:443"
public_url = "https://registry.example.com"   # used for realms + pull hints

[database]
path = "/var/lib/ruskery/ruskery.db"

[storage]                          # Tigris (S3-compatible)
endpoint = "https://t3.storage.dev"
bucket   = "my-registry-bucket"
region   = "auto"
presign_ttl_secs = 900             # lifetime of pull redirect URLs
# cdn_url = "https://cdn.example.com"  # Tigris custom domain to serve pulls from
# access_key_id / secret_access_key: set via RUSKERY_STORAGE__* env

[auth]
token_ttl_secs   = 300             # registry bearer token lifetime
session_ttl_secs = 604800          # dashboard session lifetime

[tls]
enabled       = true               # automatic Let's Encrypt (serves HTTP until a domain is added)
contact_email = "admin@example.com"
staging       = false              # use LE staging while testing

[gc]
interval_secs = 0                  # 0 = off; run `ruskery gc` manually instead
```

### CLI

```text
ruskery serve         # run migrations and start the server (default)
ruskery migrate       # apply database migrations and exit
ruskery gc            # one-off garbage-collection sweep
ruskery admin ...     # create-user / create-org / add-member / create-token
```

### Run with Docker

Multi-arch images (`linux/amd64` + `linux/arm64`) are published to the GitHub
Container Registry on every release — `ghcr.io/jacobsamro/ruskery`, tagged
`latest`, `<major>.<minor>`, and the exact version (e.g. `0.2.0`). Pin to a
version for reproducible deploys; `latest` always tracks the newest stable
release:

```sh
docker run -d --name ruskery -p 80:80 -p 443:443 \
  -v ruskery-data:/var/lib/ruskery \
  -e RUSKERY_STORAGE__BUCKET=my-bucket \
  -e RUSKERY_STORAGE__ACCESS_KEY_ID=... \
  -e RUSKERY_STORAGE__SECRET_ACCESS_KEY=... \
  ghcr.io/jacobsamro/ruskery:latest
```

## Architecture

```
                       ┌──────────────────── droplet (1 process) ───────────────────┐
 docker / browser  ──► │  axum + tokio + rustls(ACME)                                │
                       │   /v2/*    OCI Distribution v2 (token auth, RBAC scopes)    │
                       │   /api/v1  dashboard REST (session cookie)                  │
                       │   /        embedded Nuxt dashboard                          │
                       │   SQLite (WAL): orgs/teams/users/tokens/manifests/audit     │
                       └─────────────┬──────────────────────────────┬───────────────┘
              pull: 307 redirect ────┘            push: stream ──────┘ (S3 multipart)
                       ▼                                             ▼
                Tigris CDN edge  ◄───────────  Tigris bucket: orgs/<id>/blobs/sha256/<hex>
```

- **Manifests** (small JSON) live in SQLite for instant serving and tag resolution; **layer/config
  blobs** live in Tigris.
- **Storage is namespaced per org** (`orgs/<org_id>/blobs/…`) so there is no cross‑tenant dedupe or
  existence oracle.
- Built with [axum](https://github.com/tokio-rs/axum), [aws-sdk-s3](https://crates.io/crates/aws-sdk-s3),
  [sqlx](https://crates.io/crates/sqlx), [rustls-acme](https://crates.io/crates/rustls-acme), and a
  [Nuxt 4](https://nuxt.com) + Tailwind UI embedded via [rust-embed](https://crates.io/crates/rust-embed).

## Security

- Argon2id password hashing; high‑entropy, hashed personal access tokens — optionally **scoped to a single organization or repository** (the issued registry token is always the owner's RBAC ∩ the token's scope).
- Registry access uses short‑lived, **scoped** JWT bearer tokens (per‑repository `pull`/`push`/`delete`).
- Per‑IP rate limiting on authentication endpoints; CSP, HSTS (under TLS), `X-Frame-Options`, and
  `X-Content-Type-Options` on every response.
- Per‑org storage isolation; presigned pull URLs are short‑lived.
- **The confidentiality boundary is the organization.** Layer/config blobs are content‑addressed and
  deduplicated *within* an org, so any member with `pull` on any repo in that org can fetch a blob by its
  exact digest — repo scopes confine pushes, manifests, and tags, not raw blob bytes by digest. There is
  no cross‑org dedupe or existence oracle, so this never crosses a tenant. Put mutually‑untrusted
  workloads in **separate organizations**.
- An append‑only audit log records pushes, membership changes, and domain changes.

Found a vulnerability? Please report it privately via a GitHub security advisory rather than a public issue.

## Development

```sh
# Terminal 1 — backend (point at any S3-compatible store; RustFS/MinIO work great locally)
RUSKERY_DATABASE__PATH=./ruskery.db \
RUSKERY_STORAGE__ENDPOINT=http://127.0.0.1:9000 \
RUSKERY_STORAGE__BUCKET=ruskery RUSKERY_STORAGE__FORCE_PATH_STYLE=true \
RUSKERY_STORAGE__ACCESS_KEY_ID=key RUSKERY_STORAGE__SECRET_ACCESS_KEY=secret \
  cargo run -- --config /dev/null serve

# Terminal 2 — dashboard with hot reload (proxies /api and /v2 to :8080)
cd web && bun install && bun run dev
```

For a production build, generate the UI first so it gets embedded:

```sh
cd web && bun install --frozen-lockfile && bun run generate && cd ..
cargo build --release
```

## Contributing

Issues and pull requests are welcome. Please run `cargo fmt`, `cargo clippy`, and `cargo test`
before submitting. For larger changes, open an issue first to discuss the approach.

## Acknowledgements

Built on the work of others: [Tokio](https://tokio.rs) (async runtime), [axum](https://github.com/tokio-rs/axum) (web), [aws-sdk-s3](https://github.com/awslabs/aws-sdk-rust) (Tigris/S3), [sqlx](https://github.com/launchbadge/sqlx) (SQLite), [rustls](https://github.com/rustls/rustls) + [rustls-acme](https://github.com/FlorianUekermann/rustls-acme) (TLS / Let's Encrypt), [governor](https://github.com/boinkor-net/governor) (rate limiting), [argon2](https://github.com/RustCrypto/password-hashes) + [jsonwebtoken](https://github.com/Keats/jsonwebtoken) (auth), [rust-embed](https://github.com/pyrossh/rust-embed) (embedded UI), and [Nuxt](https://nuxt.com) + [Tailwind CSS](https://tailwindcss.com) (dashboard). Standing on [Tigris](https://www.tigrisdata.com/), [Let's Encrypt](https://letsencrypt.org), and the [OCI Distribution Spec](https://github.com/opencontainers/distribution-spec).

## License

Licensed under the [MIT License](LICENSE).
