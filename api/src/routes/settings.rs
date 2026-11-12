//! D09 settings: workspace/company profile, company + workspace creation and activation
//! (UAT Superadmin only), and the per-workspace catalog editor.
//!
//! Default deny: reads need `catalog.read` (every member, read-only); changes need the
//! explicit `admin.settings` grant (Lead CS has none by role). Creating, deactivating and
//! reactivating a company is platform administration and only the UAT Superadmin may do it;
//! a new company never grants access to anyone. Every change checks the caller's version
//! (409 on mismatch) and writes its audit row in the same transaction.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission};
use crate::config::AccessMode;
use crate::error::AppError;
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::{ListBody, parse_id};
use crate::session::Session;

/// Business time zones of Indonesia (WIB/WITA/WIT). Nothing else is accepted.
pub const TIMEZONES: [&str; 3] = ["Asia/Jakarta", "Asia/Makassar", "Asia/Jayapura"];

/// PostgreSQL `unique_violation`.
const UNIQUE_VIOLATION: &str = "23505";

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some(UNIQUE_VIOLATION))
}

fn text(raw: &str, max: usize, field: &'static str) -> Result<String, AppError> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(AppError::Validation(field));
    }
    Ok(t.to_owned())
}

fn optional_text(
    raw: Option<&str>,
    max: usize,
    field: &'static str,
) -> Result<Option<String>, AppError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(t) if t.chars().count() > max => Err(AppError::Validation(field)),
        Some(t) => Ok(Some(t.to_owned())),
    }
}

fn timezone(raw: &str) -> Result<&'static str, AppError> {
    TIMEZONES
        .into_iter()
        .find(|t| *t == raw)
        .ok_or(AppError::Validation("timezone"))
}

/// Branding presets bundled with the web app (logo + palette). Nothing else is accepted: a
/// company never points at a free URL or an uploaded file.
pub const BRAND_KEYS: [&str; 3] = ["deca", "ajr", "bmt"];

fn brand_key(raw: Option<&str>) -> Result<Option<&'static str>, AppError> {
    raw.map(|k| {
        BRAND_KEYS
            .into_iter()
            .find(|b| *b == k)
            .ok_or(AppError::Validation("brandKey"))
    })
    .transpose()
}

fn company_code(raw: &str) -> Result<String, AppError> {
    let c = raw.trim();
    let ok = (2..=10).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if ok {
        Ok(c.to_owned())
    } else {
        Err(AppError::Validation("code"))
    }
}

/// Capability named in platform denied audit rows for company administration.
pub const PLATFORM_COMPANY_ADMIN: &str = "platform.companies";

/// CP5a: company and workspace configuration is not edited from the app outside the UAT
/// Superadmin mode (companies come from SSO later). Writes answer 404 there, like an unknown
/// route, so nothing about the old editor is exposed.
fn config_writes_available(state: &AppState) -> Result<(), AppError> {
    if state.config.access_mode == AccessMode::UatSuperadmin {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

fn superadmin(state: &AppState, s: &Session) -> Result<(), AppError> {
    if authz::superadmin_effective(state, s) {
        Ok(())
    } else {
        tracing::info!(user_id = %s.user_id, "platform administration denied");
        crate::denied_audit::record_platform(s.user_id, PLATFORM_COMPANY_ADMIN);
        Err(AppError::Forbidden)
    }
}

// ---------- workspace settings ----------

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSettings {
    workspace_id: Uuid,
    workspace_code: String,
    workspace_name: String,
    timezone: String,
    company_id: Uuid,
    company_code: String,
    company_name: String,
    reporter_code: Option<String>,
    /// Branding preset (`deca` | `ajr` | `bmt`); `None` = not configured (neutral look).
    brand_key: Option<String>,
    /// Display name override; `None` = the company name.
    brand_name: Option<String>,
    /// Codes this workspace used before, oldest first. Tickets issued under them keep their
    /// numbers, and the codes stay reserved for this workspace.
    previous_codes: Vec<String>,
    active: bool,
    /// Concurrency token for settings changes (workspace version).
    version: i32,
    member_count: i64,
}

const SETTINGS_SQL: &str = "SELECT w.id AS workspace_id, w.code AS workspace_code, \
        w.name AS workspace_name, w.timezone, c.id AS company_id, c.code AS company_code, \
        c.name AS company_name, c.reporter_code, c.brand_key, c.brand_name, \
        ARRAY(SELECT h.code FROM workspace_code_history h \
              WHERE h.workspace_id = w.id AND h.code <> w.code ORDER BY h.first_used_at, h.code) \
            AS previous_codes, \
        (w.active AND c.active) AS active, w.version, \
        (SELECT count(*) FROM workspace_memberships m WHERE m.workspace_id = w.id AND m.active) \
            AS member_count \
     FROM workspaces w JOIN companies c ON c.id = w.company_id";

async fn settings_of(conn: &mut PgConnection, ws: Uuid) -> Result<WorkspaceSettings, AppError> {
    Ok(
        sqlx::query_as::<_, WorkspaceSettings>(&format!("{SETTINGS_SQL} WHERE w.id = $1"))
            .bind(ws)
            .fetch_one(&mut *conn)
            .await?,
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBody {
    settings: WorkspaceSettings,
    /// Caller holds `admin.settings` here (UI hint only; enforced per request).
    can_edit: bool,
    timezones: [&'static str; 3],
}

pub async fn get_settings(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<SettingsBody>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::CatalogRead)?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(SettingsBody {
        settings: settings_of(&mut conn, access.workspace_id).await?,
        can_edit: access.allows(Permission::AdminSettings),
        timezones: TIMEZONES,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    workspace_name: String,
    company_name: String,
    timezone: String,
    reporter_code: Option<String>,
    version: i32,
}

/// Company administration edits the profile plus the branding. Both branding fields are
/// required (each may be null) so a client that omits them cannot clear them by accident.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyProfilePatch {
    /// Business code of the company and its workspace (prefix of NEW ticket numbers).
    code: String,
    workspace_name: String,
    company_name: String,
    timezone: String,
    reporter_code: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    brand_key: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    brand_name: Option<String>,
    version: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResult {
    settings: WorkspaceSettings,
    audit_event_id: Option<Uuid>,
}

pub async fn update_settings(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<SettingsPatch>, JsonRejection>,
) -> Result<Json<SettingsResult>, AppError> {
    config_writes_available(&state)?;
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::AdminSettings)?;
    let b = body(payload)?;
    let profile = Profile::parse(&b)?;
    let mut tx = state.pool.begin().await?;
    let (ws, company, version): (Uuid, Uuid, i32) = sqlx::query_as(
        "SELECT w.id, c.id, w.version FROM workspaces w JOIN companies c ON c.id = w.company_id \
         WHERE w.id = $1 FOR UPDATE OF w, c",
    )
    .bind(access.workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    let audit_event_id = apply_profile(
        &mut tx,
        ProfileTarget {
            workspace: ws,
            company,
            version,
            expected: b.version,
        },
        &profile,
        None,
        access.user_id,
        ("settings.update", "workspace", ws),
        &headers,
    )
    .await?;
    let settings = settings_of(&mut tx, ws).await?;
    tx.commit().await?;
    Ok(Json(SettingsResult {
        settings,
        audit_event_id,
    }))
}

/// Validated editable profile fields. Workspace settings cannot change the code (the body
/// rejects unknown fields); only company administration can, see [`update_company`].
struct Profile {
    workspace_name: String,
    company_name: String,
    timezone: &'static str,
    reporter_code: Option<String>,
    /// New company/workspace code; None keeps it (workspace settings never send one).
    code: Option<String>,
}

impl Profile {
    fn parse(b: &SettingsPatch) -> Result<Self, AppError> {
        Self::from_parts(
            &b.workspace_name,
            &b.company_name,
            &b.timezone,
            b.reporter_code.as_deref(),
        )
    }

    fn from_parts(
        workspace_name: &str,
        company_name: &str,
        tz: &str,
        reporter_code: Option<&str>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            workspace_name: text(workspace_name, 200, "workspaceName")?,
            company_name: text(company_name, 200, "companyName")?,
            timezone: timezone(tz)?,
            reporter_code: optional_text(reporter_code, 40, "reporterCode")?,
            code: None,
        })
    }
}

/// Validated branding: a bundled preset (or none) and an optional display name.
struct Brand {
    key: Option<&'static str>,
    name: Option<String>,
}

impl Brand {
    fn parse(b: &CompanyProfilePatch) -> Result<Self, AppError> {
        Ok(Self {
            key: brand_key(b.brand_key.as_deref())?,
            name: optional_text(b.brand_name.as_deref(), 60, "brandName")?,
        })
    }
}

/// Current workspace name, company name, time zone, reporter code, brand key, brand name and
/// workspace code.
type OldProfile = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

/// Rows the caller already locked `FOR UPDATE OF w, c`.
struct ProfileTarget {
    workspace: Uuid,
    company: Uuid,
    /// Current workspace version (the listed concurrency token).
    version: i32,
    /// Version the caller saw.
    expected: i32,
}

/// Applies a profile change inside the caller's transaction: 409 on a stale version; when
/// nothing changed, no version bump and no audit row (`None`); otherwise both rows bump
/// their version and one audit row names the changed fields (not their values). Branding is
/// only touched when `brand` is given (company administration); workspace settings leave it.
async fn apply_profile(
    tx: &mut PgConnection,
    t: ProfileTarget,
    p: &Profile,
    brand: Option<&Brand>,
    actor: Uuid,
    (action, entity, entity_id): (&str, &str, Uuid),
    headers: &HeaderMap,
) -> Result<Option<Uuid>, AppError> {
    if t.version != t.expected {
        return Err(AppError::VersionConflict);
    }
    let old: OldProfile = sqlx::query_as(
        "SELECT w.name, c.name, w.timezone, c.reporter_code, c.brand_key, c.brand_name, w.code \
         FROM workspaces w JOIN companies c ON c.id = w.company_id WHERE w.id = $1",
    )
    .bind(t.workspace)
    .fetch_one(&mut *tx)
    .await?;
    let (old_ws_name, old_co_name, old_tz, old_reporter, old_brand_key, old_brand_name, old_code) =
        old;
    let new_code = p.code.as_deref().filter(|c| *c != old_code);
    let mut changed = Vec::new();
    if new_code.is_some() {
        changed.push("code");
    }
    if old_ws_name != p.workspace_name {
        changed.push("workspaceName");
    }
    if old_co_name != p.company_name {
        changed.push("companyName");
    }
    if old_tz != p.timezone {
        changed.push("timezone");
    }
    if old_reporter != p.reporter_code {
        changed.push("reporterCode");
    }
    if let Some(b) = brand {
        if old_brand_key.as_deref() != b.key {
            changed.push("brandKey");
        }
        if old_brand_name != b.name {
            changed.push("brandName");
        }
    }
    if changed.is_empty() {
        return Ok(None);
    }
    // A new code only relabels NEW ticket numbers: ids, keys and issued numbers stay as they
    // are. The database reserves every code per workspace forever (migration 0014), so a code
    // held now or earlier by another workspace answers unique_violation → 422 `code`.
    let unique_code = |e: sqlx::Error| {
        if is_unique_violation(&e) {
            AppError::Validation("code")
        } else {
            e.into()
        }
    };
    sqlx::query(
        "UPDATE workspaces SET name = $2, timezone = $3, code = COALESCE($4, code), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(t.workspace)
    .bind(&p.workspace_name)
    .bind(p.timezone)
    .bind(new_code)
    .execute(&mut *tx)
    .await
    .map_err(unique_code)?;
    sqlx::query(
        "UPDATE companies SET name = $2, reporter_code = $3, \
         brand_key = CASE WHEN $4 THEN $5 ELSE brand_key END, \
         brand_name = CASE WHEN $4 THEN $6 ELSE brand_name END, \
         code = COALESCE($7, code), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(t.company)
    .bind(&p.company_name)
    .bind(&p.reporter_code)
    .bind(brand.is_some())
    .bind(brand.and_then(|b| b.key))
    .bind(brand.and_then(|b| b.name.as_deref()))
    .bind(new_code)
    .execute(&mut *tx)
    .await
    .map_err(unique_code)?;
    let mut change = serde_json::json!({ "changed": changed, "version": t.version + 1 });
    if let Some(c) = new_code {
        // Business codes are not secret; the trail shows which prefix applied from when.
        change["previousCode"] = old_code.into();
        change["code"] = c.into();
    }
    let id = audit_entity(
        tx,
        t.workspace,
        Some(actor),
        action,
        entity,
        entity_id,
        change,
        &correlation(headers),
    )
    .await?;
    Ok(Some(id))
}

// ---------- companies (UAT Superadmin only) ----------

pub async fn list_companies(
    State(state): State<AppState>,
    s: Session,
) -> Result<Json<ListBody<WorkspaceSettings>>, AppError> {
    superadmin(&state, &s)?;
    let items =
        sqlx::query_as::<_, WorkspaceSettings>(&format!("{SETTINGS_SQL} ORDER BY c.code, w.code"))
            .fetch_all(&state.pool)
            .await?;
    Ok(Json(ListBody { items }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyCreate {
    code: String,
    name: String,
    timezone: String,
    reporter_code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyResult {
    item: WorkspaceSettings,
    audit_event_id: Uuid,
}

/// Existing priority scale for a new workspace (D01 freeze: no new scale).
const PRIORITIES: [(&str, &str); 4] = [
    ("low", "Rendah"),
    ("normal", "Normal"),
    ("high", "Tinggi"),
    ("urgent", "Mendesak"),
];

/// Creates a company and its workspace atomically. Nobody gets access automatically:
/// memberships are added afterwards through membership administration.
pub async fn create_company(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    payload: Result<Json<CompanyCreate>, JsonRejection>,
) -> Result<(StatusCode, Json<CompanyResult>), AppError> {
    config_writes_available(&state)?;
    superadmin(&state, &s)?;
    authz::require_step_up(&state, &s)?;
    let b = body(payload)?;
    let code = company_code(&b.code)?;
    let name = text(&b.name, 200, "name")?;
    let tz = timezone(&b.timezone)?;
    let reporter = optional_text(b.reporter_code.as_deref(), 40, "reporterCode")?;
    let mut tx = state.pool.begin().await?;
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM companies WHERE code = $1) \
             OR EXISTS (SELECT 1 FROM workspaces WHERE code = $1)",
    )
    .bind(&code)
    .fetch_one(&mut *tx)
    .await?;
    if taken {
        return Err(AppError::Validation("code"));
    }
    let company: Uuid = match sqlx::query_scalar(
        "INSERT INTO companies (code, name, reporter_code) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(&code)
    .bind(&name)
    .bind(&reporter)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(id) => id,
        Err(e) if is_unique_violation(&e) => return Err(AppError::Validation("code")),
        Err(e) => return Err(e.into()),
    };
    let ws: Uuid = match sqlx::query_scalar(
        "INSERT INTO workspaces (company_id, code, name, timezone) VALUES ($1, $2, $3, $4) \
         RETURNING id",
    )
    .bind(company)
    .bind(&code)
    .bind(&name)
    .bind(tz)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(id) => id,
        Err(e) if is_unique_violation(&e) => return Err(AppError::Validation("code")),
        Err(e) => return Err(e.into()),
    };
    for (order, (code, label)) in (0_i32..).zip(PRIORITIES) {
        sqlx::query(
            "INSERT INTO priorities (workspace_id, code, label, sort_order) VALUES ($1, $2, $3, $4)",
        )
        .bind(ws)
        .bind(code)
        .bind(label)
        .bind(order)
        .execute(&mut *tx)
        .await?;
    }
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        "company.create",
        "company",
        company,
        serde_json::json!({ "companyCode": code, "workspaceId": ws, "timezone": tz,
                            "reporterCode": reporter.is_some(), "membershipsCreated": 0 }),
        &correlation(&headers),
    )
    .await?;
    let item = settings_of(&mut tx, ws).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(CompanyResult {
            item,
            audit_event_id,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyActivation {
    active: bool,
    /// Workspace version as listed.
    version: i32,
}

/// Deactivates or reactivates a company and its workspace. History stays; while inactive
/// every workspace request answers 404 and the workspace takes no new input.
pub async fn set_company_active(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<CompanyActivation>, JsonRejection>,
) -> Result<Json<CompanyResult>, AppError> {
    config_writes_available(&state)?;
    superadmin(&state, &s)?;
    authz::require_step_up(&state, &s)?;
    let company = parse_id(&raw)?;
    let b = body(payload)?;
    let mut tx = state.pool.begin().await?;
    let (ws, version, active): (Uuid, i32, bool) = sqlx::query_as(
        "SELECT w.id, w.version, (w.active AND c.active) FROM companies c \
         JOIN workspaces w ON w.company_id = c.id WHERE c.id = $1 FOR UPDATE OF w, c",
    )
    .bind(company)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if version != b.version {
        return Err(AppError::VersionConflict);
    }
    if active == b.active {
        return Err(AppError::RuleViolation);
    }
    sqlx::query(
        "UPDATE companies SET active = $2, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(company)
    .bind(b.active)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE workspaces SET active = $2, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(ws)
    .bind(b.active)
    .execute(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(s.user_id),
        if b.active {
            "company.activate"
        } else {
            "company.deactivate"
        },
        "company",
        company,
        serde_json::json!({ "active": b.active, "version": version + 1 }),
        &correlation(&headers),
    )
    .await?;
    let item = settings_of(&mut tx, ws).await?;
    tx.commit().await?;
    Ok(Json(CompanyResult {
        item,
        audit_event_id,
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyUpdateResult {
    item: WorkspaceSettings,
    /// `None` when nothing changed (no version bump, no audit row).
    audit_event_id: Option<Uuid>,
}

/// Edits an existing company's profile (code, company and workspace name, time zone, reporter
/// code, branding), active or not. A new code changes the company and workspace code together
/// and prefixes only tickets issued from now on: each ticket keeps the prefix it was issued
/// with, the counter continues, and the old code stays reserved for this workspace. The BI
/// reporter code is a separate field and never follows the code. Tickets, history and keys
/// are untouched; only the two profile rows change.
pub async fn update_company(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<CompanyProfilePatch>, JsonRejection>,
) -> Result<Json<CompanyUpdateResult>, AppError> {
    config_writes_available(&state)?;
    superadmin(&state, &s)?;
    let company = parse_id(&raw)?;
    let b = body(payload)?;
    let mut profile = Profile::from_parts(
        &b.workspace_name,
        &b.company_name,
        &b.timezone,
        b.reporter_code.as_deref(),
    )?;
    profile.code = Some(company_code(&b.code)?);
    let brand = Brand::parse(&b)?;
    let mut tx = state.pool.begin().await?;
    let (ws, version): (Uuid, i32) = sqlx::query_as(
        "SELECT w.id, w.version FROM companies c JOIN workspaces w ON w.company_id = c.id \
         WHERE c.id = $1 FOR UPDATE OF w, c",
    )
    .bind(company)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let audit_event_id = apply_profile(
        &mut tx,
        ProfileTarget {
            workspace: ws,
            company,
            version,
            expected: b.version,
        },
        &profile,
        Some(&brand),
        s.user_id,
        ("company.update", "company", company),
        &headers,
    )
    .await?;
    let item = settings_of(&mut tx, ws).await?;
    tx.commit().await?;
    Ok(Json(CompanyUpdateResult {
        item,
        audit_event_id,
    }))
}

// ---------- catalog editor ----------

/// Editable catalogs: URL segment → table. Table names are compile-time constants.
const CATALOG_KINDS: [(&str, &str); 6] = [
    ("priorities", "priorities"),
    ("categories", "categories"),
    ("products", "products"),
    ("receiving-units", "receiving_units"),
    ("regions", "regions"),
    ("intake-channels", "intake_channels"),
];

fn catalog_table(kind: &str) -> Result<&'static str, AppError> {
    CATALOG_KINDS
        .into_iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, t)| t)
        .ok_or(AppError::NotFound)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    id: Uuid,
    code: String,
    label: String,
    active: bool,
    sort_order: i32,
    version: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntryResult {
    kind: String,
    item: CatalogEntry,
    /// `null` when nothing changed (no version bump, no audit row).
    audit_event_id: Option<Uuid>,
}

fn catalog_code(raw: &str) -> Result<String, AppError> {
    let c = raw.trim();
    let ok = (1..=40).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(c.to_owned())
    } else {
        Err(AppError::Validation("code"))
    }
}

fn sort_order(v: i32) -> Result<i32, AppError> {
    if (0..=9999).contains(&v) {
        Ok(v)
    } else {
        Err(AppError::Validation("sortOrder"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogCreate {
    code: String,
    label: String,
    sort_order: i32,
}

pub async fn create_catalog_entry(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, kind)): Path<(String, String)>,
    payload: Result<Json<CatalogCreate>, JsonRejection>,
) -> Result<(StatusCode, Json<CatalogEntryResult>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let table = catalog_table(&kind)?;
    access.require(Permission::AdminSettings)?;
    let b = body(payload)?;
    // Priority is the existing fixed scale: labels/order/active may change, codes may not.
    if table == "priorities" {
        return Err(AppError::RuleViolation);
    }
    let code = catalog_code(&b.code)?;
    let label = text(&b.label, 120, "label")?;
    let order = sort_order(b.sort_order)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let sql = format!(
        "INSERT INTO {table} (workspace_id, code, label, sort_order) VALUES ($1, $2, $3, $4) \
         RETURNING id, code, label, active, sort_order, version"
    );
    let item = match sqlx::query_as::<_, CatalogEntry>(&sql)
        .bind(ws)
        .bind(&code)
        .bind(&label)
        .bind(order)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(item) => item,
        // Duplicate code (active or not) in this workspace.
        Err(e) if is_unique_violation(&e) => return Err(AppError::Validation("code")),
        Err(e) => return Err(e.into()),
    };
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "catalog.create",
        table,
        item.id,
        serde_json::json!({ "kind": kind, "code": code, "label": label, "sortOrder": order }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(CatalogEntryResult {
            kind,
            item,
            audit_event_id: Some(audit_event_id),
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogPatch {
    label: String,
    active: bool,
    sort_order: i32,
    version: i32,
}

/// Changes label, order or active flag. The code is stable and entries are never deleted
/// (deactivated entries stay on existing tickets and history; they are not offered for new
/// input).
pub async fn update_catalog_entry(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, kind, raw_id)): Path<(String, String, String)>,
    payload: Result<Json<CatalogPatch>, JsonRejection>,
) -> Result<Json<CatalogEntryResult>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let table = catalog_table(&kind)?;
    access.require(Permission::AdminSettings)?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let label = text(&b.label, 120, "label")?;
    let order = sort_order(b.sort_order)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let old = sqlx::query_as::<_, CatalogEntry>(&format!(
        "SELECT id, code, label, active, sort_order, version FROM {table} \
         WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if old.version != b.version {
        return Err(AppError::VersionConflict);
    }
    if old.label == label && old.active == b.active && old.sort_order == order {
        tx.commit().await?;
        return Ok(Json(CatalogEntryResult {
            kind,
            item: old,
            audit_event_id: None,
        }));
    }
    let item = sqlx::query_as::<_, CatalogEntry>(&format!(
        "UPDATE {table} SET label = $3, active = $4, sort_order = $5, version = version + 1 \
         WHERE workspace_id = $1 AND id = $2 \
         RETURNING id, code, label, active, sort_order, version"
    ))
    .bind(ws)
    .bind(id)
    .bind(&label)
    .bind(b.active)
    .bind(order)
    .fetch_one(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "catalog.update",
        table,
        id,
        serde_json::json!({
            "kind": kind,
            "code": old.code,
            "label": { "from": old.label, "to": item.label },
            "active": { "from": old.active, "to": item.active },
            "sortOrder": { "from": old.sort_order, "to": item.sort_order },
            "version": item.version,
        }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(CatalogEntryResult {
        kind,
        item,
        audit_event_id: Some(audit_event_id),
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEditorBody {
    kind: String,
    items: Vec<CatalogEntry>,
    can_edit: bool,
}

/// One catalog with versions (for the editor). Members read; `canEdit` = `admin.settings`.
pub async fn list_catalog(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, kind)): Path<(String, String)>,
) -> Result<Json<CatalogEditorBody>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let table = catalog_table(&kind)?;
    access.require(Permission::CatalogRead)?;
    let items = sqlx::query_as::<_, CatalogEntry>(&format!(
        "SELECT id, code, label, active, sort_order, version FROM {table} \
         WHERE workspace_id = $1 ORDER BY sort_order, label, id"
    ))
    .bind(access.workspace_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(CatalogEditorBody {
        kind,
        items,
        can_edit: access.allows(Permission::AdminSettings),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_follow_database_rules() {
        assert!(company_code("DUMC").is_ok());
        assert!(company_code("dumc").is_err());
        assert!(company_code("D").is_err());
        assert!(company_code("ABCDEFGHIJK").is_err());
        assert!(catalog_code("kategori_baru").is_ok());
        assert!(catalog_code("Kategori").is_err());
        assert!(catalog_code("").is_err());
        assert!(timezone("Asia/Makassar").is_ok());
        assert!(timezone("UTC").is_err());
    }
}
