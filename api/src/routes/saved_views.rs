//! C32 saved views (D06): private List/Board presets of the caller in one workspace.
//!
//! - Scope is always `workspace_id` + the session user. Another user's view, or a view of
//!   another workspace, answers 404 exactly like an unknown ID; there is no public/shared view.
//! - A view stores the view mode and the List/Board filters/sort in their canonical
//!   query-string form. Search text, paging, cursors, sessions and secrets are never stored;
//!   unknown keys and `q` are refused.
//! - Filters are parsed and validated against the workspace when saved, but a view grants
//!   nothing: applying it only opens the List/Board URL, which re-checks permission and
//!   re-validates every filter (a stale ID answers 400 there).
//! - C32 properties: a view may also store which optional List columns are shown, from a fixed
//!   allowlist (`COLUMNS`), deduplicated and kept in canonical order. Number and title are always
//!   shown and cannot be listed. No stored columns = all columns (the default).
//! - Rename is optimistic (`version`, 409); names are unique per user+workspace
//!   (case-insensitive, 422); at most `MAX_VIEWS` per user+workspace (422).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::error::AppError;
use crate::routes::ticket_list::{self, ListQuery, TicketQuery, body};
use crate::routes::workspaces::{ListBody, parse_id};
use crate::session::Session;

pub const MAX_VIEWS: i64 = 50;
pub const MAX_NAME_CHARS: usize = 60;
/// Optional List columns in display order (the default when a view stores none).
pub const COLUMNS: [&str; 6] = [
    "status", "priority", "owner", "channel", "received", "updated",
];
const UNIQUE_VIOLATION: &str = "23505";

macro_rules! utc {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}

const SELECT: &str = concat!(
    "SELECT v.id, v.name, v.view, v.query, v.columns, v.version, ",
    utc!("v.created_at"),
    " AS created_at, ",
    utc!("v.updated_at"),
    " AS updated_at FROM saved_views v"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    List,
    Board,
}

impl ViewMode {
    fn code(self) -> &'static str {
        match self {
            ViewMode::List => "list",
            ViewMode::Board => "board",
        }
    }
}

/// Filters/sort a view may hold: the List/Board query keys minus `q`, `limit` and `cursor`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedQueryInput {
    status: Option<String>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
    received_from: Option<String>,
    received_before: Option<String>,
    resolved_from: Option<String>,
    resolved_before: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

impl SavedQueryInput {
    fn into_list_query(self) -> ListQuery {
        ListQuery {
            status: self.status,
            owner: self.owner,
            channel: self.channel,
            category: self.category,
            priority: self.priority,
            received_from: self.received_from,
            received_before: self.received_before,
            resolved_from: self.resolved_from,
            resolved_before: self.resolved_before,
            sort: self.sort,
            order: self.order,
            ..ListQuery::default()
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBody {
    name: String,
    view: ViewMode,
    #[serde(default)]
    query: SavedQueryInput,
    columns: Option<Vec<String>>,
}

/// Rename and/or change the shown columns; at least one of them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameBody {
    name: Option<String>,
    columns: Option<Vec<String>>,
    version: i32,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    view: String,
    query: sqlx::types::Json<BTreeMap<String, String>>,
    columns: Option<Vec<String>>,
    version: i32,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedView {
    id: Uuid,
    name: String,
    view: String,
    /// Canonical List/Board query parameters (string values); never contains `q`.
    query: BTreeMap<String, String>,
    /// Optional List columns shown, canonical order (all of `COLUMNS` when none were stored).
    columns: Vec<String>,
    version: i32,
    created_at: String,
    updated_at: String,
}

impl From<Row> for SavedView {
    fn from(r: Row) -> Self {
        SavedView {
            id: r.id,
            name: r.name,
            view: r.view,
            query: r.query.0,
            columns: r
                .columns
                .unwrap_or_else(|| COLUMNS.iter().map(|c| (*c).to_owned()).collect()),
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

fn name(raw: &str) -> Result<String, AppError> {
    let n = raw.trim();
    let len = n.chars().count();
    if len == 0 || len > MAX_NAME_CHARS || n.chars().any(char::is_control) {
        return Err(AppError::InvalidRequest);
    }
    Ok(n.to_owned())
}

/// Validates a column choice against the allowlist; returns it deduplicated in canonical order.
pub fn columns(raw: &[String]) -> Result<Vec<String>, AppError> {
    if raw.is_empty() || raw.len() > COLUMNS.len() * 2 {
        return Err(AppError::InvalidRequest);
    }
    if raw.iter().any(|c| !COLUMNS.contains(&c.as_str())) {
        return Err(AppError::InvalidRequest);
    }
    Ok(COLUMNS
        .iter()
        .filter(|c| raw.iter().any(|r| r == *c))
        .map(|c| (*c).to_owned())
        .collect())
}

/// Duplicate names are a rule of the resource, not a server fault.
fn unique_to_rule(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some(UNIQUE_VIOLATION) => {
            tracing::info!(
                error.kind = "saved_view_name_taken",
                "saved view name refused"
            );
            AppError::RuleViolation
        }
        _ => e.into(),
    }
}

async fn access(state: &AppState, s: &Session, raw_ws: &str) -> Result<WorkspaceAccess, AppError> {
    let access = authz::resolve(state, s, parse_id(raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    Ok(access)
}

pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
) -> Result<Json<ListBody<SavedView>>, AppError> {
    let access = access(&state, &s, &raw_ws).await?;
    let rows = sqlx::query_as::<_, Row>(&format!(
        "{SELECT} WHERE v.workspace_id = $1 AND v.user_id = $2 ORDER BY lower(v.name), v.id"
    ))
    .bind(access.workspace_id)
    .bind(access.user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(ListBody {
        items: rows.into_iter().map(SavedView::from).collect(),
    }))
}

pub async fn create(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
    payload: Result<Json<CreateBody>, JsonRejection>,
) -> Result<Response, AppError> {
    let access = access(&state, &s, &raw_ws).await?;
    let b = body(payload)?;
    let name = name(&b.name)?;
    let cols = b.columns.as_deref().map(columns).transpose()?;
    let filters = TicketQuery::parse(&b.query.into_list_query())?;
    let params = filters.saved_params();

    let mut tx = state.pool.begin().await?;
    ticket_list::validate(&mut tx, access.workspace_id, &filters).await?;
    // Serialise creates of one user+workspace so the cap holds under concurrency.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "saved_views:{}:{}",
            access.workspace_id, access.user_id
        ))
        .execute(&mut *tx)
        .await?;
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM saved_views WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if existing >= MAX_VIEWS {
        tracing::info!(error.kind = "saved_view_cap", "saved view cap reached");
        return Err(AppError::RuleViolation);
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO saved_views (workspace_id, user_id, name, view, query, columns) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .bind(&name)
    .bind(b.view.code())
    .bind(sqlx::types::Json(&params))
    .bind(&cols)
    .fetch_one(&mut *tx)
    .await
    .map_err(unique_to_rule)?;
    let row = sqlx::query_as::<_, Row>(&format!("{SELECT} WHERE v.id = $1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(
        workspace_id = %access.workspace_id,
        saved_view_id = %id,
        "saved view created"
    );
    Ok((StatusCode::CREATED, Json(SavedView::from(row))).into_response())
}

pub async fn rename(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<RenameBody>, JsonRejection>,
) -> Result<Json<SavedView>, AppError> {
    let access = access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    if b.version < 1 {
        return Err(AppError::InvalidRequest);
    }
    if b.name.is_none() && b.columns.is_none() {
        return Err(AppError::InvalidRequest);
    }
    let name = b.name.as_deref().map(name).transpose()?;
    let cols = b.columns.as_deref().map(columns).transpose()?;

    let mut tx = state.pool.begin().await?;
    let current: Option<i32> = sqlx::query_scalar(
        "SELECT version FROM saved_views \
         WHERE id = $1 AND workspace_id = $2 AND user_id = $3 FOR UPDATE",
    )
    .bind(id)
    .bind(access.workspace_id)
    .bind(access.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    match current {
        None => return Err(AppError::NotFound),
        Some(v) if v != b.version => return Err(AppError::VersionConflict),
        Some(_) => {}
    }
    sqlx::query(
        "UPDATE saved_views SET name = COALESCE($2, name), columns = COALESCE($3, columns), \
           version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(&cols)
    .execute(&mut *tx)
    .await
    .map_err(unique_to_rule)?;
    let row = sqlx::query_as::<_, Row>(&format!("{SELECT} WHERE v.id = $1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(SavedView::from(row)))
}

pub async fn delete(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let access = access(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let done =
        sqlx::query("DELETE FROM saved_views WHERE id = $1 AND workspace_id = $2 AND user_id = $3")
            .bind(id)
            .bind(access.workspace_id)
            .bind(access.user_id)
            .execute(&state.pool)
            .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tracing::info!(
        workspace_id = %access.workspace_id,
        saved_view_id = %id,
        "saved view deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}
