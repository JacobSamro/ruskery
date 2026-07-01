-- Registry-to-registry bulk import jobs. Each row tracks one background import
-- of an upstream OCI registry's whole catalog into a target org. Credentials are
-- never persisted here (they live only in the running job's memory); only the
-- upstream base URL and progress counters are stored.
CREATE TABLE IF NOT EXISTS imports (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    upstream     TEXT NOT NULL,                       -- base URL (no credentials)
    status       TEXT NOT NULL DEFAULT 'running',     -- running | completed | failed
    repos_total  INTEGER NOT NULL DEFAULT 0,
    repos_done   INTEGER NOT NULL DEFAULT 0,
    tags_total   INTEGER NOT NULL DEFAULT 0,
    tags_done    INTEGER NOT NULL DEFAULT 0,
    blobs_done   INTEGER NOT NULL DEFAULT 0,
    bytes_done   INTEGER NOT NULL DEFAULT 0,
    error        TEXT,
    created_by   TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_imports_org ON imports (org_id, created_at DESC);
