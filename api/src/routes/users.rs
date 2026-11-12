//! R1 I-1 CP2d: platform user administration (US-1.3 create, US-1.5 password reset,
//! US-1.7 deactivate). Only callers with the `platform_admin` grant; refusals leave a platform
//! denied-audit row. Temporary passwords are shown once and never stored or logged in clear.
use super::password::{HASH_SLOTS, auth_event_by, hash_password};
use super::ticket_list::body;
use crate::{AppState, config::AccessMode, error::AppError, session::Session};
use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

/// Capability named in platform denied audit rows for user administration.
pub const PLATFORM_USER_ADMIN: &str = "platform.users";

async fn platform_admin(state: &AppState, s: &Session) -> Result<(), AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    let granted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM platform_grants WHERE user_id=$1 AND grant_code='platform_admin')",
    )
    .bind(s.user_id)
    .fetch_one(&state.pool)
    .await?;
    if granted {
        Ok(())
    } else {
        tracing::info!(user_id = %s.user_id, "user administration denied");
        crate::denied_audit::record_platform(s.user_id, PLATFORM_USER_ADMIN);
        Err(AppError::Forbidden)
    }
}

fn user_id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

/// 24 URL-safe characters from 18 CSPRNG bytes; hashed off the async runtime.
async fn temporary_password() -> Result<(String, String), AppError> {
    let mut raw = [0u8; 18];
    getrandom::fill(&mut raw).map_err(|_| AppError::Unavailable)?;
    let password = URL_SAFE_NO_PAD.encode(raw);
    let clear = password.clone();
    let hash = tokio::task::spawn_blocking(move || hash_password(&clear))
        .await
        .map_err(|_| AppError::Unavailable)?
        .map_err(|_| AppError::Unavailable)?;
    Ok((password, hash))
}

fn hash_slot() -> Result<tokio::sync::SemaphorePermit<'static>, AppError> {
    HASH_SLOTS.try_acquire().map_err(|_| AppError::RateLimited {
        retry_after_secs: 5,
    })
}

fn no_store() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-store")]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateUserBody {
    username: String,
    display_name: String,
    #[serde(default)]
    email: Option<String>,
}

/// US-1.3: creates a local user with a one-time temporary password (must change on login).
pub async fn create(
    State(state): State<AppState>,
    s: Session,
    payload: Result<Json<CreateUserBody>, JsonRejection>,
) -> Result<Response, AppError> {
    platform_admin(&state, &s).await?;
    let b = body(payload)?;
    let username = b.username.trim().to_lowercase();
    if !(3..=254).contains(&username.len())
        || username
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(AppError::Validation("username"));
    }
    let display_name = b.display_name.trim().to_owned();
    if display_name.is_empty() || display_name.chars().count() > 200 {
        return Err(AppError::Validation("displayName"));
    }
    let email = match b.email.as_deref().map(|e| e.trim().to_lowercase()) {
        None => None,
        Some(e) if e.is_empty() => None,
        Some(e) if (3..=254).contains(&e.len()) && e.contains('@') && !e.contains(' ') => Some(e),
        Some(_) => return Err(AppError::Validation("email")),
    };
    let _permit = hash_slot()?;
    let (password, hash) = temporary_password().await?;
    let mut tx = state.pool.begin().await?;
    let taken: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM local_credentials WHERE username=$1)")
            .bind(&username)
            .fetch_one(&mut *tx)
            .await?;
    if taken {
        return Err(AppError::Conflict("username_taken"));
    }
    if let Some(e) = &email {
        let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email=$1)")
            .bind(e)
            .fetch_one(&mut *tx)
            .await?;
        if used {
            return Err(AppError::Conflict("email_taken"));
        }
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (display_name, email, created_by) VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(&display_name)
    .bind(&email)
    .bind(s.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(unique("users_email_uq", "email_taken"))?;
    sqlx::query(
        "INSERT INTO local_credentials (user_id, username, password_hash, must_change_password) \
         VALUES ($1,$2,$3,true)",
    )
    .bind(id)
    .bind(&username)
    .bind(hash)
    .execute(&mut *tx)
    .await
    .map_err(unique("local_credentials_username_key", "username_taken"))?;
    auth_event_by(
        &mut tx,
        Some(s.user_id),
        Some(id),
        &username,
        "user_create",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(actor = %s.user_id, user_id = %id, "user created");
    Ok((
        StatusCode::CREATED,
        no_store(),
        Json(json!({
            "user": {"id": id, "username": username, "displayName": display_name,
                     "email": email, "active": true, "mustChangePassword": true},
            "temporaryPassword": password,
        })),
    )
        .into_response())
}

/// Maps a concurrent unique violation on `constraint` to a 409 with `code`.
fn unique(constraint: &'static str, code: &'static str) -> impl Fn(sqlx::Error) -> AppError {
    move |e| match &e {
        sqlx::Error::Database(db) if db.constraint() == Some(constraint) => {
            AppError::Conflict(code)
        }
        _ => AppError::from(e),
    }
}

/// US-1.5: issues a new temporary password; every session of the user is revoked.
pub async fn reset_password(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Response, AppError> {
    platform_admin(&state, &s).await?;
    let id = user_id(&raw)?;
    let _permit = hash_slot()?;
    let (password, hash) = temporary_password().await?;
    let mut tx = state.pool.begin().await?;
    let (username, inactive): (String, bool) = sqlx::query_as(
        "SELECT c.username, (NOT u.active OR u.deactivated_at IS NOT NULL) \
         FROM users u JOIN local_credentials c ON c.user_id = u.id WHERE u.id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if inactive {
        return Err(AppError::Conflict("user_inactive"));
    }
    sqlx::query(
        "UPDATE local_credentials SET password_hash=$2, credential_version=credential_version+1, \
         must_change_password=true, failed_attempts=0, locked_until=NULL, updated_at=now() \
         WHERE user_id=$1",
    )
    .bind(id)
    .bind(hash)
    .execute(&mut *tx)
    .await?;
    revoke_all(&mut tx, id).await?;
    auth_event_by(
        &mut tx,
        Some(s.user_id),
        Some(id),
        &username,
        "password_reset_issue",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(actor = %s.user_id, user_id = %id, "temporary password issued");
    Ok((
        StatusCode::OK,
        no_store(),
        Json(json!({"temporaryPassword": password})),
    )
        .into_response())
}

async fn revoke_all(tx: &mut sqlx::PgConnection, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sessions SET revoked_at=now(), revoked_reason='admin_revoke' \
         WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(tx)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserBody {
    #[serde(default)]
    active: Option<bool>,
}

/// US-1.7: only `{"active": false}` for now. History stays; sessions are revoked. The last
/// active `platform_admin` cannot be deactivated.
pub async fn update(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
    payload: Result<Json<UpdateUserBody>, JsonRejection>,
) -> Result<Response, AppError> {
    platform_admin(&state, &s).await?;
    let id = user_id(&raw)?;
    // Fields other than `active` (incl. `version`) are deferred: refused as 422, not 400.
    let b = body(payload).map_err(|_| AppError::Validation("active"))?;
    if b.active != Some(false) {
        return Err(AppError::Validation("active"));
    }
    let mut tx = state.pool.begin().await?;
    // Lock every active admin so two concurrent deactivations cannot both pass the check.
    let admins: Vec<Uuid> = sqlx::query_scalar(
        "SELECT u.id FROM users u JOIN platform_grants g ON g.user_id = u.id \
         WHERE g.grant_code='platform_admin' AND u.active AND u.deactivated_at IS NULL \
         ORDER BY u.id FOR UPDATE OF u",
    )
    .fetch_all(&mut *tx)
    .await?;
    let inactive: bool = sqlx::query_scalar(
        "SELECT (NOT active OR deactivated_at IS NOT NULL) FROM users WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if inactive {
        return Err(AppError::Conflict("user_inactive"));
    }
    if admins == [id] {
        return Err(AppError::Conflict("last_admin"));
    }
    sqlx::query("UPDATE users SET active=false, deactivated_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    revoke_all(&mut tx, id).await?;
    auth_event_by(
        &mut tx,
        Some(s.user_id),
        Some(id),
        "",
        "user_deactivate",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(actor = %s.user_id, user_id = %id, "user deactivated");
    Ok(Json(json!({"id": id, "active": false})).into_response())
}
