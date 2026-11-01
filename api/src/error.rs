//! Stable public error envelope: `{ "error": { "code", "message" } }`.
//! Internal details are logged (without secrets), never sent to the client.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("csrf_failed")]
    CsrfFailed,
    #[error("not_found")]
    NotFound,
    #[error("uat_not_available")]
    UatNotAvailable,
    #[error("dependency_unavailable")]
    Unavailable,
    #[error("internal")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl AppError {
    pub fn internal(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        AppError::Internal(Box::new(err))
    }

    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            AppError::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "Sesi tidak ada atau sudah berakhir.",
            ),
            AppError::CsrfFailed => (
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "Permintaan ditolak oleh perlindungan CSRF.",
            ),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Data tidak ditemukan."),
            AppError::UatNotAvailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "uat_not_available",
                "Mode uji tidak tersedia pada database ini.",
            ),
            AppError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "dependency_unavailable",
                "Layanan pendukung sedang tidak tersedia.",
            ),
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Terjadi kesalahan pada server.",
            ),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::PoolTimedOut | sqlx::Error::Io(_) | sqlx::Error::PoolClosed => {
                tracing::warn!(error.kind = "db_unavailable", "database unavailable");
                AppError::Unavailable
            }
            other => AppError::internal(other),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let AppError::Internal(err) = &self {
            // sqlx errors do not include bind values; never log request bodies here.
            tracing::error!(error = %err, "internal error");
        }
        let (status, code, message) = self.parts();
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}
