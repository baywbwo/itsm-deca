//! Temporary local authentication. Credentials and sessions are scoped to one deployment DB.
use super::ticket_list::body;
use crate::{AppState, config::AccessMode, error::AppError, session};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
pub(crate) static HASH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$MTIzNDU2Nzg5MDEyMzQ1Ng$RErWP3YjxuMHT9L/ITq8biVGsJy3t58HdRDSuZqSwSc";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginBody {
    username: String,
    password: String,
}

pub fn hash_password(password: &str) -> Result<String, String> {
    if password.chars().count() < 12 || password.len() > 1024 {
        return Err("Password minimal 12 karakter dan maksimal 1024 byte".into());
    }
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| "random unavailable")?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| "salt failed")?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|v| v.to_string())
        .map_err(|_| "hash failed".into())
}
fn verify(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}
/// Appends an auth audit row for the user's own action (no actor).
pub(crate) async fn auth_event(
    tx: &mut sqlx::PgConnection,
    user_id: Option<Uuid>,
    username: &str,
    event: &str,
    outcome: &str,
    failure_code: Option<&str>,
) -> Result<(), AppError> {
    auth_event_by(tx, None, user_id, username, event, outcome, failure_code).await
}

/// Appends an auth audit row; `actor` is the admin acting on `user_id` (US-1.3/1.5/1.7).
/// Unknown usernames are kept only as a SHA-256 digest; no secrets.
pub(crate) async fn auth_event_by(
    tx: &mut sqlx::PgConnection,
    actor: Option<Uuid>,
    user_id: Option<Uuid>,
    username: &str,
    event: &str,
    outcome: &str,
    failure_code: Option<&str>,
) -> Result<(), AppError> {
    use sha2::{Digest, Sha256};
    let username_hash = user_id
        .is_none()
        .then(|| Sha256::digest(username.as_bytes()).to_vec());
    sqlx::query("INSERT INTO auth_events(user_id,username_hash,event,outcome,failure_code,actor_id) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(user_id).bind(username_hash).bind(event).bind(outcome).bind(failure_code).bind(actor)
        .execute(tx).await?;
    Ok(())
}
pub async fn login(
    State(state): State<AppState>,
    payload: Result<Json<LoginBody>, JsonRejection>,
) -> Result<Response, AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    crate::db::verify_runtime_guard(&state.config, &state.pool)
        .await
        .map_err(|_| AppError::Unavailable)?;
    let b = body(payload)?;
    if b.username.len() > 254 || b.password.len() > 1024 {
        return Err(AppError::Unauthenticated);
    }
    let _permit = HASH_SLOTS
        .try_acquire()
        .map_err(|_| AppError::RateLimited {
            retry_after_secs: 5,
        })?;
    let username = b.username.trim().to_lowercase();
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid,String,String,bool,bool,bool)> = sqlx::query_as("SELECT u.id, u.display_name, c.password_hash, coalesce(c.locked_until > now(), false), u.active AND u.deactivated_at IS NULL, c.must_change_password FROM local_credentials c JOIN users u ON u.id=c.user_id WHERE c.username=$1 FOR UPDATE OF c")
        .bind(&username).fetch_optional(&mut *tx).await?;
    let hash = row
        .as_ref()
        .map(|r| r.2.clone())
        .unwrap_or_else(|| DUMMY.into());
    let valid = tokio::task::spawn_blocking(move || verify(&hash, &b.password))
        .await
        .map_err(|_| AppError::Unavailable)?;
    let Some((user_id, display_name, _, locked, enabled, must_change)) = row else {
        auth_event(
            &mut tx,
            None,
            &username,
            "login",
            "failed",
            Some("unknown_user"),
        )
        .await?;
        tx.commit().await?;
        return Err(AppError::Unauthenticated);
    };
    // US-1.7: deactivated users never get a session, whatever the password.
    if !enabled {
        auth_event(
            &mut tx,
            Some(user_id),
            &username,
            "login",
            "denied",
            Some("deactivated"),
        )
        .await?;
        tx.commit().await?;
        return Err(AppError::Unauthenticated);
    }
    if locked {
        auth_event(
            &mut tx,
            Some(user_id),
            &username,
            "login",
            "denied",
            Some("locked"),
        )
        .await?;
        tx.commit().await?;
        return Err(AppError::Unauthenticated);
    }
    if !valid {
        // US-1.1: 5 consecutive failures lock the account for 15 minutes.
        let now_locked: bool = sqlx::query_scalar("UPDATE local_credentials SET failed_attempts=failed_attempts+1, locked_until=CASE WHEN failed_attempts+1 >= 5 THEN now()+interval '15 minutes' ELSE NULL END WHERE user_id=$1 RETURNING locked_until IS NOT NULL").bind(user_id).fetch_one(&mut *tx).await?;
        auth_event(
            &mut tx,
            Some(user_id),
            &username,
            "login",
            "failed",
            Some("bad_password"),
        )
        .await?;
        if now_locked {
            auth_event(
                &mut tx,
                Some(user_id),
                &username,
                "lockout",
                "success",
                None,
            )
            .await?;
        }
        tx.commit().await?;
        return Err(AppError::Unauthenticated);
    }
    sqlx::query(
        "UPDATE local_credentials SET failed_attempts=0, locked_until=NULL WHERE user_id=$1",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    // US-1.2: an enrolled user gets a challenge instead of a session (401 mfa_required). A user
    // who must use MFA but has not enrolled gets a pre-MFA session that can only enrol.
    // ITSM_MFA=off (default): plain password login; stored factors are ignored, not removed.
    let (mfa_enrolled, mfa_required) = if state.config.mfa_enabled {
        super::mfa::status(&mut tx, user_id).await?
    } else {
        (false, false)
    };
    if mfa_enrolled {
        if state.config.mfa.is_none() {
            tracing::warn!(error.kind = "mfa_keys_missing", "mfa login without keys");
            return Err(AppError::Unavailable);
        }
        let challenge_id = super::mfa::create_challenge(&mut tx, user_id).await?;
        tx.commit().await?;
        return Err(AppError::MfaRequired { challenge_id });
    }
    auth_event(&mut tx, Some(user_id), &username, "login", "success", None).await?;
    tx.commit().await?;
    session_issued(
        &state,
        StatusCode::CREATED,
        user_id,
        display_name,
        must_change,
        false,
        mfa_required,
    )
    .await
}

/// Issues a password-mode session and answers with the session body, cookie and CSRF token.
pub(crate) async fn session_issued(
    state: &AppState,
    status: StatusCode,
    user_id: Uuid,
    display_name: String,
    must_change: bool,
    mfa_verified: bool,
    mfa_required: bool,
) -> Result<Response, AppError> {
    let issued = session::issue_with_mfa(&state.pool, user_id, "password", mfa_verified).await?;
    let platform_admin = super::users::is_platform_admin(&state.pool, user_id).await?;
    Ok((status,[(header::SET_COOKIE,session::set_cookie_value(true,&issued.token))],Json(json!({"user":{"id":user_id,"displayName":display_name},"banner":null,"uatSuperadmin":false,"mustChangePassword":must_change,"platformAdmin":platform_admin,"mfaEnabled":state.config.mfa_enabled,"mfaEnrolled":mfa_verified,"mfaRequired":mfa_required,"csrfToken":issued.csrf_token}))).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChangePasswordBody {
    current_password: String,
    new_password: String,
}
/// US-1.4: policy check and hashing run off the async runtime. Returns the failure code.
fn check_new_password(
    hash: &str,
    username: &str,
    current: &str,
    new: &str,
) -> Result<String, &'static str> {
    if !verify(hash, current) {
        return Err("bad_password");
    }
    if new.chars().count() < 12 || new.len() > 1024 {
        return Err("too_short");
    }
    if new.trim().to_lowercase() == username {
        return Err("same_as_username");
    }
    if verify(hash, new) {
        return Err("reused");
    }
    hash_password(new).map_err(|_| "hash_failed")
}
/// US-1.4: changes the caller's own password. Other sessions are revoked; this one stays.
pub async fn change_password(
    State(state): State<AppState>,
    s: session::Session,
    payload: Result<Json<ChangePasswordBody>, JsonRejection>,
) -> Result<Response, AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    let b = body(payload)?;
    if b.current_password.len() > 1024 || b.new_password.len() > 1024 {
        return Err(AppError::Validation("newPassword"));
    }
    let _permit = HASH_SLOTS
        .try_acquire()
        .map_err(|_| AppError::RateLimited {
            retry_after_secs: 5,
        })?;
    let mut tx = state.pool.begin().await?;
    let (username, hash): (String, String) = sqlx::query_as(
        "SELECT username, password_hash FROM local_credentials WHERE user_id=$1 FOR UPDATE",
    )
    .bind(s.user_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Forbidden)?;
    let name = username.clone();
    let checked = tokio::task::spawn_blocking(move || {
        check_new_password(&hash, &name, &b.current_password, &b.new_password)
    })
    .await
    .map_err(|_| AppError::Unavailable)?;
    let new_hash = match checked {
        Ok(h) => h,
        Err(code) => {
            auth_event(
                &mut tx,
                Some(s.user_id),
                &username,
                "password_change",
                "failed",
                Some(code),
            )
            .await?;
            tx.commit().await?;
            return Err(AppError::Validation(if code == "bad_password" {
                "currentPassword"
            } else {
                "newPassword"
            }));
        }
    };
    let version: i32 = sqlx::query_scalar("UPDATE local_credentials SET password_hash=$2, credential_version=credential_version+1, must_change_password=false, password_changed_at=now(), failed_attempts=0, locked_until=NULL WHERE user_id=$1 RETURNING credential_version")
        .bind(s.user_id).bind(new_hash).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE sessions SET revoked_at=now(), revoked_reason='password_change' WHERE user_id=$1 AND id<>$2 AND revoked_at IS NULL")
        .bind(s.user_id).bind(s.id).execute(&mut *tx).await?;
    sqlx::query("UPDATE sessions SET credential_version=$2 WHERE id=$1")
        .bind(s.id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    auth_event(
        &mut tx,
        Some(s.user_id),
        &username,
        "password_change",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hashes_and_verifies() {
        let hash = hash_password("synthetic-password-123").unwrap_or_default();
        assert!(verify(&hash, "synthetic-password-123"));
        assert!(!verify(&hash, "wrong"));
        assert!(hash_password("short").is_err());
    }
}
