//! CP5c company sign-in through OpenID Connect (Authorization Code + PKCE S256).
//!
//! - `GET /api/v1/auth/oidc/{company_code}/start` opens a single-use login attempt (state and nonce
//!   kept only as SHA-256, the PKCE verifier sealed with the keyring, 10 minutes) and sends the
//!   browser to the company IdP. A short-lived `SameSite=Lax` cookie carries the state so the
//!   callback only completes in the browser that started it (no login CSRF).
//! - `GET /api/v1/auth/oidc/callback` consumes the attempt, exchanges the code (client secret from
//!   the keyring), verifies the ID token and issues the usual session cookie, then sends the
//!   browser to `/`. Every failure ends at `/?sso=<code>` with an audit row; no IdP text is shown.
//!
//! Users are linked by (IdP, subject), never by e-mail. A new subject is provisioned just in time
//! only with a verified address in one of the IdP's domains that no account uses yet; its role
//! comes from the group mapping (no match = signed in without access).

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::AppState;
use crate::authz::Role;
use crate::config::AccessMode;
use crate::crypto::{Purpose, load_secret};
use crate::error::AppError;
use crate::oidc::{self, Expected, HostPolicy, IdToken, OidcError, TokenError, cache};
use crate::routes::password::{auth_event, auth_event_by};
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::correlation;
use crate::session;

/// Lifetime of a login attempt (the table allows at most 15 minutes).
pub const ATTEMPT_MINUTES: i32 = 10;
const SCOPE: &str = "openid email profile";
/// Longest `state` or `code` accepted from the browser.
const MAX_PARAM: usize = 2048;

fn bind_cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-itsm_oidc"
    } else {
        "itsm_oidc"
    }
}

fn bind_cookie(secure: bool, value: &str, max_age: i32) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure_attr}",
        bind_cookie_name(secure)
    )
}

fn random() -> Result<String, AppError> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|_| AppError::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Ties the sealed verifier to its attempt row.
fn attempt_aad(state_hash: &[u8]) -> Vec<u8> {
    let mut aad = b"itsm-oidc-attempt/v1|".to_vec();
    aad.extend_from_slice(state_hash);
    aad
}

fn enabled(state: &AppState) -> Result<(), AppError> {
    if state.config.access_mode != AccessMode::Password || !state.config.auth.methods.oidc() {
        return Err(AppError::NotFound);
    }
    Ok(())
}

fn redirect(location: &str, cookies: &[String]) -> Response {
    let mut res = StatusCode::FOUND.into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(location) {
        h.insert(header::LOCATION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            h.append(header::SET_COOKIE, v);
        }
    }
    res
}

/// Sends the browser back to the sign-in page with a stable code and drops the binding cookie.
fn fail(state: &AppState, code: &'static str) -> Response {
    redirect(
        &format!("/?sso={code}"),
        &[bind_cookie(state.config.cookie_secure, "", 0)],
    )
}

/// Audits a refused or failed sign-in, then [`fail`]s.
async fn refuse(
    state: &AppState,
    user: Option<Uuid>,
    who: &str,
    outcome: &str,
    audit_code: &str,
    code: &'static str,
) -> Result<Response, AppError> {
    let mut conn = state.pool.acquire().await?;
    auth_event(
        &mut conn,
        user,
        who,
        "oidc_login",
        outcome,
        Some(audit_code),
    )
    .await?;
    Ok(fail(state, code))
}

fn policy(state: &AppState, issuer: &str, extra_hosts: &[String]) -> Option<HostPolicy> {
    HostPolicy::new(issuer, extra_hosts, state.config.auth.oidc_lab)
}

fn timeout(state: &AppState) -> std::time::Duration {
    std::time::Duration::from_millis(state.config.integrations.timeout_ms)
}

pub async fn start(
    State(state): State<AppState>,
    Path(company): Path<String>,
) -> Result<Response, AppError> {
    enabled(&state)?;
    crate::db::verify_runtime_guard(&state.config, &state.pool)
        .await
        .map_err(|_| AppError::Unavailable)?;
    let code = company.to_ascii_uppercase();
    if !(2..=10).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Ok(fail(&state, "not_configured"));
    }
    let row: Option<(Uuid, String, String, Vec<String>)> = sqlx::query_as(
        "SELECT p.id, p.issuer, p.client_id, p.extra_hosts FROM identity_providers p \
         JOIN workspaces w ON w.id = p.workspace_id \
         WHERE w.code = $1 AND w.active AND p.enabled",
    )
    .bind(&code)
    .fetch_optional(&state.pool)
    .await?;
    let Some((idp, issuer, client_id, extra_hosts)) = row else {
        return Ok(fail(&state, "not_configured"));
    };
    let (Some(keyring), Some(redirect_uri)) = (
        state.config.keyring.as_deref(),
        state.config.auth.redirect_uri(),
    ) else {
        tracing::warn!(error.kind = "keyring_missing", "oidc start without keyring");
        return Ok(fail(&state, "unavailable"));
    };
    let Some(policy) = policy(&state, &issuer, &extra_hosts) else {
        return Ok(fail(&state, "not_configured"));
    };
    let discovery = match cache::discovery(&issuer, &policy, timeout(&state)).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(idp_id = %idp, error.kind = %e, "oidc discovery failed");
            return Ok(fail(&state, "idp_unavailable"));
        }
    };

    let state_token = random()?;
    let nonce = random()?;
    let verifier = zeroize::Zeroizing::new(random()?);
    let challenge = URL_SAFE_NO_PAD.encode(oidc::sha256(verifier.as_bytes()));
    let state_hash = oidc::sha256(state_token.as_bytes());
    let (key_id, sealed_nonce, ct) = keyring
        .seal(verifier.as_bytes(), &attempt_aad(&state_hash))
        .map_err(AppError::internal)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM oidc_login_attempts WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO oidc_login_attempts (state_hash, idp_id, nonce_hash, verifier_key, \
         verifier_nonce, verifier_ct, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(mins => $7))",
    )
    .bind(state_hash.as_slice())
    .bind(idp)
    .bind(oidc::sha256(nonce.as_bytes()).as_slice())
    .bind(&key_id)
    .bind(&sealed_nonce)
    .bind(&ct)
    .bind(ATTEMPT_MINUTES)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let enc = crate::providers::enc;
    let query = format!(
        "response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}\
         &code_challenge={}&code_challenge_method=S256",
        enc(&client_id),
        enc(&redirect_uri),
        enc(SCOPE),
        state_token,
        nonce,
        challenge,
    );
    let sep = if discovery.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    Ok(redirect(
        &format!("{}{sep}{query}", discovery.authorization_endpoint),
        &[bind_cookie(
            state.config.cookie_secure,
            &state_token,
            ATTEMPT_MINUTES * 60,
        )],
    ))
}

/// Query of the redirect from the IdP. Unknown parameters (`session_state`, `iss`, …) are ignored.
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Mapping {
    group: String,
    role: String,
}

#[derive(sqlx::FromRow)]
struct Idp {
    workspace_id: Uuid,
    issuer: String,
    client_id: String,
    client_secret_id: Option<Uuid>,
    extra_hosts: Vec<String>,
    email_domains: Vec<String>,
    group_claim: String,
    role_mapping: serde_json::Value,
    sync_roles: bool,
    enabled: bool,
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<CallbackQuery>, QueryRejection>,
) -> Result<Response, AppError> {
    enabled(&state)?;
    crate::db::verify_runtime_guard(&state.config, &state.pool)
        .await
        .map_err(|_| AppError::Unavailable)?;
    let Ok(Query(q)) = query else {
        return Ok(fail(&state, "expired"));
    };
    let Some(state_token) = q.state.filter(|s| !s.is_empty() && s.len() <= MAX_PARAM) else {
        return Ok(fail(&state, "expired"));
    };
    // Only the browser that started the sign-in may finish it.
    let bound = session::read_cookie(&headers, bind_cookie_name(state.config.cookie_secure))
        .is_some_and(|c| bool::from(c.as_bytes().ct_eq(state_token.as_bytes())));
    if !bound {
        return refuse(
            &state,
            None,
            "oidc",
            "denied",
            "browser_mismatch",
            "expired",
        )
        .await;
    }
    let state_hash = oidc::sha256(state_token.as_bytes());
    type AttemptRow = (Uuid, Vec<u8>, String, Vec<u8>, Vec<u8>, bool);
    let attempt: Option<AttemptRow> = sqlx::query_as(
        "DELETE FROM oidc_login_attempts WHERE state_hash = $1 \
         RETURNING idp_id, nonce_hash, verifier_key, verifier_nonce, verifier_ct, expires_at > now()",
    )
    .bind(state_hash.as_slice())
    .fetch_optional(&state.pool)
    .await?;
    let Some((idp_id, nonce_hash, key_id, sealed_nonce, ct, live)) = attempt else {
        return refuse(&state, None, "oidc", "denied", "unknown_state", "expired").await;
    };
    if !live {
        return refuse(&state, None, "oidc", "denied", "attempt_expired", "expired").await;
    }
    if q.error.is_some() {
        // The IdP refused or the user cancelled; its text is not shown or stored.
        return refuse(&state, None, "oidc", "failed", "idp_error", "idp_error").await;
    }
    let Some(code) = q.code.filter(|c| !c.is_empty() && c.len() <= MAX_PARAM) else {
        return refuse(&state, None, "oidc", "failed", "missing_code", "idp_error").await;
    };
    let idp: Idp = sqlx::query_as(
        "SELECT workspace_id, issuer, client_id, client_secret_id, extra_hosts, email_domains, \
         group_claim, role_mapping, sync_roles, enabled FROM identity_providers WHERE id = $1",
    )
    .bind(idp_id)
    .fetch_one(&state.pool)
    .await?;
    let (Some(secret_id), true) = (idp.client_secret_id, idp.enabled) else {
        return refuse(
            &state,
            None,
            "oidc",
            "denied",
            "idp_disabled",
            "not_configured",
        )
        .await;
    };
    let (Some(keyring), Some(redirect_uri)) = (
        state.config.keyring.as_deref(),
        state.config.auth.redirect_uri(),
    ) else {
        return refuse(
            &state,
            None,
            "oidc",
            "failed",
            "keyring_missing",
            "unavailable",
        )
        .await;
    };
    let Ok(opened) = keyring.open(&key_id, &sealed_nonce, &ct, &attempt_aad(&state_hash)) else {
        return refuse(
            &state,
            None,
            "oidc",
            "failed",
            "attempt_unreadable",
            "expired",
        )
        .await;
    };
    let Ok(verifier) = std::str::from_utf8(opened.expose()) else {
        return refuse(
            &state,
            None,
            "oidc",
            "failed",
            "attempt_unreadable",
            "expired",
        )
        .await;
    };
    let Ok(nonce_hash) = <[u8; 32]>::try_from(nonce_hash.as_slice()) else {
        return refuse(
            &state,
            None,
            "oidc",
            "failed",
            "attempt_unreadable",
            "expired",
        )
        .await;
    };
    let secret = match load_secret(
        &state.pool,
        keyring,
        secret_id,
        Some(idp.workspace_id),
        Purpose::OidcClientSecret,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(idp_id = %idp_id, error.kind = %e, "oidc client secret unreadable");
            return refuse(
                &state,
                None,
                "oidc",
                "failed",
                "secret_unreadable",
                "unavailable",
            )
            .await;
        }
    };
    let Some(policy) = policy(&state, &idp.issuer, &idp.extra_hosts) else {
        return refuse(
            &state,
            None,
            "oidc",
            "failed",
            "bad_issuer",
            "not_configured",
        )
        .await;
    };
    let t = timeout(&state);
    let discovery = match cache::discovery(&idp.issuer, &policy, t).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(idp_id = %idp_id, error.kind = %e, "oidc discovery failed");
            return refuse(
                &state,
                None,
                "oidc",
                "failed",
                "discovery",
                "idp_unavailable",
            )
            .await;
        }
    };
    let raw = match oidc::exchange_code(
        &discovery,
        &policy,
        &idp.client_id,
        secret.expose(),
        &code,
        verifier,
        &redirect_uri,
        t,
    )
    .await
    {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!(idp_id = %idp_id, error.kind = %e, "oidc code exchange failed");
            let (audit, code) = match e {
                OidcError::Status(400..=499) => ("token_rejected", "denied"),
                _ => ("token_endpoint", "idp_unavailable"),
            };
            return refuse(&state, None, "oidc", "failed", audit, code).await;
        }
    };
    drop(secret);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let expected = Expected {
        issuer: &discovery.issuer,
        client_id: &idp.client_id,
        nonce_hash: &nonce_hash,
        now,
    };
    let token = match verify(&idp.issuer, &policy, &discovery, &raw, &expected, t).await {
        Ok(token) => token,
        Err(kind) => {
            let audit = format!("token_{kind}");
            return refuse(&state, None, "oidc", "denied", &audit, "denied").await;
        }
    };

    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let who = format!("oidc|{idp_id}|{}", token.subject);
    let user = match resolve(&mut tx, idp_id, &idp, &token, &corr).await? {
        Ok(user) => user,
        Err((user, code)) => {
            auth_event(&mut tx, user, &who, "oidc_login", "denied", Some(code)).await?;
            tx.commit().await?;
            return Ok(fail(&state, code));
        }
    };
    let issued = session::issue_oidc(&mut tx, user, AccessMode::Password.as_str(), idp_id).await?;
    auth_event(&mut tx, Some(user), &who, "oidc_login", "success", None).await?;
    tx.commit().await?;
    Ok(redirect(
        "/",
        &[
            session::set_cookie_value(state.config.cookie_secure, &issued.token),
            bind_cookie(state.config.cookie_secure, "", 0),
        ],
    ))
}

/// Verifies the ID token with the cached keys, refreshing them once for an unknown `kid`.
async fn verify(
    issuer: &str,
    policy: &HostPolicy,
    discovery: &oidc::Discovery,
    raw: &str,
    expected: &Expected<'_>,
    t: std::time::Duration,
) -> Result<IdToken, String> {
    let jwks = |refresh| cache::jwks(issuer, policy, discovery, refresh, t);
    let keys = jwks(false)
        .await
        .map_err(|_| "jwks".to_owned())?
        .ok_or_else(|| "jwks".to_owned())?;
    match oidc::verify_id_token(raw, &keys, expected) {
        Err(TokenError::UnknownKey) => match jwks(true).await {
            Ok(Some(keys)) => {
                oidc::verify_id_token(raw, &keys, expected).map_err(|e| e.to_string())
            }
            Ok(None) => Err(TokenError::UnknownKey.to_string()),
            Err(_) => Err("jwks".to_owned()),
        },
        other => other.map_err(|e| e.to_string()),
    }
}

/// The display name from the token, when usable.
fn display_name(token: &IdToken) -> Option<String> {
    token
        .name()
        .map(str::trim)
        .filter(|n| (1..=200).contains(&n.chars().count()))
        .map(str::to_owned)
}

/// The highest role mapped from the token's groups (`lead_cs` over `cs`), if any.
fn mapped_role(idp: &Idp, token: &IdToken) -> Option<Role> {
    let mapping: Vec<Mapping> = serde_json::from_value(idp.role_mapping.clone()).ok()?;
    let groups = token.strings(&idp.group_claim);
    let roles: Vec<Role> = mapping
        .iter()
        .filter(|m| groups.contains(&m.group))
        .filter_map(|m| Role::from_code(&m.role))
        .collect();
    [Role::LeadCs, Role::Cs]
        .into_iter()
        .find(|r| roles.contains(r))
}

type Resolved = Result<Uuid, (Option<Uuid>, &'static str)>;

/// Finds or provisions the user of `token` and applies the role mapping. `Err` = refused with
/// the user (when known) and the `?sso=` code.
async fn resolve(
    tx: &mut sqlx::PgConnection,
    idp_id: Uuid,
    idp: &Idp,
    token: &IdToken,
    corr: &Option<String>,
) -> Result<Resolved, AppError> {
    let ws = idp.workspace_id;
    let name = display_name(token);
    let linked: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT e.user_id, u.active AND u.deactivated_at IS NULL FROM external_identities e \
         JOIN users u ON u.id = e.user_id WHERE e.idp_id = $1 AND e.subject = $2 FOR UPDATE OF e",
    )
    .bind(idp_id)
    .bind(&token.subject)
    .fetch_optional(&mut *tx)
    .await?;
    let (user, provisioned) = match linked {
        Some((user, false)) => return Ok(Err((Some(user), "inactive"))),
        Some((user, true)) => {
            if let Some(name) = &name {
                sqlx::query(
                    "UPDATE users SET display_name = $2 WHERE id = $1 AND display_name <> $2",
                )
                .bind(user)
                .bind(name)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "UPDATE external_identities SET last_login_at = now() \
                 WHERE idp_id = $1 AND subject = $2",
            )
            .bind(idp_id)
            .bind(&token.subject)
            .execute(&mut *tx)
            .await?;
            (user, false)
        }
        None => {
            let email = token
                .email()
                .map(|e| e.trim().to_lowercase())
                .filter(|e| (3..=254).contains(&e.len()));
            let domain_ok = |e: &str| {
                e.rsplit_once('@').is_some_and(|(local, domain)| {
                    !local.is_empty() && idp.email_domains.iter().any(|d| d == domain)
                })
            };
            let Some(email) = email.filter(|e| token.email_verified() && domain_ok(e)) else {
                return Ok(Err((None, "not_linked")));
            };
            // An existing account is never taken over by address; an admin links it.
            let created: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO users (display_name, email) VALUES ($1, $2) \
                 ON CONFLICT (email) WHERE email IS NOT NULL DO NOTHING RETURNING id",
            )
            .bind(name.as_deref().unwrap_or(&email))
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(user) = created else {
                return Ok(Err((None, "email_in_use")));
            };
            sqlx::query(
                "INSERT INTO external_identities (user_id, idp_id, subject, last_login_at) \
                 VALUES ($1, $2, $3, now())",
            )
            .bind(user)
            .bind(idp_id)
            .bind(&token.subject)
            .execute(&mut *tx)
            .await?;
            auth_event_by(tx, None, Some(user), "", "oidc_provision", "success", None).await?;
            (user, true)
        }
    };

    // Roles: set at provisioning; afterwards only with sync_roles. A deactivated membership is
    // the admin's decision and is never reopened here; removal stays manual (no deprovisioning).
    if !(provisioned || idp.sync_roles) {
        return Ok(Ok(user));
    }
    let Some(role) = mapped_role(idp, token) else {
        return Ok(Ok(user));
    };
    let membership: Option<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT id, role, active FROM workspace_memberships \
         WHERE workspace_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    match membership {
        None => {
            if super::members::one_company(tx, user, ws).await.is_err() {
                tracing::info!(idp_id = %idp_id, "oidc role mapping skipped: user in another company");
                return Ok(Ok(user));
            }
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
                 VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(ws)
            .bind(user)
            .bind(role.code())
            .fetch_one(&mut *tx)
            .await?;
            audit_entity(
                tx,
                ws,
                None,
                "membership.add",
                "workspace_membership",
                id,
                serde_json::json!({ "userId": user, "role": role.code(), "source": "oidc" }),
                corr,
            )
            .await?;
        }
        Some((id, old, true)) if old != role.code() => {
            sqlx::query(
                "UPDATE workspace_memberships SET role = $2, version = version + 1 WHERE id = $1",
            )
            .bind(id)
            .bind(role.code())
            .execute(&mut *tx)
            .await?;
            audit_entity(
                tx,
                ws,
                None,
                "membership.update",
                "workspace_membership",
                id,
                serde_json::json!({ "role": { "from": old, "to": role.code() }, "source": "oidc" }),
                corr,
            )
            .await?;
        }
        Some(_) => {}
    }
    Ok(Ok(user))
}
