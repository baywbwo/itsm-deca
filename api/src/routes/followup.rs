//! D08b ticket follow-up: operational notes (a record of handling, not team chat) and the
//! read-only customer conversation panel. No omnichannel adapter is connected in this build,
//! so conversations can only hold labelled simulation rows and there is no send path.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission};
use crate::error::AppError;
use crate::routes::intake::{idempotency_key, required_text};
use crate::routes::ticket_detail::{lock_open_ticket, request_hash};
use crate::routes::ticket_list::{audit, body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

macro_rules! utc {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}

pub(crate) const MAX_NOTE_CHARS: usize = 5000;
const NOTES_DEFAULT_LIMIT: i64 = 30;
const NOTES_MAX_LIMIT: i64 = 100;
const MAX_CURSOR_CHARS: usize = 100;
const MAX_CONVERSATIONS: i64 = 20;
const MAX_MESSAGES: i64 = 500;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    id: Uuid,
    label: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct NoteRow {
    id: Uuid,
    body: String,
    #[serde(skip)]
    activity_type: String,
    #[serde(skip)]
    author_id: Option<Uuid>,
    #[serde(skip)]
    author_name: Option<String>,
    #[serde(skip)]
    external_author: Option<String>,
    created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteView {
    id: Uuid,
    /// `note` (ITSM officer) | `jira_comment` | `jira_status` | `jira_assignee` (CP6a webhook).
    kind: String,
    body: String,
    /// ITSM author; `null` for entries written by the Jira webhook.
    author: Option<Person>,
    /// Jira display name for Jira entries (as sent by Jira; not an ITSM account).
    external_author: Option<String>,
    created_at: String,
}

impl From<NoteRow> for NoteView {
    fn from(r: NoteRow) -> Self {
        NoteView {
            id: r.id,
            kind: r.activity_type,
            body: r.body,
            author: r
                .author_id
                .zip(r.author_name)
                .map(|(id, label)| Person { id, label }),
            external_author: r.external_author,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotePage {
    items: Vec<NoteView>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteCreated {
    note: NoteView,
    replayed: bool,
    audit_event_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotesQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteBody {
    body: String,
}

const NOTE_SELECT: &str = concat!(
    "SELECT n.id, n.body, n.activity_type, n.actor_id AS author_id, u.display_name AS author_name, ",
    "n.external_author, ",
    utc!("n.created_at"),
    " AS created_at FROM ticket_activities n LEFT JOIN users u ON u.id = n.actor_id "
);

async fn ticket_exists(
    conn: &mut sqlx::PgConnection,
    ws: Uuid,
    ticket_id: Uuid,
) -> Result<(), AppError> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tickets WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(ticket_id)
            .fetch_optional(&mut *conn)
            .await?;
    found.map(|_| ()).ok_or(AppError::NotFound)
}

fn parse_cursor(raw: &str) -> Result<(String, Uuid), AppError> {
    if raw.len() > MAX_CURSOR_CHARS {
        return Err(AppError::InvalidRequest);
    }
    let (ts, id) = raw.split_once('~').ok_or(AppError::InvalidRequest)?;
    let ts = crate::routes::ticket_list::instant(Some(ts))?.ok_or(AppError::InvalidRequest)?;
    let id = Uuid::parse_str(id).map_err(|_| AppError::InvalidRequest)?;
    Ok((ts, id))
}

pub async fn list_notes(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    q: Result<Query<NotesQuery>, QueryRejection>,
) -> Result<Json<NotePage>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let Query(q) = q.map_err(|_| AppError::InvalidRequest)?;
    let limit = match q.limit.as_deref() {
        None | Some("") => NOTES_DEFAULT_LIMIT,
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=NOTES_MAX_LIMIT).contains(n))
            .ok_or(AppError::InvalidRequest)?,
    };
    let cursor = q
        .cursor
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(parse_cursor)
        .transpose()?;

    let mut conn = state.pool.acquire().await?;
    ticket_exists(&mut conn, access.workspace_id, ticket_id).await?;
    let mut b = QueryBuilder::<Postgres>::new(NOTE_SELECT);
    b.push("WHERE n.workspace_id = ")
        .push_bind(access.workspace_id)
        .push(" AND n.ticket_id = ")
        .push_bind(ticket_id);
    if let Some((ts, id)) = &cursor {
        b.push(" AND (n.created_at, n.id) < (")
            .push_bind(ts.clone())
            .push("::timestamptz, ")
            .push_bind(*id)
            .push(")");
    }
    b.push(" ORDER BY n.created_at DESC, n.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut rows: Vec<NoteRow> = b.build_query_as().fetch_all(&mut *conn).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = if more {
        rows.last().map(|r| format!("{}~{}", r.created_at, r.id))
    } else {
        None
    };
    Ok(Json(NotePage {
        items: rows.into_iter().map(NoteView::from).collect(),
        next_cursor,
    }))
}

async fn note_by_id(
    conn: &mut sqlx::PgConnection,
    ws: Uuid,
    id: Uuid,
) -> Result<NoteView, AppError> {
    let mut b = QueryBuilder::<Postgres>::new(NOTE_SELECT);
    b.push("WHERE n.workspace_id = ")
        .push_bind(ws)
        .push(" AND n.id = ")
        .push_bind(id);
    let row: NoteRow = b.build_query_as().fetch_one(&mut *conn).await?;
    Ok(row.into())
}

/// Adds an operational note. Idempotent per user + workspace + `Idempotency-Key`: a replay
/// with the same text returns the original note (200); different text is refused (409).
pub async fn create_note(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    payload: Result<Json<NoteBody>, JsonRejection>,
) -> Result<(StatusCode, Json<NoteCreated>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let key = idempotency_key(&headers)?;
    let b = body(payload)?;
    let text = required_text(&b.body, MAX_NOTE_CHARS, "body")?;
    access.require(Permission::TicketUpdate)?;
    let ws = access.workspace_id;
    let hash = request_hash(&serde_json::json!({
        "v": "d08b-note-v1",
        "ticketId": ticket_id,
        "body": text,
    }))?;

    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("note-create:{ws}:{}:{key}", access.user_id))
        .execute(&mut *tx)
        .await?;
    let prior: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id, request_hash FROM ticket_activities \
         WHERE workspace_id = $1 AND actor_id = $2 AND idempotency_key = $3",
    )
    .bind(ws)
    .bind(access.user_id)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, stored)) = prior {
        if stored != hash {
            tracing::info!(workspace_id = %ws, "note key reused with another payload");
            return Err(AppError::IdempotencyConflict);
        }
        let note = note_by_id(&mut tx, ws, id).await?;
        tx.commit().await?;
        return Ok((
            StatusCode::OK,
            Json(NoteCreated {
                note,
                replayed: true,
                audit_event_id: None,
            }),
        ));
    }
    // Notes are allowed on resolved tickets (follow-up), never on the closed archive.
    lock_open_ticket(&mut tx, ws, ticket_id, false).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO ticket_activities (workspace_id, ticket_id, actor_id, activity_type, body, \
           idempotency_key, request_hash) VALUES ($1, $2, $3, 'note', $4, $5, $6) RETURNING id",
    )
    .bind(ws)
    .bind(ticket_id)
    .bind(access.user_id)
    .bind(&text)
    .bind(&key)
    .bind(&hash)
    .fetch_one(&mut *tx)
    .await?;
    // The audit row names the note only; the text stays in the protected table.
    let audit_id = audit(
        &mut tx,
        &access,
        "ticket.note",
        ticket_id,
        serde_json::json!({ "noteId": id }),
        correlation(&headers),
    )
    .await?;
    let note = note_by_id(&mut tx, ws, id).await?;
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, note_id = %id, "note added");
    Ok((
        StatusCode::CREATED,
        Json(NoteCreated {
            note,
            replayed: false,
            audit_event_id: Some(audit_id),
        }),
    ))
}

// ---------- conversations (read only) ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    id: Uuid,
    /// `inbound` | `outbound`.
    direction: String,
    body: String,
    /// `received` | `queued` | `sent` | `delivered` | `failed` | `unknown`.
    delivery_state: String,
    occurred_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    id: Uuid,
    /// Always `simulation` in this build.
    source: String,
    channel_label: String,
    external_thread_id: String,
    messages: Vec<MessageView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPanel {
    /// No omnichannel adapter exists yet, so this is always `false` and nothing can be sent.
    adapter_configured: bool,
    conversations: Vec<ConversationView>,
    /// `true` when more messages exist than were returned.
    truncated: bool,
}

#[derive(sqlx::FromRow)]
struct ConvRow {
    id: Uuid,
    source: String,
    channel_label: String,
    external_thread_id: String,
}

#[derive(sqlx::FromRow)]
struct MsgRow {
    id: Uuid,
    conversation_id: Uuid,
    direction: String,
    body: String,
    delivery_state: String,
    occurred_at: String,
}

pub async fn conversations(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
) -> Result<Json<ConversationPanel>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    ticket_exists(&mut tx, ws, ticket_id).await?;
    let convs: Vec<ConvRow> = sqlx::query_as(
        "SELECT id, source, channel_label, external_thread_id FROM conversations \
         WHERE workspace_id = $1 AND ticket_id = $2 ORDER BY created_at, id LIMIT $3",
    )
    .bind(ws)
    .bind(ticket_id)
    .bind(MAX_CONVERSATIONS)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<Uuid> = convs.iter().map(|c| c.id).collect();
    let mut msgs: Vec<MsgRow> = sqlx::query_as(concat!(
        "SELECT id, conversation_id, direction, body, delivery_state, ",
        utc!("occurred_at"),
        " AS occurred_at FROM messages WHERE workspace_id = $1 AND conversation_id = ANY($2) \
         ORDER BY occurred_at, id LIMIT $3"
    ))
    .bind(ws)
    .bind(&ids)
    .bind(MAX_MESSAGES + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let truncated = msgs.len() as i64 > MAX_MESSAGES;
    msgs.truncate(MAX_MESSAGES as usize);
    let conversations = convs
        .into_iter()
        .map(|c| ConversationView {
            messages: msgs
                .iter()
                .filter(|m| m.conversation_id == c.id)
                .map(|m| MessageView {
                    id: m.id,
                    direction: m.direction.clone(),
                    body: m.body.clone(),
                    delivery_state: m.delivery_state.clone(),
                    occurred_at: m.occurred_at.clone(),
                })
                .collect(),
            id: c.id,
            source: c.source,
            channel_label: c.channel_label,
            external_thread_id: c.external_thread_id,
        })
        .collect();
    Ok(Json(ConversationPanel {
        adapter_configured: false,
        conversations,
        truncated,
    }))
}
