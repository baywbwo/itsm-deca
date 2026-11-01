use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
}

async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Ready only when the database answers and the runtime guard still holds.
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let check = tokio::time::timeout(
        Duration::from_secs(2),
        crate::db::verify_runtime_guard(&state.config, &state.pool),
    )
    .await;
    match check {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "status": "ready" }))),
        Ok(Err(e)) => {
            tracing::warn!(reason = %e, "readiness failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready" })),
            )
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready" })),
        ),
    }
}
