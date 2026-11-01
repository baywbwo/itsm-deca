pub mod health;
pub mod session;
pub mod workspaces;

use axum::Router;
use axum::routing::{delete, get, post};

use crate::AppState;

/// All `/api/v1` routes, declared with full paths so the session layer sees them as-is.
/// Keep in sync with `openapi/openapi.json` (checked by `tests/contract.rs`).
pub const API_ROUTES: &[(&str, &str)] = &[
    ("get", "/api/v1/meta"),
    ("post", "/api/v1/uat/session"),
    ("get", "/api/v1/session"),
    ("delete", "/api/v1/session"),
    ("get", "/api/v1/workspaces"),
    ("get", "/api/v1/workspaces/{workspace_id}"),
    ("get", "/api/v1/workspaces/{workspace_id}/members"),
];

pub fn api_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/meta", get(session::meta))
        .route("/api/v1/uat/session", post(session::uat_enter))
        .route(
            "/api/v1/session",
            get(session::current).merge(delete(session::logout)),
        )
        .route("/api/v1/workspaces", get(workspaces::list))
        .route(
            "/api/v1/workspaces/{workspace_id}",
            get(workspaces::get_one),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/members",
            get(workspaces::members),
        )
}
