//! US-1.2 / TM-02: TOTP enrolment, the second login step and recovery codes (password mode).
//!
//! Seeds are sealed in `encrypted_secrets` (purpose `totp_seed`); recovery codes are stored as
//! HMAC-SHA256. The otpauth URI and the recovery codes appear in exactly one response each and
//! are never logged or audited; audit rows carry only event, outcome and a failure code.
use super::password::{auth_event, session_issued};
use super::ticket_list::body;
use crate::crypto::{self, Purpose};
use crate::mfa::{self, MfaKeys};
use crate::{AppState, config::AccessMode, error::AppError, session::Session};
use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgConnection;
use std::sync::Arc;
use uuid::Uuid;

const NO_STORE: [(header::HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];
const MAX_CODE_LEN: usize = 64;

/// `(enrolled, required)`: enrolled = a confirmed TOTP factor exists.
pub(crate) async fn status(
    conn: &mut PgConnection,
    user_id: Uuid,
) -> Result<(bool, bool), AppError> {
    Ok(sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM user_mfa_totp WHERE user_id = $1 AND confirmed_at IS NOT NULL), \
                itsm_mfa_required($1)",
    )
    .bind(user_id)
    .fetch_one(conn)
    .await?)
}

fn keys(state: &AppState) -> Result<Arc<MfaKeys>, AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    state.config.mfa.clone().ok_or_else(|| {
        tracing::warn!(
            error.kind = "mfa_keys_missing",
            "mfa requested without keys"
        );
        AppError::Unavailable
    })
}

/// US-1.2 login step 1 (called by `password::login` after a correct password for an enrolled
/// user): a single-use challenge bound to the current credential version. Older challenges of
/// the user are dropped.
pub(crate) async fn create_challenge(
    tx: &mut PgConnection,
    user_id: Uuid,
) -> Result<Uuid, AppError> {
    sqlx::query("DELETE FROM mfa_login_challenges WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(sqlx::query_scalar(
        "INSERT INTO mfa_login_challenges (user_id, credential_version, expires_at) \
         SELECT $1, credential_version, now() + make_interval(mins => $2) \
         FROM local_credentials WHERE user_id = $1 RETURNING id",
    )
    .bind(user_id)
    .bind(mfa::CHALLENGE_TTL_MINUTES)
    .fetch_one(&mut *tx)
    .await?)
}

/// Ten fresh recovery codes as a new batch; only their HMAC is stored.
async fn issue_recovery_codes(
    tx: &mut PgConnection,
    keys: &MfaKeys,
    user_id: Uuid,
) -> Result<Vec<String>, AppError> {
    let batch = Uuid::new_v4();
    let mut codes = Vec::with_capacity(mfa::RECOVERY_CODE_COUNT);
    for _ in 0..mfa::RECOVERY_CODE_COUNT {
        let code = mfa::new_recovery_code().ok_or(AppError::Unavailable)?;
        let normalized = mfa::normalize_recovery_code(&code).ok_or(AppError::Unavailable)?;
        sqlx::query(
            "INSERT INTO mfa_recovery_codes (user_id, batch_id, code_hash) VALUES ($1, $2, $3)",
        )
        .bind(user_id)
        .bind(batch)
        .bind(keys.recovery_hash(&normalized))
        .execute(&mut *tx)
        .await?;
        codes.push(code);
    }
    Ok(codes)
}

/// Counts a wrong second factor; the fifth in a row locks TOTP for 15 minutes (TM-02 d).
/// A lock that already ran out starts a fresh count. Returns whether this failure locked it.
async fn count_failure(tx: &mut PgConnection, user_id: Uuid) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "UPDATE user_mfa_totp SET \
           failed_attempts = CASE WHEN locked_until IS NOT NULL AND locked_until <= now() \
                                  THEN 1 ELSE failed_attempts + 1 END, \
           locked_until = CASE WHEN (CASE WHEN locked_until IS NOT NULL AND locked_until <= now() \
                                          THEN 1 ELSE failed_attempts + 1 END) >= $2 \
                               THEN now() + make_interval(mins => $3) ELSE NULL END \
         WHERE user_id = $1 RETURNING locked_until IS NOT NULL",
    )
    .bind(user_id)
    .bind(mfa::MAX_FAILED)
    .bind(mfa::LOCK_MINUTES)
    .fetch_one(tx)
    .await?)
}

/// A second factor as presented by the client.
pub(crate) enum Factor {
    Totp(String),
    Recovery(String),
}

impl Factor {
    fn len(&self) -> usize {
        match self {
            Factor::Totp(v) | Factor::Recovery(v) => v.len(),
        }
    }
}

/// Checks a second factor of `user_id` inside `tx` and audits the outcome: TOTP under
/// `totp_event` (`mfa_verify` at login, `step_up` for a step-up), recovery codes always as
/// `recovery_code_used`. The fifth failure in a row locks TOTP (TM-02 d); a recovery code still
/// works while TOTP is locked and clears the lock (TM-10). Returns false on any refusal; the
/// caller commits the audit and answers `mfa_invalid`.
async fn check_factor(
    state: &AppState,
    keys: &MfaKeys,
    tx: &mut PgConnection,
    user_id: Uuid,
    username: &str,
    factor: Factor,
    totp_event: &str,
) -> Result<bool, AppError> {
    let row: Option<(Uuid, Option<i64>, bool)> = sqlx::query_as(
        "SELECT secret_id, last_used_step, coalesce(locked_until > now(), false) \
         FROM user_mfa_totp WHERE user_id = $1 AND confirmed_at IS NOT NULL FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((secret_id, last_used_step, locked)) = row else {
        auth_event(
            tx,
            Some(user_id),
            username,
            totp_event,
            "denied",
            Some("not_enrolled"),
        )
        .await?;
        return Ok(false);
    };
    let (event, failure) = match factor {
        Factor::Totp(code) => {
            if locked {
                auth_event(
                    tx,
                    Some(user_id),
                    username,
                    totp_event,
                    "denied",
                    Some("locked"),
                )
                .await?;
                return Ok(false);
            }
            let seed = crypto::load_secret(&state.pool, &keys.keyring, secret_id, None, Purpose::TotpSeed)
                .await
                .map_err(|e| {
                    tracing::error!(error.kind = "totp_seed_unreadable", error = %e, "totp seed unavailable");
                    AppError::Unavailable
                })?;
            match mfa::verify_totp(seed.expose(), &code, mfa::now_step(), last_used_step) {
                Ok(step) => {
                    sqlx::query(
                        "UPDATE user_mfa_totp SET last_used_step = $2, failed_attempts = 0, \
                         locked_until = NULL, version = version + 1 WHERE user_id = $1",
                    )
                    .bind(user_id)
                    .bind(i64::try_from(step).map_err(|_| AppError::MfaInvalid)?)
                    .execute(&mut *tx)
                    .await?;
                    auth_event(tx, Some(user_id), username, totp_event, "success", None).await?;
                    return Ok(true);
                }
                Err(failure) => (totp_event, failure.as_str()),
            }
        }
        Factor::Recovery(recovery) => {
            let spent: Option<Uuid> = match mfa::normalize_recovery_code(&recovery) {
                None => None,
                Some(n) => {
                    sqlx::query_scalar(
                        "UPDATE mfa_recovery_codes SET used_at = now() \
                         WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL RETURNING id",
                    )
                    .bind(user_id)
                    .bind(keys.recovery_hash(&n))
                    .fetch_optional(&mut *tx)
                    .await?
                }
            };
            if spent.is_some() {
                sqlx::query(
                    "UPDATE user_mfa_totp SET failed_attempts = 0, locked_until = NULL, \
                     version = version + 1 WHERE user_id = $1",
                )
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
                auth_event(
                    tx,
                    Some(user_id),
                    username,
                    "recovery_code_used",
                    "success",
                    None,
                )
                .await?;
                return Ok(true);
            }
            ("recovery_code_used", "bad_code")
        }
    };
    let now_locked = count_failure(tx, user_id).await?;
    auth_event(tx, Some(user_id), username, event, "failed", Some(failure)).await?;
    if now_locked {
        auth_event(
            tx,
            Some(user_id),
            username,
            "lockout",
            "success",
            Some("mfa"),
        )
        .await?;
    }
    Ok(false)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LoginMfaBody {
    challenge_id: Uuid,
    code: Option<String>,
    recovery_code: Option<String>,
}

// user_id, expired, used, version_ok, enabled, username, display_name, must_change
type ChallengeRow = (Uuid, bool, bool, bool, bool, String, String, bool);

/// POST /login/mfa: completes a password login with a TOTP code or a recovery code. The session
/// is issued only here (TM-02). Recovery codes are not blocked by the TOTP lock (TM-10).
pub async fn login_mfa(
    State(state): State<AppState>,
    payload: Result<Json<LoginMfaBody>, JsonRejection>,
) -> Result<Response, AppError> {
    let keys = keys(&state)?;
    let b = body(payload)?;
    let factor = match (b.code, b.recovery_code) {
        (Some(c), None) => Factor::Totp(c),
        (None, Some(r)) => Factor::Recovery(r),
        _ => return Err(AppError::Validation("code")),
    };
    if factor.len() > MAX_CODE_LEN {
        return Err(AppError::MfaInvalid);
    }
    let mut tx = state.pool.begin().await?;
    let row: Option<ChallengeRow> = sqlx::query_as(
        "SELECT ch.user_id, ch.expires_at <= now(), ch.used_at IS NOT NULL, \
                ch.credential_version = c.credential_version, \
                u.active AND u.deactivated_at IS NULL, c.username, u.display_name, \
                c.must_change_password \
         FROM mfa_login_challenges ch JOIN users u ON u.id = ch.user_id \
         JOIN local_credentials c ON c.user_id = ch.user_id \
         WHERE ch.id = $1 FOR UPDATE OF ch",
    )
    .bind(b.challenge_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((user_id, expired, used, version_ok, enabled, username, display_name, must_change)) =
        row
    else {
        return Err(AppError::MfaInvalid);
    };
    if used {
        return Err(AppError::MfaInvalid);
    }
    if expired {
        return Err(AppError::MfaChallengeExpired);
    }
    if !version_ok || !enabled {
        let reason = if enabled {
            "stale_challenge"
        } else {
            "deactivated"
        };
        sqlx::query("UPDATE mfa_login_challenges SET used_at = now() WHERE id = $1")
            .bind(b.challenge_id)
            .execute(&mut *tx)
            .await?;
        auth_event(
            &mut tx,
            Some(user_id),
            &username,
            "mfa_verify",
            "denied",
            Some(reason),
        )
        .await?;
        tx.commit().await?;
        return Err(AppError::MfaInvalid);
    }
    if !check_factor(
        &state,
        &keys,
        &mut tx,
        user_id,
        &username,
        factor,
        "mfa_verify",
    )
    .await?
    {
        tx.commit().await?;
        return Err(AppError::MfaInvalid);
    }
    sqlx::query("UPDATE mfa_login_challenges SET used_at = now() WHERE id = $1")
        .bind(b.challenge_id)
        .execute(&mut *tx)
        .await?;
    auth_event(&mut tx, Some(user_id), &username, "login", "success", None).await?;
    let (_, required) = status(&mut tx, user_id).await?;
    tx.commit().await?;
    session_issued(
        &state,
        StatusCode::OK,
        user_id,
        display_name,
        must_change,
        true,
        required,
    )
    .await
}

/// GET /me/mfa
pub async fn get_status(State(state): State<AppState>, s: Session) -> Result<Response, AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    let mut conn = state.pool.acquire().await?;
    let (enrolled, required) = status(&mut conn, s.user_id).await?;
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mfa_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(s.user_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(
        Json(json!({"enrolled": enrolled, "required": required, "recoveryCodesLeft": left}))
            .into_response(),
    )
}

/// POST /me/mfa/enrollment: a new seed (an unconfirmed earlier one is replaced). The otpauth URI
/// carries the seed and is returned only here.
pub async fn start_enrollment(
    State(state): State<AppState>,
    s: Session,
) -> Result<Response, AppError> {
    let keys = keys(&state)?;
    let mut tx = state.pool.begin().await?;
    // Serialises concurrent enrolments of the same user.
    let username: String =
        sqlx::query_scalar("SELECT username FROM local_credentials WHERE user_id = $1 FOR UPDATE")
            .bind(s.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::Forbidden)?;
    let existing: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT secret_id, confirmed_at IS NOT NULL FROM user_mfa_totp WHERE user_id = $1 FOR UPDATE",
    )
    .bind(s.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    match existing {
        Some((_, true)) => return Err(AppError::Conflict("mfa_already_enrolled")),
        Some((old_secret, false)) => {
            sqlx::query("DELETE FROM user_mfa_totp WHERE user_id = $1")
                .bind(s.user_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM encrypted_secrets WHERE id = $1 AND purpose = 'totp_seed'")
                .bind(old_secret)
                .execute(&mut *tx)
                .await?;
        }
        None => {}
    }
    let mut seed = zeroize::Zeroizing::new([0u8; mfa::SEED_LEN]);
    getrandom::fill(seed.as_mut()).map_err(|_| AppError::Unavailable)?;
    let secret_id = crypto::store_secret(
        &mut tx,
        &keys.keyring,
        None,
        Purpose::TotpSeed,
        seed.as_ref(),
        Some(s.user_id),
    )
    .await
    .map_err(|e| {
        tracing::error!(error.kind = "totp_seed_store", error = %e, "totp seed not stored");
        AppError::Unavailable
    })?;
    let expires_at: String = sqlx::query_scalar(
        "INSERT INTO user_mfa_totp (user_id, secret_id) VALUES ($1, $2) \
         RETURNING to_char((enrolled_at + make_interval(mins => $3)) AT TIME ZONE 'UTC', \
                           'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .bind(s.user_id)
    .bind(secret_id)
    .bind(mfa::ENROLLMENT_TTL_MINUTES)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let uri = mfa::otpauth_uri(seed.as_ref(), &username);
    Ok((
        StatusCode::CREATED,
        NO_STORE,
        Json(json!({"enrollmentId": secret_id, "otpauthUri": uri, "expiresAt": expires_at})),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmBody {
    code: String,
}

/// POST /me/mfa/enrollment/{enrollment_id}/confirm: the first valid code activates the factor,
/// marks this session as MFA-verified and returns ten recovery codes once. A wrong code is
/// 422 `validation_failed` (field `code`), not 401: the session itself is still valid.
pub async fn confirm_enrollment(
    State(state): State<AppState>,
    s: Session,
    Path(enrollment_id): Path<String>,
    payload: Result<Json<ConfirmBody>, JsonRejection>,
) -> Result<Response, AppError> {
    let keys = keys(&state)?;
    let enrollment_id = Uuid::parse_str(&enrollment_id).map_err(|_| AppError::NotFound)?;
    let b = body(payload)?;
    if b.code.len() > MAX_CODE_LEN {
        return Err(AppError::Validation("code"));
    }
    let mut tx = state.pool.begin().await?;
    let username: String =
        sqlx::query_scalar("SELECT username FROM local_credentials WHERE user_id = $1 FOR UPDATE")
            .bind(s.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::Forbidden)?;
    let row: Option<(bool, bool)> = sqlx::query_as(
        "SELECT confirmed_at IS NOT NULL, enrolled_at + make_interval(mins => $3) <= now() \
         FROM user_mfa_totp WHERE user_id = $1 AND secret_id = $2 FOR UPDATE",
    )
    .bind(s.user_id)
    .bind(enrollment_id)
    .bind(mfa::ENROLLMENT_TTL_MINUTES)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((confirmed, expired)) = row else {
        return Err(AppError::NotFound);
    };
    if confirmed {
        return Err(AppError::Conflict("mfa_already_enrolled"));
    }
    if expired {
        return Err(AppError::MfaEnrollmentExpired);
    }
    let seed = crypto::load_secret(
        &state.pool,
        &keys.keyring,
        enrollment_id,
        None,
        Purpose::TotpSeed,
    )
    .await
    .map_err(|e| {
        tracing::error!(error.kind = "totp_seed_unreadable", error = %e, "totp seed unavailable");
        AppError::Unavailable
    })?;
    let step = match mfa::verify_totp(seed.expose(), &b.code, mfa::now_step(), None) {
        Ok(step) => i64::try_from(step).map_err(|_| AppError::Validation("code"))?,
        Err(failure) => {
            auth_event(
                &mut tx,
                Some(s.user_id),
                &username,
                "mfa_enroll",
                "failed",
                Some(failure.as_str()),
            )
            .await?;
            tx.commit().await?;
            return Err(AppError::Validation("code"));
        }
    };
    sqlx::query(
        "UPDATE user_mfa_totp SET confirmed_at = now(), last_used_step = $2, failed_attempts = 0, \
         locked_until = NULL, version = version + 1 WHERE user_id = $1",
    )
    .bind(s.user_id)
    .bind(step)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE mfa_recovery_codes SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    let codes = issue_recovery_codes(&mut tx, &keys, s.user_id).await?;
    sqlx::query("UPDATE sessions SET mfa_verified_at = now() WHERE id = $1")
        .bind(s.id)
        .execute(&mut *tx)
        .await?;
    auth_event(
        &mut tx,
        Some(s.user_id),
        &username,
        "mfa_enroll",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::OK,
        NO_STORE,
        Json(json!({"recoveryCodes": codes})),
    )
        .into_response())
}

/// POST /me/mfa/recovery-codes: a new batch of ten; every unused code of the old batch is spent.
/// Needs a fresh step-up (A-9).
pub async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    s: Session,
) -> Result<Response, AppError> {
    let keys = keys(&state)?;
    let mut tx = state.pool.begin().await?;
    let username: String =
        sqlx::query_scalar("SELECT username FROM local_credentials WHERE user_id = $1 FOR UPDATE")
            .bind(s.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::Forbidden)?;
    let (enrolled, _) = status(&mut tx, s.user_id).await?;
    if !enrolled {
        return Err(AppError::Conflict("mfa_not_enrolled"));
    }
    crate::authz::require_step_up(&state, &s)?;
    sqlx::query(
        "UPDATE mfa_recovery_codes SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    let codes = issue_recovery_codes(&mut tx, &keys, s.user_id).await?;
    auth_event(
        &mut tx,
        Some(s.user_id),
        &username,
        "recovery_codes_issue",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        NO_STORE,
        Json(json!({"recoveryCodes": codes})),
    )
        .into_response())
}

/// Drops every second-factor record of `user_id`: the TOTP factor and its sealed seed, recovery
/// codes and pending login challenges. Returns whether a factor (confirmed or not) existed.
pub(crate) async fn remove_factor(tx: &mut PgConnection, user_id: Uuid) -> Result<bool, AppError> {
    let secret: Option<Uuid> =
        sqlx::query_scalar("DELETE FROM user_mfa_totp WHERE user_id = $1 RETURNING secret_id")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(secret) = secret {
        sqlx::query("DELETE FROM encrypted_secrets WHERE id = $1 AND purpose = 'totp_seed'")
            .bind(secret)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM mfa_recovery_codes WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM mfa_login_challenges WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(secret.is_some())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepUpBody {
    code: String,
}

/// POST /session/step-up (A-9): a TOTP code (six digits) or a recovery code re-entered on this
/// session opens sensitive actions for five minutes. Failures count towards the same TOTP lock as
/// the login; a wrong, reused or locked factor is 401 `mfa_invalid`.
pub async fn step_up(
    State(state): State<AppState>,
    s: Session,
    payload: Result<Json<StepUpBody>, JsonRejection>,
) -> Result<Response, AppError> {
    let keys = keys(&state)?;
    let b = body(payload)?;
    if b.code.len() > MAX_CODE_LEN {
        return Err(AppError::MfaInvalid);
    }
    let trimmed = b.code.trim();
    // Recovery codes are 16 base32 characters, never six digits.
    let factor =
        if trimmed.len() == mfa::DIGITS as usize && trimmed.bytes().all(|c| c.is_ascii_digit()) {
            Factor::Totp(b.code)
        } else {
            Factor::Recovery(b.code)
        };
    let mut tx = state.pool.begin().await?;
    let username: String =
        sqlx::query_scalar("SELECT username FROM local_credentials WHERE user_id = $1 FOR UPDATE")
            .bind(s.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::Forbidden)?;
    let (enrolled, _) = status(&mut tx, s.user_id).await?;
    if !enrolled {
        return Err(AppError::Conflict("mfa_not_enrolled"));
    }
    if !check_factor(
        &state, &keys, &mut tx, s.user_id, &username, factor, "step_up",
    )
    .await?
    {
        tx.commit().await?;
        return Err(AppError::MfaInvalid);
    }
    sqlx::query("UPDATE sessions SET step_up_at = now() WHERE id = $1")
        .bind(s.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// DELETE /me/mfa: removes the caller's own second factor. Refused (409 `mfa_required_for_role`)
/// while MFA is mandatory for the caller (TM-02 a); needs a fresh step-up. Idempotent: without a
/// factor it answers 204 and changes nothing.
pub async fn delete_mfa(State(state): State<AppState>, s: Session) -> Result<Response, AppError> {
    keys(&state)?;
    let mut tx = state.pool.begin().await?;
    let username: String =
        sqlx::query_scalar("SELECT username FROM local_credentials WHERE user_id = $1 FOR UPDATE")
            .bind(s.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::Forbidden)?;
    let (enrolled, required) = status(&mut tx, s.user_id).await?;
    if required {
        return Err(AppError::Conflict("mfa_required_for_role"));
    }
    if !enrolled {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    crate::authz::require_step_up(&state, &s)?;
    remove_factor(&mut tx, s.user_id).await?;
    auth_event(
        &mut tx,
        Some(s.user_id),
        &username,
        "mfa_disable",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
