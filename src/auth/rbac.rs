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
/// (`<org-slug>/<repo>`) for `user_id`, further constrained by the access
/// token's `scope`. Returns the granted subset (owner RBAC ∩ token scope).
pub async fn grant_repository(
    db: &Db,
    user_id: &str,
    name: &str,
    requested: &[String],
    scope: &TokenScope,
) -> Result<Vec<String>> {
    let Some((org_slug, repo_name)) = name.split_once('/') else {
        return Ok(vec![]);
    };
    let Some(org) = orgs::find_org_by_slug(db, org_slug).await? else {
        return Ok(vec![]);
    };

    let repo = orgs::find_repo(db, &org.id, repo_name).await?;

    // Enforce the token's scope before computing any grants.
    match scope {
        TokenScope::All => {}
        TokenScope::Org(oid) if *oid == org.id => {}
        TokenScope::Repo(rid) if repo.as_ref().map(|r| &r.id == rid).unwrap_or(false) => {}
        // Token is not allowed to touch this org/repo.
        _ => return Ok(vec![]),
    }

    let mut granted = Vec::new();

    match repo {
        Some(repo) => {
            let perm = orgs::repo_permission(db, &repo.id, &org.id, user_id).await?;
            for action in requested {
                let ok = match action.as_str() {
                    "pull" => perm.allows(Permission::Pull),
                    "push" => perm.allows(Permission::Push),
                    "delete" => perm.allows(Permission::Admin),
                    _ => false,
                };
                if ok {
                    granted.push(action.clone());
                }
            }
        }
        None => {
            // Repository does not exist yet. Only org owners/admins may create
            // one (via push); pull/delete on a missing repo grant nothing.
            let is_org_admin = orgs::org_role(db, &org.id, user_id)
                .await?
                .map(|r| r >= OrgRole::Admin)
                .unwrap_or(false);
            if is_org_admin && requested.iter().any(|a| a == "push") {
                granted.push("push".to_string());
                if requested.iter().any(|a| a == "pull") {
                    granted.push("pull".to_string());
                }
            }
        }
    }

    Ok(granted)
}
