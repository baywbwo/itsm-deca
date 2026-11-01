//! D07 Jira/Lark delivery — SIMULATION only (no real provider, no credentials).
//!
//! Intake writes one outbox intent per target inside the ticket transaction. After commit the
//! simulated delivery settles each intent to the outcome configured on its connection
//! (`config.simulatedOutcome`: `success` | `failed` | `unknown` | `transient`, default `success`;
//! claim/settle/backoff live in [`crate::outbox`]):
//! - `success`: Jira create gets a simulated issue (`SIM-…`, no URL) linked to related work;
//!   Jira link and Lark notifications are marked delivered.
//! - `failed`: definite failure with a safe message; `integration.recovery` may retry.
//! - `unknown`: the outcome cannot be known (simulated timeout). It is never retried blindly;
//!   recovery records a reconciliation with evidence instead.
//!
//! A delivery that does not settle in time stays `pending`; it is never reported as success.

use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::config::Environment;
use crate::error::AppError;
use crate::outbox;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

/// Upper bound for the simulated delivery of one ticket inside the create request.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_EVIDENCE_CHARS: usize = 2000;

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SyncView {
    id: Uuid,
    provider: String,
    operation: String,
    /// `pending` | `success` | `failed` | `unknown`.
    state: String,
    attempt: i32,
    /// Dummy assignee / destination / issue label shown to the user.
    target_label: Option<String>,
    issue_key: Option<String>,
    safe_error: Option<String>,
    reconciled: bool,
    /// Transient failure: next automatic attempt (UTC), otherwise null.
    next_attempt_at: Option<String>,
    /// Automatic attempts used since the last manual retry (bounded by the worker).
    auto_attempts: i32,
    /// A delivery attempt is in flight (claimed, not settled yet).
    in_flight: bool,
    updated_at: String,
    /// Always `simulation` in this build.
    mode: String,
}

const VIEW_SQL: &str = concat!(
    "SELECT o.id, o.provider, o.operation, o.state, o.attempt, ",
    "COALESCE(d.display_name, a.display_name) AS target_label, ",
    "ji.issue_key, o.safe_error, (o.reconcile_evidence IS NOT NULL) AS reconciled, ",
    "to_char(o.next_attempt_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS next_attempt_at, ",
    "o.auto_attempts, (o.claim_token IS NOT NULL) AS in_flight, ",
    "to_char(o.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS updated_at, ",
    "c.mode ",
    "FROM integration_outbox o ",
    "JOIN integration_connections c ON c.workspace_id = o.workspace_id AND c.id = o.connection_id ",
    "LEFT JOIN lark_destinations d ON d.workspace_id = o.workspace_id AND d.id = o.lark_destination_id ",
    "LEFT JOIN related_work r ON r.workspace_id = o.workspace_id AND r.id = o.related_work_id ",
    "LEFT JOIN jira_assignees a ON a.workspace_id = r.workspace_id AND a.id = r.jira_assignee_id ",
    "LEFT JOIN jira_issues ji ON ji.workspace_id = r.workspace_id AND ji.id = r.jira_issue_id ",
);

pub(crate) async fn for_ticket(
    conn: &mut PgConnection,
    ws: Uuid,
    ticket: Uuid,
) -> Result<Vec<SyncView>, AppError> {
    let sql = format!(
        "{VIEW_SQL} WHERE o.workspace_id = $1 AND o.ticket_id = $2 ORDER BY o.provider, o.created_at, o.id"
    );
    Ok(sqlx::query_as::<_, SyncView>(&sql)
        .bind(ws)
        .bind(ticket)
        .fetch_all(&mut *conn)
        .await?)
}

async fn one(conn: &mut PgConnection, ws: Uuid, id: Uuid) -> Result<SyncView, AppError> {
    let sql = format!("{VIEW_SQL} WHERE o.workspace_id = $1 AND o.id = $2");
    Ok(sqlx::query_as::<_, SyncView>(&sql)
        .bind(ws)
        .bind(id)
        .fetch_one(&mut *conn)
        .await?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn enqueue(
    conn: &mut PgConnection,
    ws: Uuid,
    ticket: Uuid,
    connection: Uuid,
    provider: &str,
    operation: &str,
    related_work: Option<Uuid>,
    lark_destination: Option<Uuid>,
    corr: &Option<String>,
) -> Result<Uuid, AppError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, \
           related_work_id, lark_destination_id, idempotency_key, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $2::text || ':' || $4, $8) RETURNING id",
    )
    .bind(ws)
    .bind(ticket)
    .bind(connection)
    .bind(provider)
    .bind(operation)
    .bind(related_work)
    .bind(lark_destination)
    .bind(corr)
    .fetch_one(&mut *conn)
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn audit_entity(
    conn: &mut PgConnection,
    ws: Uuid,
    actor: Option<Uuid>,
    action: &str,
    entity_type: &str,
    entity: Uuid,
    change: serde_json::Value,
    corr: &Option<String>,
) -> Result<Uuid, AppError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_events (workspace_id, actor_id, action, entity_type, entity_id, \
           safe_change, outcome, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, 'success', $7) RETURNING id",
    )
    .bind(ws)
    .bind(actor)
    .bind(action)
    .bind(entity_type)
    .bind(entity)
    .bind(change)
    .bind(corr)
    .fetch_one(&mut *conn)
    .await?)
}

#[derive(sqlx::FromRow)]
pub(crate) struct Intent {
    pub(crate) ticket_id: Uuid,
    pub(crate) connection_id: Uuid,
    pub(crate) operation: String,
    pub(crate) related_work_id: Option<Uuid>,
    pub(crate) state: String,
    pub(crate) attempt: i32,
    pub(crate) auto_attempts: i32,
    pub(crate) claim_token: Option<Uuid>,
    pub(crate) outcome: String,
    pub(crate) ws_code: String,
    pub(crate) number: i64,
}

pub(crate) async fn lock_intent(
    conn: &mut PgConnection,
    ws: Uuid,
    id: Uuid,
) -> Result<Intent, AppError> {
    sqlx::query_as::<_, Intent>(
        "SELECT o.ticket_id, o.connection_id, o.operation, o.related_work_id, o.state, o.attempt, \
                o.auto_attempts, o.claim_token, COALESCE(c.config->>'simulatedOutcome', 'success') AS outcome, w.code AS ws_code, t.number \
         FROM integration_outbox o \
         JOIN integration_connections c ON c.workspace_id = o.workspace_id AND c.id = o.connection_id \
         JOIN workspaces w ON w.id = o.workspace_id \
         JOIN tickets t ON t.workspace_id = o.workspace_id AND t.id = o.ticket_id \
         WHERE o.workspace_id = $1 AND o.id = $2 FOR UPDATE OF o",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::NotFound)
}

/// Links a (simulated or reconciled) Jira issue to the intent's related work.
pub(crate) async fn link_issue(
    conn: &mut PgConnection,
    ws: Uuid,
    intent: &Intent,
    external_id: String,
    key: &str,
) -> Result<(), AppError> {
    let issue: Uuid = sqlx::query_scalar(
        "INSERT INTO jira_issues (workspace_id, connection_id, external_issue_id, issue_key, external_status) \
         VALUES ($1, $2, $3, $4, 'Simulasi') \
         ON CONFLICT (connection_id, external_issue_id) DO UPDATE SET issue_key = EXCLUDED.issue_key \
         RETURNING id",
    )
    .bind(ws)
    .bind(intent.connection_id)
    .bind(external_id)
    .bind(key)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE related_work SET jira_issue_id = $3, version = version + 1, updated_at = now() \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(intent.related_work_id)
    .bind(issue)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Claims and settles one intent on behalf of a request. No-op when it is not pending, already
/// in flight or not due yet (another claimer owns it).
async fn deliver(
    state: &AppState,
    ws: Uuid,
    id: Uuid,
    actor: Uuid,
    corr: &Option<String>,
) -> Result<(), AppError> {
    let claim = {
        let mut conn = state.pool.acquire().await?;
        outbox::claim_one(&mut conn, ws, id).await?
    };
    if let Some(claim) = claim {
        outbox::settle(state, claim, Some(actor), corr).await?;
    }
    Ok(())
}

/// Delivers every pending intent of a ticket. Runs on its own task so a dropped request
/// cannot cut it halfway; the caller waits at most [`DISPATCH_TIMEOUT`] and then reads
/// whatever state is committed (still `pending` if not settled).
pub(crate) async fn dispatch_ticket(
    state: &AppState,
    ws: Uuid,
    ticket: Uuid,
    actor: Uuid,
    corr: Option<String>,
) {
    let st = state.clone();
    let task = tokio::spawn(async move {
        let ids: Vec<Uuid> = match sqlx::query_scalar(
            "SELECT id FROM integration_outbox WHERE workspace_id = $1 AND ticket_id = $2 \
             AND state = 'pending' ORDER BY created_at, id",
        )
        .bind(ws)
        .bind(ticket)
        .fetch_all(&st.pool)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error.kind = "dispatch_read", error = %e, "simulated dispatch skipped");
                return;
            }
        };
        for id in ids {
            if let Err(e) = deliver(&st, ws, id, actor, &corr).await {
                tracing::warn!(outbox_id = %id, error = ?e, "simulated delivery left pending");
            }
        }
    });
    if tokio::time::timeout(DISPATCH_TIMEOUT, task).await.is_err() {
        tracing::warn!(workspace_id = %ws, ticket_id = %ticket, "simulated dispatch still running");
    }
}

// ---------- reads ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketSync {
    ticket_id: Uuid,
    items: Vec<SyncView>,
    /// Caller holds `integration.recovery` (retry failed, reconcile unknown).
    can_recover: bool,
}

pub async fn ticket_sync(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
) -> Result<Json<TicketSync>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let mut conn = state.pool.acquire().await?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tickets WHERE workspace_id = $1 AND id = $2")
            .bind(access.workspace_id)
            .bind(ticket_id)
            .fetch_optional(&mut *conn)
            .await?;
    if found.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(Json(TicketSync {
        ticket_id,
        items: for_ticket(&mut conn, access.workspace_id, ticket_id).await?,
        can_recover: access.allows(Permission::IntegrationRecovery),
    }))
}

// ---------- recovery (integration.recovery) ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryBody {
    /// Attempt number the caller saw; a different one means someone else acted first.
    attempt: i32,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    Success,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileBody {
    attempt: i32,
    outcome: ReconcileOutcome,
    /// What the officer checked at the provider (safe text, no secrets).
    evidence: String,
    /// Jira create confirmed as delivered: the key found at the provider.
    issue_key: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryResult {
    item: SyncView,
    audit_event_id: Uuid,
}

async fn recovery_access(
    state: &AppState,
    s: &Session,
    raw_ws: &str,
) -> Result<WorkspaceAccess, AppError> {
    let access = authz::resolve(state, s, parse_id(raw_ws)?).await?;
    access.require(Permission::IntegrationRecovery)?;
    Ok(access)
}

pub async fn retry(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<RetryBody>, JsonRejection>,
) -> Result<Json<RecoveryResult>, AppError> {
    let access = recovery_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let ws = access.workspace_id;
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let intent = lock_intent(&mut tx, ws, id).await?;
    if intent.attempt != b.attempt {
        return Err(AppError::VersionConflict);
    }
    // Only a definite failure is retried; unknown must be reconciled, success is final.
    if intent.state != "failed" {
        return Err(AppError::RuleViolation);
    }
    // A manual retry starts a fresh automatic budget.
    sqlx::query(
        "UPDATE integration_outbox SET state = 'pending', safe_error = NULL, auto_attempts = 0, \
         next_attempt_at = NULL WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.retry",
        "integration_outbox",
        id,
        serde_json::json!({ "ticketId": intent.ticket_id, "from": "failed", "attempt": intent.attempt }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    if let Err(e) = deliver(&state, ws, id, access.user_id, &corr).await {
        tracing::warn!(outbox_id = %id, error = ?e, "retry left pending");
    }
    let mut conn = state.pool.acquire().await?;
    Ok(Json(RecoveryResult {
        item: one(&mut conn, ws, id).await?,
        audit_event_id,
    }))
}

pub async fn reconcile(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<ReconcileBody>, JsonRejection>,
) -> Result<Json<RecoveryResult>, AppError> {
    let access = recovery_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let evidence = b.evidence.trim().to_owned();
    if evidence.is_empty() || evidence.chars().count() > MAX_EVIDENCE_CHARS {
        return Err(AppError::Validation("evidence"));
    }
    let issue_key = match b.issue_key.as_deref().map(str::trim) {
        Some(k) if k.is_empty() || k.chars().count() > 64 => {
            return Err(AppError::Validation("issueKey"));
        }
        k => k.map(str::to_owned),
    };
    let ws = access.workspace_id;
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let intent = lock_intent(&mut tx, ws, id).await?;
    if intent.attempt != b.attempt {
        return Err(AppError::VersionConflict);
    }
    if intent.state != "unknown" {
        return Err(AppError::RuleViolation);
    }
    let next = match b.outcome {
        ReconcileOutcome::Success => "success",
        ReconcileOutcome::Failed => "failed",
    };
    if b.outcome == ReconcileOutcome::Success && intent.operation == "jira_create_issue" {
        let key = issue_key
            .as_deref()
            .ok_or(AppError::Validation("issueKey"))?;
        link_issue(&mut tx, ws, &intent, format!("reconciled-{id}"), key).await?;
    } else if issue_key.is_some() {
        return Err(AppError::Validation("issueKey"));
    }
    let error = (next == "failed").then_some(
        "Rekonsiliasi: petugas memastikan pengiriman tidak terjadi. Boleh dicoba ulang.",
    );
    sqlx::query(
        "UPDATE integration_outbox SET state = $3, reconcile_evidence = $4, safe_error = $5 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(next)
    .bind(&evidence)
    .bind(error)
    .execute(&mut *tx)
    .await?;
    // The evidence text stays on the outbox row; the audit records that it was given.
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.reconcile",
        "integration_outbox",
        id,
        serde_json::json!({
            "ticketId": intent.ticket_id,
            "from": "unknown",
            "to": next,
            "attempt": intent.attempt,
            "evidence": true,
            "issueKey": issue_key,
        }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(RecoveryResult {
        item: one(&mut conn, ws, id).await?,
        audit_event_id,
    }))
}

// ---------- simulation control (UAT only, integration.manage) ----------

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum SimulatedOutcome {
    Success,
    Failed,
    Unknown,
    /// Definite non-delivery that the worker retries automatically (bounded, with backoff).
    Transient,
}

impl SimulatedOutcome {
    fn code(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Transient => "transient",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulationBody {
    outcome: SimulatedOutcome,
    version: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResult {
    connection_id: Uuid,
    simulated_outcome: String,
    version: i32,
    audit_event_id: Uuid,
}

pub async fn set_simulation(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_conn)): Path<(String, String)>,
    payload: Result<Json<SimulationBody>, JsonRejection>,
) -> Result<Json<SimulationResult>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::IntegrationManage)?;
    // Outcome switching exists for UAT only; production never exposes it.
    if state.config.environment != Environment::Uat {
        return Err(AppError::NotFound);
    }
    let id = parse_id(&raw_conn)?;
    let b = body(payload)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let current: i32 = sqlx::query_scalar(
        "SELECT version FROM integration_connections WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if current != b.version {
        return Err(AppError::VersionConflict);
    }
    let version: i32 = sqlx::query_scalar(
        "UPDATE integration_connections \
         SET config = jsonb_set(config, '{simulatedOutcome}', to_jsonb($3::text)), version = version + 1 \
         WHERE workspace_id = $1 AND id = $2 RETURNING version",
    )
    .bind(ws)
    .bind(id)
    .bind(b.outcome.code())
    .fetch_one(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.simulation",
        "integration_connection",
        id,
        serde_json::json!({ "simulatedOutcome": b.outcome.code(), "version": version }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(SimulationResult {
        connection_id: id,
        simulated_outcome: b.outcome.code().to_owned(),
        version,
        audit_event_id,
    }))
}
