# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Organizations view for super admins.** Instance admins now get an
  "Organizations" entry in the sidebar linking to a page that lists every
  organization on the instance (with repo + member counts) and can create new
  ones. Backed by a new admin-only `GET /api/v1/admin/orgs`.

- **ACME contact email is now editable in the dashboard** (Settings → Domains &
  TLS) instead of only via `config.toml`. Stored in the DB and used when
  registering the Let's Encrypt account; the dashboard value wins over config.
  A contact email is now **required before a domain can be added** (enforced by
  both the API and the dashboard), so a certificate request always has a contact.

## [0.2.1] - 2026-06-30

### Changed

- **Automatic TLS is now on by default.** Previously `tls.enabled` defaulted to
  `false`, so adding a domain in the dashboard did nothing until you edited the
  config and restarted. Now the server listens on `:443` and provisions a
  Let's Encrypt certificate as soon as a domain is added — no restart. Until a
  domain exists it keeps serving plain HTTP (so a fresh, IP-only box stays
  reachable for first-run setup instead of redirecting to an unservable `:443`).

  Existing installs keep whatever is in their `config.toml`; set
  `[tls] enabled = true` and restart to adopt the new behavior.

## [0.2.0] - 2026-06-30

### Fixed

- **Dashboard failed to load under the default Content-Security-Policy.** A bare
  `script-src 'self'` blocked the Nuxt SPA's inline bootstrap script
  (`window.__NUXT__ = …`), so the app never mounted (`Cannot read properties of
  undefined (reading 'app')`). The server now computes the SHA-256 of each
  executable inline script in the embedded `index.html` at serve time and emits
  matching `'sha256-…'` tokens in the CSP for HTML responses — keeping a strict
  `script-src 'self'` (no `'unsafe-inline'`) for every other response. The hashes
  are derived from the shipped bytes, so they stay correct across rebuilds even
  though Nuxt regenerates `buildId` each build.

### Changed

- CI: the `docker e2e` macOS leg now runs on an Intel runner (`macos-15-intel`)
  with Colima. GitHub's Apple-silicon hosted runners are themselves VMs and
  Apple's Hypervisor.framework forbids nested virtualization, so no Linux Docker
  daemon can start there.

## [0.1.0-beta] - 2026-06-30

First public beta. A self-contained, multi-tenant OCI/Docker registry in Rust,
backed by Tigris (S3) for storage and CDN, with an embedded Nuxt dashboard.

### Added

- **OCI Distribution v2 registry**: token auth with per-repository RBAC scopes;
  monolithic, chunked, and multipart blob uploads streamed into Tigris with
  on-the-fly SHA-256 verification; cross-repo blob mount; resumable uploads.
- **CDN-offloaded pulls**: blob `GET` returns a `307` to a short-lived presigned
  Tigris URL, so layer bytes never pass through the server.
- **Multi-tenant model**: organizations → teams → users with `pull`/`push`/`admin`
  grants; per-org storage isolation (`orgs/<id>/blobs/...`).
- **Access tokens**: personal access tokens for `docker login`, optionally
  **scoped** to one org or repo and **capped** to a permission level
  (`pull`/`push`/`admin`). Effective access = owner RBAC ∩ scope ∩ cap.
- **Sign in with Google** (optional, configured in the dashboard): shows the
  exact GCP redirect URI; auto-provisions users from an allowed email domain.
- **Embedded dashboard** (Nuxt 4 + Tailwind): first-run setup wizard, repo/tag
  browser, members/teams/tokens, audit log, and live-editable storage + CDN and
  Google sign-in settings (hot-swapped, no restart).
- **Automatic HTTPS** via Let's Encrypt (rustls-acme, TLS-ALPN-01) with
  dashboard-managed custom domains and an HTTP→HTTPS redirect.
- **Garbage collection** (mark-and-sweep with a grace window), append-only
  **audit log**, per-IP **rate limiting** on auth endpoints, and CSP/HSTS/
  security headers.
- **Packaging**: one-line `curl | sh` installer (checksum-verified, in-place
  upgrades), systemd unit, native x86_64 + aarch64 (gnu) release binaries, and
  multi-arch container images published to GHCR.
- **CLI**: `serve`, `migrate`, `gc`, and `admin` (create-user/org, add-member,
  create-token).

### Security

- Argon2id password hashing; high-entropy, hashed personal access tokens; short
  lived scoped HS256 registry JWTs.
- Upload sessions are bound to their org and storage client; strict digest
  parsing; repository-name validation; non-JSON manifests rejected.

[Unreleased]: https://github.com/jacobsamro/ruskery/compare/v0.1.0-beta...HEAD
[0.1.0-beta]: https://github.com/jacobsamro/ruskery/releases/tag/v0.1.0-beta
