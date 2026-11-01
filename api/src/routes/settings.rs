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
    active: bool,
    /// Concurrency token for settings changes (workspace version).
    version: i32,
    member_count: i64,
}

const SETTINGS_SQL: &str = "SELECT w.id AS workspace_id, w.code AS workspace_code, \
        w.name AS workspace_name, w.timezone, c.id AS company_id, c.code AS company_code, \
        c.name AS company_name, c.reporter_code, (w.active AND c.active) AS active, w.version, \
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
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::AdminSettings)?;
    let b = body(payload)?;
    let workspace_name = text(&b.workspace_name, 200, "workspaceName")?;
    let company_name = text(&b.company_name, 200, "companyName")?;
    let tz = timezone(&b.timezone)?;
    let reporter = optional_text(b.reporter_code.as_deref(), 40, "reporterCode")?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let current: (i32, Uuid, String, String, String, Option<String>) = sqlx::query_as(
        "SELECT w.version, c.id, w.name, c.name, w.timezone, c.reporter_code \
         FROM workspaces w JOIN companies c ON c.id = w.company_id \
         WHERE w.id = $1 FOR UPDATE OF w, c",
    )
    .bind(ws)
    .fetch_one(&mut *tx)
    .await?;
    let (version, company, old_ws_name, old_co_name, old_tz, old_reporter) = current;
    if version != b.version {
        return Err(AppError::VersionConflict);
    }
    let mut changed = Vec::new();
    if old_ws_name != workspace_name {
        changed.push("workspaceName");
    }
    if old_co_name != company_name {
        changed.push("companyName");
    }
    if old_tz != tz {
        changed.push("timezone");
    }
    if old_reporter != reporter {
        changed.push("reporterCode");
    }
    // Nothing changed: no version bump, no audit row.
    if changed.is_empty() {
        let settings = settings_of(&mut tx, ws).await?;
        tx.commit().await?;
        return Ok(Json(SettingsResult {
            settings,
            audit_event_id: None,
        }));
    }
    sqlx::query(
        "UPDATE workspaces SET name = $2, timezone = $3, version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(ws)
    .bind(&workspace_name)
    .bind(tz)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE companies SET name = $2, reporter_code = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(company)
    .bind(&company_name)
    .bind(&reporter)
    .execute(&mut *tx)
    .await?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "settings.update",
        "workspace",
        ws,
        serde_json::json!({ "changed": changed, "version": version + 1 }),
        &correlation(&headers),
    )
    .await?;
    let settings = settings_of(&mut tx, ws).await?;
    tx.commit().await?;
    Ok(Json(SettingsResult {
        settings,
        audit_event_id: Some(audit_event_id),
    }))
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
    superadmin(&state, &s)?;
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
    superadmin(&state, &s)?;
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
