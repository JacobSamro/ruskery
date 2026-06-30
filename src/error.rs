//! Central error type. Maps internal failures to HTTP responses, using the
//! OCI distribution error schema for `/v2/*` routes and plain JSON elsewhere.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// An OCI-coded registry error (see <https://github.com/opencontainers/distribution-spec>).
    #[error("registry error: {code}")]
    Oci {
        status: StatusCode,
        code: &'static str,
        message: String,
    },

    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Error::BadRequest(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into())
    }

    /// Build an OCI-coded error response.
    pub fn oci(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Error::Oci {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        // OCI errors carry their own schema: { "errors": [ { code, message } ] }.
        if let Error::Oci {
            status,
            code,
            message,
        } = &self
        {
            let body = json!({ "errors": [ { "code": code, "message": message } ] });
            return (*status, Json(body)).into_response();
        }

        let (status, code, message) = match &self {
            Error::NotFound => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Error::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", self.to_string()),
            Error::Forbidden => (StatusCode::FORBIDDEN, "forbidden", self.to_string()),
            Error::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request", self.to_string()),
            Error::Conflict(_) => (StatusCode::CONFLICT, "conflict", self.to_string()),
            Error::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error".to_string(),
                )
            }
            Error::Other(e) => {
                tracing::error!(error = %e, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error".to_string(),
                )
            }
            Error::Oci { .. } => unreachable!("handled above"),
        };

        let body = json!({ "error": { "code": code, "message": message } });
        (status, Json(body)).into_response()
    }
}
