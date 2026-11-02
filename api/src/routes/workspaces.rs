//! Workspace reads. Access = active membership in an active workspace, or the UAT
//! Superadmin grant. Foreign or unknown IDs both answer 404 (no enumeration).

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::authz;
use crate::error::AppError;
use crate::session::Session;

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    pub(crate) id: Uuid,
    pub(crate) code: String,
    name: String,
    timezone: String,
    company_id: Uuid,
    version: i32,
    /// Company branding preset (`deca` | `ajr`); `null` = not configured (neutral look).
    brand_key: Option<String>,
    /// Name shown next to the logo: the branding display name, else the company name.
    brand_name: String,
    /// Caller's business role here; `null` for UAT Superadmin without membership.
    my_role: Option<String>,
}

#[derive(Serialize)]
pub struct ListBody<T> {
    pub(crate) items: Vec<T>,
}

const WORKSPACE_SELECT: &str = "SELECT w.id, w.code, w.name, w.timezone, w.company_id, w.version, \
            c.brand_key, COALESCE(c.brand_name, c.name) AS brand_name, m.role AS my_role \
     FROM workspaces w \
     JOIN companies c ON c.id = w.company_id \
     LEFT JOIN workspace_memberships m \
            ON m.workspace_id = w.id AND m.user_id = $1 AND m.active \
     WHERE w.active AND (m.id IS NOT NULL OR $2)";

pub(crate) fn parse_id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

pub async fn list(
    State(state): State<AppState>,
    s: Session,
) -> Result<Json<ListBody<WorkspaceView>>, AppError> {
    let items = sqlx::query_as::<_, WorkspaceView>(&format!("{WORKSPACE_SELECT} ORDER BY w.code"))
        .bind(s.user_id)
        .bind(authz::superadmin_effective(&state, &s))
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(ListBody { items }))
}

pub(crate) async fn find_accessible(
    state: &AppState,
    s: &Session,
    workspace_id: Uuid,
) -> Result<WorkspaceView, AppError> {
    sqlx::query_as::<_, WorkspaceView>(&format!("{WORKSPACE_SELECT} AND w.id = $3"))
        .bind(s.user_id)
        .bind(authz::superadmin_effective(state, s))
        .bind(workspace_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn get_one(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<WorkspaceView>, AppError> {
    let id = parse_id(&raw)?;
    Ok(Json(find_accessible(&state, &s, id).await?))
}
