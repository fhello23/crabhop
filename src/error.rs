//! Shared error type. Handlers map this into HTML/text or JSON responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("slug already exists")]
    Conflict,
    #[error("link not found")]
    NotFound,
    #[error("link is gone")]
    Gone,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("unsupported media type: {0}")]
    UnsupportedMediaType(String),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Conflict => StatusCode::CONFLICT,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Gone => StatusCode::GONE,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Message safe to expose to clients. Internal details are redacted.
    pub fn public_message(&self) -> String {
        match self {
            Self::Validation(m) => m.clone(),
            Self::Conflict => "slug already exists".to_string(),
            Self::NotFound => "not found".to_string(),
            Self::Gone => "link has expired".to_string(),
            Self::Forbidden(m) => m.clone(),
            Self::Unauthorized => "unauthorized".to_string(),
            Self::UnsupportedMediaType(m) => m.clone(),
            Self::PayloadTooLarge => "request body too large".to_string(),
            Self::Internal(_) => "internal server error".to_string(),
        }
    }

    pub fn internal(err: impl Into<anyhow::Error>) -> Self {
        Self::Internal(err.into())
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        // Never leak SQL text to clients; map to generic errors.
        if is_unique_violation(&e) {
            return Self::Conflict;
        }
        Self::Internal(anyhow::Error::new(e).context("database error"))
    }
}

pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = e {
        // SQLite extended code 2067: UNIQUE constraint failed.
        return db.code().as_deref() == Some("2067")
            || db.message().contains("UNIQUE constraint failed");
    }
    false
}

// Default rendering: plain text. API handlers convert to JSON explicitly,
// admin handlers convert to HTML where appropriate.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = self.public_message();
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_message_is_generic() {
        let e = AppError::internal(anyhow::anyhow!("secret db password=hunter2"));
        assert_eq!(e.public_message(), "internal server error");
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn error_status_mapping() {
        assert_eq!(
            AppError::Validation("x".into()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(AppError::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(AppError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(AppError::Gone.status(), StatusCode::GONE);
    }
}
