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
    /// A state conflict with a stable code (`username_taken`, `user_inactive`, `last_admin`).
    #[error("conflict")]
    Conflict(&'static str),
    /// A database domain guard refused the change. The guard's own message is never sent
    /// to the client (it may name tables or values); the client gets a generic rule error.
    #[error("rule_violation")]
    RuleViolation,
    /// A query or body value is outside the documented contract.
    #[error("invalid_request")]
    InvalidRequest,
    /// A body value failed a business validation; `field` names the request field (camelCase,
    /// dotted for nested objects) so the client can focus it. Never carries the value.
    #[error("validation_failed")]
    Validation(&'static str),
    /// The idempotency key was already used by this user in this workspace for a different
    /// request. Nothing was created.
    #[error("idempotency_conflict")]
    IdempotencyConflict,
    /// An official BI / regulator report was requested. It stays held until a verified
    /// mapping exists (D01); nothing was created.
    #[error("regulator_mapping_unavailable")]
    RegulatorMappingUnavailable,
    /// The report artifact is past its expiry. Request a new export.
    #[error("report_expired")]
    ReportExpired,
    #[error("uat_not_available")]
    UatNotAvailable,
    /// Upload larger than the configured cap. Nothing was kept.
    #[error("attachment_too_large")]
    AttachmentTooLarge,
    /// Content (sniffed by the server, not the client's type) is not on the allow-list.
    #[error("attachment_type_refused")]
    AttachmentTypeRefused,
    /// The scanner reported malware. The bytes were deleted and never bound.
    #[error("attachment_infected")]
    AttachmentInfected,
    /// The scanner answered but did not complete the scan. Fail closed: nothing bound.
    #[error("scan_failed")]
    ScanFailed,
    /// No scanner reachable (or not configured). Fail closed: nothing bound.
    #[error("scanner_unavailable")]
    ScannerUnavailable,
    /// Too many requests for this user (or peer, without a session) in the current window.
    /// Nothing was processed; `Retry-After` says when the next request is accepted.
    #[error("rate_limited")]
    RateLimited { retry_after_secs: u64 },
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
            AppError::Conflict(code) => (
                StatusCode::CONFLICT,
                code,
                "Permintaan bertentangan dengan keadaan data saat ini.",
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
            AppError::Validation(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_failed",
                "Isian belum valid. Periksa kolom yang ditandai.",
            ),
            AppError::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Kunci pengiriman ini sudah dipakai untuk isian berbeda. Tidak ada data baru yang disimpan.",
            ),
            AppError::RegulatorMappingUnavailable => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "regulator_mapping_unavailable",
                "Ekspor resmi BI ditahan: format, kode dan field wajib belum diverifikasi. Gunakan rekap operasional.",
            ),
            AppError::ReportExpired => (
                StatusCode::GONE,
                "report_expired",
                "Berkas laporan sudah kedaluwarsa. Buat ekspor baru.",
            ),
            AppError::UatNotAvailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "uat_not_available",
                "Mode uji tidak tersedia pada database ini.",
            ),
            AppError::AttachmentTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "attachment_too_large",
                "Ukuran berkas melebihi batas. Berkas tidak disimpan.",
            ),
            AppError::AttachmentTypeRefused => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "attachment_type_refused",
                "Jenis berkas tidak diizinkan (hanya PDF, PNG atau JPEG). Berkas tidak disimpan.",
            ),
            AppError::AttachmentInfected => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "attachment_infected",
                "Pemindai menandai berkas berbahaya. Berkas dihapus dan tidak dilampirkan.",
            ),
            AppError::ScanFailed => (
                StatusCode::BAD_GATEWAY,
                "scan_failed",
                "Pemindaian berkas gagal. Berkas tidak dilampirkan; coba lagi nanti.",
            ),
            AppError::ScannerUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "scanner_unavailable",
                "Pemindai berkas tidak tersedia. Berkas tidak dilampirkan.",
            ),
            AppError::RateLimited { .. } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Terlalu banyak permintaan. Tunggu sebentar lalu coba lagi; permintaan ini tidak diproses.",
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
        let body = match &self {
            AppError::Validation(field) => {
                json!({ "error": { "code": code, "message": message, "field": field } })
            }
            AppError::RateLimited { retry_after_secs } => json!({
                "error": { "code": code, "message": message, "retryAfterSeconds": retry_after_secs }
            }),
            _ => json!({ "error": { "code": code, "message": message } }),
        };
        let mut response = (status, Json(body)).into_response();
        if let AppError::RateLimited { retry_after_secs } = &self {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from(*retry_after_secs),
            );
        }
        if matches!(self, AppError::Forbidden) {
            response
                .extensions_mut()
                .insert(crate::denied_audit::ForbiddenMarker);
        }
        response
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
