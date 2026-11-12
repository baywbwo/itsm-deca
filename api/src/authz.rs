//! D03 authorization foundation. Server-side and default deny:
//! - access to a workspace = active membership in an active workspace, or the UAT Superadmin
//!   grant while the server runs in UAT Superadmin mode; anything else is 404 (no enumeration);
//! - inside an accessible workspace an operation needs an explicit permission, else 403;
//! - business permissions come only from the role (CS / Lead CS) as fixed below, never from a
//!   UI role label or client input;
//! - administration/integration permissions come only from `workspace_grants` rows; no role
//!   carries them, Lead CS included.

use std::collections::BTreeSet;

use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::config::{AccessMode, Environment};
use crate::error::AppError;
use crate::session::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    MemberRead,
    CatalogRead,
    TicketRead,
    TicketCreate,
    TicketUpdate,
    TicketTransition,
    TicketResolve,
    TicketAssignSelf,
    TicketAssignAny,
    RelatedWorkManage,
    AttachmentRead,
    AttachmentUpload,
    ReportView,
    AdminMembers,
    AdminSettings,
    IntegrationManage,
    IntegrationCredentials,
    IntegrationRecovery,
}

/// Where a permission can come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionSource {
    /// Implied by the business role (CS / Lead CS) as listed in `role_permissions`.
    Role,
    /// Only through an explicit `workspace_grants` row (or UAT Superadmin).
    Grant,
}

impl Permission {
    pub const ALL: [Permission; 18] = [
        Permission::MemberRead,
        Permission::CatalogRead,
        Permission::TicketRead,
        Permission::TicketCreate,
        Permission::TicketUpdate,
        Permission::TicketTransition,
        Permission::TicketResolve,
        Permission::TicketAssignSelf,
        Permission::TicketAssignAny,
        Permission::RelatedWorkManage,
        Permission::AttachmentRead,
        Permission::AttachmentUpload,
        Permission::ReportView,
        Permission::AdminMembers,
        Permission::AdminSettings,
        Permission::IntegrationManage,
        Permission::IntegrationCredentials,
        Permission::IntegrationRecovery,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Permission::MemberRead => "member.read",
            Permission::CatalogRead => "catalog.read",
            Permission::TicketRead => "ticket.read",
            Permission::TicketCreate => "ticket.create",
            Permission::TicketUpdate => "ticket.update",
            Permission::TicketTransition => "ticket.transition",
            Permission::TicketResolve => "ticket.resolve",
            Permission::TicketAssignSelf => "ticket.assign_self",
            Permission::TicketAssignAny => "ticket.assign_any",
            Permission::RelatedWorkManage => "related_work.manage",
            Permission::AttachmentRead => "attachment.read",
            Permission::AttachmentUpload => "attachment.upload",
            Permission::ReportView => "report.view",
            Permission::AdminMembers => "admin.members",
            Permission::AdminSettings => "admin.settings",
            Permission::IntegrationManage => "integration.manage",
            Permission::IntegrationCredentials => "integration.credentials",
            Permission::IntegrationRecovery => "integration.recovery",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Permission::MemberRead => "Lihat anggota workspace",
            Permission::CatalogRead => "Lihat katalog referensi",
            Permission::TicketRead => "Lihat tiket",
            Permission::TicketCreate => "Buat tiket",
            Permission::TicketUpdate => "Ubah properti tiket",
            Permission::TicketTransition => "Ubah status tiket",
            Permission::TicketResolve => "Selesaikan dan buka ulang tiket",
            Permission::TicketAssignSelf => "Ambil tiket belum ditugaskan untuk diri sendiri",
            Permission::TicketAssignAny => "Ganti PIC ke anggota aktif (dengan alasan)",
            Permission::RelatedWorkManage => {
                "Kelola pekerjaan terkait dan konfirmasi selesai lokal"
            }
            Permission::AttachmentRead => "Lihat lampiran yang lolos pemindaian",
            Permission::AttachmentUpload => "Unggah lampiran",
            Permission::ReportView => "Lihat laporan operasional",
            Permission::AdminMembers => "Kelola anggota dan grant workspace",
            Permission::AdminSettings => "Kelola pengaturan dan katalog workspace",
            Permission::IntegrationManage => "Konfigurasi, uji dan aktifkan integrasi",
            Permission::IntegrationCredentials => "Kelola kredensial integrasi",
            Permission::IntegrationRecovery => "Retry dan rekonsiliasi sinkronisasi gagal",
        }
    }

    pub fn source(self) -> PermissionSource {
        match self {
            Permission::AdminMembers
            | Permission::AdminSettings
            | Permission::IntegrationManage
            | Permission::IntegrationCredentials
            | Permission::IntegrationRecovery => PermissionSource::Grant,
            _ => PermissionSource::Role,
        }
    }

    pub fn from_code(code: &str) -> Option<Permission> {
        Permission::ALL.into_iter().find(|p| p.code() == code)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Cs,
    LeadCs,
}

impl Role {
    pub const ALL: [Role; 2] = [Role::Cs, Role::LeadCs];

    pub fn code(self) -> &'static str {
        match self {
            Role::Cs => "cs",
            Role::LeadCs => "lead_cs",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Cs => "CS",
            Role::LeadCs => "Lead CS",
        }
    }

    pub fn from_code(code: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.code() == code)
    }
}

const CS_PERMISSIONS: &[Permission] = &[
    Permission::MemberRead,
    Permission::CatalogRead,
    Permission::TicketRead,
    Permission::TicketCreate,
    Permission::TicketUpdate,
    Permission::TicketTransition,
    Permission::TicketResolve,
    Permission::TicketAssignSelf,
    Permission::RelatedWorkManage,
    Permission::AttachmentRead,
    Permission::AttachmentUpload,
    Permission::ReportView,
];

const LEAD_CS_PERMISSIONS: &[Permission] = &[
    Permission::MemberRead,
    Permission::CatalogRead,
    Permission::TicketRead,
    Permission::TicketCreate,
    Permission::TicketUpdate,
    Permission::TicketTransition,
    Permission::TicketResolve,
    Permission::TicketAssignSelf,
    Permission::TicketAssignAny,
    Permission::RelatedWorkManage,
    Permission::AttachmentRead,
    Permission::AttachmentUpload,
    Permission::ReportView,
];

/// Business permissions implied by a role (D01 freeze). Never contains a grant permission.
pub fn role_permissions(role: Role) -> &'static [Permission] {
    match role {
        Role::Cs => CS_PERMISSIONS,
        Role::LeadCs => LEAD_CS_PERMISSIONS,
    }
}

/// The UAT Superadmin grant counts only while the server itself runs in UAT Superadmin mode
/// (the session lookup already matched the session's access mode to the config).
pub fn superadmin_effective(state: &AppState, s: &Session) -> bool {
    s.uat_superadmin
        && state.config.access_mode == AccessMode::UatSuperadmin
        && state.config.environment == Environment::Uat
}

/// A-9: sensitive actions need a second factor re-entered on this session in the last 5 minutes
/// (POST /session/step-up). Call after the permission check (403 first, then 401).
///
/// With MFA off (`ITSM_MFA`, default) there is no second factor to re-enter, so it passes.
/// UAT Superadmin skips it: that mode has no password or TOTP to step up with, exists only in a
/// UAT environment (refused at config load in production) and never reaches the password-mode
/// identity endpoints (they answer 404 outside password mode).
pub fn require_step_up(state: &AppState, s: &Session) -> Result<(), AppError> {
    if !state.config.mfa_enabled || superadmin_effective(state, s) || s.step_up_fresh {
        Ok(())
    } else {
        Err(AppError::StepUpRequired)
    }
}

/// The caller's verified access to one workspace for the current request.
#[derive(Debug, Clone)]
pub struct WorkspaceAccess {
    pub workspace_id: Uuid,
    pub workspace_code: String,
    pub user_id: Uuid,
    pub membership_id: Option<Uuid>,
    pub role: Option<Role>,
    pub uat_superadmin: bool,
    /// Explicit live grants of this membership (grant-only permissions).
    pub grants: BTreeSet<Permission>,
    permissions: BTreeSet<Permission>,
}

impl WorkspaceAccess {
    pub fn new(
        workspace_id: Uuid,
        workspace_code: String,
        user_id: Uuid,
        membership: Option<(Uuid, Role)>,
        uat_superadmin: bool,
        grants: BTreeSet<Permission>,
    ) -> Self {
        let mut permissions = BTreeSet::new();
        if uat_superadmin {
            permissions.extend(Permission::ALL);
        } else {
            if let Some((_, role)) = membership {
                permissions.extend(role_permissions(role).iter().copied());
            }
            // Grants count only on top of an active membership, and only grant-type codes.
            if membership.is_some() {
                permissions.extend(
                    grants
                        .iter()
                        .copied()
                        .filter(|p| p.source() == PermissionSource::Grant),
                );
            }
        }
        WorkspaceAccess {
            workspace_id,
            workspace_code,
            user_id,
            membership_id: membership.map(|(id, _)| id),
            role: membership.map(|(_, r)| r),
            uat_superadmin,
            grants,
            permissions,
        }
    }

    pub fn allows(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    pub fn require(&self, p: Permission) -> Result<(), AppError> {
        if self.allows(p) {
            Ok(())
        } else {
            tracing::info!(
                permission = p.code(),
                workspace_id = %self.workspace_id,
                user_id = %self.user_id,
                "permission denied"
            );
            crate::denied_audit::record_permission(p.code());
            Err(AppError::Forbidden)
        }
    }

    pub fn permissions(&self) -> impl Iterator<Item = Permission> + '_ {
        self.permissions.iter().copied()
    }
}

#[derive(sqlx::FromRow)]
struct AccessRow {
    id: Uuid,
    code: String,
    membership_id: Option<Uuid>,
    role: Option<String>,
    grants: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown membership role in database")]
struct UnknownRole;

/// Resolves the caller's access to `workspace_id`. No access (unknown, inactive, foreign)
/// is `NotFound`, exactly like an ID that does not exist.
pub async fn resolve(
    state: &AppState,
    s: &Session,
    workspace_id: Uuid,
) -> Result<WorkspaceAccess, AppError> {
    let superadmin = superadmin_effective(state, s);
    let row = sqlx::query_as::<_, AccessRow>(
        "SELECT w.id, w.code, m.id AS membership_id, m.role, \
                ARRAY(SELECT g.grant_code FROM workspace_grants g \
                      WHERE g.workspace_id = w.id AND g.membership_id = m.id \
                        AND g.revoked_at IS NULL ORDER BY g.grant_code) AS grants \
         FROM workspaces w \
         LEFT JOIN workspace_memberships m \
                ON m.workspace_id = w.id AND m.user_id = $1 AND m.active \
         WHERE w.active AND w.id = $3 AND (m.id IS NOT NULL OR $2)",
    )
    .bind(s.user_id)
    .bind(superadmin)
    .bind(workspace_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let membership = match (row.membership_id, row.role.as_deref()) {
        (Some(id), Some(code)) => Some((
            id,
            Role::from_code(code).ok_or_else(|| AppError::internal(UnknownRole))?,
        )),
        _ => None,
    };
    // Unknown codes cannot exist (CHECK constraint); ignore rather than widen access.
    let grants = row
        .grants
        .iter()
        .filter_map(|c| Permission::from_code(c.as_str()))
        .collect();
    crate::denied_audit::record_access(row.id, s.user_id);
    Ok(WorkspaceAccess::new(
        row.id, row.code, s.user_id, membership, superadmin, grants,
    ))
}

/// What a permitted PIC change is, so the mutation (D07) knows whether a reason and an audit
/// entry are required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentKind {
    /// No change requested.
    Unchanged,
    /// CS / Lead CS takes an unassigned ticket for themselves.
    TakeForSelf,
    /// Change or removal of the PIC by a holder of `ticket.assign_any`; needs reason + audit.
    Reassign,
}

/// Server-side PIC policy (D01 freeze): CS may only take an unassigned ticket for themselves;
/// replacing or removing another PIC needs `ticket.assign_any` (Lead CS). That the new PIC is
/// an active member of the same workspace is enforced by the database trigger.
pub fn check_assignment(
    access: &WorkspaceAccess,
    current_owner: Option<Uuid>,
    new_owner: Option<Uuid>,
) -> Result<AssignmentKind, AppError> {
    if current_owner == new_owner {
        return Ok(AssignmentKind::Unchanged);
    }
    if current_owner.is_none()
        && new_owner == Some(access.user_id)
        && access.allows(Permission::TicketAssignSelf)
    {
        return Ok(AssignmentKind::TakeForSelf);
    }
    if access.allows(Permission::TicketAssignAny) {
        return Ok(AssignmentKind::Reassign);
    }
    tracing::info!(workspace_id = %access.workspace_id, "assignment denied");
    crate::denied_audit::record_permission(Permission::TicketAssignAny.code());
    Err(AppError::Forbidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(role: Option<Role>, superadmin: bool, grants: &[Permission]) -> WorkspaceAccess {
        WorkspaceAccess::new(
            Uuid::nil(),
            "DUMA".to_owned(),
            Uuid::from_u128(1),
            role.map(|r| (Uuid::from_u128(9), r)),
            superadmin,
            grants.iter().copied().collect(),
        )
    }

    #[test]
    fn codes_round_trip_and_are_unique() {
        let mut seen = BTreeSet::new();
        for p in Permission::ALL {
            assert_eq!(Permission::from_code(p.code()), Some(p));
            assert!(seen.insert(p.code()));
        }
        assert_eq!(Permission::from_code("workspace_admin"), None);
    }

    #[test]
    fn no_role_carries_a_grant_permission() {
        for role in Role::ALL {
            for p in role_permissions(role) {
                assert_eq!(p.source(), PermissionSource::Role, "{role:?} has {p:?}");
            }
        }
    }

    #[test]
    fn lead_cs_is_cs_plus_assign_any_only() {
        let cs: BTreeSet<_> = role_permissions(Role::Cs).iter().copied().collect();
        let lead: BTreeSet<_> = role_permissions(Role::LeadCs).iter().copied().collect();
        assert!(cs.is_subset(&lead));
        let extra: Vec<_> = lead.difference(&cs).copied().collect();
        assert_eq!(extra, vec![Permission::TicketAssignAny]);
    }

    #[test]
    fn lead_cs_has_no_admin_retry_or_credential_rights_without_grant() {
        let a = access(Some(Role::LeadCs), false, &[]);
        for p in [
            Permission::AdminMembers,
            Permission::AdminSettings,
            Permission::IntegrationManage,
            Permission::IntegrationCredentials,
            Permission::IntegrationRecovery,
        ] {
            assert!(!a.allows(p), "{p:?}");
            assert!(a.require(p).is_err());
        }
    }

    #[test]
    fn grants_add_only_the_granted_code() {
        let a = access(Some(Role::Cs), false, &[Permission::IntegrationRecovery]);
        assert!(a.allows(Permission::IntegrationRecovery));
        assert!(!a.allows(Permission::IntegrationCredentials));
        assert!(!a.allows(Permission::TicketAssignAny));
    }

    #[test]
    fn grant_without_membership_or_role_code_as_grant_is_ignored() {
        // No membership: grants never count on their own.
        let none = access(None, false, &[Permission::AdminMembers]);
        assert_eq!(none.permissions().count(), 0);
        // A role-type code smuggled into the grant set does not widen a CS.
        let cs = access(Some(Role::Cs), false, &[Permission::TicketAssignAny]);
        assert!(!cs.allows(Permission::TicketAssignAny));
    }

    #[test]
    fn uat_superadmin_has_everything() {
        let a = access(None, true, &[]);
        assert!(Permission::ALL.into_iter().all(|p| a.allows(p)));
    }

    #[test]
    fn assignment_policy_follows_freeze() {
        let me = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let third = Uuid::from_u128(3);
        let cs = access(Some(Role::Cs), false, &[]);
        let lead = access(Some(Role::LeadCs), false, &[]);

        assert!(matches!(
            check_assignment(&cs, None, Some(me)),
            Ok(AssignmentKind::TakeForSelf)
        ));
        assert!(matches!(
            check_assignment(&cs, Some(other), Some(other)),
            Ok(AssignmentKind::Unchanged)
        ));
        // CS: not someone else's ticket, not for someone else, not unassign.
        for (cur, new) in [
            (None, Some(other)),
            (Some(other), Some(me)),
            (Some(other), Some(third)),
            (Some(me), Some(other)),
            (Some(me), None),
        ] {
            assert!(
                matches!(check_assignment(&cs, cur, new), Err(AppError::Forbidden)),
                "{cur:?} -> {new:?}"
            );
        }
        // Lead CS: reassign (needs reason + audit at mutation time).
        assert!(matches!(
            check_assignment(&lead, Some(other), Some(third)),
            Ok(AssignmentKind::Reassign)
        ));
        assert!(matches!(
            check_assignment(&lead, None, Some(me)),
            Ok(AssignmentKind::TakeForSelf)
        ));
        // Member without any role permissions (e.g. none) cannot assign at all.
        let nobody = access(None, false, &[]);
        assert!(check_assignment(&nobody, None, Some(me)).is_err());
    }
}
