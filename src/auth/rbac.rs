//! Translate a user + requested registry scope into the set of actions we will
//! actually grant in the issued bearer token.

use crate::db::{orgs, Db};
use crate::error::Result;
use crate::models::{OrgRole, Permission, TokenScope};

/// A parsed registry scope, e.g. `repository:acme/api:pull,push`.
#[derive(Debug, Clone)]
pub struct Scope {
    pub kind: String,         // "repository"
    pub name: String,         // "<org>/<repo>"
    pub actions: Vec<String>, // requested actions
}

impl Scope {
    /// Parse a single `type:name:action[,action...]` scope string. The name may
    /// itself contain colons only in the resource part (it does not here), so we
    /// split into exactly three fields from the left/right.
    pub fn parse(raw: &str) -> Option<Scope> {
        // type is up to the first ':'; actions are after the last ':'.
        let first = raw.find(':')?;
        let last = raw.rfind(':')?;
        if first == last {
            return None;
        }
        let kind = raw[..first].to_string();
        let name = raw[first + 1..last].to_string();
        let actions = raw[last + 1..]
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        Some(Scope {
            kind,
            name,
            actions,
        })
    }
}

/// Decide which of the `requested` actions to grant on repository `name`
/// (`<org-slug>/<repo>`) for `user_id`, constrained by the token's resource
/// `scope` and permission `cap`. Returns the granted subset
/// (owner RBAC ∩ scope ∩ cap).
pub async fn grant_repository(
    db: &Db,
    user_id: &str,
    name: &str,
    requested: &[String],
    scope: &TokenScope,
    cap: Permission,
) -> Result<Vec<String>> {
    let Some((org_slug, repo_name)) = name.split_once('/') else {
        return Ok(vec![]);
    };
    let Some(org) = orgs::find_org_by_slug(db, org_slug).await? else {
        return Ok(vec![]);
    };

    let repo = orgs::find_repo(db, &org.id, repo_name).await?;

    // Enforce the token's resource scope before computing any grants.
    match scope {
        TokenScope::All => {}
        TokenScope::Org(oid) if *oid == org.id => {}
        TokenScope::Repo(rid) if repo.as_ref().map(|r| &r.id == rid).unwrap_or(false) => {}
        // Token is not allowed to touch this org/repo.
        _ => return Ok(vec![]),
    }

    // The owner's capability on the repository...
    let capability = match &repo {
        Some(repo) => orgs::repo_permission(db, &repo.id, &org.id, user_id).await?,
        None => {
            // Repository does not exist yet; only org owners/admins may create
            // one, which is a push-level capability (no delete on a new repo).
            let is_org_admin = orgs::org_role(db, &org.id, user_id)
                .await?
                .map(|r| r >= OrgRole::Admin)
                .unwrap_or(false);
            if is_org_admin {
                Permission::Push
            } else {
                Permission::None
            }
        }
    };

    // ...capped by the token's permission cap.
    let effective = capability.min(cap);

    let mut granted = Vec::new();
    for action in requested {
        let required = match action.as_str() {
            "pull" => Permission::Pull,
            "push" => Permission::Push,
            "delete" => Permission::Admin,
            _ => continue,
        };
        if effective >= required {
            granted.push(action.clone());
        }
    }

    Ok(granted)
}
