//! Server-side sessions with a synchronizer CSRF token.
//!
//! Tokens are 32 bytes from the OS CSPRNG; only their SHA-256 digests are stored.
//! The CSRF token is compared in constant time. No custom cryptography.

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::AppState;
use crate::error::AppError;

pub const SESSION_TTL_HOURS: i32 = 12;
/// Sessions unused for this long are revoked (US-1.6).
pub const SESSION_IDLE_MINUTES: i32 = 30;
/// A-9: a step-up (POST /session/step-up) authorises sensitive actions for this long.
pub const STEP_UP_MINUTES: i32 = 5;
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const UAT_ENTRY_HEADER: &str = "x-itsm-uat-entry";
pub const UAT_ENTRY_PATH: &str = "/api/v1/uat/session";
/// D11: re-issues the CSRF token of the current session (a reloaded tab keeps the cookie but
/// lost the in-memory token). Guarded like the UAT entry: a custom header forces a CORS
/// preflight, which no cross-site caller passes (no CORS is configured).
pub const CSRF_REFRESH_PATH: &str = "/api/v1/session/csrf";
pub const CSRF_REFRESH_HEADER: &str = "x-itsm-csrf-refresh";
/// US-1.4: the only paths a session with `must_change_password` may use.
const MUST_CHANGE_ALLOWED: &[&str] = &[
    "/api/v1/account/password",
    "/api/v1/session",
    "/api/v1/session/csrf",
    "/api/v1/session/logout-all",
    "/api/v1/meta",
];
pub const LOGIN_MFA_PATH: &str = "/api/v1/login/mfa";
const ENROLLMENT_PATH: &str = "/api/v1/me/mfa/enrollment";
/// US-1.2 / TM-02 (b): a pre-MFA session (MFA mandatory, not yet verified) may only read its MFA
/// state, enrol, confirm the enrolment, and use the session/meta paths.
const MFA_PENDING_ALLOWED: &[&str] = &[
    "/api/v1/me/mfa",
    ENROLLMENT_PATH,
    "/api/v1/session",
    "/api/v1/session/csrf",
    "/api/v1/session/logout-all",
    "/api/v1/meta",
];

fn mfa_pending_allows(path: &str) -> bool {
    MFA_PENDING_ALLOWED.contains(&path)
        || path
            .strip_prefix(ENROLLMENT_PATH)
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.strip_suffix("/confirm"))
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    pub uat_superadmin: bool,
    /// US-1.4: until the password is changed only the account paths are reachable.
    pub must_change_password: bool,
    /// US-1.2: MFA is mandatory for this user and this session has not passed it yet.
    pub mfa_pending: bool,
    /// A-9: a second factor was re-entered on this session within [`STEP_UP_MINUTES`].
    pub step_up_fresh: bool,
    csrf_hash: Vec<u8>,
}

/// A freshly issued pair of secrets, returned to the client once.
pub struct IssuedSession {
    pub token: String,
    pub csrf_token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("os random source unavailable")]
    Random,
}

fn random_token() -> Result<String, TokenError> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|_| TokenError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn digest(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn cookie_name(secure: bool) -> &'static str {
    // `__Host-` requires Secure; plain-HTTP local UAT uses an unprefixed name.
    if secure {
        "__Host-itsm_session"
    } else {
        "itsm_session"
    }
}

pub fn set_cookie_value(secure: bool, token: &str) -> String {
    let max_age = i64::from(SESSION_TTL_HOURS) * 3600;
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure_attr}",
        cookie_name(secure)
    )
}

pub fn clear_cookie_value(secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure_attr}",
        cookie_name(secure)
    )
}

pub(crate) fn read_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// Inserts a session row. Callers are responsible for authorising the user first.
pub async fn issue(
    pool: &PgPool,
    user_id: Uuid,
    access_mode: &str,
) -> Result<IssuedSession, AppError> {
    issue_with_mfa(pool, user_id, access_mode, false).await
}

/// Like [`issue`]; `mfa_verified` marks a session created after a passed second factor.
pub async fn issue_with_mfa(
    pool: &PgPool,
    user_id: Uuid,
    access_mode: &str,
    mfa_verified: bool,
) -> Result<IssuedSession, AppError> {
    let token = random_token().map_err(AppError::internal)?;
    let csrf_token = random_token().map_err(AppError::internal)?;
    sqlx::query(
        "INSERT INTO sessions (token_hash, csrf_hash, user_id, access_mode, expires_at, \
                               last_seen_at, idle_expires_at, credential_version, mfa_verified_at) \
         VALUES ($1, $2, $3, $4, now() + make_interval(hours => $5), now(), \
                 now() + make_interval(mins => $6), \
                 (SELECT credential_version FROM local_credentials WHERE user_id = $3), \
                 CASE WHEN $7 THEN now() END)",
    )
    .bind(digest(&token))
    .bind(digest(&csrf_token))
    .bind(user_id)
    .bind(access_mode)
    .bind(SESSION_TTL_HOURS)
    .bind(SESSION_IDLE_MINUTES)
    .bind(mfa_verified)
    .execute(pool)
    .await?;
    Ok(IssuedSession { token, csrf_token })
}

/// CP5c: a session signed in through the company IdP. It names the IdP, never passes the local
/// MFA or password-change gates (the IdP owns those) and ends when that IdP is disabled or
/// `ITSM_AUTH` drops OIDC.
pub async fn issue_oidc(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
    access_mode: &str,
    idp_id: Uuid,
) -> Result<IssuedSession, AppError> {
    let token = random_token().map_err(AppError::internal)?;
    let csrf_token = random_token().map_err(AppError::internal)?;
    sqlx::query(
        "INSERT INTO sessions (token_hash, csrf_hash, user_id, access_mode, expires_at, \
                               last_seen_at, idle_expires_at, auth_method, idp_id) \
         VALUES ($1, $2, $3, $4, now() + make_interval(hours => $5), now(), \
                 now() + make_interval(mins => $6), 'oidc', $7)",
    )
    .bind(digest(&token))
    .bind(digest(&csrf_token))
    .bind(user_id)
    .bind(access_mode)
    .bind(SESSION_TTL_HOURS)
    .bind(SESSION_IDLE_MINUTES)
    .bind(idp_id)
    .execute(conn)
    .await?;
    Ok(IssuedSession { token, csrf_token })
}

/// Rotates the CSRF token of a live session and returns the new token (only its digest is kept).
/// Any older token for this session stops working.
pub async fn rotate_csrf(pool: &PgPool, session_id: Uuid) -> Result<String, AppError> {
    let csrf_token = random_token().map_err(AppError::internal)?;
    let updated = sqlx::query(
        "UPDATE sessions SET csrf_hash = $1 \
         WHERE id = $2 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(digest(&csrf_token))
    .bind(session_id)
    .execute(pool)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::Unauthenticated);
    }
    Ok(csrf_token)
}

// id, user_id, csrf_hash, uat_superadmin grant, must_change_password, mfa_pending, step_up_fresh
type SessionRow = (Uuid, Uuid, Vec<u8>, bool, bool, bool, bool);

async fn lookup(state: &AppState, token: &str) -> Result<Option<Session>, AppError> {
    let row: Option<SessionRow> = sqlx::query_as(
        "SELECT s.id, s.user_id, s.csrf_hash, \
                EXISTS (SELECT 1 FROM platform_grants g \
                        WHERE g.user_id = s.user_id AND g.grant_code = 'uat_superadmin'), \
                s.auth_method = 'password' AND coalesce((SELECT c.must_change_password \
                          FROM local_credentials c WHERE c.user_id = s.user_id), false), \
                (s.access_mode = 'password' AND s.auth_method = 'password' \
                 AND s.mfa_verified_at IS NULL AND itsm_mfa_required(s.user_id)), \
                coalesce(s.step_up_at > now() - make_interval(mins => $3), false) \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now() \
           AND (s.idle_expires_at IS NULL OR s.idle_expires_at > now()) \
           AND s.access_mode = $2 AND u.active AND u.deactivated_at IS NULL \
           AND (s.credential_version IS NULL OR s.credential_version = \
                (SELECT c.credential_version FROM local_credentials c WHERE c.user_id = s.user_id)) \
           AND (s.auth_method = 'password' OR ($4 AND EXISTS (SELECT 1 FROM identity_providers p \
                WHERE p.id = s.idp_id AND p.enabled)))",
    )
    .bind(digest(token))
    .bind(state.config.access_mode.as_str())
    .bind(STEP_UP_MINUTES)
    .bind(state.config.auth.methods.oidc())
    .fetch_optional(&state.pool)
    .await?;
    match &row {
        Some((id, ..)) => {
            sqlx::query(
                "UPDATE sessions SET last_seen_at = now(), \
                 idle_expires_at = LEAST(now() + make_interval(mins => $2), expires_at) WHERE id = $1",
            )
            .bind(id)
            .bind(SESSION_IDLE_MINUTES)
            .execute(&state.pool)
            .await?;
        }
        None => {
            // US-1.6: an expired session is revoked once and audited without the token;
            // the absolute limit wins over idle when both have passed.
            sqlx::query(
                "WITH r AS (UPDATE sessions SET revoked_at = now(), revoked_reason = \
                   CASE WHEN expires_at <= now() THEN 'expired' ELSE 'idle' END \
                 WHERE token_hash = $1 AND revoked_at IS NULL \
                   AND (expires_at <= now() OR idle_expires_at <= now()) \
                 RETURNING user_id, revoked_reason) \
                 INSERT INTO auth_events (user_id, event, outcome, failure_code) \
                 SELECT user_id, 'session_expire', 'success', \
                   CASE revoked_reason WHEN 'expired' THEN 'absolute' ELSE 'idle' END FROM r",
            )
            .bind(digest(token))
            .execute(&state.pool)
            .await?;
        }
    }
    Ok(row.map(
        |(id, user_id, csrf_hash, grant, must_change_password, mfa_pending, step_up_fresh)| {
            Session {
                id,
                user_id,
                must_change_password,
                // ITSM_MFA=off: no pre-MFA session, whatever the user's grants are.
                mfa_pending: mfa_pending && state.config.mfa_enabled,
                step_up_fresh,
                // The grant is honoured only while the server runs in UAT Superadmin mode;
                // the session's access_mode was matched above.
                uat_superadmin: grant
                    && state.config.access_mode == crate::config::AccessMode::UatSuperadmin,
                csrf_hash,
            }
        },
    ))
}

fn csrf_matches(session: &Session, headers: &HeaderMap) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|presented| digest(presented).ct_eq(&session.csrf_hash).into())
        .unwrap_or(false)
}

/// The refresh needs the custom header and, when the browser sends fetch metadata, a
/// same-origin request.
fn csrf_refresh_allowed(headers: &HeaderMap) -> bool {
    let header_ok = headers
        .get(CSRF_REFRESH_HEADER)
        .is_some_and(|v| v.as_bytes() == b"1");
    let site_ok = headers
        .get("sec-fetch-site")
        .is_none_or(|v| v.as_bytes() == b"same-origin");
    header_ok && site_ok
}

/// Resolves the session for every `/api/v1` request and enforces CSRF on unsafe
/// methods (deny by default). The UAT entry endpoint instead requires a custom
/// header, which forces a CORS preflight for cross-site callers.
pub async fn session_layer(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let unsafe_method = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let is_uat_entry = req.uri().path() == UAT_ENTRY_PATH;
    let is_login = matches!(req.uri().path(), "/api/v1/login" | LOGIN_MFA_PATH);
    let is_csrf_refresh = req.uri().path() == CSRF_REFRESH_PATH;

    let session = match read_cookie(req.headers(), cookie_name(state.config.cookie_secure)) {
        Some(token) => match lookup(&state, token).await {
            Ok(s) => s,
            Err(e) => return e.into_response(),
        },
        None => None,
    };

    if unsafe_method {
        let allowed = if is_login {
            req.headers()
                .get("x-itsm-login")
                .is_some_and(|v| v.as_bytes() == b"1")
                && req
                    .headers()
                    .get("sec-fetch-site")
                    .is_none_or(|v| v.as_bytes() == b"same-origin")
        } else if is_uat_entry {
            req.headers()
                .get(UAT_ENTRY_HEADER)
                .is_some_and(|v| v.as_bytes() == b"1")
        } else if is_csrf_refresh {
            session.is_some() && csrf_refresh_allowed(req.headers())
        } else {
            session
                .as_ref()
                .is_some_and(|s| csrf_matches(s, req.headers()))
        };
        if !allowed {
            return if session.is_none() && !is_uat_entry {
                AppError::Unauthenticated.into_response()
            } else {
                AppError::CsrfFailed.into_response()
            };
        }
    }

    // MFA first (TM-02 b), then the forced password change (US-1.4).
    if let Some(s) = &session {
        let path = req.uri().path();
        let refused = if s.mfa_pending {
            !mfa_pending_allows(path)
        } else {
            s.must_change_password && !MUST_CHANGE_ALLOWED.contains(&path)
        };
        if refused {
            return AppError::Forbidden.into_response();
        }
    }

    if let Some(s) = session {
        req.extensions_mut().insert(s);
    }
    next.run(req).await
}

/// Extractor for handlers that require an authenticated session.
impl<S: Send + Sync> FromRequestParts<S> for Session {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .ok_or(AppError::Unauthenticated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn tokens_are_random_and_url_safe() {
        let a = random_token().map_err(|e| e.to_string());
        let b = random_token().map_err(|e| e.to_string());
        assert!(matches!((&a, &b), (Ok(x), Ok(y)) if x != y && x.len() == 43));
        if let Ok(a) = a {
            assert!(
                a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            );
        }
    }

    #[test]
    fn cookie_attributes_are_strict() {
        let c = set_cookie_value(true, "abc");
        assert!(c.starts_with("__Host-itsm_session=abc;"));
        for attr in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(c.contains(attr), "missing {attr}");
        }
        assert!(!set_cookie_value(false, "abc").contains("Secure"));
    }

    #[test]
    fn cookie_parser_finds_exact_name_only() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("xitsm_session=bad; itsm_session=good"),
        );
        assert_eq!(read_cookie(&h, "itsm_session"), Some("good"));
        assert_eq!(read_cookie(&h, "other"), None);
    }

    #[test]
    fn pre_mfa_allowlist_is_exact() {
        for ok in [
            "/api/v1/me/mfa",
            "/api/v1/me/mfa/enrollment",
            "/api/v1/me/mfa/enrollment/0b0c/confirm",
            "/api/v1/session",
            "/api/v1/meta",
        ] {
            assert!(mfa_pending_allows(ok), "{ok}");
        }
        for no in [
            "/api/v1/users",
            "/api/v1/account/password",
            "/api/v1/me/mfa/recovery-codes",
            "/api/v1/me/mfa/enrollment//confirm",
            "/api/v1/me/mfa/enrollment/a/b/confirm",
            "/api/v1/me/mfa/enrollmentx",
        ] {
            assert!(!mfa_pending_allows(no), "{no}");
        }
    }

    #[test]
    fn csrf_compare_rejects_missing_and_wrong_tokens() {
        let s = Session {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            uat_superadmin: false,
            must_change_password: false,
            mfa_pending: false,
            step_up_fresh: false,
            csrf_hash: digest("right"),
        };
        let mut h = HeaderMap::new();
        assert!(!csrf_matches(&s, &h));
        h.insert(CSRF_HEADER, HeaderValue::from_static("wrong"));
        assert!(!csrf_matches(&s, &h));
        h.insert(CSRF_HEADER, HeaderValue::from_static("right"));
        assert!(csrf_matches(&s, &h));
    }
}
