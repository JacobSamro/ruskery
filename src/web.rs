//! Serve the embedded single-page dashboard. Static assets are matched by path;
//! anything else falls back to `index.html` so client-side routing works.

use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/.output/public"]
struct Assets;

/// Fallback handler for all non-API, non-registry routes.
pub async fn handler(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    if let Some(resp) = serve(path) {
        return resp;
    }
    // SPA fallback.
    serve("index.html").unwrap_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "dashboard assets not built (run the web build)",
        )
            .into_response()
    })
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = Body::from(file.data.into_owned());
    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            body,
        )
            .into_response(),
    )
}

/// Convenience used by the router fallback when only the URI is available.
#[allow(dead_code)]
pub async fn handler_uri(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    serve(path)
        .or_else(|| serve("index.html"))
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "not found").into_response())
}
