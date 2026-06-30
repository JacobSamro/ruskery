//! Tag listing.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::db;
use crate::db::orgs::Org;
use crate::error::{Error, Result};
use crate::state::AppState;

/// `GET /v2/<name>/tags/list`
pub async fn list(state: &AppState, org: &Org, repo_name: &str, name: &str) -> Result<Response> {
    let repo = db::orgs::find_repo(state.db(), &org.id, repo_name)
        .await?
        .ok_or_else(|| Error::oci(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository unknown"))?;
    let tags = db::content::list_tags(state.db(), &repo.id).await?;
    Ok((StatusCode::OK, Json(json!({ "name": name, "tags": tags }))).into_response())
}
