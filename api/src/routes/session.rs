use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use uuid::Uuid;

use crate::config::{AccessMode, Environment};
use crate::error::AppError;
use crate::session::{self, Session};
use crate::{AppState, UAT_BANNER, db};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    environment: &'static str,
    access_mode: &'static str,
    banner: Option<&'static str>,
}

pub async fn meta(State(state): State<AppState>) -> Json<Meta> {
    let uat = state.config.access_mode == AccessMode::UatSuperadmin;
    Json(Meta {
        environment: match state.config.environment {
            Environment::Uat => "uat",
            Environment::Production => "production",
        },
        access_mode: state.config.access_mode.as_str(),
        banner: uat.then_some(UAT_BANNER),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    user: UserView,
    banner: Option<&'static str>,
    uat_superadmin: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserView {
    id: Uuid,
    display_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnterResponse {
    #[serde(flatten)]
    session: SessionView,
    csrf_token: String,
}

/// Direct UAT Superadmin entry. Only in UAT access mode on a marked test database;
/// re-checked per request so a database swapped after startup is still refused.
pub async fn uat_enter(State(state): State<AppState>) -> Result<Response, AppError> {
    if state.config.access_mode != AccessMode::UatSuperadmin
        || state.config.environment != Environment::Uat
    {
        return Err(AppError::NotFound);
    }
    if !db::is_marked_test_database(&state.pool).await? {
        tracing::warn!("uat entry refused: database is not marked as test");
        return Err(AppError::UatNotAvailable);
    }
    let user: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT u.id, u.display_name FROM users u \
         JOIN platform_grants g ON g.user_id = u.id AND g.grant_code = 'uat_superadmin' \
         WHERE u.active ORDER BY u.created_at, u.id LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?;
    let Some((user_id, display_name)) = user else {
        return Err(AppError::UatNotAvailable);
    };

    let issued = session::issue(&state.pool, user_id, AccessMode::UatSuperadmin.as_str()).await?;
    tracing::info!(user_id = %user_id, "uat superadmin session issued");
    let body = EnterResponse {
        session: SessionView {
            user: UserView {
                id: user_id,
                display_name,
            },
            banner: Some(UAT_BANNER),
            uat_superadmin: true,
        },
        csrf_token: issued.csrf_token,
    };
    Ok((
        StatusCode::CREATED,
        [(
            header::SET_COOKIE,
            session::set_cookie_value(state.config.cookie_secure, &issued.token),
        )],
        Json(body),
    )
        .into_response())
}

pub async fn current(
    State(state): State<AppState>,
    s: Session,
) -> Result<Json<SessionView>, AppError> {
    let display_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(s.user_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(SessionView {
        user: UserView {
            id: s.user_id,
            display_name,
        },
        banner: (state.config.access_mode == AccessMode::UatSuperadmin).then_some(UAT_BANNER),
        uat_superadmin: s.uat_superadmin,
    }))
}

pub async fn logout(State(state): State<AppState>, s: Session) -> Result<Response, AppError> {
    session::revoke(&state.pool, s.id).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            session::clear_cookie_value(state.config.cookie_secure),
        )],
    )
        .into_response())
}
