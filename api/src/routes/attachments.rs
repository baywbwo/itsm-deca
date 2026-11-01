//! D08b attachments: quarantine → clamd scan → bind, and ACL-checked download.
//!
//! A file is written to `quarantine/` only, sniffed by magic bytes, scanned, and moved to
//! `store/` and bound to its ticket only on an explicit clean verdict. Every other verdict
//! (infected, scan error, scanner unavailable or not configured) is recorded, audited as
//! failed, and the bytes are deleted. Downloads re-check the session's access at request time,
//! only serve bound clean files of the same workspace and ticket, and verify the stored hash.

use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::clamd::{self, ScanOutcome};
use crate::error::AppError;
use crate::routes::ticket_detail::{self, TicketDetail, lock_open_ticket};
use crate::routes::ticket_list::{audit, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

/// Concurrent uploads in flight per process (disk + scanner backpressure).
const UPLOAD_SLOTS: usize = 4;
static SLOTS: Semaphore = Semaphore::const_new(UPLOAD_SLOTS);
const FILE_NAME_HEADER: &str = "x-file-name";
const MAX_FILE_NAME_CHARS: usize = 255;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadQuery {
    visibility: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploaded {
    attachment_id: Uuid,
    detail: TicketDetail,
    audit_event_id: Uuid,
}

/// Deletes a quarantined/stored file unless disarmed (request cancelled, error, refusal).
struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn retarget(&mut self, p: PathBuf) {
        self.0 = Some(p);
    }
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.0.take()
            && let Err(e) = std::fs::remove_file(&p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error.kind = ?e.kind(), "attachment file cleanup failed");
        }
    }
}

/// Decodes a percent-encoded UTF-8 file name and rejects anything path-like or unprintable.
pub(crate) fn file_name(headers: &HeaderMap) -> Result<String, AppError> {
    let bad = || AppError::Validation("fileName");
    let raw = headers
        .get(FILE_NAME_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(bad)?;
    if raw.len() > MAX_FILE_NAME_CHARS * 12 {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(raw.len());
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).ok_or_else(bad)?;
            let s = std::str::from_utf8(hex).map_err(|_| bad())?;
            out.push(u8::from_str_radix(s, 16).map_err(|_| bad())?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let name = String::from_utf8(out).map_err(|_| bad())?;
    // Checked before trimming so an encoded control character is never silently dropped.
    if name
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(bad());
    }
    let name = name.trim();
    let n = name.chars().count();
    if n == 0
        || n > MAX_FILE_NAME_CHARS
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(bad());
    }
    Ok(name.to_owned())
}

/// Server-side type detection by magic bytes; the client's Content-Type is ignored.
pub(crate) fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

async fn ensure_dir(p: &FsPath) -> Result<(), AppError> {
    tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(p)
        .await
        .map_err(|e| {
            tracing::warn!(error.kind = ?e.kind(), "attachment storage not writable");
            AppError::Unavailable
        })
}

async fn ticket_writable(
    conn: &mut PgConnection,
    ws: Uuid,
    ticket_id: Uuid,
) -> Result<(), AppError> {
    let status: String = sqlx::query_scalar(
        "SELECT workflow_status FROM tickets WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(ticket_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::NotFound)?;
    if status == "closed" {
        return Err(AppError::RuleViolation);
    }
    Ok(())
}

async fn audit_attachment(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    action: &str,
    attachment_id: Uuid,
    outcome: &str,
    change: serde_json::Value,
    correlation_id: Option<String>,
) -> Result<Uuid, AppError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_events (workspace_id, actor_id, action, entity_type, entity_id, \
           safe_change, outcome, correlation_id) \
         VALUES ($1, $2, $3, 'attachment', $4, $5, $6, $7) RETURNING id",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .bind(action)
    .bind(attachment_id)
    .bind(change)
    .bind(outcome)
    .bind(correlation_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// `POST …/tickets/{ticket_id}/attachments?visibility=internal|public`, raw file body,
/// file name in `X-File-Name` (percent-encoded UTF-8).
pub async fn upload(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    q: Result<Query<UploadQuery>, QueryRejection>,
    body: Body,
) -> Result<(StatusCode, Json<AttachmentUploaded>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let Query(q) = q.map_err(|_| AppError::InvalidRequest)?;
    let visibility = match q.visibility.as_deref() {
        None | Some("") | Some("internal") => "internal",
        Some("public") => "public",
        Some(_) => return Err(AppError::Validation("visibility")),
    };
    let name = file_name(&headers)?;
    access.require(Permission::AttachmentUpload)?;
    let ws = access.workspace_id;
    let corr = correlation(&headers);
    let root = state.config.attachment_dir.clone().ok_or_else(|| {
        tracing::info!("attachment storage not configured");
        AppError::Unavailable
    })?;
    let max = state.config.attachment_max_bytes;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > max) {
        return Err(AppError::AttachmentTooLarge);
    }
    {
        let mut conn = state.pool.acquire().await?;
        ticket_writable(&mut conn, ws, ticket_id).await?;
    }
    let _slot = SLOTS.try_acquire().map_err(|_| {
        tracing::info!("attachment upload slots exhausted");
        AppError::Unavailable
    })?;

    // 1. Stream into quarantine, bounded by the configured cap.
    let quarantine = root.join("quarantine");
    let store = root.join("store");
    ensure_dir(&quarantine).await?;
    ensure_dir(&store).await?;
    let id = Uuid::new_v4();
    let part = quarantine.join(format!("{id}.part"));
    let mut guard = RemoveOnDrop(Some(part.clone()));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&part)
        .await
        .map_err(|e| {
            tracing::warn!(error.kind = ?e.kind(), "quarantine file not created");
            AppError::Unavailable
        })?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut head: Vec<u8> = Vec::with_capacity(16);
    let mut body = body;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| AppError::InvalidRequest)?;
        let Ok(chunk) = frame.into_data() else {
            continue;
        };
        size += chunk.len() as u64;
        if size > max {
            return Err(AppError::AttachmentTooLarge);
        }
        if head.len() < 16 {
            let take = (16 - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| {
            tracing::warn!(error.kind = ?e.kind(), "quarantine write failed");
            AppError::Unavailable
        })?;
    }
    if size == 0 {
        return Err(AppError::Validation("file"));
    }
    file.sync_all().await.map_err(|_| AppError::Unavailable)?;
    drop(file);
    let mime = sniff(&head).ok_or(AppError::AttachmentTypeRefused)?;
    let sha = hasher.finalize().to_vec();
    let size_i64 = i64::try_from(size).map_err(AppError::internal)?;

    // 2. Record the pending row (not bound, invisible to the ticket).
    sqlx::query(
        "INSERT INTO attachments (id, workspace_id, uploaded_by, object_key, file_name, \
           inspected_mime, size_bytes, sha256, visibility, target_ticket_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(ws)
    .bind(access.user_id)
    .bind(format!("att/{id}"))
    .bind(&name)
    .bind(mime)
    .bind(size_i64)
    .bind(&sha)
    .bind(visibility)
    .bind(ticket_id)
    .execute(&state.pool)
    .await?;

    // 3. Scan. No configured scanner is the same as an unavailable one: fail closed.
    let outcome = match &state.config.clamd {
        Some(addr) => clamd::scan_file(addr, &part).await,
        None => ScanOutcome::Unavailable,
    };
    if outcome != ScanOutcome::Clean {
        let mut tx = state.pool.begin().await?;
        sqlx::query(
            "UPDATE attachments SET scan_status = $3 \
             WHERE workspace_id = $1 AND id = $2 AND scan_status = 'pending'",
        )
        .bind(ws)
        .bind(id)
        .bind(outcome.status_code())
        .execute(&mut *tx)
        .await?;
        audit_attachment(
            &mut tx,
            &access,
            "attachment.upload",
            id,
            "failed",
            serde_json::json!({ "ticketId": ticket_id, "scanStatus": outcome.status_code() }),
            corr,
        )
        .await?;
        tx.commit().await?;
        tracing::info!(workspace_id = %ws, attachment_id = %id, scan.outcome = outcome.status_code(), "attachment refused");
        // `guard` drops here and deletes the quarantined bytes.
        return Err(match outcome {
            ScanOutcome::Infected => AppError::AttachmentInfected,
            ScanOutcome::Failed => AppError::ScanFailed,
            ScanOutcome::Unavailable | ScanOutcome::Clean => AppError::ScannerUnavailable,
        });
    }

    // 4. Clean: move out of quarantine, then bind in one transaction with the audit row.
    let stored = store.join(id.to_string());
    tokio::fs::rename(&part, &stored).await.map_err(|e| {
        tracing::warn!(error.kind = ?e.kind(), "attachment move failed");
        AppError::Unavailable
    })?;
    guard.retarget(stored);
    let mut tx = state.pool.begin().await?;
    if let Err(e) = lock_open_ticket(&mut tx, ws, ticket_id, false).await {
        drop(tx);
        // Closed while scanning: the clean file is never bound.
        sqlx::query(
            "UPDATE attachments SET scan_status = 'rejected' \
             WHERE workspace_id = $1 AND id = $2 AND scan_status = 'pending'",
        )
        .bind(ws)
        .bind(id)
        .execute(&state.pool)
        .await?;
        return Err(e);
    }
    sqlx::query(
        "UPDATE attachments SET scan_status = 'clean', ticket_id = $3, bound_at = now() \
         WHERE workspace_id = $1 AND id = $2 AND scan_status = 'pending'",
    )
    .bind(ws)
    .bind(id)
    .bind(ticket_id)
    .execute(&mut *tx)
    .await?;
    let audit_id = audit(
        &mut tx,
        &access,
        "attachment.upload",
        ticket_id,
        serde_json::json!({ "attachmentId": id, "visibility": visibility }),
        corr,
    )
    .await?;
    let detail = ticket_detail::load(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    guard.disarm();
    tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, attachment_id = %id, "attachment bound");
    Ok((
        StatusCode::CREATED,
        Json(AttachmentUploaded {
            attachment_id: id,
            detail,
            audit_event_id: audit_id,
        }),
    ))
}

#[derive(sqlx::FromRow)]
struct StoredRow {
    object_key: String,
    file_name: String,
    inspected_mime: Option<String>,
    size_bytes: i64,
    sha256: Option<Vec<u8>>,
}

fn disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut pct = String::with_capacity(name.len() * 3);
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
            pct.push(b as char);
        } else {
            pct.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{pct}")
}

/// `GET …/tickets/{ticket_id}/attachments/{attachment_id}/content`.
pub async fn download(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket, raw_att)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    access.require(Permission::AttachmentRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let id = parse_id(&raw_att)?;
    let ws = access.workspace_id;
    let root = state
        .config
        .attachment_dir
        .clone()
        .ok_or(AppError::Unavailable)?;
    let row: StoredRow = sqlx::query_as(
        "SELECT object_key, file_name, inspected_mime, size_bytes, sha256 FROM attachments \
         WHERE workspace_id = $1 AND ticket_id = $2 AND id = $3 \
           AND scan_status = 'clean' AND bound_at IS NOT NULL",
    )
    .bind(ws)
    .bind(ticket_id)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    if row.object_key != format!("att/{id}") {
        return Err(AppError::internal(StorageMismatch));
    }
    let bytes = tokio::fs::read(root.join("store").join(id.to_string()))
        .await
        .map_err(|e| {
            tracing::warn!(error.kind = ?e.kind(), attachment_id = %id, "stored attachment unreadable");
            AppError::Unavailable
        })?;
    let digest = Sha256::digest(&bytes);
    if i64::try_from(bytes.len()).ok() != Some(row.size_bytes)
        || row.sha256.as_deref() != Some(digest.as_slice())
    {
        return Err(AppError::internal(StorageMismatch));
    }
    let mut conn = state.pool.acquire().await?;
    audit_attachment(
        &mut conn,
        &access,
        "attachment.download",
        id,
        "success",
        serde_json::json!({ "ticketId": ticket_id }),
        correlation(&headers),
    )
    .await?;
    drop(conn);
    let mime = row
        .inspected_mime
        .as_deref()
        .and_then(|m| HeaderValue::from_str(m).ok())
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
    let disp = HeaderValue::from_str(&disposition(&row.file_name)).map_err(AppError::internal)?;
    let mut resp = Body::from(bytes).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, mime);
    h.insert(header::CONTENT_DISPOSITION, disp);
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    Ok(resp)
}

use axum::http::HeaderName;

#[derive(Debug)]
struct StorageMismatch;

impl std::fmt::Display for StorageMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stored attachment does not match its record")
    }
}

impl std::error::Error for StorageMismatch {}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(
            FILE_NAME_HEADER,
            HeaderValue::from_str(v).unwrap_or(HeaderValue::from_static("")),
        );
        m
    }

    #[test]
    fn file_names_are_decoded_and_path_like_names_refused() {
        assert_eq!(
            file_name(&h("bukti%20transfer.pdf")).ok().as_deref(),
            Some("bukti transfer.pdf")
        );
        assert_eq!(
            file_name(&h("struk%C3%A9.png")).ok().as_deref(),
            Some("struké.png")
        );
        for bad in [
            "",
            "..",
            "a%2Fb.pdf",
            "a%5Cb.pdf",
            "%0A.pdf",
            "%0Aa.pdf",
            "a.pdf%09",
            "%ZZ",
            "%C3",
        ] {
            assert!(file_name(&h(bad)).is_err(), "{bad}");
        }
        assert!(file_name(&HeaderMap::new()).is_err());
    }

    #[test]
    fn sniffing_uses_magic_bytes_only() {
        assert_eq!(sniff(b"%PDF-1.7\n"), Some("application/pdf"));
        assert_eq!(
            sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]),
            Some("image/png")
        );
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(sniff(b"MZ\x90\x00"), None);
    }

    #[test]
    fn disposition_is_header_safe() {
        let d = disposition("bukti \"x\"é.pdf");
        assert!(d.starts_with("attachment; filename=\"bukti _x__.pdf\""));
        assert!(d.contains("filename*=UTF-8''bukti%20%22x%22%C3%A9.pdf"));
        assert!(HeaderValue::from_str(&d).is_ok());
    }
}
