//! ITSM API (D02a foundation + D02 domain schema).

pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod routes;
pub mod seed;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use sqlx::PgPool;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveHeadersLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::config::Config;

pub const UAT_BANNER: &str = "Mode uji — Superadmin";
const BODY_LIMIT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
}

pub fn app(state: AppState) -> Router {
    let request_id = HeaderName::from_static("x-request-id");
    let api = routes::api_router().layer(axum::middleware::from_fn_with_state(
        state.clone(),
        session::session_layer,
    ));

    Router::new()
        .merge(routes::health::router())
        .merge(api)
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(TraceLayer::new_for_http())
        .layer(SetSensitiveHeadersLayer::new([
            header::COOKIE,
            header::SET_COOKIE,
            header::AUTHORIZATION,
            HeaderName::from_static(session::CSRF_HEADER),
        ]))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
}
