//! Permission metadata for the frontend (D03). The UI may hide or disable actions from this,
//! but every operation is still authorised server-side; this is never a source of access.

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, PermissionSource, Role, role_permissions};
use crate::error::AppError;
use crate::routes::workspaces::{ListBody, parse_id};
use crate::session::Session;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionInfo {
    code: &'static str,
    label: &'static str,
    source: PermissionSource,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleInfo {
    code: &'static str,
    label: &'static str,
    permissions: Vec<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionCatalog {
    permissions: Vec<PermissionInfo>,
    roles: Vec<RoleInfo>,
}

pub async fn catalog(_s: Session) -> Json<PermissionCatalog> {
    Json(PermissionCatalog {
        permissions: Permission::ALL
            .into_iter()
            .map(|p| PermissionInfo {
                code: p.code(),
                label: p.label(),
                source: p.source(),
            })
            .collect(),
        roles: Role::ALL
            .into_iter()
            .map(|r| RoleInfo {
                code: r.code(),
                label: r.label(),
                permissions: role_permissions(r).iter().map(|p| p.code()).collect(),
            })
            .collect(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePermissions {
    workspace_id: Uuid,
    /// Business role in this workspace; `null` for UAT Superadmin without membership.
    role: Option<&'static str>,
    uat_superadmin: bool,
    /// Effective permissions for this request (role + live grants, or all for UAT Superadmin).
    permissions: Vec<&'static str>,
    /// Explicit live grants of the caller's membership.
    grants: Vec<&'static str>,
}

pub async fn mine(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<WorkspacePermissions>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    Ok(Json(WorkspacePermissions {
        workspace_id: access.workspace_id,
        role: access.role.map(Role::code),
        uat_superadmin: access.uat_superadmin,
        permissions: access.permissions().map(Permission::code).collect(),
        grants: access.grants.iter().map(|p| p.code()).collect(),
    }))
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct GrantView {
    id: Uuid,
    user_id: Uuid,
    display_name: String,
    membership_role: String,
    membership_active: bool,
    grant_code: String,
    granted_by: Uuid,
    granted_at: String,
}

/// Live grants of a workspace. Needs `admin.members`; Lead CS alone gets 403.
pub async fn list_grants(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<ListBody<GrantView>>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw)?).await?;
    access.require(Permission::AdminMembers)?;
    let items = sqlx::query_as::<_, GrantView>(
        "SELECT g.id, m.user_id, u.display_name, m.role AS membership_role, \
                m.active AS membership_active, g.grant_code, g.granted_by, \
                to_char(g.granted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                    AS granted_at \
         FROM workspace_grants g \
         JOIN workspace_memberships m ON m.workspace_id = g.workspace_id AND m.id = g.membership_id \
         JOIN users u ON u.id = m.user_id \
         WHERE g.workspace_id = $1 AND g.revoked_at IS NULL \
         ORDER BY u.display_name, g.grant_code, g.id",
    )
    .bind(access.workspace_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(ListBody { items }))
}
