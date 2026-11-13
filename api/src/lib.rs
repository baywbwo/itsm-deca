//! ITSM API (D02a foundation + D02 domain schema + D03 permission foundation).

pub mod authz;
pub mod clamd;
pub mod config;
pub mod crypto;
pub mod db;
pub mod denied_audit;
pub mod domain;
pub mod error;
pub mod live;
pub mod mfa;
pub mod observe;
pub mod oidc;
pub mod outbox;
pub mod password_bootstrap;
pub mod providers;
pub mod rate_limit;
pub mod routes;
pub mod seed;
pub mod session;
pub mod shared_limit;

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

use crate::config::Config;

pub const UAT_BANNER: &str = "Mode uji — Superadmin";
const BODY_LIMIT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// CP7d (TM-07): JSON and downloads only; a response rendered as a document can do nothing.
pub const API_CSP: &str =
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; sandbox";

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
}

pub fn app(state: AppState) -> Router {
    observe::init();
    let request_id = HeaderName::from_static(observe::REQUEST_ID_HEADER);
    let session = axum::middleware::from_fn_with_state(state.clone(), session::session_layer);
    let denied = axum::middleware::from_fn_with_state(state.clone(), denied_audit::layer);
    // One limiter per router: clones of the returned router share it.
    let limiter = Arc::new(rate_limit::RateLimiter::new(
        state.config.rate_limit.clone(),
    ));
    let limited = axum::middleware::from_fn_with_state(limiter, rate_limit::layer);
    // CP7d: sign-in, outbound and look-up routes also count in Postgres (all replicas).
    let shared = axum::middleware::from_fn_with_state(state.clone(), shared_limit::layer);
    let api = routes::api_router()
        .route_layer(denied.clone())
        .route_layer(shared)
        .layer(limited.clone())
        .layer(session.clone());
    let bounded = Router::new()
        .merge(routes::health::router())
        .merge(api)
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ));
    let upload = routes::upload_router()
        .route_layer(denied)
        .layer(limited)
        .layer(session)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            UPLOAD_TIMEOUT,
        ));

    // CP6a: Jira webhook: no session/CSRF (signed body), own limits (routes::jira_webhook).
    let webhook = routes::jira_webhook::router(&state);

    Router::new()
        .merge(bounded)
        .merge(upload)
        .merge(webhook)
        .with_state(state)
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        // CP7d (TM-07): the API never serves a page, so nothing it returns may run, embed or
        // be framed, even when reached without the ingress (which adds its own headers). A
        // route that sets a stricter policy itself (attachment download) keeps it.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(API_CSP),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        // CP7a: access log + HTTP metrics by route template (never the URI or query).
        .layer(axum::middleware::from_fn(observe::access))
        .layer(SetSensitiveHeadersLayer::new([
            header::COOKIE,
            header::SET_COOKIE,
            header::AUTHORIZATION,
            HeaderName::from_static(session::CSRF_HEADER),
            HeaderName::from_static("x-hub-signature"),
        ]))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
        .layer(axum::middleware::from_fn(observe::sanitize_request_id))
}
