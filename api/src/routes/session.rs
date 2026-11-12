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
    /// CP5c: company sign-in through OIDC is offered (`ITSM_AUTH` sso|both in password mode).
    sso: bool,
    banner: Option<&'static str>,
    /// Present only in UAT Superadmin mode; the FE shows it on every screen.
    uat: Option<UatNotice>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UatNotice {
    banner: &'static str,
    /// The banner cannot be dismissed.
    persistent: bool,
    /// Only synthetic data is allowed in this mode.
    data_scope: &'static str,
    /// Superadmin entry is for testing only; it does not prove CS / Lead CS access rules.
    superadmin_proves_rbac: bool,
}

fn uat_notice() -> UatNotice {
    UatNotice {
        banner: UAT_BANNER,
        persistent: true,
        data_scope: "dummy",
        superadmin_proves_rbac: false,
    }
}

pub async fn meta(State(state): State<AppState>) -> Json<Meta> {
    let uat = state.config.environment == Environment::Uat;
    Json(Meta {
        environment: match state.config.environment {
            Environment::Uat => "uat",
            Environment::Production => "production",
        },
        access_mode: state.config.access_mode.as_str(),
        sso: state.config.access_mode == AccessMode::Password && state.config.auth.methods.oidc(),
        banner: uat.then_some(UAT_BANNER),
        uat: uat.then(uat_notice),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    user: UserView,
    banner: Option<&'static str>,
    uat_superadmin: bool,
    /// US-1.4: the client shows only the change-password screen until this is false.
    must_change_password: bool,
    /// CP3b: the client shows the "Pengguna" admin page only when true (password mode only).
    platform_admin: bool,
    /// `ITSM_MFA`: the server offers MFA at all. False = the client hides every MFA screen.
    mfa_enabled: bool,
    /// US-1.2: a confirmed TOTP factor exists (password mode only).
    mfa_enrolled: bool,
    /// US-1.2: MFA is mandatory for this user's grants; until this session passes it, only the
    /// MFA enrolment paths are reachable (pre-MFA session).
    mfa_required: bool,
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
            must_change_password: false,
            platform_admin: false,
            mfa_enabled: false,
            mfa_enrolled: false,
            mfa_required: false,
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
    let password_mode = state.config.access_mode == AccessMode::Password;
    let platform_admin =
        password_mode && super::users::is_platform_admin(&state.pool, s.user_id).await?;
    let mfa_enabled = password_mode && state.config.mfa_enabled;
    let (mfa_enrolled, mfa_required) = if mfa_enabled {
        super::mfa::status(&mut *state.pool.acquire().await?, s.user_id).await?
    } else {
        (false, false)
    };
    Ok(Json(SessionView {
        user: UserView {
            id: s.user_id,
            display_name,
        },
        banner: (state.config.access_mode == AccessMode::UatSuperadmin).then_some(UAT_BANNER),
        uat_superadmin: s.uat_superadmin,
        must_change_password: s.must_change_password,
        platform_admin,
        mfa_enabled,
        mfa_enrolled,
        mfa_required,
    }))
}

/// Ends this session only. In password mode the logout is audited (TM-15, found in CP3c E2E).
pub async fn logout(State(state): State<AppState>, s: Session) -> Result<Response, AppError> {
    let mut tx = state.pool.begin().await?;
    let revoked = sqlx::query(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = 'logout' \
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(s.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if revoked == 1 && state.config.access_mode == AccessMode::Password {
        super::password::auth_event(&mut tx, Some(s.user_id), "", "logout", "success", None)
            .await?;
    }
    tx.commit().await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            session::clear_cookie_value(state.config.cookie_secure),
        )],
    )
        .into_response())
}

/// US-1.6: "Keluar dari semua perangkat". Revokes every session of the caller, this one included.
pub async fn logout_all(State(state): State<AppState>, s: Session) -> Result<Response, AppError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = 'logout_all' \
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO auth_events (user_id, event, outcome) VALUES ($1, 'logout_all', 'success')",
    )
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            session::clear_cookie_value(state.config.cookie_secure),
        )],
    )
        .into_response())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CsrfIssued {
    csrf_token: String,
}

/// D11: a reloaded tab still has its session cookie but not the in-memory CSRF token.
/// The session layer only lets this through with the refresh header from the same origin.
pub async fn refresh_csrf(
    State(state): State<AppState>,
    s: Session,
) -> Result<Json<CsrfIssued>, AppError> {
    let csrf_token = session::rotate_csrf(&state.pool, s.id).await?;
    tracing::info!(session_id = %s.id, "csrf token re-issued");
    Ok(Json(CsrfIssued { csrf_token }))
}
