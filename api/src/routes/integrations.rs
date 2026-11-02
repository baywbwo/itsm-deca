//! D09 integration connections (C26/C27/C29) — SIMULATION only.
//!
//! Lifecycle per PRD §7: save (draft) → test → activate; disable at any time. Only an
//! `active` connection receives deliveries (intake and the outbox worker check it). The test
//! is a simulated step: it proves only that the configuration is complete enough to try, it
//! never contacts a provider. No real provider, endpoint, credential or field/status mapping
//! exists in this build; the API states that explicitly (`credential`/`mapping` =
//! `not_available`) and stores no secret at all.
//!
//! Every operation needs the explicit `integration.manage` grant (Lead CS has none by role),
//! checks the connection version (409) and writes its audit row in the same transaction.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::error::AppError;
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

const RECENT_LOG: i64 = 10;

/// What the simulated test proves (shown next to the test time).
const TEST_NOTE: &str = "Uji simulasi: konfigurasi lengkap untuk dicoba. Penyedia nyata tidak dihubungi; \
     kredensial dan mapping belum tersedia.";

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    action: String,
    outcome: String,
    occurred_at: String,
    actor: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ConnectionRow {
    id: Uuid,
    provider: String,
    display_name: String,
    mode: String,
    state: String,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    simulated_outcome: String,
    version: i32,
    directory_count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    id: Uuid,
    provider: String,
    display_name: String,
    /// Always `simulation`: no real provider is connected.
    mode: String,
    /// `draft` | `tested` | `active` | `disabled`.
    state: String,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    simulated_outcome: String,
    version: i32,
    /// Active dummy assignees (Jira) or destinations (Lark).
    directory_count: i64,
    /// Always `not_available`: no credential store / real provider in this build.
    credential: &'static str,
    /// Always `not_available`: field/status mapping needs the office contract.
    mapping: &'static str,
    /// Safe audit trail of this connection (no secrets, no ticket content).
    recent: Vec<LogEntry>,
}

const CONNECTION_SQL: &str = "SELECT c.id, c.provider, c.display_name, c.mode, c.state, \
        to_char(c.tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
        c.last_test_note, COALESCE(c.config->>'simulatedOutcome', 'success') AS simulated_outcome, \
        c.version, \
        CASE c.provider \
          WHEN 'jira' THEN (SELECT count(*) FROM jira_assignees a \
                            WHERE a.workspace_id = c.workspace_id AND a.connection_id = c.id AND a.active) \
          ELSE (SELECT count(*) FROM lark_destinations d \
                WHERE d.workspace_id = c.workspace_id AND d.connection_id = c.id AND d.active) \
        END AS directory_count \
     FROM integration_connections c WHERE c.workspace_id = $1";

async fn view(
    conn: &mut PgConnection,
    ws: Uuid,
    row: ConnectionRow,
) -> Result<ConnectionView, AppError> {
    let recent = sqlx::query_as::<_, LogEntry>(
        "SELECT e.action, e.outcome, \
                to_char(e.occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS occurred_at, \
                u.display_name AS actor \
         FROM audit_events e LEFT JOIN users u ON u.id = e.actor_id \
         WHERE e.workspace_id = $1 AND e.entity_type = 'integration_connection' AND e.entity_id = $2 \
         ORDER BY e.occurred_at DESC, e.id DESC LIMIT $3",
    )
    .bind(ws)
    .bind(row.id)
    .bind(RECENT_LOG)
    .fetch_all(&mut *conn)
    .await?;
    Ok(ConnectionView {
        id: row.id,
        provider: row.provider,
        display_name: row.display_name,
        mode: row.mode,
        state: row.state,
        tested_at: row.tested_at,
        last_test_note: row.last_test_note,
        simulated_outcome: row.simulated_outcome,
        version: row.version,
        directory_count: row.directory_count,
        credential: "not_available",
        mapping: "not_available",
        recent,
    })
}

async fn one(conn: &mut PgConnection, ws: Uuid, id: Uuid) -> Result<ConnectionView, AppError> {
    let row = sqlx::query_as::<_, ConnectionRow>(&format!("{CONNECTION_SQL} AND c.id = $2"))
        .bind(ws)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AppError::NotFound)?;
    view(conn, ws, row).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationsBody {
    workspace_id: Uuid,
    items: Vec<ConnectionView>,
    /// Providers a simulated connection can be created for (one per workspace each).
    creatable: Vec<&'static str>,
    /// Caller holds `integration.credentials`; still nothing to manage (not available).
    can_manage_credentials: bool,
}

const PROVIDERS: [&str; 2] = ["jira", "lark"];

async fn manage_access(
    state: &AppState,
    s: &Session,
    raw_ws: &str,
) -> Result<WorkspaceAccess, AppError> {
    let access = authz::resolve(state, s, parse_id(raw_ws)?).await?;
    access.require(Permission::IntegrationManage)?;
    Ok(access)
}

pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<IntegrationsBody>, AppError> {
    let access = manage_access(&state, &s, &raw).await?;
    let ws = access.workspace_id;
    let mut conn = state.pool.acquire().await?;
    let rows = sqlx::query_as::<_, ConnectionRow>(&format!("{CONNECTION_SQL} ORDER BY c.provider"))
        .bind(ws)
        .fetch_all(&mut *conn)
        .await?;
    let present: Vec<String> = rows.iter().map(|r| r.provider.clone()).collect();
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(view(&mut conn, ws, row).await?);
    }
    Ok(Json(IntegrationsBody {
        workspace_id: ws,
        items,
        creatable: PROVIDERS
            .into_iter()
            .filter(|p| !present.iter().any(|x| x == p))
            .collect(),
        can_manage_credentials: access.allows(Permission::IntegrationCredentials),
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResult {
    item: ConnectionView,
    audit_event_id: Uuid,
}

fn display_name(raw: &str) -> Result<String, AppError> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > 200 {
        return Err(AppError::Validation("displayName"));
    }
    Ok(t.to_owned())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionCreate {
    provider: String,
    display_name: String,
}

pub async fn create(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<ConnectionCreate>, JsonRejection>,
) -> Result<(StatusCode, Json<ConnectionResult>), AppError> {
    let access = manage_access(&state, &s, &raw).await?;
    let b = body(payload)?;
    let provider = PROVIDERS
        .into_iter()
        .find(|p| *p == b.provider)
        .ok_or(AppError::Validation("provider"))?;
    let name = display_name(&b.display_name)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id, provider, mode, display_name) \
         VALUES ($1, $2, 'simulation', $3) \
         ON CONFLICT (workspace_id, provider) DO NOTHING RETURNING id",
    )
    .bind(ws)
    .bind(provider)
    .bind(&name)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Validation("provider"))?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.create",
        "integration_connection",
        id,
        serde_json::json!({ "provider": provider, "state": "draft", "mode": "simulation" }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok((
        StatusCode::CREATED,
        Json(ConnectionResult {
            item: one(&mut conn, ws, id).await?,
            audit_event_id,
        }),
    ))
}

/// Locks the connection and checks the version; returns its current state.
async fn lock(
    conn: &mut PgConnection,
    ws: Uuid,
    id: Uuid,
    version: i32,
) -> Result<String, AppError> {
    let (state, current): (String, i32) = sqlx::query_as(
        "SELECT state, version FROM integration_connections \
         WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::NotFound)?;
    if current != version {
        return Err(AppError::VersionConflict);
    }
    Ok(state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionPatch {
    display_name: String,
    version: i32,
}

/// Saves the configuration that exists today (the display name). A real provider
/// configuration (base URL, project, mapping) is not available and is not accepted.
pub async fn update(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<ConnectionPatch>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    let access = manage_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let name = display_name(&b.display_name)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    lock(&mut tx, ws, id, b.version).await?;
    sqlx::query(
        "UPDATE integration_connections SET display_name = $3, version = version + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(&name)
    .execute(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.update",
        "integration_connection",
        id,
        serde_json::json!({ "changed": ["displayName"], "version": b.version + 1 }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(ConnectionResult {
        item: one(&mut conn, ws, id).await?,
        audit_event_id,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionBody {
    version: i32,
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Test,
    Activate,
    Disable,
}

impl Step {
    fn action(self) -> &'static str {
        match self {
            Step::Test => "integration.test",
            Step::Activate => "integration.activate",
            Step::Disable => "integration.disable",
        }
    }

    /// Next state from `from`, or `None` when the step is not allowed there.
    fn next(self, from: &str) -> Option<&'static str> {
        match (self, from) {
            // Re-testing an active connection keeps it active.
            (Step::Test, "active") => Some("active"),
            (Step::Test, "draft" | "tested" | "disabled") => Some("tested"),
            (Step::Activate, "tested") => Some("active"),
            (Step::Disable, "draft" | "tested" | "active") => Some("disabled"),
            _ => None,
        }
    }
}

async fn step(
    state: &AppState,
    s: &Session,
    headers: &HeaderMap,
    raw_ws: &str,
    raw_id: &str,
    payload: Result<Json<VersionBody>, JsonRejection>,
    step: Step,
) -> Result<Json<ConnectionResult>, AppError> {
    let access = manage_access(state, s, raw_ws).await?;
    let id = parse_id(raw_id)?;
    let b = body(payload)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let from = lock(&mut tx, ws, id, b.version).await?;
    let to = step.next(&from).ok_or(AppError::RuleViolation)?;
    let tested = matches!(step, Step::Test);
    sqlx::query(
        "UPDATE integration_connections SET state = $3, version = version + 1, \
           tested_at = CASE WHEN $4 THEN now() ELSE tested_at END, \
           last_test_note = CASE WHEN $4 THEN $5 ELSE last_test_note END \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(to)
    .bind(tested)
    .bind(TEST_NOTE)
    .execute(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        step.action(),
        "integration_connection",
        id,
        serde_json::json!({ "from": from, "to": to, "mode": "simulation", "version": b.version + 1 }),
        &correlation(headers),
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(ConnectionResult {
        item: one(&mut conn, ws, id).await?,
        audit_event_id,
    }))
}

pub async fn test(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    step(&state, &s, &headers, &raw_ws, &raw_id, payload, Step::Test).await
}

pub async fn activate(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    step(
        &state,
        &s,
        &headers,
        &raw_ws,
        &raw_id,
        payload,
        Step::Activate,
    )
    .await
}

pub async fn disable(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    step(
        &state,
        &s,
        &headers,
        &raw_ws,
        &raw_id,
        payload,
        Step::Disable,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_is_save_test_activate() {
        assert_eq!(Step::Activate.next("draft"), None);
        assert_eq!(Step::Test.next("draft"), Some("tested"));
        assert_eq!(Step::Activate.next("tested"), Some("active"));
        assert_eq!(Step::Activate.next("active"), None);
        assert_eq!(Step::Activate.next("disabled"), None);
        assert_eq!(Step::Test.next("disabled"), Some("tested"));
        assert_eq!(Step::Test.next("active"), Some("active"));
        assert_eq!(Step::Disable.next("active"), Some("disabled"));
        assert_eq!(Step::Disable.next("disabled"), None);
    }
}
