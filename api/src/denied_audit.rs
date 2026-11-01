//! D11 denied audit. A member whose request is refused with 403 `forbidden` inside a workspace
//! they can access leaves one append-only `audit_events` row with `outcome = 'denied'`:
//! action `access.denied`, the route template (never IDs, query or body), the method and the
//! permission that was missing when known. Nothing else is recorded.
//!
//! Scope, deliberately narrow:
//! - only after `authz::resolve` succeeded for that request, so the workspace exists and the
//!   caller is a member (or the UAT Superadmin). A non-member gets 404 and no row: writing one
//!   would tie the caller to a workspace they cannot see;
//! - only `AppError::Forbidden` (a marker set by its response), not CSRF failures, 404 or 422;
//! - platform-level refusals without a workspace (company administration) go to the separate
//!   append-only `platform_audit_events` (migration 0011), never into a workspace's
//!   `audit_events`: `record_platform` is called by the platform guard and only applies when
//!   no workspace was resolved for the request. Same safe projection: actor, capability,
//!   method, route template, correlation id.
//!
//! The row is written after the handler returned, outside its transaction (a refused request
//! has none). A failed insert is logged and never changes the response.

use std::cell::RefCell;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use crate::AppState;

tokio::task_local! {
    static DENIAL: RefCell<Option<Context>>;
}

#[derive(Debug, Clone, Copy)]
enum Scope {
    Workspace(Uuid),
    /// Platform administration (no workspace); the capability that was required.
    Platform(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct Context {
    scope: Scope,
    user_id: Uuid,
    permission: Option<&'static str>,
}

/// Marker extension put on a response by `AppError::Forbidden`.
#[derive(Debug, Clone, Copy)]
pub struct ForbiddenMarker;

/// Called by `authz::resolve` once access to a workspace is established for this request.
pub(crate) fn record_access(workspace_id: Uuid, user_id: Uuid) {
    let _ = DENIAL.try_with(|c| {
        *c.borrow_mut() = Some(Context {
            scope: Scope::Workspace(workspace_id),
            user_id,
            permission: None,
        });
    });
}

/// Called by the platform guard before it refuses a caller. Ignored when a workspace was
/// already resolved for this request: a workspace refusal stays in that workspace's audit.
pub(crate) fn record_platform(user_id: Uuid, capability: &'static str) {
    let _ = DENIAL.try_with(|c| {
        let mut slot = c.borrow_mut();
        if slot.is_none() {
            *slot = Some(Context {
                scope: Scope::Platform(capability),
                user_id,
                permission: None,
            });
        }
    });
}

/// Names the permission that was missing; only meaningful after `record_access`.
pub(crate) fn record_permission(code: &'static str) {
    let _ = DENIAL.try_with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.permission = Some(code);
        }
    });
}

/// `route_layer` middleware (it needs the matched route template).
pub async fn layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned());
    let method = req.method().as_str().to_owned();
    let correlation = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 100)
        .map(str::to_owned);

    let (response, ctx) = DENIAL
        .scope(RefCell::new(None), async move {
            let response = next.run(req).await;
            let ctx = DENIAL.with(|c| c.borrow_mut().take());
            (response, ctx)
        })
        .await;

    if let Some(ctx) = ctx
        && response.extensions().get::<ForbiddenMarker>().is_some()
    {
        write(&state, ctx, route, method, correlation).await;
    }
    response
}

async fn write(
    state: &AppState,
    ctx: Context,
    route: Option<String>,
    method: String,
    correlation: Option<String>,
) {
    let workspace_id = match ctx.scope {
        Scope::Workspace(id) => id,
        Scope::Platform(capability) => {
            return write_platform(state, ctx.user_id, capability, route, method, correlation)
                .await;
        }
    };
    let change = serde_json::json!({
        "permission": ctx.permission,
        "method": method,
        "route": route,
    });
    let res = sqlx::query(
        "INSERT INTO audit_events (workspace_id, actor_id, action, entity_type, entity_id, \
           safe_change, outcome, correlation_id) \
         VALUES ($1, $2, 'access.denied', 'access', NULL, $3, 'denied', $4)",
    )
    .bind(workspace_id)
    .bind(ctx.user_id)
    .bind(change)
    .bind(correlation)
    .execute(&state.pool)
    .await;
    if let Err(err) = res {
        tracing::warn!(error = %err, workspace_id = %workspace_id, "denied audit not written");
    }
}

async fn write_platform(
    state: &AppState,
    user_id: Uuid,
    capability: &'static str,
    route: Option<String>,
    method: String,
    correlation: Option<String>,
) {
    let Some(route) = route else {
        tracing::warn!("platform denied audit not written: no route template");
        return;
    };
    let res = sqlx::query(
        "INSERT INTO platform_audit_events \
           (actor_id, action, capability, method, route, outcome, correlation_id) \
         VALUES ($1, 'platform.access.denied', $2, $3, $4, 'denied', $5)",
    )
    .bind(user_id)
    .bind(capability)
    .bind(method)
    .bind(route)
    .bind(correlation)
    .execute(&state.pool)
    .await;
    if let Err(err) = res {
        tracing::warn!(error = %err, "platform denied audit not written");
    }
}
