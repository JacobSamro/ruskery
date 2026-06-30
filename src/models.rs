//! Core domain types shared across the registry and dashboard.

use serde::{Deserialize, Serialize};

/// A user account.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub is_admin: bool,
    pub created_at: String,
}

/// Organization-level role, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgRole {
    Member,
    Admin,
    Owner,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Member => "member",
            OrgRole::Admin => "admin",
            OrgRole::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "member" => Some(OrgRole::Member),
            "admin" => Some(OrgRole::Admin),
            "owner" => Some(OrgRole::Owner),
            _ => None,
        }
    }
}

/// What a personal access token is allowed to reach, independent of (and
/// intersected with) its owner's RBAC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenScope {
    /// Everything the owner can reach (default).
    All,
    /// Only repositories within this org id.
    Org(String),
    /// Only this single repository id.
    Repo(String),
}

/// Permission level on a repository, ordered so comparisons express "at least".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// No access.
    None,
    /// Read (docker pull).
    Pull,
    /// Read + write (docker push).
    Push,
    /// Full control (manage perms, delete).
    Admin,
}

impl Permission {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::None => "none",
            Permission::Pull => "pull",
            Permission::Push => "push",
            Permission::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Permission::None),
            "pull" => Some(Permission::Pull),
            "push" => Some(Permission::Push),
            "admin" => Some(Permission::Admin),
            _ => None,
        }
    }

    /// Whether this permission satisfies a required level.
    pub fn allows(self, required: Permission) -> bool {
        self >= required
    }
}
