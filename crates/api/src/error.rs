use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Application error, two classes:
/// - Internal: unexpected failures (DB down, etc.) → 500, details logged, client sees a generic message.
/// - Client: caller-side mistakes (e.g. unknown tenant) → 4xx + a clear message that is safe to share.
#[derive(Debug)]
pub enum AppError {
    Internal(anyhow::Error),
    Client(StatusCode, String),
}

impl AppError {
    /// Build a 4xx error with a message that is safe to show the client.
    pub fn client(status: StatusCode, msg: impl Into<String>) -> Self {
        Self::Client(status, msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            // The original message goes only to the log, never leaked to the client.
            AppError::Internal(err) => {
                tracing::error!("error handler: {:#}", err);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal server error" })),
                )
                    .into_response()
            }
            AppError::Client(status, msg) => {
                (status, Json(json!({ "error": msg }))).into_response()
            }
        }
    }
}

// The `?` operator on any error convertible into anyhow::Error → automatically becomes Internal.
// So IO handlers can keep using `?` as usual; the Client case is constructed explicitly.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self::Internal(err.into())
    }
}
