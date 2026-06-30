-- Per-token permission cap, independent of the owner's role:
--   'pull'  -> read-only            (docker pull)
--   'push'  -> read + write         (docker pull/push)
--   'admin' -> full (incl. delete)  (default; no cap beyond owner RBAC)
-- The issued registry token is owner RBAC ∩ scope ∩ this cap.
ALTER TABLE personal_access_tokens ADD COLUMN max_perm TEXT NOT NULL DEFAULT 'admin';
