pub mod access;
pub mod catalogs;
pub mod dashboard;
pub mod health;
pub mod intake;
pub mod saved_views;
pub mod session;
pub mod sync;
pub mod ticket_list;
pub mod tickets;
pub mod workspaces;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::AppState;

/// All `/api/v1` routes, declared with full paths so the session layer sees them as-is.
/// Keep in sync with `openapi/openapi.json` (checked by `tests/contract.rs`).
pub const API_ROUTES: &[(&str, &str)] = &[
    ("get", "/api/v1/meta"),
    ("post", "/api/v1/uat/session"),
    ("get", "/api/v1/session"),
    ("delete", "/api/v1/session"),
    ("get", "/api/v1/ticket-statuses"),
    ("get", "/api/v1/permissions"),
    ("get", "/api/v1/workspaces"),
    ("get", "/api/v1/workspaces/{workspace_id}"),
    ("get", "/api/v1/workspaces/{workspace_id}/members"),
    ("get", "/api/v1/workspaces/{workspace_id}/catalogs"),
    ("get", "/api/v1/workspaces/{workspace_id}/permissions"),
    ("get", "/api/v1/workspaces/{workspace_id}/grants"),
    ("get", "/api/v1/workspaces/{workspace_id}/dashboard"),
    ("get", "/api/v1/workspaces/{workspace_id}/saved-views"),
    ("post", "/api/v1/workspaces/{workspace_id}/saved-views"),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/saved-views/{saved_view_id}",
    ),
    (
        "delete",
        "/api/v1/workspaces/{workspace_id}/saved-views/{saved_view_id}",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/intake-directory"),
    (
        "put",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/simulation",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/retry",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/reconcile",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/tickets"),
    ("post", "/api/v1/workspaces/{workspace_id}/tickets"),
    ("get", "/api/v1/workspaces/{workspace_id}/tickets/board"),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/transitions",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment-history",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/integrations",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments/{attachment_id}",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/report-runs/{report_run_id}",
    ),
];

pub fn api_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/meta", get(session::meta))
        .route("/api/v1/uat/session", post(session::uat_enter))
        .route(
            "/api/v1/session",
            get(session::current).merge(delete(session::logout)),
        )
        .route("/api/v1/ticket-statuses", get(catalogs::statuses))
        .route("/api/v1/permissions", get(access::catalog))
        .route("/api/v1/workspaces", get(workspaces::list))
        .route(
            "/api/v1/workspaces/{workspace_id}",
            get(workspaces::get_one),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/members",
            get(workspaces::members),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/catalogs",
            get(catalogs::list),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/permissions",
            get(access::mine),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/grants",
            get(access::list_grants),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/dashboard",
            get(dashboard::get),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/saved-views",
            get(saved_views::list).merge(post(saved_views::create)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/saved-views/{saved_view_id}",
            patch(saved_views::rename).merge(delete(saved_views::delete)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets",
            get(ticket_list::list).merge(post(intake::create)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/board",
            get(ticket_list::board),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}",
            get(tickets::get_one),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/transitions",
            post(ticket_list::transition),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment",
            post(ticket_list::assign),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment-history",
            get(intake::assignment_history),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/integrations",
            get(sync::ticket_sync),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/intake-directory",
            get(intake::directory),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/simulation",
            put(sync::set_simulation),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/retry",
            post(sync::retry),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/reconcile",
            post(sync::reconcile),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments/{attachment_id}",
            get(tickets::attachment),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/report-runs/{report_run_id}",
            get(tickets::report_run),
        )
}
