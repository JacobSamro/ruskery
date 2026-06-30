//! Tag listing, with OCI pagination (`?n=` / `?last=` + `Link` header).

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::db;
use crate::db::orgs::Org;
use crate::error::{Error, Result};
use crate::state::AppState;

/// `GET /v2/<name>/tags/list[?n=<count>&last=<tag>]`
pub async fn list(
    state: &AppState,
    org: &Org,
    repo_name: &str,
    name: &str,
    n: Option<usize>,
    last: Option<&str>,
) -> Result<Response> {
    let repo = db::orgs::find_repo(state.db(), &org.id, repo_name)
        .await?
        .ok_or_else(|| Error::oci(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository unknown"))?;
    // `list_tags` returns names in lexical order, which is the OCI page order.
    let mut tags = db::content::list_tags(state.db(), &repo.id).await?;
    if let Some(last) = last {
        tags.retain(|t| t.as_str() > last);
    }

    // Truncate to the requested page size, emitting a `Link: rel="next"` when
    // more remain.
    let mut next_link: Option<String> = None;
    if let Some(n) = n {
        if tags.len() > n {
            tags.truncate(n);
            if let Some(last_tag) = tags.last() {
                next_link = Some(format!("/v2/{name}/tags/list?n={n}&last={last_tag}"));
            }
        }
    }

    let body = Json(json!({ "name": name, "tags": tags }));
    match next_link {
        Some(link) => Ok((
            StatusCode::OK,
            [(header::LINK, format!("<{link}>; rel=\"next\""))],
            body,
        )
            .into_response()),
        None => Ok((StatusCode::OK, body).into_response()),
    }
}
