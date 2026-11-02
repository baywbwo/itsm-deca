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
static HASH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
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
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid,String,String,bool)> = sqlx::query_as("SELECT u.id, u.display_name, c.password_hash, coalesce(c.locked_until > now(), false) FROM local_credentials c JOIN users u ON u.id=c.user_id WHERE c.username=$1 AND u.active FOR UPDATE OF c")
        .bind(b.username.trim().to_lowercase()).fetch_optional(&mut *tx).await?;
    let hash = row
        .as_ref()
        .map(|r| r.2.clone())
        .unwrap_or_else(|| DUMMY.into());
    let valid = tokio::task::spawn_blocking(move || verify(&hash, &b.password))
        .await
        .map_err(|_| AppError::Unavailable)?;
    let Some((user_id, display_name, _, locked)) = row else {
        return Err(AppError::Unauthenticated);
    };
    if locked {
        return Err(AppError::Unauthenticated);
    }
    if !valid {
        sqlx::query("UPDATE local_credentials SET failed_attempts=failed_attempts+1, locked_until=CASE WHEN failed_attempts+1 >= 5 THEN now()+interval '15 minutes' ELSE NULL END WHERE user_id=$1").bind(user_id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Err(AppError::Unauthenticated);
    }
    sqlx::query(
        "UPDATE local_credentials SET failed_attempts=0, locked_until=NULL WHERE user_id=$1",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let issued = session::issue(&state.pool, user_id, "password").await?;
    Ok((StatusCode::CREATED,[(header::SET_COOKIE,session::set_cookie_value(true,&issued.token))],Json(json!({"user":{"id":user_id,"displayName":display_name},"banner":null,"uatSuperadmin":false,"csrfToken":issued.csrf_token}))).into_response())
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
