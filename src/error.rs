use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    #[allow(dead_code)]
    Forbidden,

    #[error("not found")]
    NotFound,

    #[error("too many requests")]
    TooManyRequests,

    #[error("payload too large")]
    PayloadTooLarge,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),

    #[error("{0}")]
    Internal(String),
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".into()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            AppError::TooManyRequests => {
                (StatusCode::TOO_MANY_REQUESTS, "too many requests".into())
            }
            AppError::PayloadTooLarge => {
                (StatusCode::PAYLOAD_TOO_LARGE, "payload too large".into())
            }
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Sqlx(err) => {
                tracing::error!(error = %err, "database_error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
            AppError::Other(err) => {
                tracing::error!(error = %err, "internal_error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
            AppError::Internal(msg) => {
                tracing::error!(error = %msg, "internal_error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
