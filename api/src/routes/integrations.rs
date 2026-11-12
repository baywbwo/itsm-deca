//! Integration connections (C26/C27/C29): Jira Cloud and Lark, live or simulation.
//!
//! The server switch `ITSM_INTEGRATIONS=live|simulation` decides how a connection is tested and
//! delivered. A connection remembers the mode it was configured/tested in (`mode`); a connection
//! from the other mode is never used until it is configured and tested again.
//!
//! Lifecycle per PRD §7: save (draft) → test → activate; disable at any time. Only an `active`
//! connection receives deliveries. In live mode the test calls the provider for real (Jira:
//! account, project, issue type, priorities; Lark: tenant token, bot, chat list) and activation
//! needs a passing test, a stored credential and the server keyring. Changing the target (URL,
//! account, project, app) or the credential sends the connection back to `draft`.
//!
//! Credentials (Jira API token / Lark app secret) are write-only: encrypted with the server
//! keyring, never returned, never logged, never put in an audit row. Every change needs the
//! explicit `integration.manage` grant (credentials also `integration.credentials`), checks the
//! connection version (409) and writes its audit row in the same transaction.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::config::IntegrationsMode;
use crate::crypto;
use crate::error::AppError;
use crate::live::{self, JiraSettings, LarkSettings, TICKET_STATUSES};
use crate::providers::{IdName, provider_base_url};
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

const RECENT_LOG: i64 = 10;

/// What the simulated test proves (shown next to the test time).
const TEST_NOTE: &str = "Uji simulasi: konfigurasi lengkap untuk dicoba. Penyedia nyata tidak dihubungi \
     (server berjalan dengan ITSM_INTEGRATIONS=simulation).";

const PRIORITY_CODES: [&str; 4] = ["low", "normal", "high", "urgent"];

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
    last_test_ok: Option<bool>,
    config: Value,
    credential_updated_at: Option<String>,
    simulated_outcome: String,
    version: i32,
    directory_count: i64,
}

/// Non-secret settings. Jira: base URL, account email, project, issue type, mappings.
/// Lark: base URL and app id.
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    base_url: Option<String>,
    email: Option<String>,
    project_key: Option<String>,
    issue_type_id: Option<String>,
    app_id: Option<String>,
    /// ITSM priority code → Jira priority id.
    priority_map: BTreeMap<String, String>,
    /// ITSM ticket status → Jira status id (stored for the office; no automatic transition).
    status_map: BTreeMap<String, String>,
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

impl SettingsView {
    fn of(provider: &str, config: &Value) -> Self {
        if provider == "jira" {
            let s = JiraSettings::from_config(config);
            SettingsView {
                base_url: non_empty(s.base_url),
                email: non_empty(s.email),
                project_key: non_empty(s.project_key),
                issue_type_id: non_empty(s.issue_type_id),
                app_id: None,
                priority_map: s.priority_map,
                status_map: s.status_map,
            }
        } else {
            let base = config
                .get("baseUrl")
                .and_then(Value::as_str)
                .map(str::to_owned);
            SettingsView {
                base_url: base,
                app_id: non_empty(LarkSettings::from_config(config).app_id),
                ..SettingsView::default()
            }
        }
    }
}

/// The credential is never returned: only whether one is stored and when it was replaced.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialView {
    /// `missing` | `stored`.
    state: &'static str,
    updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    id: Uuid,
    provider: String,
    display_name: String,
    /// Mode this connection was last configured/tested in: `simulation` | `live`.
    mode: String,
    /// `draft` | `tested` | `active` | `disabled`.
    state: String,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    /// Outcome of the last test; `null` when not tested since the last target change.
    last_test_ok: Option<bool>,
    simulated_outcome: String,
    version: i32,
    /// Active assignees (Jira) or destinations (Lark) known locally.
    directory_count: i64,
    settings: SettingsView,
    credential: CredentialView,
    /// Safe audit trail of this connection (no secrets, no ticket content).
    recent: Vec<LogEntry>,
}

const CONNECTION_SQL: &str = "SELECT c.id, c.provider, c.display_name, c.mode, c.state, \
        to_char(c.tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
        c.last_test_note, c.last_test_ok, c.config, \
        to_char(c.credential_updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS credential_updated_at, \
        COALESCE(c.config->>'simulatedOutcome', 'success') AS simulated_outcome, \
        c.version, \
        CASE c.provider \
          WHEN 'jira' THEN (SELECT count(*) FROM jira_assignees a \
                            WHERE a.workspace_id = c.workspace_id AND a.connection_id = c.id AND a.active \
                              AND (c.mode = 'simulation' OR a.source <> 'dummy')) \
          ELSE (SELECT count(*) FROM lark_destinations d \
                WHERE d.workspace_id = c.workspace_id AND d.connection_id = c.id AND d.active \
                  AND (c.mode = 'simulation' OR d.source <> 'dummy')) \
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
        settings: SettingsView::of(&row.provider, &row.config),
        credential: CredentialView {
            state: if row.credential_updated_at.is_some() {
                "stored"
            } else {
                "missing"
            },
            updated_at: row.credential_updated_at,
        },
        id: row.id,
        provider: row.provider,
        display_name: row.display_name,
        mode: row.mode,
        state: row.state,
        tested_at: row.tested_at,
        last_test_note: row.last_test_note,
        last_test_ok: row.last_test_ok,
        simulated_outcome: row.simulated_outcome,
        version: row.version,
        directory_count: row.directory_count,
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

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PriorityOption {
    code: String,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationsBody {
    workspace_id: Uuid,
    /// Server switch `ITSM_INTEGRATIONS`: `live` | `simulation`.
    integrations_mode: &'static str,
    /// The keyring (`ITSM_KEYRING_FILE`) is set: credentials can be stored and live connections activated.
    keyring_configured: bool,
    items: Vec<ConnectionView>,
    /// Providers a connection can be created for (one per workspace each).
    creatable: Vec<&'static str>,
    /// Caller holds `integration.credentials` (may replace or delete credentials).
    can_manage_credentials: bool,
    /// Workspace priorities (keys of the Jira priority mapping).
    priorities: Vec<PriorityOption>,
    /// Ticket statuses (keys of the Jira status mapping).
    ticket_statuses: Vec<&'static str>,
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

fn server_mode(state: &AppState) -> &'static str {
    state.config.integrations.mode.as_str()
}

fn require_live(state: &AppState) -> Result<(), AppError> {
    if state.config.integrations.mode == IntegrationsMode::Live {
        Ok(())
    } else {
        Err(AppError::IntegrationNotLive)
    }
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
    let priorities = sqlx::query_as::<_, PriorityOption>(
        "SELECT code, label FROM priorities WHERE workspace_id = $1 AND active \
         ORDER BY sort_order, code",
    )
    .bind(ws)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(IntegrationsBody {
        workspace_id: ws,
        integrations_mode: server_mode(&state),
        keyring_configured: state.config.keyring.is_some(),
        items,
        creatable: PROVIDERS
            .into_iter()
            .filter(|p| !present.iter().any(|x| x == p))
            .collect(),
        can_manage_credentials: access.allows(Permission::IntegrationCredentials),
        priorities,
        ticket_statuses: TICKET_STATUSES.to_vec(),
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResult {
    item: ConnectionView,
    audit_event_id: Uuid,
}

async fn result(
    state: &AppState,
    ws: Uuid,
    id: Uuid,
    audit_event_id: Uuid,
) -> Result<Json<ConnectionResult>, AppError> {
    let mut conn = state.pool.acquire().await?;
    Ok(Json(ConnectionResult {
        item: one(&mut conn, ws, id).await?,
        audit_event_id,
    }))
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
    let mode = server_mode(&state);
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id, provider, mode, display_name) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (workspace_id, provider) DO NOTHING RETURNING id",
    )
    .bind(ws)
    .bind(provider)
    .bind(mode)
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
        serde_json::json!({ "provider": provider, "state": "draft", "mode": mode }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    let Json(r) = result(&state, ws, id, audit_event_id).await?;
    Ok((StatusCode::CREATED, Json(r)))
}

struct Locked {
    state: String,
    provider: String,
    mode: String,
    config: Value,
    credential_secret_id: Option<Uuid>,
}

/// Locks the connection and checks the version.
async fn lock(
    conn: &mut PgConnection,
    ws: Uuid,
    id: Uuid,
    version: i32,
) -> Result<Locked, AppError> {
    let (state, provider, mode, config, credential_secret_id, current): (
        String,
        String,
        String,
        Value,
        Option<Uuid>,
        i32,
    ) = sqlx::query_as(
        "SELECT state, provider, mode, config, credential_secret_id, version \
         FROM integration_connections WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::NotFound)?;
    if current != version {
        return Err(AppError::VersionConflict);
    }
    Ok(Locked {
        state,
        provider,
        mode,
        config,
        credential_secret_id,
    })
}

/// State after the target or credential changed: a tested/active connection must be re-tested.
fn back_to_draft(from: &str) -> &'static str {
    if from == "disabled" {
        "disabled"
    } else {
        "draft"
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionPatch {
    display_name: String,
    version: i32,
}

/// Renames the connection (no effect on its state).
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
    result(&state, ws, id, audit_event_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigBody {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    project_key: Option<String>,
    #[serde(default)]
    issue_type_id: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    priority_map: BTreeMap<String, String>,
    #[serde(default)]
    status_map: BTreeMap<String, String>,
    version: i32,
}

fn trimmed(v: &Option<String>) -> &str {
    v.as_deref().map(str::trim).unwrap_or_default()
}

fn jira_id(raw: &str) -> bool {
    (1..=20).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_digit())
}

fn mapping(
    raw: &BTreeMap<String, String>,
    keys: &[&str],
    field: &'static str,
) -> Result<BTreeMap<String, String>, AppError> {
    let mut out = BTreeMap::new();
    for (k, v) in raw {
        if !keys.contains(&k.as_str()) {
            return Err(AppError::Validation(field));
        }
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        if !jira_id(v) {
            return Err(AppError::Validation(field));
        }
        out.insert(k.clone(), v.to_owned());
    }
    Ok(out)
}

/// Validated settings as stored in `config` (plus the keys kept from the old config).
fn validated(
    provider: &str,
    b: &ConfigBody,
    lab: bool,
) -> Result<serde_json::Map<String, Value>, AppError> {
    let mut m = serde_json::Map::new();
    let base = trimmed(&b.base_url);
    if provider == "jira" {
        let base = provider_base_url("jira", base, lab).ok_or(AppError::Validation("baseUrl"))?;
        let email = trimmed(&b.email);
        let email_ok = (3..=254).contains(&email.len())
            && email.contains('@')
            && !email.chars().any(|c| c.is_whitespace() || c.is_control());
        if !email_ok {
            return Err(AppError::Validation("email"));
        }
        let project = trimmed(&b.project_key).to_ascii_uppercase();
        let project_ok = (2..=31).contains(&project.len())
            && project.starts_with(|c: char| c.is_ascii_uppercase())
            && project
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_');
        if !project_ok {
            return Err(AppError::Validation("projectKey"));
        }
        let issue_type = trimmed(&b.issue_type_id);
        if !issue_type.is_empty() && !jira_id(issue_type) {
            return Err(AppError::Validation("issueTypeId"));
        }
        if !trimmed(&b.app_id).is_empty() {
            return Err(AppError::Validation("appId"));
        }
        m.insert("baseUrl".into(), base.into());
        m.insert("email".into(), email.into());
        m.insert("projectKey".into(), project.into());
        m.insert("issueTypeId".into(), issue_type.into());
        let pm = mapping(&b.priority_map, &PRIORITY_CODES, "priorityMap")?;
        let sm = mapping(&b.status_map, &TICKET_STATUSES, "statusMap")?;
        m.insert(
            "priorityMap".into(),
            serde_json::to_value(pm).unwrap_or_default(),
        );
        m.insert(
            "statusMap".into(),
            serde_json::to_value(sm).unwrap_or_default(),
        );
    } else {
        if !base.is_empty() {
            let base =
                provider_base_url("lark", base, lab).ok_or(AppError::Validation("baseUrl"))?;
            m.insert("baseUrl".into(), base.into());
        }
        let app_id = trimmed(&b.app_id);
        let app_ok = (1..=64).contains(&app_id.len())
            && app_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
        if !app_ok {
            return Err(AppError::Validation("appId"));
        }
        for (field, set) in [
            ("email", &b.email),
            ("projectKey", &b.project_key),
            ("issueTypeId", &b.issue_type_id),
        ] {
            if !trimmed(set).is_empty() {
                return Err(AppError::Validation(field));
            }
        }
        if !b.priority_map.is_empty() {
            return Err(AppError::Validation("priorityMap"));
        }
        if !b.status_map.is_empty() {
            return Err(AppError::Validation("statusMap"));
        }
        m.insert("appId".into(), app_id.into());
    }
    Ok(m)
}

const TARGET_KEYS: [&str; 5] = ["baseUrl", "email", "projectKey", "issueTypeId", "appId"];

/// What a stored credential is bound to (CP5B-01): the address it is sent to and the account it
/// belongs to. A Lark connection without `baseUrl` uses the international default.
fn credential_target(provider: &str, m: &serde_json::Map<String, Value>) -> [String; 3] {
    let text = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut base = text("baseUrl");
    if provider == "lark" && base.is_empty() {
        base = live::LARK_DEFAULT_BASE.to_owned();
    }
    [base, text("email"), text("appId")]
}

/// Saves the provider settings (no secret). A changed target (or mode) sends the connection back
/// to `draft`; a mapping-only change keeps its state. A new address or account deletes the stored
/// credential in the same transaction (CP5B-01): only a holder of `integration.credentials` may
/// decide where a secret is sent, so it has to be entered again for the new target.
pub async fn configure(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<ConfigBody>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    let access = manage_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let ws = access.workspace_id;
    let mode = server_mode(&state);
    let mut tx = state.pool.begin().await?;
    let cur = lock(&mut tx, ws, id, b.version).await?;
    let fresh = validated(&cur.provider, &b, state.config.integrations.lab)?;
    let old = cur.config.as_object().cloned().unwrap_or_default();
    let changed: Vec<&str> = fresh
        .keys()
        .chain(old.keys())
        .filter(|k| k.as_str() != "simulatedOutcome")
        .filter(|k| old.get(k.as_str()) != fresh.get(k.as_str()))
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let target_changed = cur.mode != mode || changed.iter().any(|k| TARGET_KEYS.contains(k));
    let cleared = cur.credential_secret_id.filter(|_| {
        credential_target(&cur.provider, &old) != credential_target(&cur.provider, &fresh)
    });
    let mut next = fresh.clone();
    if let Some(sim) = old.get("simulatedOutcome") {
        next.insert("simulatedOutcome".into(), sim.clone());
    }
    let to = if target_changed {
        back_to_draft(&cur.state)
    } else {
        match cur.state.as_str() {
            "active" => "active",
            "tested" => "tested",
            "disabled" => "disabled",
            _ => "draft",
        }
    };
    sqlx::query(
        "UPDATE integration_connections SET config = $3, mode = $4, state = $5, \
           last_test_ok = CASE WHEN $6 THEN NULL ELSE last_test_ok END, \
           credential_secret_id = CASE WHEN $7 THEN NULL ELSE credential_secret_id END, \
           credential_updated_at = CASE WHEN $7 THEN NULL ELSE credential_updated_at END, \
           version = version + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(Value::Object(next))
    .bind(mode)
    .bind(to)
    .bind(target_changed)
    .bind(cleared.is_some())
    .execute(&mut *tx)
    .await?;
    if let Some(old_secret) = cleared {
        sqlx::query("DELETE FROM encrypted_secrets WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(old_secret)
            .execute(&mut *tx)
            .await?;
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.configure",
        "integration_connection",
        id,
        serde_json::json!({
            "changed": changed, "targetChanged": target_changed,
            "credentialCleared": cleared.is_some(), "from": cur.state, "to": to,
            "mode": mode, "version": b.version + 1
        }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    result(&state, ws, id, audit_event_id).await
}

/// A credential as received: never printed, wiped from memory on drop.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct SecretInput(String);

impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretInput([redacted])")
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SecretInput {
    /// The value without surrounding blanks, refused when empty, longer than 4000 bytes or
    /// holding control characters.
    pub(crate) fn checked(&self) -> Result<&str, AppError> {
        let value = self.0.trim();
        if !(1..=4000).contains(&value.len()) || value.chars().any(char::is_control) {
            return Err(AppError::Validation("secret"));
        }
        Ok(value)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialBody {
    secret: SecretInput,
    version: i32,
}

async fn credential_access(
    state: &AppState,
    s: &Session,
    raw_ws: &str,
) -> Result<WorkspaceAccess, AppError> {
    let access = manage_access(state, s, raw_ws).await?;
    access.require(Permission::IntegrationCredentials)?;
    Ok(access)
}

/// Stores (or replaces) the Jira API token / Lark app secret, encrypted with the server keyring.
/// Write-only: the value is never returned; the connection goes back to `draft`.
pub async fn put_credential(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<CredentialBody>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    let access = credential_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let keyring = state
        .config
        .keyring
        .clone()
        .ok_or(AppError::KeyringMissing)?;
    let value = b.secret.checked()?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let cur = lock(&mut tx, ws, id, b.version).await?;
    let new_id = crypto::store_secret(
        &mut tx,
        &keyring,
        Some(ws),
        live::purpose(&cur.provider),
        value.as_bytes(),
        Some(access.user_id),
    )
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))?;
    let to = back_to_draft(&cur.state);
    sqlx::query(
        "UPDATE integration_connections SET credential_secret_id = $3, credential_updated_at = now(), \
           state = $4, last_test_ok = NULL, version = version + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(new_id)
    .bind(to)
    .execute(&mut *tx)
    .await?;
    if let Some(old) = cur.credential_secret_id {
        sqlx::query("DELETE FROM encrypted_secrets WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(old)
            .execute(&mut *tx)
            .await?;
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.credential.replace",
        "integration_connection",
        id,
        serde_json::json!({
            "provider": cur.provider, "replaced": cur.credential_secret_id.is_some(),
            "from": cur.state, "to": to, "version": b.version + 1
        }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    result(&state, ws, id, audit_event_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionBody {
    version: i32,
}

/// Deletes the stored credential; the connection goes back to `draft` (no delivery without it).
pub async fn delete_credential(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Json<ConnectionResult>, AppError> {
    let access = credential_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let cur = lock(&mut tx, ws, id, b.version).await?;
    let old = cur
        .credential_secret_id
        .ok_or(AppError::Conflict("integration_not_configured"))?;
    let to = back_to_draft(&cur.state);
    sqlx::query(
        "UPDATE integration_connections SET credential_secret_id = NULL, credential_updated_at = NULL, \
           state = $3, last_test_ok = NULL, version = version + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(to)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM encrypted_secrets WHERE workspace_id = $1 AND id = $2")
        .bind(ws)
        .bind(old)
        .execute(&mut *tx)
        .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.credential.delete",
        "integration_connection",
        id,
        serde_json::json!({
            "provider": cur.provider, "from": cur.state, "to": to, "version": b.version + 1
        }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    result(&state, ws, id, audit_event_id).await
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

    /// Next state from `from` after a passing test (or the step itself), or `None` when the step
    /// is not allowed there.
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

/// State after a failing live test: never `tested`; an active connection stays active (its
/// deliveries retry) and the failure is shown.
fn after_failed_test(from: &str) -> &'static str {
    match from {
        "active" => "active",
        "disabled" => "disabled",
        _ => "draft",
    }
}

#[allow(clippy::too_many_arguments)]
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
    let mode = server_mode(state);
    let live_mode = state.config.integrations.mode == IntegrationsMode::Live;

    // The live test runs before the transaction: no row lock is held while the provider answers.
    // The version check below refuses the result if the settings changed meanwhile.
    let tested = if matches!(step, Step::Test) {
        if live_mode {
            let conn = live::load_conn(state, ws, id).await?;
            Some(live::test_connection(state, ws, &conn).await)
        } else {
            Some(live::TestResult {
                ok: true,
                note: TEST_NOTE.to_owned(),
            })
        }
    } else {
        None
    };

    let mut tx = state.pool.begin().await?;
    let cur = lock(&mut tx, ws, id, b.version).await?;
    let mut to = step.next(&cur.state).ok_or(AppError::RuleViolation)?;
    if let Some(t) = &tested
        && !t.ok
    {
        to = after_failed_test(&cur.state);
    }
    if matches!(step, Step::Activate) {
        if cur.mode != mode {
            return Err(AppError::Conflict("integration_mode_mismatch"));
        }
        if live_mode {
            if state.config.keyring.is_none() {
                return Err(AppError::KeyringMissing);
            }
            if cur.credential_secret_id.is_none() {
                return Err(AppError::Conflict("integration_not_configured"));
            }
            let ok: Option<bool> = sqlx::query_scalar(
                "SELECT last_test_ok FROM integration_connections WHERE workspace_id = $1 AND id = $2",
            )
            .bind(ws)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            if ok != Some(true) {
                return Err(AppError::RuleViolation);
            }
        }
    }
    sqlx::query(
        "UPDATE integration_connections SET state = $3, version = version + 1, \
           mode = CASE WHEN $4 THEN $5 ELSE mode END, \
           tested_at = CASE WHEN $4 THEN now() ELSE tested_at END, \
           last_test_note = CASE WHEN $4 THEN $6 ELSE last_test_note END, \
           last_test_ok = CASE WHEN $4 THEN $7 ELSE last_test_ok END \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(to)
    .bind(tested.is_some())
    .bind(mode)
    .bind(tested.as_ref().map(|t| t.note.as_str()))
    .bind(tested.as_ref().map(|t| t.ok))
    .execute(&mut *tx)
    .await?;
    let mut details = serde_json::json!({
        "from": cur.state, "to": to, "mode": mode, "version": b.version + 1
    });
    if let Some(t) = &tested {
        details["ok"] = t.ok.into();
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        step.action(),
        "integration_connection",
        id,
        details,
        &correlation(headers),
    )
    .await?;
    tx.commit().await?;
    result(state, ws, id, audit_event_id).await
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraOptionsBody {
    issue_types: Vec<IdName>,
    priorities: Vec<IdName>,
    statuses: Vec<IdName>,
}

/// Jira metadata for the settings and mapping form, read live from the provider.
pub async fn options(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_id)): Path<(String, String)>,
) -> Result<Json<JiraOptionsBody>, AppError> {
    let access = manage_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    require_live(&state)?;
    let ws = access.workspace_id;
    let conn = live::load_conn(&state, ws, id).await?;
    if conn.provider != "jira" {
        return Err(AppError::NotFound);
    }
    let o = live::jira_options(&state, ws, &conn)
        .await
        .map_err(live::Failure::into_app_error)?;
    Ok(Json(JiraOptionsBody {
        issue_types: o.issue_types,
        priorities: o.priorities,
        statuses: o.statuses,
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectorySyncResult {
    item: ConnectionView,
    audit_event_id: Uuid,
    /// Chats the bot is a member of (now offered at intake).
    synced: usize,
    /// Previously known chats no longer returned by Lark (no longer offered).
    deactivated: u64,
}

/// Reads the Lark chats the bot belongs to and makes them the intake destinations.
pub async fn sync_directory(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
) -> Result<Json<DirectorySyncResult>, AppError> {
    let access = manage_access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    require_live(&state)?;
    let ws = access.workspace_id;
    let conn = live::load_conn(&state, ws, id).await?;
    if conn.provider != "lark" {
        return Err(AppError::NotFound);
    }
    let chats = live::lark_chats(&state, ws, &conn)
        .await
        .map_err(live::Failure::into_app_error)?;
    let chats: Vec<(String, String)> = chats
        .into_iter()
        .filter(|c| (1..=128).contains(&c.chat_id.trim().len()))
        .map(|c| {
            let name: String = c
                .name
                .chars()
                .filter(|ch| !ch.is_control())
                .take(200)
                .collect();
            let name = if name.trim().is_empty() {
                c.chat_id.clone()
            } else {
                name
            };
            (c.chat_id.trim().to_owned(), name)
        })
        .collect();
    let mut tx = state.pool.begin().await?;
    for (chat, name) in &chats {
        sqlx::query(
            "INSERT INTO lark_destinations (workspace_id, connection_id, external_chat_ref, display_name, source) \
             VALUES ($1, $2, $3, $4, 'lark') \
             ON CONFLICT (connection_id, external_chat_ref) \
             DO UPDATE SET display_name = EXCLUDED.display_name, source = 'lark', active = true",
        )
        .bind(ws)
        .bind(id)
        .bind(chat)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    }
    let refs: Vec<&str> = chats.iter().map(|(c, _)| c.as_str()).collect();
    let deactivated = sqlx::query(
        "UPDATE lark_destinations SET active = false \
         WHERE workspace_id = $1 AND connection_id = $2 AND source = 'lark' AND active \
           AND NOT (external_chat_ref = ANY($3))",
    )
    .bind(ws)
    .bind(id)
    .bind(&refs)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.directory.sync",
        "integration_connection",
        id,
        serde_json::json!({ "synced": chats.len(), "deactivated": deactivated }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    let Json(r) = result(&state, ws, id, audit_event_id).await?;
    Ok(Json(DirectorySyncResult {
        item: r.item,
        audit_event_id,
        synced: chats.len(),
        deactivated,
    }))
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
        assert_eq!(after_failed_test("tested"), "draft");
        assert_eq!(after_failed_test("active"), "active");
        assert_eq!(back_to_draft("active"), "draft");
        assert_eq!(back_to_draft("disabled"), "disabled");
    }

    fn cfg(v: Value) -> ConfigBody {
        match serde_json::from_value(v) {
            Ok(b) => b,
            Err(e) => panic!("body: {e}"),
        }
    }

    #[test]
    fn settings_are_validated_per_provider() {
        let ok = cfg(serde_json::json!({
            "baseUrl": "https://acme.atlassian.net/", "email": "bot@acme.test",
            "projectKey": "ops", "issueTypeId": "10001",
            "priorityMap": { "high": "2", "low": "" }, "version": 1
        }));
        let m = validated("jira", &ok, false).unwrap_or_default();
        assert_eq!(m["baseUrl"], "https://acme.atlassian.net");
        assert_eq!(m["projectKey"], "OPS");
        assert_eq!(m["priorityMap"], serde_json::json!({ "high": "2" }));
        let bad = |v: Value, field: &str| {
            let r = validated("jira", &cfg(v), false);
            assert!(
                matches!(r, Err(AppError::Validation(f)) if f == field),
                "{field}"
            );
        };
        let base = |extra: Value| {
            let mut b = serde_json::json!({
                "baseUrl": "https://acme.atlassian.net", "email": "bot@acme.test",
                "projectKey": "OPS", "version": 1
            });
            if let (Some(o), Some(e)) = (b.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            b
        };
        bad(
            base(serde_json::json!({ "baseUrl": "http://acme.test" })),
            "baseUrl",
        );
        bad(
            base(serde_json::json!({ "baseUrl": "https://127.0.0.1" })),
            "baseUrl",
        );
        bad(base(serde_json::json!({ "email": "nope" })), "email");
        bad(
            base(serde_json::json!({ "projectKey": "1X" })),
            "projectKey",
        );
        bad(
            base(serde_json::json!({ "issueTypeId": "abc" })),
            "issueTypeId",
        );
        bad(
            base(serde_json::json!({ "priorityMap": { "critical": "1" } })),
            "priorityMap",
        );
        bad(
            base(serde_json::json!({ "statusMap": { "open": "x" } })),
            "statusMap",
        );
        let lark = cfg(serde_json::json!({ "appId": "cli_a1b2", "version": 1 }));
        assert!(validated("lark", &lark, false).is_ok());
        let lark = cfg(serde_json::json!({ "appId": "cli a", "version": 1 }));
        assert!(matches!(
            validated("lark", &lark, false),
            Err(AppError::Validation("appId"))
        ));
    }

    #[test]
    fn secret_input_never_prints() {
        let b: CredentialBody = match serde_json::from_value(
            serde_json::json!({ "secret": "tok-123-secret", "version": 1 }),
        ) {
            Ok(b) => b,
            Err(e) => panic!("{e}"),
        };
        assert!(!format!("{b:?}").contains("tok-123"));
    }
}
