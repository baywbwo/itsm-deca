//! D07 intake (O05): create one ticket UI → API → DB, idempotently.
//!
//! - The caller sends an `Idempotency-Key` (8–128 chars, per user and workspace). The server
//!   stores it with a SHA-256 fingerprint of the normalized request. The same key with the
//!   same payload returns the original ticket (`200`, `replayed: true`), and nothing is created
//!   twice even under concurrent submits (advisory lock + unique key). The same key with a
//!   different payload answers `409 idempotency_conflict`.
//! - Company = the workspace (read-only, never in the body). `receivedAt` is an RFC 3339
//!   instant (the UI sends Jakarta wall time with its offset). Subject, chronology and intake
//!   channel are required. Priority/category/product/unit/region/merchant are optional,
//!   scoped and must be active; no regulator-mandatory field is assumed (D01 freeze).
//! - PIC: optional. Yourself needs `ticket.assign_self`, anyone else `ticket.assign_any`;
//!   the DB guard requires an active member.
//! - Jira (none/create/link) and Lark (ticket-created notification) are SIMULATION
//!   connections only. The ticket, related work, outbox intents and audit row commit in one
//!   transaction; the simulated delivery runs afterwards and can never undo the ticket.
//!
//! Also here: the intake directory (simulation connections, dummy Jira assignees and Lark
//! destinations) and the assignment history read used by the PIC dialog.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::config::IntegrationsMode;
use crate::error::AppError;
use crate::live;
use crate::routes::sync::{self, SyncView};
use crate::routes::ticket_list::{self, TicketListItem, body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

const IDEMPOTENCY_HEADER: &str = "idempotency-key";
pub(crate) const MAX_SUBJECT_CHARS: usize = 300;
pub(crate) const MAX_CHRONOLOGY_CHARS: usize = 20_000;
const MAX_JIRA_TITLE_CHARS: usize = 300;
const MAX_ISSUE_KEY_CHARS: usize = 64;
/// Clock skew accepted for a receive time "now" typed on another device.
const FUTURE_SKEW: &str = "5 minutes";

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JiraMode {
    None,
    Create,
    Link,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JiraBody {
    mode: JiraMode,
    /// `create`: title of the new issue (required).
    title: Option<String>,
    /// `link`: key of an existing issue, validated against the (simulated) provider.
    issue_key: Option<String>,
    /// Dummy Jira assignee of this workspace's connection.
    assignee_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBody {
    subject: String,
    chronology: String,
    received_at: String,
    intake_channel_id: Option<Uuid>,
    priority_id: Option<Uuid>,
    category_id: Option<Uuid>,
    product_id: Option<Uuid>,
    receiving_unit_id: Option<Uuid>,
    region_id: Option<Uuid>,
    merchant_id: Option<Uuid>,
    owner_user_id: Option<Uuid>,
    jira: Option<JiraBody>,
    lark_destination_id: Option<Uuid>,
}

/// The normalized request; its JSON (fixed field order) is what the fingerprint hashes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Normalized {
    v: &'static str,
    subject: String,
    chronology: String,
    received_at: String,
    intake_channel_id: Uuid,
    priority_id: Option<Uuid>,
    category_id: Option<Uuid>,
    product_id: Option<Uuid>,
    receiving_unit_id: Option<Uuid>,
    region_id: Option<Uuid>,
    merchant_id: Option<Uuid>,
    owner_user_id: Option<Uuid>,
    jira_mode: JiraMode,
    jira_title: Option<String>,
    jira_issue_key: Option<String>,
    jira_assignee_id: Option<Uuid>,
    lark_destination_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    ticket: TicketListItem,
    /// `true` when this answer replays an earlier create with the same key and payload.
    replayed: bool,
    /// Jira/Lark delivery intents of this ticket (simulation) with their honest state.
    integrations: Vec<SyncView>,
}

pub(crate) fn required_text(
    raw: &str,
    max: usize,
    field: &'static str,
) -> Result<String, AppError> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(AppError::Validation(field));
    }
    Ok(t.to_owned())
}

fn optional_text(
    raw: Option<String>,
    max: usize,
    field: &'static str,
) -> Result<Option<String>, AppError> {
    raw.map(|s| required_text(&s, max, field)).transpose()
}

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let v = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::InvalidRequest)?;
    let ok = (8..=128).contains(&v.len())
        && v.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':' | b'.'));
    if ok {
        Ok(v.to_owned())
    } else {
        Err(AppError::InvalidRequest)
    }
}

/// Body checks that need no database. `received_at` is normalized later by PostgreSQL.
fn normalize(b: CreateBody, received_at: String) -> Result<Normalized, AppError> {
    let subject = required_text(&b.subject, MAX_SUBJECT_CHARS, "subject")?;
    let chronology = required_text(&b.chronology, MAX_CHRONOLOGY_CHARS, "chronology")?;
    let intake_channel_id = b
        .intake_channel_id
        .ok_or(AppError::Validation("intakeChannelId"))?;
    let (jira_mode, jira_title, jira_issue_key, jira_assignee_id) = match b.jira {
        None => (JiraMode::None, None, None, None),
        Some(j) => match j.mode {
            JiraMode::None => {
                if j.title.is_some() || j.issue_key.is_some() || j.assignee_id.is_some() {
                    return Err(AppError::Validation("jira.mode"));
                }
                (JiraMode::None, None, None, None)
            }
            JiraMode::Create => {
                if j.issue_key.is_some() {
                    return Err(AppError::Validation("jira.issueKey"));
                }
                let title = j.title.ok_or(AppError::Validation("jira.title"))?;
                let title = required_text(&title, MAX_JIRA_TITLE_CHARS, "jira.title")?;
                (JiraMode::Create, Some(title), None, j.assignee_id)
            }
            JiraMode::Link => {
                let key = j.issue_key.ok_or(AppError::Validation("jira.issueKey"))?;
                let key = required_text(&key, MAX_ISSUE_KEY_CHARS, "jira.issueKey")?;
                let title = optional_text(j.title, MAX_JIRA_TITLE_CHARS, "jira.title")?;
                (JiraMode::Link, title, Some(key), j.assignee_id)
            }
        },
    };
    Ok(Normalized {
        v: "d07-create-v1",
        subject,
        chronology,
        received_at,
        intake_channel_id,
        priority_id: b.priority_id,
        category_id: b.category_id,
        product_id: b.product_id,
        receiving_unit_id: b.receiving_unit_id,
        region_id: b.region_id,
        merchant_id: b.merchant_id,
        owner_user_id: b.owner_user_id,
        jira_mode,
        jira_title,
        jira_issue_key,
        jira_assignee_id,
        lark_destination_id: b.lark_destination_id,
    })
}

fn fingerprint(n: &Normalized) -> Result<Vec<u8>, AppError> {
    let bytes = serde_json::to_vec(n).map_err(AppError::internal)?;
    Ok(Sha256::digest(&bytes).to_vec())
}

/// Normalizes an RFC 3339 instant to UTC (`…Z`) and reports whether it lies in the future
/// beyond the accepted skew. Runs outside the create transaction: a cast error must not
/// abort it. Malformed or impossible instants are a field error, never a 500.
async fn received_instant(state: &AppState, raw: &str) -> Result<(String, bool), AppError> {
    let shaped = ticket_list::instant(Some(raw))
        .ok()
        .flatten()
        .ok_or(AppError::Validation("receivedAt"))?;
    let sql = format!(
        "SELECT to_char(($1::timestamptz) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
                ($1::timestamptz) > now() + interval '{FUTURE_SKEW}' \
                OR ($1::timestamptz) < timestamptz '2000-01-01T00:00:00Z'"
    );
    match sqlx::query_as::<_, (String, bool)>(&sql)
        .bind(&shaped)
        .fetch_one(&state.pool)
        .await
    {
        Ok(v) => Ok(v),
        Err(sqlx::Error::Database(db)) if db.code().is_some_and(|c| c.starts_with("22")) => {
            Err(AppError::Validation("receivedAt"))
        }
        Err(e) => Err(e.into()),
    }
}

/// `true` when the row exists in this workspace and is active.
async fn active_in(
    conn: &mut PgConnection,
    table: &'static str,
    ws: Uuid,
    id: Uuid,
) -> Result<bool, AppError> {
    // Table names are compile-time constants, never request input.
    let sql = format!("SELECT active FROM {table} WHERE workspace_id = $1 AND id = $2");
    Ok(sqlx::query_scalar::<_, bool>(&sql)
        .bind(ws)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .unwrap_or(false))
}

pub(crate) async fn require_active(
    conn: &mut PgConnection,
    table: &'static str,
    ws: Uuid,
    id: Option<Uuid>,
    field: &'static str,
) -> Result<(), AppError> {
    match id {
        Some(id) if !active_in(conn, table, ws, id).await? => Err(AppError::Validation(field)),
        _ => Ok(()),
    }
}

/// The workspace's connection for a provider that is active in the server mode `mode`, if any
/// (D09: save → test → activate; draft, tested and disabled connections receive nothing).
pub(crate) async fn connection(
    conn: &mut PgConnection,
    ws: Uuid,
    provider: &str,
    mode: &str,
) -> Result<Option<Uuid>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM integration_connections \
         WHERE workspace_id = $1 AND provider = $2 AND mode = $3 AND state = 'active' \
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(ws)
    .bind(provider)
    .bind(mode)
    .fetch_optional(&mut *conn)
    .await?)
}

/// PIC at intake: none, yourself (`ticket.assign_self`) or anyone (`ticket.assign_any`).
fn check_owner(access: &WorkspaceAccess, owner: Option<Uuid>) -> Result<(), AppError> {
    match owner {
        None => Ok(()),
        Some(u) if u == access.user_id && access.allows(Permission::TicketAssignSelf) => Ok(()),
        Some(_) if access.allows(Permission::TicketAssignAny) => Ok(()),
        Some(_) => {
            tracing::info!(workspace_id = %access.workspace_id, "intake owner denied");
            crate::denied_audit::record_permission(Permission::TicketAssignAny.code());
            Err(AppError::Forbidden)
        }
    }
}

async fn existing(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    key: &str,
) -> Result<Option<(Uuid, Option<Vec<u8>>)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT id, create_request_hash FROM tickets \
         WHERE workspace_id = $1 AND created_by = $2 AND create_idempotency_key = $3",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .bind(key)
    .fetch_optional(&mut *conn)
    .await?)
}

pub async fn create(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw_ws): Path<String>,
    payload: Result<Json<CreateBody>, JsonRejection>,
) -> Result<(StatusCode, Json<CreateResult>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketCreate)?;
    let key = idempotency_key(&headers)?;
    let b = body(payload)?;
    let (received_at, out_of_range) = received_instant(&state, &b.received_at).await?;
    let n = normalize(b, received_at)?;
    let hash = fingerprint(&n)?;
    let ws = access.workspace_id;
    let corr = correlation(&headers);

    let mut tx = state.pool.begin().await?;
    // Serializes concurrent submits of the same key; the unique key is the backstop.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("ticket-create:{ws}:{}:{key}", access.user_id))
        .execute(&mut *tx)
        .await?;
    if let Some((id, stored)) = existing(&mut tx, &access, &key).await? {
        if stored.as_deref() != Some(hash.as_slice()) {
            tracing::info!(workspace_id = %ws, "idempotency key reused with another payload");
            return Err(AppError::IdempotencyConflict);
        }
        let ticket = ticket_list::reload(&mut tx, &access, id).await?;
        let integrations = sync::for_ticket(&mut tx, ws, id).await?;
        tx.commit().await?;
        tracing::info!(workspace_id = %ws, ticket_id = %id, "ticket create replayed");
        return Ok((
            StatusCode::OK,
            Json(CreateResult {
                ticket,
                replayed: true,
                integrations,
            }),
        ));
    }

    // New create: every reference is checked against this workspace, then written at once.
    if out_of_range {
        return Err(AppError::Validation("receivedAt"));
    }
    check_owner(&access, n.owner_user_id)?;
    require_active(
        &mut tx,
        "intake_channels",
        ws,
        Some(n.intake_channel_id),
        "intakeChannelId",
    )
    .await?;
    require_active(&mut tx, "priorities", ws, n.priority_id, "priorityId").await?;
    require_active(&mut tx, "categories", ws, n.category_id, "categoryId").await?;
    require_active(&mut tx, "products", ws, n.product_id, "productId").await?;
    require_active(
        &mut tx,
        "receiving_units",
        ws,
        n.receiving_unit_id,
        "receivingUnitId",
    )
    .await?;
    require_active(&mut tx, "regions", ws, n.region_id, "regionId").await?;
    if let Some(m) = n.merchant_id {
        let dummy: Option<bool> = sqlx::query_scalar(
            "SELECT source = 'dummy' FROM merchants WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(m)
        .fetch_optional(&mut *tx)
        .await?;
        if dummy != Some(true) {
            return Err(AppError::Validation("merchantId"));
        }
    }
    if let Some(owner) = n.owner_user_id {
        let active: Option<bool> = sqlx::query_scalar(
            "SELECT m.active AND u.active FROM workspace_memberships m JOIN users u ON u.id = m.user_id \
             WHERE m.workspace_id = $1 AND m.user_id = $2",
        )
        .bind(ws)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        if active != Some(true) {
            return Err(AppError::Validation("ownerUserId"));
        }
    }

    // Jira: connection, assignee and (link) the existing issue, all in this workspace. Only a
    // connection active in the server's mode is used; live mode never takes a dummy entry.
    let mode = state.config.integrations.mode.as_str();
    let live_mode = mode == "live";
    let jira_conn = match n.jira_mode {
        JiraMode::None => None,
        _ => Some(
            connection(&mut tx, ws, "jira", mode)
                .await?
                .ok_or(AppError::Validation("jira.mode"))?,
        ),
    };
    if let (Some(conn), Some(a)) = (jira_conn, n.jira_assignee_id) {
        let ok: Option<bool> = sqlx::query_scalar(
            "SELECT active AND ($4 = 'simulation' OR source <> 'dummy') FROM jira_assignees \
             WHERE workspace_id = $1 AND connection_id = $2 AND id = $3",
        )
        .bind(ws)
        .bind(conn)
        .bind(a)
        .bind(mode)
        .fetch_optional(&mut *tx)
        .await?;
        if ok != Some(true) {
            return Err(AppError::Validation("jira.assigneeId"));
        }
    }
    // Live: the key is checked at Jira by the delivery (an issue linked before is reused).
    // Simulation: only an issue already known locally can be linked.
    let requested_key: Option<String> = match (&n.jira_issue_key, live_mode) {
        (Some(k), true) => {
            let k = k.trim().to_ascii_uppercase();
            if !jira_key(&k) {
                return Err(AppError::Validation("jira.issueKey"));
            }
            Some(k)
        }
        _ => None,
    };
    let linked_issue: Option<Uuid> = match (jira_conn, &n.jira_issue_key) {
        (Some(conn), Some(k)) => {
            let key = requested_key.clone().unwrap_or_else(|| k.clone());
            let found: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM jira_issues WHERE workspace_id = $1 AND connection_id = $2 \
                 AND issue_key = $3",
            )
            .bind(ws)
            .bind(conn)
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
            if found.is_none() && !live_mode {
                return Err(AppError::Validation("jira.issueKey"));
            }
            found
        }
        _ => None,
    };
    let lark = match n.lark_destination_id {
        None => None,
        Some(d) => {
            let conn: Option<Uuid> = sqlx::query_scalar(
                "SELECT d.connection_id FROM lark_destinations d \
                 JOIN integration_connections c ON c.workspace_id = d.workspace_id AND c.id = d.connection_id \
                 WHERE d.workspace_id = $1 AND d.id = $2 AND d.active AND c.state = 'active' \
                   AND c.mode = $3 AND ($3 = 'simulation' OR d.source <> 'dummy')",
            )
            .bind(ws)
            .bind(d)
            .bind(mode)
            .fetch_optional(&mut *tx)
            .await?;
            Some((conn.ok_or(AppError::Validation("larkDestinationId"))?, d))
        }
    };

    let (ticket_id, number): (Uuid, i64) = sqlx::query_as(
        "INSERT INTO tickets (workspace_id, number, subject, chronology, received_at, \
           intake_channel_id, priority_id, category_id, product_id, receiving_unit_id, region_id, \
           merchant_id, owner_user_id, created_by, create_idempotency_key, create_request_hash) \
         VALUES ($1, 1, $2, $3, $4::timestamptz, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
         RETURNING id, number",
    )
    .bind(ws)
    .bind(&n.subject)
    .bind(&n.chronology)
    .bind(&n.received_at)
    .bind(n.intake_channel_id)
    .bind(n.priority_id)
    .bind(n.category_id)
    .bind(n.product_id)
    .bind(n.receiving_unit_id)
    .bind(n.region_id)
    .bind(n.merchant_id)
    .bind(n.owner_user_id)
    .bind(access.user_id)
    .bind(&key)
    .bind(&hash)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(conn) = jira_conn {
        let title = match (&n.jira_title, &n.jira_issue_key) {
            (Some(t), _) => t.clone(),
            (None, Some(k)) => format!("Issue Jira {k}"),
            (None, None) => return Err(AppError::Validation("jira.title")),
        };
        let related: Uuid = sqlx::query_scalar(
            "INSERT INTO related_work (workspace_id, ticket_id, title, requested_by, jira_issue_id, \
               jira_assignee_id) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(ws)
        .bind(ticket_id)
        .bind(&title)
        .bind(access.user_id)
        .bind(linked_issue)
        .bind(n.jira_assignee_id)
        .fetch_one(&mut *tx)
        .await?;
        let op = if n.jira_mode == JiraMode::Create {
            "jira_create_issue"
        } else {
            "jira_link_issue"
        };
        sync::enqueue(
            &mut tx,
            ws,
            ticket_id,
            conn,
            "jira",
            op,
            Some(related),
            None,
            requested_key.as_deref(),
            &corr,
        )
        .await?;
    }
    if let Some((conn, dest)) = lark {
        sync::enqueue(
            &mut tx,
            ws,
            ticket_id,
            conn,
            "lark",
            "lark_ticket_created",
            None,
            Some(dest),
            None,
            &corr,
        )
        .await?;
    }
    // Ids and flags only: subject, chronology and merchant data stay out of the audit row.
    ticket_list::audit(
        &mut tx,
        &access,
        "ticket.create",
        ticket_id,
        serde_json::json!({
            "number": number,
            "kind": "initial",
            "to": n.owner_user_id,
            "intakeChannelId": n.intake_channel_id,
            "priorityId": n.priority_id,
            "categoryId": n.category_id,
            "merchant": n.merchant_id.is_some(),
            "jira": n.jira_mode,
            "lark": n.lark_destination_id.is_some(),
            "version": 1,
        }),
        corr.clone(),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, number, "ticket created");

    // Simulated delivery after commit: a failure here leaves honest outbox states and never
    // touches the committed ticket.
    sync::dispatch_ticket(&state, ws, ticket_id, access.user_id, corr).await;

    let mut conn = state.pool.acquire().await?;
    let ticket = ticket_list::reload(&mut conn, &access, ticket_id).await?;
    let integrations = sync::for_ticket(&mut conn, ws, ticket_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateResult {
            ticket,
            replayed: false,
            integrations,
        }),
    ))
}

// ---------- intake directory ----------

/// Jira issue key as Jira issues it (`PROJ-123`).
pub(crate) fn jira_key(k: &str) -> bool {
    let Some((project, number)) = k.split_once('-') else {
        return false;
    };
    (1..=31).contains(&project.len())
        && project.starts_with(|c: char| c.is_ascii_uppercase())
        && project
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        && (1..=12).contains(&number.len())
        && number.bytes().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    id: Uuid,
    display_name: String,
    /// `jira` / `lark` (read from the provider) or `dummy` (simulation only).
    source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    connection_id: Uuid,
    display_name: String,
    /// Mode the connection was configured in: `live` | `simulation`.
    mode: String,
    state: String,
    /// Outcome the simulated delivery will report (`success`/`failed`/`unknown`); simulation only.
    simulated_outcome: String,
    version: i32,
    /// Usable now: active and configured for the server mode.
    usable: bool,
    /// Jira: assignees picked before (search Jira for more). Lark: groups the bot belongs to.
    options: Vec<DirectoryEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeDirectory {
    workspace_id: Uuid,
    /// Server switch `ITSM_INTEGRATIONS`: `live` | `simulation`.
    integrations_mode: &'static str,
    /// `null` = not configured for this workspace (the UI says so; nothing is sent).
    jira: Option<ProviderView>,
    lark: Option<ProviderView>,
    merchants: Vec<MerchantView>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MerchantView {
    id: Uuid,
    display_name: String,
    /// Dummy reference, not a CIF lookup.
    reference: String,
    source: String,
}

async fn provider(
    conn: &mut PgConnection,
    ws: Uuid,
    provider: &'static str,
    server_mode: &str,
) -> Result<Option<ProviderView>, AppError> {
    let row: Option<(Uuid, String, String, String, String, i32)> = sqlx::query_as(
        "SELECT id, display_name, mode, state, \
                COALESCE(config->>'simulatedOutcome', 'success'), version \
         FROM integration_connections WHERE workspace_id = $1 AND provider = $2 \
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(ws)
    .bind(provider)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((id, display_name, mode, state, simulated_outcome, version)) = row else {
        return Ok(None);
    };
    let sql = if provider == "jira" {
        "SELECT id, display_name, source FROM jira_assignees \
         WHERE workspace_id = $1 AND connection_id = $2 AND active \
           AND ($3 = 'simulation' OR source <> 'dummy') ORDER BY display_name, id LIMIT 200"
    } else {
        "SELECT id, display_name, source FROM lark_destinations \
         WHERE workspace_id = $1 AND connection_id = $2 AND active \
           AND ($3 = 'simulation' OR source <> 'dummy') ORDER BY display_name, id LIMIT 500"
    };
    let usable = state == "active" && mode == server_mode;
    let options = if !usable {
        Vec::new()
    } else {
        sqlx::query_as::<_, DirectoryEntry>(sql)
            .bind(ws)
            .bind(id)
            .bind(server_mode)
            .fetch_all(&mut *conn)
            .await?
    };
    Ok(Some(ProviderView {
        connection_id: id,
        display_name,
        mode,
        state,
        simulated_outcome,
        version,
        usable,
        options,
    }))
}

pub async fn directory(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
) -> Result<Json<IntakeDirectory>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketCreate)?;
    let ws = access.workspace_id;
    let mode = state.config.integrations.mode.as_str();
    let mut conn = state.pool.acquire().await?;
    let merchants = sqlx::query_as::<_, MerchantView>(
        "SELECT id, display_name, cif_reference AS reference, source FROM merchants \
         WHERE workspace_id = $1 AND source = 'dummy' ORDER BY display_name, id",
    )
    .bind(ws)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(IntakeDirectory {
        workspace_id: ws,
        integrations_mode: mode,
        jira: provider(&mut conn, ws, "jira", mode).await?,
        lark: provider(&mut conn, ws, "lark", mode).await?,
        merchants,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuery {
    query: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraUsers {
    items: Vec<DirectoryEntry>,
}

/// Searches Jira users assignable in the configured project (live only). Matches are remembered
/// locally (account id + display name) so intake can reference them by id.
pub async fn jira_users(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
    q: Result<Query<UserQuery>, QueryRejection>,
) -> Result<Json<JiraUsers>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketCreate)?;
    if state.config.integrations.mode != IntegrationsMode::Live {
        return Err(AppError::IntegrationNotLive);
    }
    let Query(q) = q.map_err(|_| AppError::InvalidRequest)?;
    let query: String = q.query.unwrap_or_default().trim().to_owned();
    let len = query.chars().count();
    if !(2..=100).contains(&len) || query.chars().any(char::is_control) {
        return Err(AppError::Validation("query"));
    }
    let ws = access.workspace_id;
    let id = {
        let mut conn = state.pool.acquire().await?;
        connection(&mut conn, ws, "jira", "live")
            .await?
            .ok_or(AppError::Conflict("integration_not_configured"))?
    };
    let conn = live::load_conn(&state, ws, id).await?;
    let users = live::jira_users(&state, ws, &conn, &query)
        .await
        .map_err(live::Failure::into_app_error)?;
    let mut tx = state.pool.begin().await?;
    let mut items = Vec::with_capacity(users.len());
    for u in users {
        let account = u.account_id.trim();
        if account.is_empty() || account.len() > 128 {
            continue;
        }
        let name: String = u
            .display_name
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        let name = if name.trim().is_empty() {
            account.to_owned()
        } else {
            name
        };
        let entry = sqlx::query_as::<_, DirectoryEntry>(
            "INSERT INTO jira_assignees (workspace_id, connection_id, external_account_ref, display_name, source) \
             VALUES ($1, $2, $3, $4, 'jira') \
             ON CONFLICT (connection_id, external_account_ref) \
             DO UPDATE SET display_name = EXCLUDED.display_name, source = 'jira', active = true \
             RETURNING id, display_name, source",
        )
        .bind(ws)
        .bind(id)
        .bind(account)
        .bind(&name)
        .fetch_one(&mut *tx)
        .await?;
        items.push(entry);
    }
    tx.commit().await?;
    Ok(Json(JiraUsers { items }))
}

// ---------- assignment history ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonRef {
    id: Uuid,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentEvent {
    id: Uuid,
    at: String,
    /// `initial` (PIC at intake), `take_for_self` or `reassign`.
    kind: String,
    actor: Option<PersonRef>,
    from: Option<PersonRef>,
    to: Option<PersonRef>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentHistory {
    ticket_id: Uuid,
    items: Vec<AssignmentEvent>,
}

#[derive(sqlx::FromRow)]
struct HistoryRow {
    id: Uuid,
    at: String,
    kind: Option<String>,
    actor_id: Option<Uuid>,
    actor_name: Option<String>,
    from_id: Option<Uuid>,
    from_name: Option<String>,
    to_id: Option<Uuid>,
    to_name: Option<String>,
    reason: Option<String>,
}

fn person(id: Option<Uuid>, name: Option<String>) -> Option<PersonRef> {
    Some(PersonRef {
        id: id?,
        label: name.unwrap_or_else(|| "Pengguna tidak dikenal".to_owned()),
    })
}

pub async fn assignment_history(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
) -> Result<Json<AssignmentHistory>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let ws = access.workspace_id;
    let mut conn = state.pool.acquire().await?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tickets WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(ticket_id)
            .fetch_optional(&mut *conn)
            .await?;
    if found.is_none() {
        return Err(AppError::NotFound);
    }
    // `from`/`to` are UUIDs written by this API; anything else reads as no person.
    let rows = sqlx::query_as::<_, HistoryRow>(concat!(
        "SELECT a.id, ",
        "to_char(a.occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS at, ",
        "a.safe_change->>'kind' AS kind, a.actor_id, ua.display_name AS actor_name, ",
        "uf.id AS from_id, uf.display_name AS from_name, ut.id AS to_id, ut.display_name AS to_name, ",
        "a.safe_change->>'reason' AS reason ",
        "FROM audit_events a ",
        "LEFT JOIN users ua ON ua.id = a.actor_id ",
        "LEFT JOIN users uf ON uf.id::text = a.safe_change->>'from' ",
        "LEFT JOIN users ut ON ut.id::text = a.safe_change->>'to' ",
        "WHERE a.workspace_id = $1 AND a.entity_type = 'ticket' AND a.entity_id = $2 ",
        "AND a.outcome = 'success' AND a.action IN ('ticket.create', 'ticket.assign') ",
        "ORDER BY a.occurred_at DESC, a.id DESC LIMIT 100"
    ))
    .bind(ws)
    .bind(ticket_id)
    .fetch_all(&mut *conn)
    .await?;
    let items = rows
        .into_iter()
        .map(|r| AssignmentEvent {
            id: r.id,
            at: r.at,
            kind: r.kind.unwrap_or_else(|| "reassign".to_owned()),
            actor: person(r.actor_id, r.actor_name),
            from: person(r.from_id, r.from_name),
            to: person(r.to_id, r.to_name),
            reason: r.reason,
        })
        .collect();
    Ok(Json(AssignmentHistory { ticket_id, items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CreateBody {
        CreateBody {
            subject: "  Judul uji  ".into(),
            chronology: "Kronologi uji".into(),
            received_at: "2026-11-01T10:00:00+07:00".into(),
            intake_channel_id: Some(Uuid::from_u128(1)),
            priority_id: None,
            category_id: None,
            product_id: None,
            receiving_unit_id: None,
            region_id: None,
            merchant_id: None,
            owner_user_id: None,
            jira: None,
            lark_destination_id: None,
        }
    }

    fn field(r: Result<Normalized, AppError>) -> &'static str {
        match r {
            Err(AppError::Validation(f)) => f,
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn normalization_trims_and_fingerprint_is_stable() {
        let a = normalize(base(), "x".into()).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(a.subject, "Judul uji");
        let mut b = base();
        b.subject = "Judul uji".into();
        let b = normalize(b, "x".into()).unwrap_or_else(|e| panic!("{e:?}"));
        let (fa, fb) = (fingerprint(&a), fingerprint(&b));
        assert_eq!(fa.ok(), fb.ok());
        let mut c = base();
        c.chronology = "Kronologi lain".into();
        let c = normalize(c, "x".into()).unwrap_or_else(|e| panic!("{e:?}"));
        assert_ne!(fingerprint(&a).ok(), fingerprint(&c).ok());
    }

    #[test]
    fn required_fields_are_named() {
        let mut b = base();
        b.subject = "   ".into();
        assert_eq!(field(normalize(b, "x".into())), "subject");
        let mut b = base();
        b.chronology = String::new();
        assert_eq!(field(normalize(b, "x".into())), "chronology");
        let mut b = base();
        b.intake_channel_id = None;
        assert_eq!(field(normalize(b, "x".into())), "intakeChannelId");
        let mut b = base();
        b.subject = "x".repeat(MAX_SUBJECT_CHARS + 1);
        assert_eq!(field(normalize(b, "x".into())), "subject");
    }

    #[test]
    fn jira_modes_require_their_own_fields() {
        let jira = |mode, title: Option<&str>, key: Option<&str>| JiraBody {
            mode,
            title: title.map(str::to_owned),
            issue_key: key.map(str::to_owned),
            assignee_id: None,
        };
        let with = |j| {
            let mut b = base();
            b.jira = Some(j);
            normalize(b, "x".into())
        };
        assert_eq!(
            field(with(jira(JiraMode::Create, None, None))),
            "jira.title"
        );
        assert_eq!(
            field(with(jira(JiraMode::Create, Some("t"), Some("K-1")))),
            "jira.issueKey"
        );
        assert_eq!(
            field(with(jira(JiraMode::Link, Some("t"), None))),
            "jira.issueKey"
        );
        assert_eq!(
            field(with(jira(JiraMode::None, Some("t"), None))),
            "jira.mode"
        );
        assert!(with(jira(JiraMode::Link, None, Some(" SIM-1 "))).is_ok());
    }

    #[test]
    fn idempotency_key_header_is_bounded() {
        let mut h = HeaderMap::new();
        assert!(idempotency_key(&h).is_err());
        for bad in ["short", "has space here", &"k".repeat(129)] {
            h.insert(
                IDEMPOTENCY_HEADER,
                bad.parse().unwrap_or_else(|e| panic!("{e}")),
            );
            assert!(idempotency_key(&h).is_err(), "{bad}");
        }
        h.insert(
            IDEMPOTENCY_HEADER,
            "intake-0b5f3c1e".parse().unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(idempotency_key(&h).is_ok());
    }
}
