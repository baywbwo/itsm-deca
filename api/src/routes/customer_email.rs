use axum::{Json, extract::{Path, State}, http::HeaderMap};
use serde::{Deserialize, Serialize};
use crate::{AppState, authz::{self, Permission}, error::AppError, session::Session};
use super::{sync::audit_entity, ticket_list::correlation, workspaces::parse_id};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery { paused: bool }

pub async fn read(State(state): State<AppState>, session: Session, Path(raw): Path<String>)
    -> Result<Json<Delivery>, AppError> {
    let ws = parse_id(&raw)?;
    authz::resolve(&state, &session, ws).await?.require(Permission::TicketRead)?;
    let paused = sqlx::query_scalar("SELECT customer_email_paused FROM workspaces WHERE id=$1")
        .bind(ws).fetch_one(&state.pool).await?;
    Ok(Json(Delivery { paused }))
}

pub async fn update(State(state): State<AppState>, session: Session, Path(raw): Path<String>,
    headers: HeaderMap, Json(body): Json<Delivery>) -> Result<Json<Delivery>, AppError> {
    let ws = parse_id(&raw)?;
    let access = authz::resolve(&state, &session, ws).await?;
    access.require(Permission::IntegrationManage)?;
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query("UPDATE workspaces SET customer_email_paused=$2 WHERE id=$1 AND customer_email_paused <> $2")
        .bind(ws).bind(body.paused).execute(&mut *tx).await?;
    if changed.rows_affected() > 0 {
        audit_entity(&mut tx, ws, Some(access.user_id), "integration.customer_email.update",
            "workspace", ws, serde_json::json!({"paused":body.paused}), &correlation(&headers)).await?;
    }
    tx.commit().await?;
    Ok(Json(body))
}
