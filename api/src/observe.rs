//! CP7a observability: access log, request-id hygiene, HTTP and worker metrics, and the
//! Prometheus text exposition served on the separate `ITSM_METRICS_BIND` listener.
//!
//! Logs and metrics name the route template (`MatchedPath`), never the URI: paths carry ids and
//! query strings carry search terms (customer names, phone numbers).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tracing::Instrument;

use crate::AppState;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
/// Upper bounds (seconds) of the request duration histogram: the p95 targets (read 300 ms,
/// write 600 ms, ticket list 1.5 s) are bucket edges.
pub const DURATION_BUCKETS: [f64; 8] = [0.05, 0.1, 0.3, 0.6, 1.0, 1.5, 3.0, 10.0];
const OUTBOX_SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);
const OUTBOX_PROVIDERS: [&str; 2] = ["jira", "lark"];
const OUTBOX_OPEN_STATES: [&str; 3] = ["pending", "failed", "unknown"];

#[derive(Default)]
struct RouteStats {
    by_status: BTreeMap<u16, u64>,
    /// Non-cumulative counts per bucket of [`DURATION_BUCKETS`]; the rest is above the last edge.
    buckets: [u64; DURATION_BUCKETS.len()],
    count: u64,
    sum: f64,
}

struct Registry {
    started: SystemTime,
    /// route template → method → stats.
    http: Mutex<BTreeMap<String, BTreeMap<&'static str, RouteStats>>>,
    worker_last_tick: AtomicI64,
    worker_tick_failures: AtomicU64,
    scrape_errors: AtomicU64,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| Registry {
    started: SystemTime::now(),
    http: Mutex::new(BTreeMap::new()),
    worker_last_tick: AtomicI64::new(0),
    worker_tick_failures: AtomicU64::new(0),
    scrape_errors: AtomicU64::new(0),
});

/// Pins the process start time (called when the router is built).
pub fn init() {
    LazyLock::force(&REGISTRY);
}

fn method_label(m: &axum::http::Method) -> &'static str {
    match m.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}

fn record_http(method: &'static str, route: &str, status: u16, elapsed: Duration) {
    let secs = elapsed.as_secs_f64();
    let mut http = REGISTRY.http.lock().unwrap_or_else(PoisonError::into_inner);
    let methods = match http.get_mut(route) {
        Some(m) => m,
        None => http.entry(route.to_owned()).or_default(),
    };
    let stats = methods.entry(method).or_default();
    *stats.by_status.entry(status).or_default() += 1;
    if let Some(i) = DURATION_BUCKETS.iter().position(|edge| secs <= *edge) {
        stats.buckets[i] += 1;
    }
    stats.count += 1;
    stats.sum += secs;
}

/// The outbox worker finished a tick (`ok`) or the tick failed.
pub fn worker_tick(ok: bool) {
    if ok {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        REGISTRY.worker_last_tick.store(now, Ordering::Relaxed);
    } else {
        REGISTRY
            .worker_tick_failures
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// A request id from the client is kept only when it is short and plain; anything else is
/// dropped so `SetRequestIdLayer` issues a fresh UUID (log and audit fields stay bounded).
pub fn valid_request_id(v: &[u8]) -> bool {
    (1..=64).contains(&v.len())
        && v.iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

pub async fn sanitize_request_id(mut req: Request, next: Next) -> Response {
    let headers = req.headers();
    let all = headers.get_all(REQUEST_ID_HEADER);
    let mut values = all.iter();
    let keep = matches!(
        (values.next(), values.next()),
        (Some(v), None) if valid_request_id(v.as_bytes())
    );
    if !keep {
        req.headers_mut().remove(REQUEST_ID_HEADER);
    }
    next.run(req).await
}

/// Access log + HTTP metrics. Runs inside the request-id layer (the header is always set) and
/// per route (so `MatchedPath` is known). Every event logged while handling the request carries
/// `request_id`, `method` and `route` through the span.
pub async fn access(req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = method_label(req.method());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let request_id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let span = tracing::info_span!("request", request_id = %request_id, method, route = %route);
    let response = next.run(req).instrument(span.clone()).await;
    let elapsed = started.elapsed();
    let status = response.status().as_u16();
    record_http(method, &route, status, elapsed);
    let latency_ms = elapsed.as_millis() as u64;
    let _entered = span.enter();
    if route.starts_with("/health/") {
        tracing::debug!(status, latency_ms, "request done");
    } else if status >= 500 {
        tracing::warn!(status, latency_ms, "request done");
    } else {
        tracing::info!(status, latency_ms, "request done");
    }
    response
}

/// Label value escaping of the text format (`\`, `"`, newline).
fn label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

fn render_http(out: &mut String) {
    let http = REGISTRY.http.lock().unwrap_or_else(PoisonError::into_inner);
    family(
        out,
        "itsm_http_requests_total",
        "counter",
        "HTTP requests handled, by route template and status.",
    );
    for (route, methods) in http.iter() {
        for (method, stats) in methods {
            for (status, n) in &stats.by_status {
                let _ = writeln!(
                    out,
                    "itsm_http_requests_total{{method=\"{method}\",route=\"{}\",status=\"{status}\"}} {n}",
                    label(route)
                );
            }
        }
    }
    family(
        out,
        "itsm_http_request_duration_seconds",
        "histogram",
        "HTTP request duration, by route template.",
    );
    for (route, methods) in http.iter() {
        let route = label(route);
        for (method, stats) in methods {
            let mut cumulative = 0;
            for (edge, n) in DURATION_BUCKETS.iter().zip(stats.buckets) {
                cumulative += n;
                let _ = writeln!(
                    out,
                    "itsm_http_request_duration_seconds_bucket{{method=\"{method}\",route=\"{route}\",le=\"{edge}\"}} {cumulative}"
                );
            }
            let _ = writeln!(
                out,
                "itsm_http_request_duration_seconds_bucket{{method=\"{method}\",route=\"{route}\",le=\"+Inf\"}} {}",
                stats.count
            );
            let _ = writeln!(
                out,
                "itsm_http_request_duration_seconds_sum{{method=\"{method}\",route=\"{route}\"}} {}",
                stats.sum
            );
            let _ = writeln!(
                out,
                "itsm_http_request_duration_seconds_count{{method=\"{method}\",route=\"{route}\"}} {}",
                stats.count
            );
        }
    }
}

type OutboxRow = (String, String, i64, f64);

async fn outbox_rows(state: &AppState) -> Option<Vec<OutboxRow>> {
    // Index-only on integration_outbox_open_idx (0028); success rows are never read.
    let query = sqlx::query_as::<_, OutboxRow>(
        "SELECT provider, state, count(*)::bigint, \
                COALESCE(EXTRACT(EPOCH FROM now() - min(created_at)), 0)::float8 \
           FROM integration_outbox WHERE state IN ('pending', 'failed', 'unknown') \
          GROUP BY provider, state",
    )
    .fetch_all(&state.pool);
    match tokio::time::timeout(OUTBOX_SCRAPE_TIMEOUT, query).await {
        Ok(Ok(rows)) => Some(rows),
        Ok(Err(e)) => {
            tracing::warn!(error.kind = "metrics_outbox", error = %e, "outbox metrics unavailable");
            None
        }
        Err(_) => {
            tracing::warn!(
                error.kind = "metrics_outbox_timeout",
                "outbox metrics unavailable"
            );
            None
        }
    }
}

/// The whole exposition. Process-local counters plus database-derived gauges (the outbox
/// gauges are the same on every replica: aggregate them with `max`, not `sum`).
pub async fn render(state: &AppState) -> String {
    let mut out = String::with_capacity(16 * 1024);
    family(
        &mut out,
        "itsm_build_info",
        "gauge",
        "Build of the running API.",
    );
    let _ = writeln!(
        out,
        "itsm_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );
    family(
        &mut out,
        "itsm_process_start_time_seconds",
        "gauge",
        "Start time of the process (unix seconds).",
    );
    let started = REGISTRY
        .started
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let _ = writeln!(out, "itsm_process_start_time_seconds {started}");

    render_http(&mut out);

    family(
        &mut out,
        "itsm_db_pool_connections",
        "gauge",
        "Database pool connections by state.",
    );
    let size = state.pool.size() as usize;
    let idle = state.pool.num_idle();
    let _ = writeln!(out, "itsm_db_pool_connections{{state=\"idle\"}} {idle}");
    let _ = writeln!(
        out,
        "itsm_db_pool_connections{{state=\"in_use\"}} {}",
        size.saturating_sub(idle)
    );
    family(
        &mut out,
        "itsm_db_pool_max_connections",
        "gauge",
        "Configured database pool size.",
    );
    let _ = writeln!(
        out,
        "itsm_db_pool_max_connections {}",
        state.pool.options().get_max_connections()
    );

    family(
        &mut out,
        "itsm_outbox_worker_enabled",
        "gauge",
        "1 when this process runs the outbox worker (ITSM_OUTBOX_WORKER).",
    );
    let _ = writeln!(
        out,
        "itsm_outbox_worker_enabled {}",
        u8::from(state.config.outbox_worker)
    );
    family(
        &mut out,
        "itsm_outbox_worker_last_tick_timestamp_seconds",
        "gauge",
        "Unix time of the last completed outbox worker tick (0 = none yet).",
    );
    let _ = writeln!(
        out,
        "itsm_outbox_worker_last_tick_timestamp_seconds {}",
        REGISTRY.worker_last_tick.load(Ordering::Relaxed)
    );
    family(
        &mut out,
        "itsm_outbox_worker_tick_failures_total",
        "counter",
        "Outbox worker ticks that failed (database unavailable and similar).",
    );
    let _ = writeln!(
        out,
        "itsm_outbox_worker_tick_failures_total {}",
        REGISTRY.worker_tick_failures.load(Ordering::Relaxed)
    );

    if let Some(rows) = outbox_rows(state).await {
        let find = |p: &str, s: &str| {
            rows.iter()
                .find(|r| r.0 == p && r.1 == s)
                .map_or((0, 0.0), |r| (r.2, r.3))
        };
        family(
            &mut out,
            "itsm_outbox_intents",
            "gauge",
            "Outbox intents not delivered, by provider and state (pending, failed, unknown).",
        );
        for p in OUTBOX_PROVIDERS {
            for s in OUTBOX_OPEN_STATES {
                let _ = writeln!(
                    out,
                    "itsm_outbox_intents{{provider=\"{p}\",state=\"{s}\"}} {}",
                    find(p, s).0
                );
            }
        }
        family(
            &mut out,
            "itsm_outbox_oldest_age_seconds",
            "gauge",
            "Age of the oldest intent in that provider and state (0 = none).",
        );
        for p in OUTBOX_PROVIDERS {
            for s in OUTBOX_OPEN_STATES {
                let _ = writeln!(
                    out,
                    "itsm_outbox_oldest_age_seconds{{provider=\"{p}\",state=\"{s}\"}} {}",
                    find(p, s).1
                );
            }
        }
    } else {
        REGISTRY.scrape_errors.fetch_add(1, Ordering::Relaxed);
    }
    family(
        &mut out,
        "itsm_metrics_scrape_errors_total",
        "counter",
        "Scrapes whose database-derived metrics could not be read.",
    );
    let _ = writeln!(
        out,
        "itsm_metrics_scrape_errors_total {}",
        REGISTRY.scrape_errors.load(Ordering::Relaxed)
    );
    out
}

async fn metrics(State(state): State<AppState>) -> Response {
    let mut response = render(&state).await.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

/// Router of the metrics listener only (`ITSM_METRICS_BIND`); never merged into [`crate::app`],
/// so the ingress cannot reach it.
pub fn metrics_app(state: AppState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_from_clients_are_short_and_plain() {
        assert!(valid_request_id(b"9f1c2d3e-0000-4000-8000-000000000001"));
        assert!(valid_request_id(b"caddy.req_1:a"));
        assert!(!valid_request_id(b""));
        assert!(!valid_request_id(&[b'a'; 65]));
        assert!(!valid_request_id(b"a b"));
        assert!(!valid_request_id(b"x\"}{\"admin\":true"));
        assert!(!valid_request_id("nasabah@example.test".as_bytes()));
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(
            label("/api/v1/workspaces/{workspace_id}"),
            "/api/v1/workspaces/{workspace_id}"
        );
    }

    #[test]
    fn durations_land_in_the_first_bucket_that_holds_them() {
        record_http("GET", "/unit/bucket", 200, Duration::from_millis(250));
        record_http("GET", "/unit/bucket", 200, Duration::from_millis(300));
        record_http("GET", "/unit/bucket", 503, Duration::from_secs(20));
        let mut out = String::new();
        render_http(&mut out);
        let line = |le: &str| {
            format!(
                "itsm_http_request_duration_seconds_bucket{{method=\"GET\",route=\"/unit/bucket\",le=\"{le}\"}}"
            )
        };
        assert!(out.contains(&format!("{} 0\n", line("0.1"))), "{out}");
        assert!(out.contains(&format!("{} 2\n", line("0.3"))), "{out}");
        assert!(out.contains(&format!("{} 2\n", line("10"))), "{out}");
        assert!(out.contains(&format!("{} 3\n", line("+Inf"))), "{out}");
        assert!(out.contains(
            "itsm_http_requests_total{method=\"GET\",route=\"/unit/bucket\",status=\"503\"} 1\n"
        ));
    }
}
