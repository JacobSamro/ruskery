-- Per-image (manifest digest) pull counter, surfaced in the dashboard's image
-- list. Keyed by (org_id, repo name, digest) so it can be bumped on the pull hot
-- path without an id lookup (mirrors how analytics is keyed by repo name).
CREATE TABLE IF NOT EXISTS image_pulls (
    org_id TEXT NOT NULL,
    repo   TEXT NOT NULL,
    digest TEXT NOT NULL,
    count  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (org_id, repo, digest)
);
