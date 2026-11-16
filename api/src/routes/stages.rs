use super::{sync::audit_entity, ticket_list::correlation, workspaces::parse_id};
use crate::{
    AppState,
    authz::{self, Permission},
    domain::{TicketStatus, TicketStatusView},
    error::AppError,
    session::Session,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stage {
    pub code: String,
    pub label: String,
    pub lifecycle: String,
    pub sort_order: i32,
    pub active: bool,
    pub version: i32,
}
impl Stage {
    pub fn status(&self) -> TicketStatusView {
        TicketStatusView {
            code: self.code.clone(),
            label: self.label.clone(),
            selectable: self.active && self.lifecycle != "closed",
            archived: self.lifecycle == "closed",
            lifecycle_code: (self.code != self.lifecycle).then(|| self.lifecycle.clone()),
        }
    }
}
pub(crate) async fn load(conn: &mut PgConnection, ws: Uuid) -> Result<Vec<Stage>, AppError> {
    Ok(sqlx::query_as("SELECT code,label,lifecycle,sort_order,active,version FROM workflow_stages WHERE workspace_id=$1 ORDER BY sort_order,code").bind(ws).fetch_all(conn).await?)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub items: Vec<Stage>,
    pub can_edit: bool,
}
pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<Settings>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::CatalogRead)?;
    Ok(Json(Settings {
        items: load(&mut *state.pool.acquire().await?, access.workspace_id).await?,
        can_edit: access.allows(Permission::AdminSettings),
    }))
}
pub async fn save(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(mut b): Json<Stage>,
) -> Result<Json<Stage>, AppError> {
    let ws = parse_id(&raw)?;
    let access = authz::resolve(&state, &s, ws).await?;
    access.require(Permission::AdminSettings)?;
    b.label = b.label.trim().to_owned();
    if b.code.is_empty()
        || b.code.len() > 64
        || !b.code.starts_with(|c: char| c.is_ascii_lowercase())
        || !b
            .code
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        || b.label.is_empty()
        || b.label.chars().count() > 100
        || !(0..=10000).contains(&b.sort_order)
        || TicketStatus::from_code(&b.lifecycle).is_none()
    {
        return Err(AppError::InvalidRequest);
    }
    let mut tx = state.pool.begin().await?;
    // A workspace lock serializes changes with transitions and intake, including removal of an empty stage.
    sqlx::query("SELECT id FROM workspaces WHERE id=$1 FOR UPDATE")
        .bind(ws)
        .execute(&mut *tx)
        .await?;
    let old = load(&mut tx, ws)
        .await?
        .into_iter()
        .find(|x| x.code == b.code);
    match old {
        Some(ref stage) => {
            if b.version != stage.version {
                return Err(AppError::VersionConflict);
            }
            if b.lifecycle != stage.lifecycle {
                return Err(AppError::Validation("lifecycle"));
            }
            if stage.active && !b.active {
                let used:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tickets WHERE workspace_id=$1 AND coalesce(stage_code,workflow_status)=$2)").bind(ws).bind(&b.code).fetch_one(&mut *tx).await?;
                if used {
                    return Err(AppError::Conflict("stage_in_use"));
                }
                if matches!(b.lifecycle.as_str(), "new" | "open" | "resolved")
                    && !load(&mut tx, ws)
                        .await?
                        .iter()
                        .any(|x| x.active && x.code != b.code && x.lifecycle == b.lifecycle)
                {
                    return Err(AppError::Conflict("required_stage"));
                }
            }
            b.version += 1;
            sqlx::query("UPDATE workflow_stages SET label=$3,sort_order=$4,active=$5,version=$6 WHERE workspace_id=$1 AND code=$2").bind(ws).bind(&b.code).bind(&b.label).bind(b.sort_order).bind(b.active).bind(b.version).execute(&mut *tx).await?;
        }
        None => {
            if b.version != 0 || !b.code.starts_with("stage_") || b.lifecycle == "closed" {
                return Err(AppError::InvalidRequest);
            }
            b.version = 1;
            sqlx::query("INSERT INTO workflow_stages(workspace_id,code,label,lifecycle,sort_order,active) VALUES($1,$2,$3,$4,$5,$6)").bind(ws).bind(&b.code).bind(&b.label).bind(&b.lifecycle).bind(b.sort_order).bind(b.active).execute(&mut *tx).await?;
        }
    }
    audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "workflow.stage.save",
        "workspace",
        ws,
        serde_json::json!({"code":b.code,"active":b.active,"version":b.version}),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(b))
}
