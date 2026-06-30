-- Per-org storage quota.
--
-- A nullable override on the org: NULL means "use the instance default"
-- ([quota] default_storage_bytes); a stored value of 0 means unlimited for this
-- org regardless of the default. Enforced on blob-upload finalize against the
-- org's deduplicated stored size (SUM of blob sizes), counting only genuinely
-- new blobs (content-addressed dedup means a re-push consumes nothing).
ALTER TABLE orgs ADD COLUMN storage_quota_bytes INTEGER;
