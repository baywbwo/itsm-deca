//! R1 I-1 CP2d: platform user administration (US-1.3 create, US-1.5 password reset,
//! US-1.7 deactivate). Only callers with the `platform_admin` grant; refusals leave a platform
//! denied-audit row. Temporary passwords are shown once and never stored or logged in clear.
//! CP3b adds the user directory (`GET /users`).
use super::password::{HASH_SLOTS, auth_event_by, hash_password};
use super::ticket_list::{body, decode_cursor, encode_cursor, non_empty, parse_limit};
use crate::{AppState, config::AccessMode, error::AppError, session::Session};
use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

/// Capability named in platform denied audit rows for user administration.
pub const PLATFORM_USER_ADMIN: &str = "platform.users";

/// Whether `user` holds the `platform_admin` grant (also shown to the client in the session).
pub(crate) async fn is_platform_admin(pool: &sqlx::PgPool, user: Uuid) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM platform_grants WHERE user_id=$1 AND grant_code='platform_admin')",
    )
    .bind(user)
    .fetch_one(pool)
    .await?)
}

async fn platform_admin(state: &AppState, s: &Session) -> Result<(), AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    if is_platform_admin(&state.pool, s.user_id).await? {
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
    crate::authz::require_step_up(&state, &s)?;
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
    crate::authz::require_step_up(&state, &s)?;
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
    crate::authz::require_step_up(&state, &s)?;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MfaResetBody {
    reason: String,
}

/// TM-02 (g): another platform admin removes a user's second factor (lost device). Factor, seed,
/// recovery codes and pending challenges are deleted and every session is revoked; a user for
/// whom MFA is mandatory must enrol again at the next login. The reason is kept in the audit row.
/// Idempotent (204 also without a factor). Needs a fresh step-up.
pub async fn mfa_reset(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
    payload: Result<Json<MfaResetBody>, JsonRejection>,
) -> Result<Response, AppError> {
    platform_admin(&state, &s).await?;
    crate::authz::require_step_up(&state, &s)?;
    let id = user_id(&raw)?;
    let b = body(payload)?;
    let reason = b.reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(AppError::Validation("reason"));
    }
    // Resetting one's own factor would let a stolen admin session drop MFA (TM-02 g).
    if id == s.user_id {
        return Err(AppError::Conflict("self_mfa_reset"));
    }
    let mut tx = state.pool.begin().await?;
    let _: Uuid =
        sqlx::query_scalar("SELECT user_id FROM local_credentials WHERE user_id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::NotFound)?;
    super::mfa::remove_factor(&mut tx, id).await?;
    sqlx::query(
        "UPDATE sessions SET revoked_at=now(), revoked_reason='mfa_reset' \
         WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO auth_events(user_id,event,outcome,actor_id,reason) \
         VALUES($1,'mfa_reset','success',$2,$3)",
    )
    .bind(id)
    .bind(s.user_id)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(actor = %s.user_id, user_id = %id, "mfa reset");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// PD-1: clears the password and TOTP lockout of a user before it runs out. Audited as
/// `user_unlock`. Sessions are untouched. Needs a fresh step-up (04-fsd, S-12).
pub async fn unlock(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Response, AppError> {
    platform_admin(&state, &s).await?;
    crate::authz::require_step_up(&state, &s)?;
    let id = user_id(&raw)?;
    let mut tx = state.pool.begin().await?;
    let username: String =
        sqlx::query_scalar("SELECT username FROM local_credentials WHERE user_id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::NotFound)?;
    sqlx::query(
        "UPDATE local_credentials SET failed_attempts=0, locked_until=NULL, updated_at=now() \
         WHERE user_id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE user_mfa_totp SET failed_attempts=0, locked_until=NULL, version=version+1 \
         WHERE user_id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    auth_event_by(
        &mut tx,
        Some(s.user_id),
        Some(id),
        &username,
        "user_unlock",
        "success",
        None,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(actor = %s.user_id, user_id = %id, "user unlocked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

const LIST_DEFAULT_LIMIT: i64 = 50;
const LIST_MAX_LIMIT: i64 = 100;
const MAX_SEARCH_CHARS: usize = 100;

#[derive(Debug, Default, Deserialize)]
pub struct ListUsersQuery {
    q: Option<String>,
    active: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct UserListItem {
    id: Uuid,
    /// Absent for users without a local credential (e.g. synthetic UAT fixtures).
    username: Option<String>,
    display_name: String,
    email: Option<String>,
    active: bool,
    must_change_password: bool,
    /// Password or TOTP lockout in force (PD-1).
    locked: bool,
    /// A confirmed TOTP factor exists (S-12). No factor data leaves the server.
    mfa_enrolled: bool,
    platform_admin: bool,
    created_at: String,
    deactivated_at: Option<String>,
    #[serde(skip)]
    sort_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserList {
    items: Vec<UserListItem>,
    next_cursor: Option<String>,
}

/// Columns only: no hash, lockout counters or credential version leave the server.
const LIST_SQL: &str = concat!(
    "SELECT u.id, c.username, u.display_name, u.email, \
     (u.active AND u.deactivated_at IS NULL) AS active, \
     coalesce(c.must_change_password, false) AS must_change_password, \
     (coalesce(c.locked_until > now(), false) OR EXISTS (SELECT 1 FROM user_mfa_totp t \
       WHERE t.user_id = u.id AND t.locked_until > now())) AS locked, \
     EXISTS (SELECT 1 FROM user_mfa_totp t WHERE t.user_id = u.id \
       AND t.confirmed_at IS NOT NULL) AS mfa_enrolled, \
     EXISTS (SELECT 1 FROM platform_grants g WHERE g.user_id = u.id \
       AND g.grant_code = 'platform_admin') AS platform_admin, ",
    "to_char(u.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created_at, ",
    "to_char(u.deactivated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS deactivated_at, ",
    "to_char(u.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS sort_key \
     FROM users u LEFT JOIN local_credentials c ON c.user_id = u.id WHERE true"
);

/// `q` as an escaped ILIKE pattern.
fn like(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// US-1.3: user directory for platform admins, newest first, keyset-paged.
pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Query(q): Query<ListUsersQuery>,
) -> Result<Json<UserList>, AppError> {
    platform_admin(&state, &s).await?;
    let search = match q.q.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(v) if v.chars().count() > MAX_SEARCH_CHARS => return Err(AppError::InvalidRequest),
        Some(v) => Some(v.to_owned()),
    };
    let active = match non_empty(q.active.as_deref()) {
        None | Some("all") => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) => return Err(AppError::InvalidRequest),
    };
    let limit = parse_limit(q.limit.as_deref(), LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT)?;
    let digest = Sha256::digest(json!(["users", search, active]).to_string().as_bytes());
    let fp = URL_SAFE_NO_PAD.encode(&digest[..16]);
    let after = match non_empty(q.cursor.as_deref()) {
        Some(c) => Some(decode_cursor(c, &fp)?),
        None => None,
    };

    let mut b = QueryBuilder::<Postgres>::new(LIST_SQL);
    if let Some(v) = &search {
        let pattern = like(v);
        b.push(" AND (c.username ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR u.display_name ILIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR u.email ILIKE ")
            .push_bind(pattern)
            .push(" ESCAPE '\\')");
    }
    match active {
        Some(true) => {
            b.push(" AND u.active AND u.deactivated_at IS NULL");
        }
        Some(false) => {
            b.push(" AND NOT (u.active AND u.deactivated_at IS NULL)");
        }
        None => {}
    }
    if let Some((key, id)) = after {
        b.push(" AND (u.created_at, u.id) < (")
            .push_bind(key)
            .push("::timestamptz, ")
            .push_bind(id)
            .push(")");
    }
    b.push(" ORDER BY u.created_at DESC, u.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut rows: Vec<UserListItem> = match b.build_query_as().fetch_all(&state.pool).await {
        Ok(r) => r,
        // A tampered cursor key that is not a timestamp.
        Err(sqlx::Error::Database(db)) if db.code().is_some_and(|c| c.starts_with("22")) => {
            return Err(AppError::InvalidRequest);
        }
        Err(e) => return Err(e.into()),
    };
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = match (more, rows.last()) {
        (true, Some(last)) => Some(encode_cursor(&fp, last.sort_key.clone(), last.id)?),
        _ => None,
    };
    Ok(Json(UserList {
        items: rows,
        next_cursor,
    }))
}
