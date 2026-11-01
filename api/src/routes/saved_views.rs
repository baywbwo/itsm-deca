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
    "SELECT v.id, v.name, v.view, v.query, v.version, ",
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameBody {
    name: String,
    version: i32,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    view: String,
    query: sqlx::types::Json<BTreeMap<String, String>>,
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
        "INSERT INTO saved_views (workspace_id, user_id, name, view, query) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .bind(&name)
    .bind(b.view.code())
    .bind(sqlx::types::Json(&params))
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
    let name = name(&b.name)?;

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
        "UPDATE saved_views SET name = $2, version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
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
