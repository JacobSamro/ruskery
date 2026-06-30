-- Pull-through cache / proxy.
--
-- When an org has an `upstream_url`, it mirrors that upstream OCI registry: a
-- pull that misses locally is fetched from the upstream, cached (manifests in
-- SQLite, blobs in object storage under the org's namespace) and served. Such
-- an org is read-only — direct pushes are refused. The optional username /
-- password authenticate to a private upstream (stored as given; treat like the
-- storage credentials).
ALTER TABLE orgs ADD COLUMN upstream_url TEXT;
ALTER TABLE orgs ADD COLUMN upstream_username TEXT;
ALTER TABLE orgs ADD COLUMN upstream_password TEXT;
