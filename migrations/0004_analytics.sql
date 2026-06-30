-- Org-level usage analytics. Capture is in-memory and aggregated; only these
-- pre-rolled daily tables are persisted (no per-request rows), so the write
-- pressure on SQLite is ~one batched upsert per flush interval regardless of
-- pull volume. Repos are keyed by name (the path component) to avoid any
-- repo-id lookup on the hot pull path.

-- Per day / org / repo / event-kind counters.
--   kind: manifest.push | manifest.pull | blob.upload | blob.serve
CREATE TABLE usage_daily (
    day     TEXT NOT NULL,            -- YYYY-MM-DD (UTC)
    org_id  TEXT NOT NULL,
    repo    TEXT NOT NULL,            -- repo name within the org
    kind    TEXT NOT NULL,
    count   INTEGER NOT NULL DEFAULT 0,
    bytes   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, org_id, repo, kind)
);
CREATE INDEX idx_usage_daily_org ON usage_daily (org_id, day);

-- Per day / org / user / event-kind counters (who is active).
CREATE TABLE usage_user_daily (
    day     TEXT NOT NULL,
    org_id  TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind    TEXT NOT NULL,
    count   INTEGER NOT NULL DEFAULT 0,
    bytes   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, org_id, user_id, kind)
);
CREATE INDEX idx_usage_user_daily_org ON usage_user_daily (org_id, day);

-- Daily storage snapshot. repo='' is the org-wide deduplicated total.
CREATE TABLE storage_daily (
    day        TEXT NOT NULL,
    org_id     TEXT NOT NULL,
    repo       TEXT NOT NULL,         -- '' = org dedup'd total
    bytes      INTEGER NOT NULL,
    blob_count INTEGER NOT NULL,
    PRIMARY KEY (day, org_id, repo)
);
CREATE INDEX idx_storage_daily_org ON storage_daily (org_id, day);
