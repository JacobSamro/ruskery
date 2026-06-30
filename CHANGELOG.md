# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **OCI spec-compliance fixes** (from a conformance-focused review):
  upload-status `GET` now returns `204 No Content` (was `202`); `PATCH` validates
  `Content-Range` and rejects out-of-order chunks with `416`; an authenticated
  caller lacking a grant now gets `403 DENIED` (was `401`); the referrers
  endpoint rejects a malformed subject digest with `400` and now includes each
  referrer's `annotations` in its descriptor.

### Added

- **OCI distribution-spec conformance in CI.** A new workflow runs the official
  conformance suite (pull, push, content-management, content-discovery) against a
  real ruskery backed by RustFS.
- **Tag listing pagination.** `GET /v2/<name>/tags/list` honours `?n=`/`?last=`
  and emits a `Link: rel="next"` header, per the spec.
- **Referrers API (OCI 1.1).** `GET /v2/<name>/referrers/<digest>` returns an
  image index of manifests whose `subject` is that digest (with `artifactType`
  filtering + `OCI-Filters-Applied`), and manifest pushes carrying a `subject`
  now echo the `OCI-Subject` header. This is what cosign signatures, SBOMs and
  attestations use for discovery.
- **Explicit multi-arch image index tracking.** An index's child manifests are
  now recorded (`manifest_manifests`), making the index→child relationship
  first-class — blob GC was already safe (children record their own blobs), and
  this keeps a future manifest-level GC correct. Covered by an e2e that pushes
  two children + an index, pulls all three, and checks referrers.

## [0.3.0] - 2026-07-01

### Added

- **Dashboard end-to-end tests (Playwright).** Browser-level coverage of the
  dashboard against the real binary (embedded UI + API on one origin): first-run
  wizard, login + sign-out confirmation, org switcher, Organizations admin view,
  tokens (scoped selects, reveal, revoke), members, teams, instance settings
  (mandatory contact email, OAuth redirect URI), analytics render + range, and
  authz guards — plus storage-backed repo/analytics specs that skip when no S3 is
  present. Runs in CI (`dashboard-e2e.yml`) across Chromium/Firefox/WebKit.

- **Org usage analytics.** A new per-org Analytics page (and
  `GET /api/v1/orgs/{slug}/analytics?range=30d`) showing pushes, pulls, storage
  (deduplicated), attributed egress, daily push/pull and storage-growth charts,
  and top repositories / most-active users. Capture is in-memory on the hot path
  (a sharded counter increment, never a per-request DB write) and flushed to
  daily rollup tables by a background task, so SQLite sees ~one batched upsert
  per flush regardless of pull volume. Push history is backfilled from the audit
  log on first run. Configurable via `[analytics] enabled / rollup_secs`.

### Fixed

- **Confirmation dialogs could resolve as "cancelled" when confirmed.** A race
  between the AlertDialog's close event and the action button's click could
  resolve the `useConfirm()` promise to `false` even when the user clicked the
  confirm button (e.g. sign-out occasionally not signing out). The implicit
  dismissal is now deferred so an explicit button choice always wins. Caught by
  the new Playwright suite.

### Changed

- **Dashboard now uses shadcn-vue components.** All native `<select>` dropdowns
  were replaced with the shadcn-vue `Select` (org switcher, token scope/permission/
  org/repo, member role, team permission), and every `window.confirm` with a
  shadcn-vue `AlertDialog` via a reusable `useConfirm()` composable (sign out,
  revoke token, remove member, remove domain, delete repository). Added through
  the official `shadcn-vue` CLI; fonts are bundled (no external Google Fonts
  request) to keep the binary self-contained.

- **Instance settings split out of the org settings page.** Storage, Domains &
  TLS, Google sign-in, and Users are instance-wide (not per-org), so they now
  live on a dedicated admin-only **Instance Settings** page (`/settings`) reached
  from the sidebar. The org settings page (`/orgs/<slug>/settings`) now only
  shows org-scoped cards. No API changes — same endpoints, clearer placement.

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

[Unreleased]: https://github.com/jacobsamro/ruskery/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/jacobsamro/ruskery/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/jacobsamro/ruskery/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/jacobsamro/ruskery/compare/v0.1.0...v0.2.0
[0.1.0-beta]: https://github.com/jacobsamro/ruskery/releases/tag/v0.1.0-beta
