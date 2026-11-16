use super::{sync::audit_entity, ticket_list::correlation, workspaces::parse_id};
use crate::{
    AppState,
    authz::{self, Permission},
    error::AppError,
    session::Session,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    paused: bool,
}

pub async fn read(
    State(state): State<AppState>,
    session: Session,
    Path(raw): Path<String>,
) -> Result<Json<Delivery>, AppError> {
    let ws = parse_id(&raw)?;
    authz::resolve(&state, &session, ws)
        .await?
        .require(Permission::TicketRead)?;
    let paused = sqlx::query_scalar("SELECT customer_email_paused FROM workspaces WHERE id=$1")
        .bind(ws)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(Delivery { paused }))
}

pub async fn update(
    State(state): State<AppState>,
    session: Session,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Delivery>,
) -> Result<Json<Delivery>, AppError> {
    let ws = parse_id(&raw)?;
    let access = authz::resolve(&state, &session, ws).await?;
    access.require(Permission::IntegrationManage)?;
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query("UPDATE workspaces SET customer_email_paused=$2 WHERE id=$1 AND customer_email_paused <> $2")
        .bind(ws).bind(body.paused).execute(&mut *tx).await?;
    if changed.rows_affected() > 0 {
        audit_entity(
            &mut tx,
            ws,
            Some(access.user_id),
            "integration.customer_email.update",
            "workspace",
            ws,
            serde_json::json!({"paused":body.paused}),
            &correlation(&headers),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(body))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Relay {
    host: String,
    port: u16,
    sender: String,
    username: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayInput {
    relay: Relay,
    password: Option<String>,
}

type RelayRow = (Option<sqlx::types::Json<Relay>>, Option<uuid::Uuid>, bool);

async fn relay_row(pool: &sqlx::PgPool, ws: uuid::Uuid) -> Result<RelayRow, AppError> {
    Ok(sqlx::query_as("SELECT customer_smtp,customer_smtp_secret,customer_email_paused FROM workspaces WHERE id=$1")
        .bind(ws).fetch_one(pool).await?)
}

pub async fn read_relay(
    State(state): State<AppState>,
    session: Session,
    Path(raw): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let access = authz::resolve(&state, &session, parse_id(&raw)?).await?;
    access.require(Permission::IntegrationManage)?;
    let (relay, secret, _) = relay_row(&state.pool, access.workspace_id).await?;
    Ok(Json(
        serde_json::json!({"relay":relay.map(|r|r.0),"hasPassword":secret.is_some(),
        "canEdit": access.allows(Permission::IntegrationCredentials), "keyringConfigured":state.config.keyring.is_some()}),
    ))
}

pub async fn save_relay(
    State(state): State<AppState>,
    session: Session,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RelayInput>,
) -> Result<axum::http::StatusCode, AppError> {
    use crate::crypto::{self, Purpose};
    let ws = parse_id(&raw)?;
    let access = authz::resolve(&state, &session, ws).await?;
    access.require(Permission::IntegrationManage)?;
    access.require(Permission::IntegrationCredentials)?;
    let mut relay = input.relay;
    relay.host = relay.host.trim().to_ascii_lowercase();
    relay.sender = relay.sender.trim().to_owned();
    relay.username = relay.username.trim().to_owned();
    let allowlist =
        std::env::var("ITSM_SMTP_HOSTS").unwrap_or_else(|_| "smtp.gmail.com".to_owned());
    let sender_ok = relay
        .sender
        .split_once('@')
        .is_some_and(|(user, host)| !user.is_empty() && host.contains('.') && !host.contains('@'));
    if !allowlist
        .split(',')
        .any(|host| host.trim().eq_ignore_ascii_case(&relay.host))
        || relay.host.is_empty()
        || !matches!(relay.port, 465 | 587)
        || !sender_ok
        || relay.sender.len() > 254
        || relay
            .sender
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || ",;<>".contains(c))
        || relay.username.is_empty()
        || relay.username.len() > 254
        || relay.username.chars().any(char::is_control)
    {
        return Err(AppError::Validation("relay"));
    }
    let keyring = state
        .config
        .keyring
        .as_ref()
        .ok_or(AppError::KeyringMissing)?;
    let password = zeroize::Zeroizing::new(input.password.unwrap_or_default());
    let mut tx = state.pool.begin().await?;
    let (previous, existing): (Option<sqlx::types::Json<Relay>>, Option<uuid::Uuid>) =
        sqlx::query_as(
            "SELECT customer_smtp,customer_smtp_secret FROM workspaces WHERE id=$1 FOR UPDATE",
        )
        .bind(ws)
        .fetch_one(&mut *tx)
        .await?;
    let secret = if password.is_empty() {
        if !previous.is_some_and(|p| {
            p.host == relay.host && p.port == relay.port && p.username == relay.username
        }) {
            return Err(AppError::Validation("password"));
        }
        existing.ok_or(AppError::Validation("password"))?
    } else {
        if password.len() > 4096 || password.chars().any(char::is_control) {
            return Err(AppError::Validation("password"));
        }
        crypto::store_secret(
            &mut tx,
            keyring,
            Some(ws),
            Purpose::SmtpPassword,
            password.as_bytes(),
            Some(access.user_id),
        )
        .await
        .map_err(AppError::internal)?
    };
    sqlx::query("UPDATE workspaces SET customer_smtp=$2,customer_smtp_secret=$3 WHERE id=$1")
        .bind(ws)
        .bind(sqlx::types::Json(relay))
        .bind(secret)
        .execute(&mut *tx)
        .await?;
    audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "integration.customer_email.configure",
        "workspace",
        ws,
        serde_json::json!({"credentialChanged":!password.is_empty()}),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn worker_relay(
    State(state): State<AppState>,
    session: Session,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    use crate::crypto::{self, Purpose};
    use subtle::ConstantTimeEq;
    let ws = parse_id(&raw)?;
    let access = authz::resolve(&state, &session, ws).await?;
    access.require(Permission::TicketRead)?;
    let token = std::env::var("ITSM_SMTP_WORKER_TOKEN").map_err(|_| AppError::Forbidden)?;
    let supplied = headers
        .get("x-itsm-smtp-worker")
        .ok_or(AppError::Forbidden)?;
    if token.len() < 32 || !bool::from(token.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(AppError::Forbidden);
    }
    let (relay, id, paused) = relay_row(&state.pool, ws).await?;
    let (Some(relay), Some(id), false) = (relay, id, paused) else {
        return Ok(Json(serde_json::json!({"relay":null})));
    };
    let ring = state
        .config
        .keyring
        .as_ref()
        .ok_or(AppError::KeyringMissing)?;
    let secret = crypto::load_secret(&state.pool, ring, id, Some(ws), Purpose::SmtpPassword)
        .await
        .map_err(AppError::internal)?;
    let password = std::str::from_utf8(secret.expose()).map_err(AppError::internal)?;
    Ok(Json(
        serde_json::json!({"relay":relay.0,"password":password}),
    ))
}
