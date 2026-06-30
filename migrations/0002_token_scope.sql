-- Optional scoping for personal access tokens. A token is one of:
--   'all'  -> every repo the owner can reach (default; previous behavior)
--   'org'  -> only repositories within scope_org_id
--   'repo' -> only the single repository scope_repo_id
-- The issued registry token is always (owner RBAC ∩ this scope).
ALTER TABLE personal_access_tokens ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'all';
ALTER TABLE personal_access_tokens ADD COLUMN scope_org_id TEXT REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE personal_access_tokens ADD COLUMN scope_repo_id TEXT REFERENCES repositories(id) ON DELETE CASCADE;
