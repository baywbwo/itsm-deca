//! D09 membership administration (C24). Default deny:
//! - list: every member (`member.read`); grant codes are shown only to `admin.members`;
//! - add / change role / deactivate / reactivate a membership and grant / revoke an
//!   administration grant need the explicit `admin.members` grant (Lead CS has none by role);
//! - nobody changes their own membership or grants (no self-escalation, no self-lockout);
//! - a grantor can only give or revoke codes they hold themselves;
//! - deactivating a membership revokes its live grants in the same transaction, so a later
//!   reactivation never brings old rights back silently. Access stops on the next request
//!   (every request re-reads the membership).
//!
//! Every change checks the caller's version (409) and writes its audit row atomically.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, PermissionSource, Role, WorkspaceAccess};
use crate::error::AppError;
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    membership_id: Uuid,
    user_id: Uuid,
    display_name: String,
    role: String,
    active: bool,
    version: i32,
    /// Live administration grants; empty unless the caller holds `admin.members`.
    grants: Vec<String>,
}

const MEMBER_SQL: &str = "SELECT m.id AS membership_id, m.user_id, u.display_name, m.role, \
        m.active, m.version, \
        CASE WHEN $2 THEN ARRAY(SELECT g.grant_code FROM workspace_grants g \
              WHERE g.workspace_id = m.workspace_id AND g.membership_id = m.id \
                AND g.revoked_at IS NULL ORDER BY g.grant_code) \
             ELSE ARRAY[]::text[] END AS grants \
     FROM workspace_memberships m JOIN users u ON u.id = m.user_id \
     WHERE m.workspace_id = $1";

async fn one(
    conn: &mut PgConnection,
    ws: Uuid,
    membership: Uuid,
    with_grants: bool,
) -> Result<MemberView, AppError> {
    sqlx::query_as::<_, MemberView>(&format!("{MEMBER_SQL} AND m.id = $3"))
        .bind(ws)
        .bind(with_grants)
        .bind(membership)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AppError::NotFound)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MembersBody {
    items: Vec<MemberView>,
    /// Caller's own membership here (`null` for UAT Superadmin without membership).
    my_membership_id: Option<Uuid>,
    /// Caller holds `admin.members` (UI hint only; enforced per request).
    can_manage: bool,
    /// Grant codes the caller may give or revoke (the ones they hold).
    grantable: Vec<&'static str>,
}

fn grantable(access: &WorkspaceAccess) -> Vec<&'static str> {
    if !access.allows(Permission::AdminMembers) {
        return Vec::new();
    }
    Permission::ALL
        .into_iter()
        .filter(|p| p.source() == PermissionSource::Grant && access.allows(*p))
        .map(Permission::code)
        .collect()
}

pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<MembersBody>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::MemberRead)?;
    let manage = access.allows(Permission::AdminMembers);
    let items = sqlx::query_as::<_, MemberView>(&format!(
        "{MEMBER_SQL} ORDER BY m.active DESC, u.display_name, m.user_id"
    ))
    .bind(access.workspace_id)
    .bind(manage)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(MembersBody {
        items,
        my_membership_id: access.membership_id,
        can_manage: manage,
        grantable: grantable(&access),
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberResult {
    item: MemberView,
    audit_event_id: Uuid,
}

fn role(raw: &str) -> Result<Role, AppError> {
    Role::from_code(raw).ok_or(AppError::Validation("role"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberCreate {
    /// Account identifier (external subject) of an existing active account.
    subject: String,
    role: String,
}

pub async fn add(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw): Path<String>,
    payload: Result<Json<MemberCreate>, JsonRejection>,
) -> Result<(StatusCode, Json<MemberResult>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::AdminMembers)?;
    let b = body(payload)?;
    let subject = b.subject.trim();
    if subject.is_empty() || subject.chars().count() > 200 {
        return Err(AppError::Validation("subject"));
    }
    let r = role(&b.role)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let user: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = $1 AND active")
            .bind(subject)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::Validation("subject"))?;
    one_company(&mut tx, user, ws).await?;
    // Existing (possibly inactive) membership: reactivate through PATCH, never a duplicate.
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role) VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, user_id) DO NOTHING RETURNING id",
    )
    .bind(ws)
    .bind(user)
    .bind(r.code())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Validation("subject"))?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "membership.add",
        "workspace_membership",
        id,
        serde_json::json!({ "userId": user, "role": r.code() }),
        &correlation(&headers),
    )
    .await?;
    let item = one(&mut tx, ws, id, true).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(MemberResult {
            item,
            audit_event_id,
        }),
    ))
}

/// CP5a: one company per user. An active membership elsewhere would never open (the server
/// picks one home company, `home_membership_sql!`), so refuse it instead of keeping a dead row.
async fn one_company(conn: &mut sqlx::PgConnection, user: Uuid, ws: Uuid) -> Result<(), AppError> {
    let elsewhere: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM workspace_memberships m \
         JOIN workspaces w ON w.id = m.workspace_id \
         WHERE m.user_id = $1 AND m.active AND w.active AND m.workspace_id <> $2)",
    )
    .bind(user)
    .bind(ws)
    .fetch_one(&mut *conn)
    .await?;
    if elsewhere {
        return Err(AppError::Conflict("user_in_other_company"));
    }
    Ok(())
}

fn not_self(access: &WorkspaceAccess, membership: Uuid) -> Result<(), AppError> {
    if access.membership_id == Some(membership) {
        tracing::info!(workspace_id = %access.workspace_id, "self administration denied");
        return Err(AppError::Forbidden);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberPatch {
    role: String,
    active: bool,
    version: i32,
}

pub async fn update(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<MemberPatch>, JsonRejection>,
) -> Result<Json<MemberResult>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::AdminMembers)?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let r = role(&b.role)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let (user, old_role, old_active, version): (Uuid, String, bool, i32) = sqlx::query_as(
        "SELECT user_id, role, active, version FROM workspace_memberships \
         WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    not_self(&access, id)?;
    if version != b.version {
        return Err(AppError::VersionConflict);
    }
    if old_role == r.code() && old_active == b.active {
        return Err(AppError::RuleViolation);
    }
    if b.active && !old_active {
        one_company(&mut tx, user, ws).await?;
    }
    sqlx::query(
        "UPDATE workspace_memberships SET role = $3, active = $4, version = version + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .bind(r.code())
    .bind(b.active)
    .execute(&mut *tx)
    .await?;
    // Deactivation revokes live grants atomically (history stays as revoked rows).
    let revoked: Vec<String> = if old_active && !b.active {
        sqlx::query_scalar(
            "UPDATE workspace_grants SET revoked_at = now(), revoked_by = $3 \
             WHERE workspace_id = $1 AND membership_id = $2 AND revoked_at IS NULL \
             RETURNING grant_code",
        )
        .bind(ws)
        .bind(id)
        .bind(access.user_id)
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };
    let action = match (old_active, b.active) {
        (true, false) => "membership.deactivate",
        (false, true) => "membership.reactivate",
        _ => "membership.update",
    };
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        action,
        "workspace_membership",
        id,
        serde_json::json!({
            "role": { "from": old_role, "to": r.code() },
            "active": { "from": old_active, "to": b.active },
            "revokedGrants": revoked,
            "version": version + 1,
        }),
        &correlation(&headers),
    )
    .await?;
    let item = one(&mut tx, ws, id, true).await?;
    tx.commit().await?;
    Ok(Json(MemberResult {
        item,
        audit_event_id,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantCreate {
    grant_code: String,
    /// Membership version the caller saw.
    version: i32,
}

/// Grant-type permission the caller holds, else 422 (unknown / role code) or 403 (not held).
fn grant_code(access: &WorkspaceAccess, raw: &str) -> Result<Permission, AppError> {
    let p = Permission::from_code(raw)
        .filter(|p| p.source() == PermissionSource::Grant)
        .ok_or(AppError::Validation("grantCode"))?;
    if !access.allows(p) {
        tracing::info!(workspace_id = %access.workspace_id, permission = p.code(),
            "grant beyond grantor's own rights denied");
        crate::denied_audit::record_permission(p.code());
        return Err(AppError::Forbidden);
    }
    Ok(p)
}

/// Locks the target membership, checks it is not the caller's and matches `version`, and
/// bumps its version (grant changes are membership changes for concurrency).
async fn lock_target(
    tx: &mut PgConnection,
    access: &WorkspaceAccess,
    id: Uuid,
    version: i32,
) -> Result<bool, AppError> {
    let (active, current): (bool, i32) = sqlx::query_as(
        "SELECT active, version FROM workspace_memberships \
         WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(access.workspace_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    not_self(access, id)?;
    if current != version {
        return Err(AppError::VersionConflict);
    }
    sqlx::query(
        "UPDATE workspace_memberships SET version = version + 1 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(access.workspace_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(active)
}

pub async fn grant(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<GrantCreate>, JsonRejection>,
) -> Result<(StatusCode, Json<MemberResult>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::AdminMembers)?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let p = grant_code(&access, &b.grant_code)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    if !lock_target(&mut tx, &access, id, b.version).await? {
        // A grant on an inactive membership would not count and would return on reactivation.
        return Err(AppError::RuleViolation);
    }
    let grant: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (membership_id, grant_code) WHERE revoked_at IS NULL DO NOTHING RETURNING id",
    )
    .bind(ws)
    .bind(id)
    .bind(p.code())
    .bind(access.user_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Validation("grantCode"))?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "grant.add",
        "workspace_membership",
        id,
        serde_json::json!({ "grantId": grant, "grantCode": p.code() }),
        &correlation(&headers),
    )
    .await?;
    let item = one(&mut tx, ws, id, true).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(MemberResult {
            item,
            audit_event_id,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantRevoke {
    grant_code: String,
    version: i32,
}

pub async fn revoke(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<GrantRevoke>, JsonRejection>,
) -> Result<Json<MemberResult>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::AdminMembers)?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let p = grant_code(&access, &b.grant_code)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    lock_target(&mut tx, &access, id, b.version).await?;
    let grant: Uuid = sqlx::query_scalar(
        "UPDATE workspace_grants SET revoked_at = now(), revoked_by = $4 \
         WHERE workspace_id = $1 AND membership_id = $2 AND grant_code = $3 \
           AND revoked_at IS NULL RETURNING id",
    )
    .bind(ws)
    .bind(id)
    .bind(p.code())
    .bind(access.user_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Validation("grantCode"))?;
    let audit_event_id = audit_entity(
        &mut tx,
        ws,
        Some(access.user_id),
        "grant.revoke",
        "workspace_membership",
        id,
        serde_json::json!({ "grantId": grant, "grantCode": p.code() }),
        &correlation(&headers),
    )
    .await?;
    let item = one(&mut tx, ws, id, true).await?;
    tx.commit().await?;
    Ok(Json(MemberResult {
        item,
        audit_event_id,
    }))
}
