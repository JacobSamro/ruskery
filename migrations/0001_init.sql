-- ruskery initial schema.
-- All ids are UUID strings. Timestamps are RFC3339 TEXT (UTC).

-- Key/value instance settings: signing secret, setup state, primary domain, etc.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- ───────────────────────── identity ─────────────────────────

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    email         TEXT NOT NULL,
    username      TEXT NOT NULL,
    password_hash TEXT NOT NULL,         -- argon2id PHC string
    is_admin      INTEGER NOT NULL DEFAULT 0, -- instance super-admin
    created_at    TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_users_email ON users (lower(email));
CREATE UNIQUE INDEX idx_users_username ON users (lower(username));

-- Personal access tokens used for `docker login` and API automation.
CREATE TABLE personal_access_tokens (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_prefix TEXT NOT NULL,          -- first chars, shown in UI for identification
    token_hash   TEXT NOT NULL,          -- sha256(hex) of the full secret
    last_used_at TEXT,
    expires_at   TEXT,
    created_at   TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_pat_hash ON personal_access_tokens (token_hash);
CREATE INDEX idx_pat_user ON personal_access_tokens (user_id);

-- Dashboard sessions (signed cookie references this row).
CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,         -- random session id
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX idx_sessions_user ON sessions (user_id);

-- ───────────────────────── tenancy ─────────────────────────

CREATE TABLE orgs (
    id         TEXT PRIMARY KEY,
    slug       TEXT NOT NULL,            -- namespace path segment
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_orgs_slug ON orgs (lower(slug));

-- Org-level role: owner | admin | member.
CREATE TABLE org_members (
    org_id  TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role    TEXT NOT NULL DEFAULT 'member',
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX idx_org_members_user ON org_members (user_id);

CREATE TABLE teams (
    id         TEXT PRIMARY KEY,
    org_id     TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    slug       TEXT NOT NULL,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_teams_org_slug ON teams (org_id, lower(slug));

CREATE TABLE team_members (
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role    TEXT NOT NULL DEFAULT 'member', -- maintainer | member
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX idx_team_members_user ON team_members (user_id);

-- ───────────────────────── repositories ─────────────────────────

CREATE TABLE repositories (
    id         TEXT PRIMARY KEY,
    org_id     TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,            -- repo path within the org (may contain '/')
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_repos_org_name ON repositories (org_id, name);

-- Team → repository permission: pull | push | admin.
CREATE TABLE team_repo_perms (
    team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    repo_id    TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    permission TEXT NOT NULL DEFAULT 'pull',
    PRIMARY KEY (team_id, repo_id)
);

-- ───────────────────────── content (OCI) ─────────────────────────

-- Per-org content-addressable layer/config blobs (bytes live in Tigris).
CREATE TABLE blobs (
    org_id     TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    digest     TEXT NOT NULL,            -- e.g. sha256:abcdef...
    size       INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (org_id, digest)
);

-- Manifest bytes are stored in-DB for instant serving + tag resolution.
CREATE TABLE manifests (
    repo_id    TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    digest     TEXT NOT NULL,            -- sha256:hex of the manifest bytes
    media_type TEXT NOT NULL,
    size       INTEGER NOT NULL,
    content    BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (repo_id, digest)
);

-- Blobs referenced by a manifest, for GC reference counting.
CREATE TABLE manifest_blobs (
    repo_id         TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    manifest_digest TEXT NOT NULL,
    blob_digest     TEXT NOT NULL,
    PRIMARY KEY (repo_id, manifest_digest, blob_digest)
);
CREATE INDEX idx_manifest_blobs_blob ON manifest_blobs (blob_digest);

CREATE TABLE tags (
    repo_id         TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY (repo_id, name)
);

-- ───────────────────────── domains / TLS ─────────────────────────

CREATE TABLE domains (
    domain     TEXT PRIMARY KEY,
    org_id     TEXT REFERENCES orgs(id) ON DELETE CASCADE, -- NULL = instance-level
    status     TEXT NOT NULL DEFAULT 'pending',            -- pending | active | failed
    is_primary INTEGER NOT NULL DEFAULT 0,
    detail     TEXT,                                       -- last ACME error, if any
    created_at TEXT NOT NULL
);

-- ───────────────────────── audit ─────────────────────────

CREATE TABLE audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            TEXT NOT NULL,
    actor_user_id TEXT,
    org_id        TEXT,
    action        TEXT NOT NULL,
    target        TEXT,
    detail        TEXT
);
CREATE INDEX idx_audit_org_ts ON audit_log (org_id, ts);
