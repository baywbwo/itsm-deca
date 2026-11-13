//! CP6a: inbound Jira Cloud webhook (`POST /api/v1/integrations/jira/{connection_id}/webhook`).
//!
//! Jira has no ITSM session, so this route lives outside the session/CSRF stack and proves itself
//! with `X-Hub-Signature: sha256=<hex>` (HMAC-SHA256 of the raw body with the per-connection
//! secret an administrator generated in ITSM). Every refusal before the signature check answers
//! the same 401, so the endpoint does not reveal which connections exist.
//!
//! Each delivery is recorded once per connection (`jira_webhook_events`): a replay answers 204
//! without effect, a payload timestamp outside −72 h … +5 min is `stale`, and an older event that
//! arrives late never overwrites a newer Jira status/assignee. Effects reach only issues already
//! linked to a ticket; the ITSM ticket status itself is never changed by Jira (D02).
//! Neither the body nor comment text is logged or audited.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use tokio::sync::Semaphore;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

use crate::AppState;
use crate::crypto::{self, Purpose};
use crate::error::AppError;
use crate::routes::reports::hex;
use crate::routes::sync;

pub const PATH: &str = "/api/v1/integrations/jira/{connection_id}/webhook";
/// Jira Cloud payloads with long descriptions stay well below this.
const BODY_LIMIT_BYTES: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
/// Deliveries processed at once; more are refused with 503 (Jira retries them).
pub const MAX_CONCURRENT: usize = 16;
pub const MAX_COMMENT_CHARS: usize = 5000;
const SIGNATURE_HEADER: &str = "x-hub-signature";
const DELIVERY_HEADER: &str = "x-atlassian-webhook-identifier";
const FALLBACK_AUTHOR: &str = "Pengguna Jira";

/// The webhook route with its own body limit, timeout, concurrency cap and (CP7d) per-address
/// limit shared by every replica (no session layer). The shared count runs inside the
/// concurrency cap, so a flood never opens more than `MAX_CONCURRENT` database writes.
/// Route layers only: with `layer` they also wrapped the merged fallback, so every unknown
/// path (404) took a webhook slot and, since CP7d, a webhook count (found by CP7d tests).
pub fn router(state: &AppState) -> Router<AppState> {
    let slots = Arc::new(Semaphore::new(MAX_CONCURRENT));
    Router::new()
        .route(PATH, post(receive))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::shared_limit::webhook_layer,
        ))
        .route_layer(axum::middleware::from_fn_with_state(slots, shed))
        .route_layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES))
        .route_layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            TIMEOUT,
        ))
}

/// Refuses instead of queueing when every slot is busy.
async fn shed(State(slots): State<Arc<Semaphore>>, req: Request, next: Next) -> Response {
    match slots.try_acquire_owned() {
        Ok(permit) => {
            let res = next.run(req).await;
            drop(permit);
            res
        }
        Err(_) => {
            tracing::warn!("jira webhook overloaded; delivery refused");
            AppError::Unavailable.into_response()
        }
    }
}

/// `sha256=<64 hex>` → the 32 signature bytes.
pub fn parse_signature(raw: &str) -> Option<[u8; 32]> {
    let hex = raw.trim().strip_prefix("sha256=")?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(pair).ok()?;
        out[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(out)
}

/// Constant-time HMAC-SHA256 check of the raw body.
pub fn signature_ok(secret: &[u8], body: &[u8], signature: &[u8; 32]) -> bool {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(signature).is_ok()
}

/// The delivery id Jira sends, or the body hash when it is missing or malformed.
pub fn delivery_id(headers: &HeaderMap, body_hash: &[u8]) -> String {
    headers
        .get(DELIVERY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| (8..=200).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("sha256:{}", hex(body_hash)))
}

/// Plain text of a Jira comment body: wiki markup (string) or Atlassian Document Format.
pub fn comment_text(body: &Value) -> String {
    fn walk(node: &Value, out: &mut String) {
        match node {
            Value::Object(o) => {
                match o.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = o.get("text").and_then(Value::as_str) {
                            out.push_str(t);
                        }
                    }
                    Some("hardBreak") => out.push('\n'),
                    Some("mention") => {
                        if let Some(t) = o
                            .get("attrs")
                            .and_then(|a| a.get("text"))
                            .and_then(Value::as_str)
                        {
                            out.push_str(t);
                        }
                    }
                    _ => {}
                }
                if let Some(Value::Array(children)) = o.get("content") {
                    for c in children {
                        walk(c, out);
                    }
                    if matches!(
                        o.get("type").and_then(Value::as_str),
                        Some("paragraph" | "heading" | "listItem" | "codeBlock" | "blockquote")
                    ) && !out.ends_with('\n')
                    {
                        out.push('\n');
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut raw = String::new();
    match body {
        Value::String(s) => raw.push_str(s),
        other => walk(other, &mut raw),
    }
    let cleaned: String = raw
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    let mut text: String = trimmed.chars().take(MAX_COMMENT_CHARS).collect();
    if trimmed.chars().count() > MAX_COMMENT_CHARS {
        text.pop();
        text.push('…');
    }
    text
}

fn label(v: Option<&Value>, max: usize) -> Option<String> {
    let s: String = v?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

#[derive(sqlx::FromRow)]
struct Conn {
    workspace_id: Uuid,
    provider: String,
    state: String,
    mode: String,
    webhook_secret_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct Issue {
    id: Uuid,
    issue_key: String,
    external_status: Option<String>,
    external_status_id: Option<String>,
    external_assignee_ref: Option<String>,
    external_assignee_name: Option<String>,
    /// The event is not older than the last applied Jira state.
    in_order: bool,
}

/// What one delivery did (stored on the event row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Applied,
    Ignored,
    UnknownIssue,
    Stale,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::Ignored => "ignored",
            Outcome::UnknownIssue => "unknown_issue",
            Outcome::Stale => "stale",
        }
    }
}

#[derive(Default)]
struct Effect {
    tickets: i64,
    status_changed: bool,
    assignee_changed: bool,
    comment: bool,
}

pub async fn receive(
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let id = Uuid::parse_str(&raw_id).map_err(|_| AppError::Unauthenticated)?;
    let conn = sqlx::query_as::<_, Conn>(
        "SELECT workspace_id, provider, state, mode, webhook_secret_id \
         FROM integration_connections WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::Unauthenticated)?;
    if conn.provider != "jira"
        || conn.state != "active"
        || conn.mode != state.config.integrations.mode.as_str()
    {
        return Err(AppError::Unauthenticated);
    }
    let secret_id = conn.webhook_secret_id.ok_or(AppError::Unauthenticated)?;
    let ws = conn.workspace_id;
    let Some(keyring) = state.config.keyring.as_deref() else {
        tracing::warn!(connection_id = %id, "jira webhook refused: keyring missing");
        return Err(AppError::Unauthenticated);
    };
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_signature)
        .ok_or(AppError::Unauthenticated)?;
    let secret = crypto::load_secret(
        &state.pool,
        keyring,
        secret_id,
        Some(ws),
        Purpose::JiraWebhookSecret,
    )
    .await
    .map_err(|_| {
        tracing::warn!(connection_id = %id, "jira webhook secret unreadable");
        AppError::Unauthenticated
    })?;
    if !signature_ok(secret.expose(), &body, &signature) {
        tracing::info!(connection_id = %id, "jira webhook signature mismatch");
        return Err(AppError::Unauthenticated);
    }
    drop(secret);

    let payload: Value = serde_json::from_slice(&body).map_err(|_| AppError::InvalidRequest)?;
    let event = label(payload.get("webhookEvent"), 100).ok_or(AppError::InvalidRequest)?;
    let body_hash = Sha256::digest(&body).to_vec();
    let delivery = delivery_id(&headers, &body_hash);
    let sent_ms = payload.get("timestamp").and_then(Value::as_i64);
    let issue_ref = label(payload.pointer("/issue/id"), 128);

    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid, bool)> = sqlx::query_as(
        "INSERT INTO jira_webhook_events (workspace_id, connection_id, delivery_id, body_sha256, \
           event, external_issue_id, sent_at, outcome) \
         VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7::double precision / 1000), 'ignored') \
         ON CONFLICT (connection_id, delivery_id) DO NOTHING \
         RETURNING id, (sent_at IS NOT NULL \
           AND sent_at BETWEEN now() - interval '72 hours' AND now() + interval '5 minutes')",
    )
    .bind(ws)
    .bind(id)
    .bind(&delivery)
    .bind(&body_hash)
    .bind(&event)
    .bind(&issue_ref)
    .bind(sent_ms)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((event_id, fresh)) = row else {
        tx.rollback().await?;
        tracing::info!(connection_id = %id, event = %event, "jira webhook replay ignored");
        return Ok(StatusCode::NO_CONTENT);
    };
    let mut effect = Effect::default();
    let mut issue_key = None;
    let outcome = if !fresh {
        Outcome::Stale
    } else if !matches!(event.as_str(), "jira:issue_updated" | "comment_created") {
        Outcome::Ignored
    } else {
        match lock_issue(&mut tx, ws, id, issue_ref.as_deref(), sent_ms).await? {
            None => Outcome::UnknownIssue,
            Some(issue) => {
                issue_key = Some(issue.issue_key.clone());
                let ctx = Ctx {
                    ws,
                    event_id,
                    body_hash: &body_hash,
                    payload: &payload,
                };
                if event == "comment_created" {
                    apply_comment(&mut tx, &ctx, &issue, &mut effect).await?
                } else if !issue.in_order {
                    Outcome::Stale
                } else {
                    apply_issue(&mut tx, &ctx, &issue, sent_ms, &mut effect).await?
                }
            }
        }
    };
    sqlx::query("UPDATE jira_webhook_events SET outcome = $2 WHERE id = $1")
        .bind(event_id)
        .bind(outcome.as_str())
        .execute(&mut *tx)
        .await?;
    if outcome == Outcome::Applied {
        // Ids and flags only: no comment text, names or body.
        sync::audit_entity(
            &mut tx,
            ws,
            None,
            "integration.webhook",
            "jira_webhook_event",
            event_id,
            serde_json::json!({
                "connectionId": id,
                "event": event,
                "issueKey": issue_key,
                "tickets": effect.tickets,
                "statusChanged": effect.status_changed,
                "assigneeChanged": effect.assignee_changed,
                "comment": effect.comment,
            }),
            &None,
        )
        .await?;
    }
    tx.commit().await?;
    tracing::info!(
        connection_id = %id,
        event = %event,
        outcome = outcome.as_str(),
        tickets = effect.tickets,
        "jira webhook received"
    );
    Ok(StatusCode::NO_CONTENT)
}

struct Ctx<'a> {
    ws: Uuid,
    event_id: Uuid,
    body_hash: &'a [u8],
    payload: &'a Value,
}

async fn lock_issue(
    tx: &mut PgConnection,
    ws: Uuid,
    connection: Uuid,
    external_id: Option<&str>,
    sent_ms: Option<i64>,
) -> Result<Option<Issue>, AppError> {
    let Some(external_id) = external_id else {
        return Ok(None);
    };
    Ok(sqlx::query_as::<_, Issue>(
        "SELECT id, issue_key, external_status, external_status_id, external_assignee_ref, \
           external_assignee_name, \
           (external_updated_at IS NULL \
             OR to_timestamp($4::double precision / 1000) >= external_updated_at) AS in_order \
         FROM jira_issues WHERE workspace_id = $1 AND connection_id = $2 AND external_issue_id = $3 \
         FOR UPDATE",
    )
    .bind(ws)
    .bind(connection)
    .bind(external_id)
    .bind(sent_ms)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Tickets that link this issue and still take timeline entries (closed tickets are archived).
async fn open_tickets(tx: &mut PgConnection, ws: Uuid, issue: Uuid) -> Result<Vec<Uuid>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT t.id FROM related_work r \
         JOIN tickets t ON t.workspace_id = r.workspace_id AND t.id = r.ticket_id \
         WHERE r.workspace_id = $1 AND r.jira_issue_id = $2 AND t.workflow_status <> 'closed'",
    )
    .bind(ws)
    .bind(issue)
    .fetch_all(&mut *tx)
    .await?)
}

async fn activity(
    tx: &mut PgConnection,
    ctx: &Ctx<'_>,
    ticket: Uuid,
    kind: &str,
    suffix: char,
    author: &str,
    body: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO ticket_activities (workspace_id, ticket_id, actor_id, activity_type, body, \
           idempotency_key, request_hash, external_author) \
         VALUES ($1, $2, NULL, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(ctx.ws)
    .bind(ticket)
    .bind(kind)
    .bind(body)
    .bind(format!("jira-{}-{ticket}-{suffix}", ctx.event_id))
    .bind(ctx.body_hash)
    .bind(author)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

fn actor_name(payload: &Value) -> String {
    label(payload.pointer("/user/displayName"), 200).unwrap_or_else(|| FALLBACK_AUTHOR.to_owned())
}

async fn apply_issue(
    tx: &mut PgConnection,
    ctx: &Ctx<'_>,
    issue: &Issue,
    sent_ms: Option<i64>,
    effect: &mut Effect,
) -> Result<Outcome, AppError> {
    let fields = ctx.payload.pointer("/issue/fields");
    let status_id = label(fields.and_then(|f| f.pointer("/status/id")), 64);
    let status_name = label(fields.and_then(|f| f.pointer("/status/name")), 100);
    // `assignee: null` means unassigned; a missing key means "not part of this payload".
    let assignee = fields.and_then(|f| f.get("assignee"));
    let (assignee_ref, assignee_name) = match assignee {
        Some(a) if a.is_object() => (
            label(a.get("accountId"), 128),
            label(a.get("displayName"), 200),
        ),
        _ => (None, None),
    };
    let status_changed =
        status_id.is_some() && status_id.as_deref() != issue.external_status_id.as_deref();
    let assignee_changed =
        assignee.is_some() && assignee_ref.as_deref() != issue.external_assignee_ref.as_deref();
    sqlx::query(
        "UPDATE jira_issues SET \
           external_status = CASE WHEN $3 THEN $4 ELSE external_status END, \
           external_status_id = CASE WHEN $3 THEN $5 ELSE external_status_id END, \
           external_assignee_ref = CASE WHEN $6 THEN $7 ELSE external_assignee_ref END, \
           external_assignee_name = CASE WHEN $6 THEN $8 ELSE external_assignee_name END, \
           external_updated_at = to_timestamp($9::double precision / 1000) \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ctx.ws)
    .bind(issue.id)
    .bind(status_id.is_some())
    .bind(&status_name)
    .bind(&status_id)
    .bind(assignee.is_some())
    .bind(&assignee_ref)
    .bind(&assignee_name)
    .bind(sent_ms)
    .execute(&mut *tx)
    .await?;
    if !status_changed && !assignee_changed {
        return Ok(Outcome::Ignored);
    }
    let author = actor_name(ctx.payload);
    let key = &issue.issue_key;
    let tickets = open_tickets(tx, ctx.ws, issue.id).await?;
    for &ticket in &tickets {
        if status_changed {
            let to = status_name.as_deref().unwrap_or("?");
            let body = match issue.external_status.as_deref() {
                Some(from) if from != to => {
                    format!("Status isu Jira {key} berubah: {from} → {to}.")
                }
                _ => format!("Status isu Jira {key} sekarang: {to}."),
            };
            activity(tx, ctx, ticket, "jira_status", 's', &author, &body).await?;
        }
        if assignee_changed {
            let body = match (&assignee_name, &assignee_ref) {
                (Some(n), _) => format!("Penerima tugas isu Jira {key}: {n}."),
                (None, Some(_)) => format!("Penerima tugas isu Jira {key} diganti."),
                (None, None) => format!(
                    "Isu Jira {key} tidak lagi punya penerima tugas{}.",
                    issue
                        .external_assignee_name
                        .as_deref()
                        .map(|n| format!(" (sebelumnya {n})"))
                        .unwrap_or_default()
                ),
            };
            activity(tx, ctx, ticket, "jira_assignee", 'a', &author, &body).await?;
        }
    }
    effect.tickets = tickets.len() as i64;
    effect.status_changed = status_changed;
    effect.assignee_changed = assignee_changed;
    Ok(Outcome::Applied)
}

async fn apply_comment(
    tx: &mut PgConnection,
    ctx: &Ctx<'_>,
    issue: &Issue,
    effect: &mut Effect,
) -> Result<Outcome, AppError> {
    let Some(comment) = ctx.payload.get("comment") else {
        return Ok(Outcome::Ignored);
    };
    let author = label(comment.pointer("/author/displayName"), 200)
        .unwrap_or_else(|| FALLBACK_AUTHOR.to_owned());
    let mut text = comment.get("body").map(comment_text).unwrap_or_default();
    if text.is_empty() {
        text = "(Komentar Jira tanpa teks.)".to_owned();
    }
    let tickets = open_tickets(tx, ctx.ws, issue.id).await?;
    if tickets.is_empty() {
        return Ok(Outcome::Ignored);
    }
    for &ticket in &tickets {
        activity(tx, ctx, ticket, "jira_comment", 'c', &author, &text).await?;
    }
    effect.tickets = tickets.len() as i64;
    effect.comment = true;
    Ok(Outcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn signature_header_is_sha256_hex_only() {
        let sig = "sha256=".to_owned() + &"ab".repeat(32);
        assert_eq!(parse_signature(&sig), Some([0xab; 32]));
        assert_eq!(parse_signature(&"ab".repeat(32)), None);
        assert_eq!(parse_signature("sha1=abcd"), None);
        assert_eq!(
            parse_signature(&("sha256=".to_owned() + &"zz".repeat(32))),
            None
        );
        assert_eq!(
            parse_signature(&("sha256=".to_owned() + &"ab".repeat(31))),
            None
        );
    }

    #[test]
    fn hmac_matches_the_raw_body_only() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let body = br#"{"webhookEvent":"jira:issue_updated"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap_or_else(|_| unreachable!());
        mac.update(body);
        let sig: [u8; 32] = mac.finalize().into_bytes().into();
        assert!(signature_ok(secret, body, &sig));
        assert!(!signature_ok(secret, b"{}", &sig));
        assert!(!signature_ok(b"another-secret", body, &sig));
    }

    #[test]
    fn delivery_id_falls_back_to_body_hash() {
        let hash = [7u8; 32];
        let mut h = HeaderMap::new();
        assert_eq!(
            delivery_id(&h, &hash),
            format!("sha256:{}", "07".repeat(32))
        );
        h.insert(DELIVERY_HEADER, HeaderValue::from_static("abc"));
        assert!(delivery_id(&h, &hash).starts_with("sha256:"));
        h.insert(DELIVERY_HEADER, HeaderValue::from_static("delivery-123456"));
        assert_eq!(delivery_id(&h, &hash), "delivery-123456");
    }

    #[test]
    fn comment_text_reads_wiki_and_adf_and_caps_length() {
        assert_eq!(
            comment_text(&Value::String("  halo *dunia* ".into())),
            "halo *dunia*"
        );
        let adf = serde_json::json!({
            "type": "doc", "version": 1, "content": [
                { "type": "paragraph", "content": [
                    { "type": "text", "text": "Sudah dicek " },
                    { "type": "mention", "attrs": { "id": "x", "text": "@Budi" } },
                    { "type": "hardBreak" },
                    { "type": "text", "text": "baris dua" }
                ]},
                { "type": "paragraph", "content": [{ "type": "text", "text": "Paragraf\u{7}" }] }
            ]
        });
        assert_eq!(comment_text(&adf), "Sudah dicek @Budi\nbaris dua\nParagraf");
        let long = Value::String("x".repeat(MAX_COMMENT_CHARS + 50));
        let t = comment_text(&long);
        assert_eq!(t.chars().count(), MAX_COMMENT_CHARS);
        assert!(t.ends_with('…'));
        assert_eq!(comment_text(&serde_json::json!({ "type": "doc" })), "");
    }
}
