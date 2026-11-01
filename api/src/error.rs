//! Stable public error envelope: `{ "error": { "code", "message" } }`.
//! Internal details are logged (without secrets), never sent to the client.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// SQLSTATE raised by `itsm_require_version_bump()`.
pub const VERSION_CONFLICT_SQLSTATE: &str = "IT409";
/// SQLSTATE raised by the domain guards (lifecycle, outbox, grants, immutable columns).
pub const RULE_VIOLATION_SQLSTATE: &str = "IT422";
/// PostgreSQL `foreign_key_violation`.
pub const FOREIGN_KEY_VIOLATION_SQLSTATE: &str = "23503";

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("csrf_failed")]
    CsrfFailed,
    #[error("forbidden")]
    Forbidden,
    #[error("not_found")]
    NotFound,
    #[error("version_conflict")]
    VersionConflict,
    /// A database domain guard refused the change. The guard's own message is never sent
    /// to the client (it may name tables or values); the client gets a generic rule error.
    #[error("rule_violation")]
    RuleViolation,
    /// A query or body value is outside the documented contract.
    #[error("invalid_request")]
    InvalidRequest,
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
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "Anda tidak memiliki izin untuk tindakan ini.",
            ),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Data tidak ditemukan."),
            AppError::VersionConflict => (
                StatusCode::CONFLICT,
                "version_conflict",
                "Data sudah diubah pihak lain. Muat ulang lalu coba lagi.",
            ),
            AppError::RuleViolation => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "rule_violation",
                "Perubahan ditolak oleh aturan proses.",
            ),
            AppError::InvalidRequest => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Parameter permintaan tidak valid.",
            ),
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
            // Raised by the version-bump guard in migration 0002.
            sqlx::Error::Database(db)
                if db.code().as_deref() == Some(VERSION_CONFLICT_SQLSTATE) =>
            {
                AppError::VersionConflict
            }
            sqlx::Error::Database(db) if db.code().as_deref() == Some(RULE_VIOLATION_SQLSTATE) => {
                // Log where it came from (table/constraint names only), not the message text.
                tracing::info!(
                    error.kind = "rule_violation",
                    db.table = db.table().unwrap_or(""),
                    db.constraint = db.constraint().unwrap_or(""),
                    "domain rule violated"
                );
                AppError::RuleViolation
            }
            // A reference to a row outside the workspace or that does not exist (D03
            // disposition): the same generic rule error, never the constraint text.
            sqlx::Error::Database(db)
                if db.code().as_deref() == Some(FOREIGN_KEY_VIOLATION_SQLSTATE) =>
            {
                tracing::info!(
                    error.kind = "foreign_key_violation",
                    db.table = db.table().unwrap_or(""),
                    db.constraint = db.constraint().unwrap_or(""),
                    "reference refused"
                );
                AppError::RuleViolation
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_codes_have_safe_generic_messages() {
        let (status, code, message) = AppError::Forbidden.parts();
        assert_eq!((status, code), (StatusCode::FORBIDDEN, "forbidden"));
        assert!(!message.is_empty());
        let (status, code, message) = AppError::RuleViolation.parts();
        assert_eq!(
            (status, code),
            (StatusCode::UNPROCESSABLE_ENTITY, "rule_violation")
        );
        assert!(!message.contains("IT422"));
    }
}
