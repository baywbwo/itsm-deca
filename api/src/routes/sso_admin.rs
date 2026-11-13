//! CP5c slice 4: company SSO administration (password access mode, `platform_admin` only; every
//! write also needs a fresh step-up).
//!
//! One identity provider per company workspace. The client secret is write-only (sealed with the
//! server keyring, never returned or audited). Changing where the IdP lives (issuer, client id,
//! extra hosts) sends it back to "not tested" and turns it off: it has to pass the discovery test
//! again before it can be enabled. Turning it off ends its sessions. Accounts that cannot be
//! provisioned just in time (no verified address, e.g. Entra ID) are linked here by subject.
//! Every change writes an `audit_events` row of the company in the same transaction; linking
//! and unlinking also write `auth_events`.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Role};
use crate::config::AccessMode;
use crate::crypto::{self, Purpose};
use crate::error::AppError;
use crate::oidc::{HostPolicy, cache};
use crate::routes::integrations::SecretInput;
use crate::routes::members::one_company;
use crate::routes::password::auth_event_by;
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::users::is_platform_admin;
use crate::session::Session;

/// Capability named in platform denied-audit rows.
pub const PLATFORM_SSO_ADMIN: &str = "platform.sso";
const MAX_DOMAINS: usize = 20;
const MAX_HOSTS: usize = 10;
const MAX_MAPPINGS: usize = 50;
const IDENTITY_PAGE: i64 = 100;

const TS: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'";

async fn admin(state: &AppState, s: &Session) -> Result<(), AppError> {
    if state.config.access_mode != AccessMode::Password {
        return Err(AppError::NotFound);
    }
    if is_platform_admin(&state.pool, s.user_id).await? {
        Ok(())
    } else {
        tracing::info!(user_id = %s.user_id, "sso administration denied");
        crate::denied_audit::record_platform(s.user_id, PLATFORM_SSO_ADMIN);
        Err(AppError::Forbidden)
    }
}

async fn writer(state: &AppState, s: &Session) -> Result<(), AppError> {
    admin(state, s).await?;
    authz::require_step_up(state, s)
}

fn parse(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct IdpView {
    id: Uuid,
    display_name: String,
    issuer: String,
    client_id: String,
    secret_set: bool,
    secret_updated_at: Option<String>,
    email_domains: Vec<String>,
    extra_hosts: Vec<String>,
    group_claim: String,
    role_mapping: Value,
    sync_roles: bool,
    enabled: bool,
    last_test_ok: Option<bool>,
    last_tested_at: Option<String>,
    version: i32,
}

fn idp_sql(filter: &str) -> String {
    format!(
        "SELECT p.id, p.display_name, p.issuer, p.client_id, \
           (p.client_secret_id IS NOT NULL) AS secret_set, \
           to_char(p.client_secret_updated_at AT TIME ZONE 'UTC', {TS}) AS secret_updated_at, \
           p.email_domains, p.extra_hosts, p.group_claim, p.role_mapping, p.sync_roles, p.enabled, \
           p.last_test_ok, to_char(p.last_tested_at AT TIME ZONE 'UTC', {TS}) AS last_tested_at, \
           p.version \
         FROM identity_providers p WHERE {filter}"
    )
}

async fn idp_of(conn: &mut PgConnection, id: Uuid) -> Result<IdpView, AppError> {
    Ok(sqlx::query_as(&idp_sql("p.id = $1"))
        .bind(id)
        .fetch_one(conn)
        .await?)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct CompanyRow {
    company_id: Uuid,
    workspace_id: Uuid,
    code: String,
    name: String,
    active: bool,
    identities: i64,
    #[sqlx(skip)]
    provider: Option<IdpView>,
    #[serde(skip)]
    provider_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerView {
    /// `ITSM_AUTH`: password | sso | both.
    auth: &'static str,
    /// The redirect URI to register at every IdP (`None` until `ITSM_PUBLIC_URL` is set).
    redirect_uri: Option<String>,
    /// A client secret can be stored and SSO used (server keyring configured).
    keyring: bool,
    /// UAT lab mode: `http://` and private IdP addresses allowed.
    lab: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoOverview {
    items: Vec<Value>,
    server: ServerView,
}

/// Every company with its identity provider (or `null`) and the server's SSO settings.
pub async fn overview(
    State(state): State<AppState>,
    s: Session,
) -> Result<Json<SsoOverview>, AppError> {
    admin(&state, &s).await?;
    let mut conn = state.pool.acquire().await?;
    let mut rows: Vec<CompanyRow> = sqlx::query_as(
        "SELECT c.id AS company_id, w.id AS workspace_id, w.code, c.name, (w.active AND c.active) AS active, \
           p.id AS provider_id, \
           (SELECT count(*) FROM external_identities e WHERE e.idp_id = p.id) AS identities \
         FROM companies c JOIN workspaces w ON w.company_id = c.id \
         LEFT JOIN identity_providers p ON p.workspace_id = w.id ORDER BY w.code",
    )
    .fetch_all(&mut *conn)
    .await?;
    for r in &mut rows {
        if let Some(id) = r.provider_id {
            r.provider = Some(idp_of(&mut conn, id).await?);
        }
    }
    let auth = &state.config.auth;
    Ok(Json(SsoOverview {
        items: rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .collect(),
        server: ServerView {
            auth: auth.methods.as_str(),
            redirect_uri: auth.redirect_uri(),
            keyring: state.config.keyring.is_some(),
            lab: auth.oidc_lab,
        },
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingInput {
    group: String,
    role: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdpInput {
    display_name: String,
    issuer: String,
    client_id: String,
    #[serde(default)]
    email_domains: Vec<String>,
    #[serde(default)]
    extra_hosts: Vec<String>,
    group_claim: String,
    #[serde(default)]
    role_mapping: Vec<MappingInput>,
    sync_roles: bool,
    /// Current version when the company already has an IdP; absent (or null) to create it.
    #[serde(default)]
    version: Option<i32>,
}

/// Validated settings, in the form stored.
#[derive(Debug, PartialEq)]
struct Settings {
    display_name: String,
    issuer: String,
    client_id: String,
    email_domains: Vec<String>,
    extra_hosts: Vec<String>,
    group_claim: String,
    role_mapping: Value,
    sync_roles: bool,
}

fn hostname(raw: &str) -> Option<String> {
    let h = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    (h.len() <= 253 && h.contains('.') && h.split('.').all(label_ok)).then_some(h)
}

fn host_list(raw: &[String], max: usize, field: &'static str) -> Result<Vec<String>, AppError> {
    let mut out = BTreeSet::new();
    for r in raw {
        if r.trim().is_empty() {
            continue;
        }
        out.insert(hostname(r).ok_or(AppError::Validation(field))?);
    }
    if out.len() > max {
        return Err(AppError::Validation(field));
    }
    Ok(out.into_iter().collect())
}

fn validate(b: &IdpInput, lab: bool) -> Result<Settings, AppError> {
    let display_name = b.display_name.trim();
    if !(1..=80).contains(&display_name.chars().count())
        || display_name.chars().any(char::is_control)
    {
        return Err(AppError::Validation("displayName"));
    }
    // The issuer is kept exactly as the IdP spells it (ID tokens must match it byte for byte).
    let issuer = b.issuer.trim();
    let url = reqwest::Url::parse(issuer).map_err(|_| AppError::Validation("issuer"))?;
    if issuer.len() > 512
        || url.query().is_some()
        || url.fragment().is_some()
        || issuer.contains(char::is_whitespace)
    {
        return Err(AppError::Validation("issuer"));
    }
    let extra_hosts = host_list(&b.extra_hosts, MAX_HOSTS, "extraHosts")?;
    if HostPolicy::new(issuer, &extra_hosts, lab).is_none() {
        return Err(AppError::Validation("issuer"));
    }
    let client_id = b.client_id.trim();
    if !(1..=255).contains(&client_id.len())
        || client_id
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(AppError::Validation("clientId"));
    }
    let email_domains = host_list(&b.email_domains, MAX_DOMAINS, "emailDomains")?;
    let group_claim = b.group_claim.trim();
    let claim_ok = (1..=100).contains(&group_claim.len())
        && group_claim
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:/-".contains(&c));
    if !claim_ok {
        return Err(AppError::Validation("groupClaim"));
    }
    if b.role_mapping.len() > MAX_MAPPINGS {
        return Err(AppError::Validation("roleMapping"));
    }
    let mut seen = BTreeSet::new();
    let mut mapping = Vec::with_capacity(b.role_mapping.len());
    for m in &b.role_mapping {
        let group = m.group.trim();
        let ok = (1..=255).contains(&group.chars().count())
            && !group.chars().any(char::is_control)
            && Role::from_code(&m.role).is_some()
            && seen.insert(group.to_owned());
        if !ok {
            return Err(AppError::Validation("roleMapping"));
        }
        mapping.push(json!({ "group": group, "role": m.role }));
    }
    Ok(Settings {
        display_name: display_name.to_owned(),
        issuer: issuer.to_owned(),
        client_id: client_id.to_owned(),
        email_domains,
        extra_hosts,
        group_claim: group_claim.to_owned(),
        role_mapping: Value::Array(mapping),
        sync_roles: b.sync_roles,
    })
}

/// The company's workspace (locked), `NotFound` for an unknown company.
async fn company_ws(conn: &mut PgConnection, company: Uuid) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "SELECT w.id FROM companies c JOIN workspaces w ON w.company_id = c.id \
         WHERE c.id = $1 FOR SHARE OF w",
    )
    .bind(company)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound)
}

#[derive(sqlx::FromRow)]
struct Current {
    id: Uuid,
    issuer: String,
    client_id: String,
    extra_hosts: Vec<String>,
    client_secret_id: Option<Uuid>,
    last_test_ok: Option<bool>,
    enabled: bool,
    version: i32,
}

/// The company's IdP, locked for update; `expected` must be its version.
async fn locked(conn: &mut PgConnection, ws: Uuid, expected: i32) -> Result<Current, AppError> {
    let cur: Current = sqlx::query_as(
        "SELECT id, issuer, client_id, extra_hosts, client_secret_id, last_test_ok, enabled, version \
         FROM identity_providers WHERE workspace_id = $1 FOR UPDATE",
    )
    .bind(ws)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound)?;
    if cur.version != expected {
        return Err(AppError::VersionConflict);
    }
    Ok(cur)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdpResult {
    item: IdpView,
    audit_event_id: Uuid,
}

async fn end_sessions(
    conn: &mut PgConnection,
    idp: Uuid,
    user: Option<Uuid>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = 'admin_revoke' \
         WHERE idp_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND revoked_at IS NULL",
    )
    .bind(idp)
    .bind(user)
    .execute(conn)
    .await?;
    Ok(())
}

/// Creates (no `version`) or updates the company's IdP settings. A new issuer, client id or
/// host list needs a new discovery test: the IdP goes back to untested and off.
pub async fn save(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<IdpInput>, JsonRejection>,
) -> Result<(StatusCode, Json<IdpResult>), AppError> {
    writer(&state, &s).await?;
    let company = parse(&raw)?;
    let b = body(payload)?;
    let v = validate(&b, state.config.auth.oidc_lab)?;
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let ws = company_ws(&mut tx, company).await?;
    let (status, id, audit_event_id, forget) = match b.version {
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO identity_providers (workspace_id, display_name, issuer, client_id, \
                   email_domains, extra_hosts, group_claim, role_mapping, sync_roles, created_by, updated_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) RETURNING id",
            )
            .bind(ws)
            .bind(&v.display_name)
            .bind(&v.issuer)
            .bind(&v.client_id)
            .bind(&v.email_domains)
            .bind(&v.extra_hosts)
            .bind(&v.group_claim)
            .bind(&v.role_mapping)
            .bind(v.sync_roles)
            .bind(s.user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(d) if d.constraint() == Some("identity_providers_workspace_id_key") => {
                    AppError::VersionConflict
                }
                _ => AppError::from(e),
            })?;
            let audit = audit_entity(
                &mut tx,
                ws,
                Some(s.user_id),
                "idp.create",
                "identity_provider",
                id,
                json!({ "issuer": v.issuer, "clientId": v.client_id, "emailDomains": v.email_domains,
                        "extraHosts": v.extra_hosts, "groupClaim": v.group_claim,
                        "mappings": v.role_mapping.as_array().map_or(0, Vec::len),
                        "syncRoles": v.sync_roles, "version": 1 }),
                &corr,
            )
            .await?;
            (StatusCode::CREATED, id, audit, None)
        }
        Some(expected) => {
            let cur = locked(&mut tx, ws, expected).await?;
            let moved = cur.issuer != v.issuer
                || cur.client_id != v.client_id
                || cur.extra_hosts != v.extra_hosts;
            sqlx::query(
                "UPDATE identity_providers SET display_name = $2, issuer = $3, client_id = $4, \
                   email_domains = $5, extra_hosts = $6, group_claim = $7, role_mapping = $8, \
                   sync_roles = $9, updated_by = $10, updated_at = now(), version = version + 1, \
                   last_test_ok = CASE WHEN $11 THEN NULL ELSE last_test_ok END, \
                   last_tested_at = CASE WHEN $11 THEN NULL ELSE last_tested_at END, \
                   enabled = enabled AND NOT $11 \
                 WHERE id = $1",
            )
            .bind(cur.id)
            .bind(&v.display_name)
            .bind(&v.issuer)
            .bind(&v.client_id)
            .bind(&v.email_domains)
            .bind(&v.extra_hosts)
            .bind(&v.group_claim)
            .bind(&v.role_mapping)
            .bind(v.sync_roles)
            .bind(s.user_id)
            .bind(moved)
            .execute(&mut *tx)
            .await?;
            let switched_off = moved && cur.enabled;
            if switched_off {
                end_sessions(&mut tx, cur.id, None).await?;
            }
            let audit = audit_entity(
                &mut tx,
                ws,
                Some(s.user_id),
                "idp.update",
                "identity_provider",
                cur.id,
                json!({ "issuer": v.issuer, "clientId": v.client_id, "emailDomains": v.email_domains,
                        "extraHosts": v.extra_hosts, "groupClaim": v.group_claim,
                        "mappings": v.role_mapping.as_array().map_or(0, Vec::len),
                        "syncRoles": v.sync_roles, "retest": moved, "disabled": switched_off,
                        "version": expected + 1 }),
                &corr,
            )
            .await?;
            (StatusCode::OK, cur.id, audit, moved.then_some(cur.issuer))
        }
    };
    let item = idp_of(&mut tx, id).await?;
    tx.commit().await?;
    if let Some(old) = forget {
        cache::forget(&old);
        cache::forget(&item.issuer);
    }
    Ok((
        status,
        Json(IdpResult {
            item,
            audit_event_id,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretBody {
    secret: SecretInput,
    version: i32,
}

/// Stores (or replaces) the client secret, sealed with the server keyring. Write-only.
pub async fn put_secret(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<SecretBody>, JsonRejection>,
) -> Result<Json<IdpResult>, AppError> {
    writer(&state, &s).await?;
    let company = parse(&raw)?;
    let b = body(payload)?;
    let keyring = state
        .config
        .keyring
        .clone()
        .ok_or(AppError::KeyringMissing)?;
    let value = b.secret.checked()?;
    let mut tx = state.pool.begin().await?;
    let ws = company_ws(&mut tx, company).await?;
    let cur = locked(&mut tx, ws, b.version).await?;
    let new_id = crypto::store_secret(
        &mut tx,
        &keyring,
        Some(ws),
        Purpose::OidcClientSecret,
        value.as_bytes(),
        Some(s.user_id),
    )
    .await
    .map_err(|e| AppError::Internal(Box::new(e)))?;
    sqlx::query(
        "UPDATE identity_providers SET client_secret_id = $2, client_secret_updated_at = now(), \
           updated_by = $3, updated_at = now(), version = version + 1 WHERE id = $1",
    )
    .bind(cur.id)
    .bind(new_id)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    if let Some(old) = cur.client_secret_id {
        sqlx::query("DELETE FROM encrypted_secrets WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(old)
            .execute(&mut *tx)
            .await?;
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        "idp.secret.set",
        "identity_provider",
        cur.id,
        json!({ "replaced": cur.client_secret_id.is_some(), "version": b.version + 1 }),
        &correlation(&headers),
    )
    .await?;
    let item = idp_of(&mut tx, cur.id).await?;
    tx.commit().await?;
    Ok(Json(IdpResult {
        item,
        audit_event_id,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionBody {
    version: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestOutcome {
    ok: bool,
    /// Where it failed: `policy` (address refused before any request), `discovery` or `jwks`.
    stage: Option<&'static str>,
    /// Stable reason (`unreachable`, `timeout`, `http_404`, `discovery_issuer`, …); no IdP text.
    code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    item: IdpView,
    result: TestOutcome,
    audit_event_id: Uuid,
}

async fn probe(state: &AppState, issuer: &str, extra_hosts: &[String]) -> TestOutcome {
    let fail = |stage, code: String| TestOutcome {
        ok: false,
        stage: Some(stage),
        code: Some(code),
    };
    let Some(policy) = HostPolicy::new(issuer, extra_hosts, state.config.auth.oidc_lab)
        .map(|p| p.with_proxy(state.config.egress_proxy.clone()))
    else {
        return fail("policy", "issuer_refused".into());
    };
    let timeout = Duration::from_millis(state.config.integrations.timeout_ms);
    cache::forget(issuer);
    let discovery = match cache::discovery(issuer, &policy, timeout).await {
        Ok(d) => d,
        Err(e) => return fail("discovery", e.to_string()),
    };
    match cache::jwks(issuer, &policy, &discovery, false, timeout).await {
        Ok(Some(_)) => TestOutcome {
            ok: true,
            stage: None,
            code: None,
        },
        Ok(None) => fail("jwks", "jwks".into()),
        Err(e) => fail("jwks", e.to_string()),
    }
}

/// Discovery test: fetches the discovery document and key set through the egress guard. A pass
/// marks the IdP ready to enable. A failure on an IdP that is on leaves it on (an IdP outage is
/// not a reason to cut sign-in) and is reported and audited; on an IdP that is off it is stored.
pub async fn test(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<VersionBody>, JsonRejection>,
) -> Result<Json<TestResult>, AppError> {
    writer(&state, &s).await?;
    let company = parse(&raw)?;
    let b = body(payload)?;
    // No lock across the network call: read, test, then re-check the version.
    let (ws, issuer, extra_hosts): (Uuid, String, Vec<String>) = sqlx::query_as(
        "SELECT p.workspace_id, p.issuer, p.extra_hosts FROM identity_providers p \
         JOIN workspaces w ON w.id = p.workspace_id WHERE w.company_id = $1 AND p.version = $2",
    )
    .bind(company)
    .bind(b.version)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::VersionConflict)?;
    let outcome = probe(&state, &issuer, &extra_hosts).await;
    let mut tx = state.pool.begin().await?;
    let cur = locked(&mut tx, ws, b.version).await?;
    let record = outcome.ok || !cur.enabled;
    if record {
        sqlx::query(
            "UPDATE identity_providers SET last_test_ok = $2, last_tested_at = now(), \
               updated_by = $3, updated_at = now(), version = version + 1 WHERE id = $1",
        )
        .bind(cur.id)
        .bind(outcome.ok)
        .bind(s.user_id)
        .execute(&mut *tx)
        .await?;
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        "idp.test",
        "identity_provider",
        cur.id,
        json!({ "ok": outcome.ok, "stage": outcome.stage, "code": outcome.code,
                "recorded": record, "version": if record { b.version + 1 } else { b.version } }),
        &correlation(&headers),
    )
    .await?;
    let item = idp_of(&mut tx, cur.id).await?;
    tx.commit().await?;
    Ok(Json(TestResult {
        item,
        result: outcome,
        audit_event_id,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnabledBody {
    enabled: bool,
    version: i32,
}

/// Turns sign-in through the IdP on (needs a stored secret, a passing test and the keyring) or
/// off (its sessions end at once).
pub async fn set_enabled(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<EnabledBody>, JsonRejection>,
) -> Result<Json<IdpResult>, AppError> {
    writer(&state, &s).await?;
    let company = parse(&raw)?;
    let b = body(payload)?;
    let mut tx = state.pool.begin().await?;
    let ws = company_ws(&mut tx, company).await?;
    let cur = locked(&mut tx, ws, b.version).await?;
    if cur.enabled == b.enabled {
        return Err(AppError::RuleViolation);
    }
    if b.enabled {
        if state.config.keyring.is_none() {
            return Err(AppError::KeyringMissing);
        }
        if cur.client_secret_id.is_none() || cur.last_test_ok != Some(true) {
            return Err(AppError::Conflict("idp_not_ready"));
        }
    }
    sqlx::query(
        "UPDATE identity_providers SET enabled = $2, updated_by = $3, updated_at = now(), \
           version = version + 1 WHERE id = $1",
    )
    .bind(cur.id)
    .bind(b.enabled)
    .bind(s.user_id)
    .execute(&mut *tx)
    .await?;
    if !b.enabled {
        end_sessions(&mut tx, cur.id, None).await?;
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        if b.enabled {
            "idp.enable"
        } else {
            "idp.disable"
        },
        "identity_provider",
        cur.id,
        json!({ "enabled": b.enabled, "version": b.version + 1 }),
        &correlation(&headers),
    )
    .await?;
    let item = idp_of(&mut tx, cur.id).await?;
    tx.commit().await?;
    if !b.enabled {
        cache::forget(&cur.issuer);
    }
    Ok(Json(IdpResult {
        item,
        audit_event_id,
    }))
}

// ---------- linked accounts ----------

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct IdentityView {
    id: Uuid,
    user_id: Uuid,
    display_name: String,
    email: Option<String>,
    user_active: bool,
    subject: String,
    /// Created by just-in-time provisioning (`false` = linked by an administrator).
    provisioned: bool,
    created_at: String,
    last_login_at: Option<String>,
}

fn identity_sql(filter: &str) -> String {
    format!(
        "SELECT e.id, e.user_id, u.display_name, u.email, \
           (u.active AND u.deactivated_at IS NULL) AS user_active, e.subject, \
           (e.linked_by IS NULL) AS provisioned, \
           to_char(e.created_at AT TIME ZONE 'UTC', {TS}) AS created_at, \
           to_char(e.last_login_at AT TIME ZONE 'UTC', {TS}) AS last_login_at \
         FROM external_identities e JOIN users u ON u.id = e.user_id WHERE {filter}"
    )
}

/// The company's IdP id (no lock).
async fn idp_id(conn: &mut PgConnection, company: Uuid) -> Result<(Uuid, Uuid), AppError> {
    sqlx::query_as(
        "SELECT p.id, p.workspace_id FROM identity_providers p \
         JOIN workspaces w ON w.id = p.workspace_id WHERE w.company_id = $1",
    )
    .bind(company)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct IdentityQuery {
    q: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityList {
    items: Vec<IdentityView>,
    /// Matching accounts in total (`items` holds at most 100, by name).
    total: i64,
}

/// Accounts linked to the company's IdP; `q` filters on name, e-mail or subject.
pub async fn identities(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
    query: Result<Query<IdentityQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<IdentityList>, AppError> {
    admin(&state, &s).await?;
    let company = parse(&raw)?;
    let Query(q) = query.map_err(|_| AppError::InvalidRequest)?;
    let needle = q.q.as_deref().map(str::trim).unwrap_or_default();
    if needle.chars().count() > 100 {
        return Err(AppError::Validation("q"));
    }
    let pattern = format!(
        "%{}%",
        needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut conn = state.pool.acquire().await?;
    let (idp, _) = idp_id(&mut conn, company).await?;
    let filter = "e.idp_id = $1 AND ($2 = '%%' OR u.display_name ILIKE $2 OR u.email ILIKE $2 \
                  OR e.subject ILIKE $2)";
    let items = sqlx::query_as(&format!(
        "{} ORDER BY u.display_name, e.id LIMIT {IDENTITY_PAGE}",
        identity_sql(filter)
    ))
    .bind(idp)
    .bind(&pattern)
    .fetch_all(&mut *conn)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM external_identities e JOIN users u ON u.id = e.user_id WHERE {filter}"
    ))
    .bind(idp)
    .bind(&pattern)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Json(IdentityList { items, total }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkBody {
    user_id: Uuid,
    subject: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityResult {
    item: IdentityView,
    audit_event_id: Uuid,
}

/// Links an existing account to an IdP subject (the `sub` the IdP sends), for accounts that
/// cannot be provisioned by address. The account must not work in another company.
pub async fn link(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<LinkBody>, JsonRejection>,
) -> Result<(StatusCode, Json<IdentityResult>), AppError> {
    writer(&state, &s).await?;
    let company = parse(&raw)?;
    let b = body(payload)?;
    let subject = b.subject.trim();
    if !(1..=255).contains(&subject.len()) || subject.chars().any(char::is_control) {
        return Err(AppError::Validation("subject"));
    }
    let mut tx = state.pool.begin().await?;
    let (idp, ws) = idp_id(&mut tx, company).await?;
    let active: bool = sqlx::query_scalar(
        "SELECT (active AND deactivated_at IS NULL) FROM users WHERE id = $1 FOR UPDATE",
    )
    .bind(b.user_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Validation("userId"))?;
    if !active {
        return Err(AppError::Conflict("user_inactive"));
    }
    one_company(&mut tx, b.user_id, ws).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO external_identities (user_id, idp_id, subject, linked_by) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(b.user_id)
    .bind(idp)
    .bind(subject)
    .bind(s.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d)
            if d.constraint() == Some("external_identities_idp_id_subject_key") =>
        {
            AppError::Conflict("subject_linked")
        }
        sqlx::Error::Database(d)
            if d.constraint() == Some("external_identities_idp_id_user_id_key") =>
        {
            AppError::Conflict("user_linked")
        }
        _ => AppError::from(e),
    })?;
    auth_event_by(
        &mut tx,
        Some(s.user_id),
        Some(b.user_id),
        "",
        "identity_link",
        "success",
        None,
    )
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        "identity.link",
        "external_identity",
        id,
        json!({ "userId": b.user_id, "idpId": idp }),
        &correlation(&headers),
    )
    .await?;
    let item = sqlx::query_as(&identity_sql("e.id = $1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(IdentityResult {
            item,
            audit_event_id,
        }),
    ))
}

/// Removes a link (provisioned or admin-made). The account and its memberships stay; its SSO
/// sessions through this IdP end. Signing in again would provision a new link only when the
/// address rules allow it (an existing account's address is never taken over).
pub async fn unlink(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw, raw_identity)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    writer(&state, &s).await?;
    let company = parse(&raw)?;
    let identity = parse(&raw_identity)?;
    let mut tx = state.pool.begin().await?;
    let (idp, ws) = idp_id(&mut tx, company).await?;
    let (user, provisioned): (Uuid, bool) = sqlx::query_as(
        "DELETE FROM external_identities WHERE id = $1 AND idp_id = $2 \
         RETURNING user_id, (linked_by IS NULL)",
    )
    .bind(identity)
    .bind(idp)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    end_sessions(&mut tx, idp, Some(user)).await?;
    auth_event_by(
        &mut tx,
        Some(s.user_id),
        Some(user),
        "",
        "identity_unlink",
        "success",
        None,
    )
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        "identity.unlink",
        "external_identity",
        identity,
        json!({ "userId": user, "idpId": idp, "provisioned": provisioned }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "id": identity, "auditEventId": audit_event_id }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(issuer: &str) -> IdpInput {
        IdpInput {
            display_name: " SSO Alfa ".into(),
            issuer: issuer.into(),
            client_id: "client-a".into(),
            email_domains: vec![
                "Alfa.Example.Test".into(),
                "alfa.example.test".into(),
                " ".into(),
            ],
            extra_hosts: vec![],
            group_claim: "groups".into(),
            role_mapping: vec![MappingInput {
                group: "g-cs".into(),
                role: "cs".into(),
            }],
            sync_roles: false,
            version: None,
        }
    }

    #[test]
    fn settings_are_normalised_and_checked() {
        let v = validate(&input("https://login.example.test/t/v2.0"), false);
        let Ok(v) = v else {
            panic!("valid input refused")
        };
        assert_eq!(v.display_name, "SSO Alfa");
        assert_eq!(v.email_domains, ["alfa.example.test"]);
        for bad in [
            "http://login.example.test",
            "https://login.example.test:8443",
            "https://u:p@login.example.test",
            "https://login.example.test/?x=1",
            "https://login.example.test/#f",
            "login.example.test",
        ] {
            assert!(validate(&input(bad), false).is_err(), "{bad}");
        }
        assert!(validate(&input("http://127.0.0.1:9000"), true).is_ok());
        let mut b = input("https://login.example.test");
        b.role_mapping.push(MappingInput {
            group: "g-cs".into(),
            role: "lead_cs".into(),
        });
        assert!(validate(&b, false).is_err(), "duplicate group");
        let mut b = input("https://login.example.test");
        b.role_mapping[0].role = "admin".into();
        assert!(validate(&b, false).is_err(), "unknown role");
        let mut b = input("https://login.example.test");
        b.email_domains = vec!["not a domain".into()];
        assert!(validate(&b, false).is_err());
        let mut b = input("https://login.example.test");
        b.group_claim = "groups claim".into();
        assert!(validate(&b, false).is_err());
    }
}
