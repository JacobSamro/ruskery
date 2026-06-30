-- Multi-arch image indexes and the OCI 1.1 referrers API.

-- Child manifests referenced by an image index / docker manifest list. Recorded
-- so the index→child relationship is explicit: blob GC is unaffected (children
-- record their own blobs), but a future manifest-level GC must keep an index's
-- children alive, and pulls of a multi-arch tag rely on the children existing.
CREATE TABLE manifest_manifests (
    repo_id         TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    manifest_digest TEXT NOT NULL,   -- the index
    child_digest    TEXT NOT NULL,   -- a manifest the index references
    PRIMARY KEY (repo_id, manifest_digest, child_digest)
);
CREATE INDEX idx_manifest_manifests_child ON manifest_manifests (repo_id, child_digest);

-- Referrers (OCI 1.1): a manifest's `subject` points at another manifest in the
-- same repo. `GET /v2/<name>/referrers/<digest>` lists the manifests that refer
-- to <digest> — how cosign signatures, SBOMs and attestations are discovered.
CREATE TABLE manifest_referrers (
    repo_id         TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    subject_digest  TEXT NOT NULL,   -- the referenced (subject) manifest digest
    referrer_digest TEXT NOT NULL,   -- the manifest carrying `subject`
    artifact_type   TEXT NOT NULL DEFAULT '', -- artifactType (or config.mediaType)
    PRIMARY KEY (repo_id, subject_digest, referrer_digest)
);
CREATE INDEX idx_manifest_referrers_subject ON manifest_referrers (repo_id, subject_digest);
