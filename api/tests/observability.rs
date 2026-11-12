//! CP7a observability: JSON access log with request id and route template (never the URI or
//! query), request-id hygiene, the separate `/metrics` listener (HTTP, pool, worker and outbox
//! metrics in the Prometheus text format), liveness vs readiness, and the alert rules shipped in
//! `deploy/observability/alerts.yml` referring only to metrics that exist.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::{AppState, app, observe, outbox, seed, session};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

fn state(pool: &PgPool) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
        ("ITSM_OUTBOX_POLL_MS", "200"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    AppState {
        config: Arc::new(ok(Config::from_map(&vars), "test config")),
        pool: pool.clone(),
    }
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One process-wide JSON subscriber (the production format), at TRACE so even the most verbose
/// setting is checked for leaks.
fn capture() -> Captured {
    static CAPTURED: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
    CAPTURED
        .get_or_init(|| {
            let captured = Captured::default();
            let writer = captured.clone();
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .init();
            captured
        })
        .clone()
}

fn log_text(c: &Captured) -> String {
    String::from_utf8_lossy(
        &c.0.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .into_owned()
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn call(router: Router, req: Request<Body>) -> Resp {
    let res = match router.oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn get(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    ok(b.body(Body::empty()), "request")
}

struct Fx {
    ws: Uuid,
    cookie: String,
}

async fn fixture(pool: &PgPool) -> Fx {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    let ws: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM workspaces WHERE code = 'DUMA'")
            .fetch_one(pool)
            .await,
        "workspace",
    );
    let lead: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:lead-alfa'")
            .fetch_one(pool)
            .await,
        "lead",
    );
    let s = ok(session::issue(pool, lead, "uat_superadmin").await, "issue");
    Fx {
        ws,
        cookie: format!("{}={}", session::cookie_name(false), s.token),
    }
}

/// JSON log lines of the `request done` events for one request id.
fn access_lines(log: &str, request_id: &str) -> Vec<Value> {
    log.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            v["fields"]["message"] == "request done" && v["span"]["request_id"] == request_id
        })
        .collect()
}

const SECRET_QUERY: &str = "RahasiaNasabah0812";
const SECRET_PATH: &str = "JalurRahasia77";

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7a_access_log_names_route_and_request_id_never_the_uri(pool: PgPool) {
    let captured = capture();
    let st = state(&pool);
    let fx = fixture(&pool).await;
    let list = format!("/api/v1/workspaces/{}/tickets?q={SECRET_QUERY}", fx.ws);

    // A plain client request id is kept and echoed.
    let r = call(
        app(st.clone()),
        get(
            &list,
            &[("cookie", &fx.cookie), ("x-request-id", "cp7a-req.1")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.headers.get("x-request-id").map(|v| v.as_bytes()),
        Some(&b"cp7a-req.1"[..])
    );

    // Anything else is replaced by a fresh UUID (bounded log/audit field, no injection).
    for bad in [
        "a b",
        "x\"},{\"level\":\"ERROR",
        &"z".repeat(65),
        "nasabah@example.test",
    ] {
        let r = call(
            app(st.clone()),
            get(&list, &[("cookie", &fx.cookie), ("x-request-id", bad)]),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        let echoed = r
            .headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(Uuid::parse_str(echoed).is_ok(), "{bad:?} → {echoed:?}");
    }
    // Two request ids: neither is trusted.
    let mut req = get(
        &list,
        &[("cookie", &fx.cookie), ("x-request-id", "cp7a-dup-1")],
    );
    req.headers_mut().append(
        "x-request-id",
        axum::http::HeaderValue::from_static("cp7a-dup-2"),
    );
    let r = call(app(st.clone()), req).await;
    let echoed = r
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(Uuid::parse_str(echoed).is_ok(), "{echoed}");

    // Unknown path: logged as `unmatched`, the path itself never.
    let r = call(
        app(st.clone()),
        get(
            &format!("/api/v1/{SECRET_PATH}/x"),
            &[("x-request-id", "cp7a-req.2")],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Health probes are quiet at INFO (DEBUG only).
    let r = call(
        app(st.clone()),
        get("/health/live", &[("x-request-id", "cp7a-req.3")]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let log = log_text(&captured);
    let lines = access_lines(&log, "cp7a-req.1");
    assert_eq!(lines.len(), 1, "{log}");
    let line = &lines[0];
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["span"]["method"], "GET");
    assert_eq!(
        line["span"]["route"],
        "/api/v1/workspaces/{workspace_id}/tickets"
    );
    assert_eq!(line["fields"]["status"], 200);
    assert!(line["fields"]["latency_ms"].is_u64(), "{line}");

    let lines = access_lines(&log, "cp7a-req.2");
    assert_eq!(lines.len(), 1, "{log}");
    assert_eq!(lines[0]["span"]["route"], "unmatched");
    assert_eq!(lines[0]["fields"]["status"], 404);

    let lines = access_lines(&log, "cp7a-req.3");
    assert_eq!(lines.len(), 1, "{log}");
    assert_eq!(lines[0]["level"], "DEBUG");

    for needle in [
        SECRET_QUERY,
        SECRET_PATH,
        "a b",
        "nasabah@example.test",
        "cp7a-dup-",
        fx.cookie.as_str(),
    ] {
        assert!(!log.contains(needle), "{needle:?} reached the log");
    }
    // Requests leave no `uri` field anywhere (the old TraceLayer span carried it).
    assert!(
        !log.lines().any(|l| l.contains("\"uri\"")),
        "uri field in log"
    );
}

/// Minimal check of the Prometheus text format: every sample belongs to a declared family,
/// label values are quoted, values are numbers.
fn parse_exposition(text: &str) -> Vec<(String, String, f64)> {
    let mut declared = BTreeSet::new();
    let mut samples = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let mut parts = rest.split(' ');
            let name = parts.next().unwrap_or_default().to_owned();
            let kind = parts.next().unwrap_or_default();
            assert!(["counter", "gauge", "histogram"].contains(&kind), "{line}");
            assert!(declared.insert(name), "family declared twice: {line}");
            continue;
        }
        if line.starts_with("# HELP ") || line.is_empty() {
            continue;
        }
        let (series, value) = match line.rsplit_once(' ') {
            Some(p) => p,
            None => panic!("no value: {line}"),
        };
        let value: f64 = match value {
            "+Inf" => f64::INFINITY,
            v => ok(v.parse(), line),
        };
        let (name, labels) = match series.split_once('{') {
            Some((n, l)) => {
                let l = match l.strip_suffix('}') {
                    Some(l) => l,
                    None => panic!("labels not closed: {line}"),
                };
                (n, l)
            }
            None => (series, ""),
        };
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "{line}"
        );
        // Each label is key="value" with the value's quotes balanced.
        let mut rest = labels;
        while !rest.is_empty() {
            let (key, after) = match rest.split_once("=\"") {
                Some(p) => p,
                None => panic!("label: {line}"),
            };
            assert!(
                key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{line}"
            );
            let mut end = None;
            let mut escaped = false;
            for (i, c) in after.char_indices() {
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => {
                        end = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let end = match end {
                Some(e) => e,
                None => panic!("unterminated label: {line}"),
            };
            rest = after[end + 1..]
                .strip_prefix(',')
                .unwrap_or(&after[end + 1..]);
        }
        let family = ["_bucket", "_sum", "_count"]
            .iter()
            .find_map(|s| name.strip_suffix(s).filter(|f| declared.contains(*f)))
            .unwrap_or(name);
        assert!(declared.contains(family), "undeclared family: {line}");
        samples.push((name.to_owned(), labels.to_owned(), value));
    }
    samples
}

fn sample(samples: &[(String, String, f64)], name: &str, labels: &str) -> Option<f64> {
    samples
        .iter()
        .find(|(n, l, _)| n == name && l == labels)
        .map(|s| s.2)
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7a_metrics_listener_exports_http_pool_worker_and_outbox(pool: PgPool) {
    let st = state(&pool);
    let fx = fixture(&pool).await;
    let list = format!("/api/v1/workspaces/{}/tickets", fx.ws);
    for _ in 0..3 {
        let r = call(app(st.clone()), get(&list, &[("cookie", &fx.cookie)])).await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let r = call(app(st.clone()), get(&list, &[])).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let r = call(observe::metrics_app(st.clone()), get("/metrics", &[])).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers.get(header::CONTENT_TYPE).map(|v| v.as_bytes()),
        Some(&b"text/plain; version=0.0.4; charset=utf-8"[..])
    );
    let samples = parse_exposition(&r.body);
    let route = "/api/v1/workspaces/{workspace_id}/tickets";
    let ok_count = sample(
        &samples,
        "itsm_http_requests_total",
        &format!("method=\"GET\",route=\"{route}\",status=\"200\""),
    );
    assert!(ok_count.is_some_and(|n| n >= 3.0), "{}", r.body);
    assert!(
        sample(
            &samples,
            "itsm_http_requests_total",
            &format!("method=\"GET\",route=\"{route}\",status=\"401\""),
        )
        .is_some_and(|n| n >= 1.0)
    );
    let inf = sample(
        &samples,
        "itsm_http_request_duration_seconds_bucket",
        &format!("method=\"GET\",route=\"{route}\",le=\"+Inf\""),
    );
    let count = sample(
        &samples,
        "itsm_http_request_duration_seconds_count",
        &format!("method=\"GET\",route=\"{route}\""),
    );
    assert!(inf.is_some() && inf == count, "{}", r.body);
    for p in ["jira", "lark"] {
        for s in ["pending", "failed", "unknown"] {
            let labels = format!("provider=\"{p}\",state=\"{s}\"");
            assert_eq!(sample(&samples, "itsm_outbox_intents", &labels), Some(0.0));
            assert_eq!(
                sample(&samples, "itsm_outbox_oldest_age_seconds", &labels),
                Some(0.0)
            );
        }
    }
    assert!(sample(&samples, "itsm_db_pool_max_connections", "").is_some_and(|n| n >= 1.0));
    assert!(sample(&samples, "itsm_db_pool_connections", "state=\"idle\"").is_some());
    assert_eq!(
        sample(&samples, "itsm_outbox_worker_enabled", ""),
        Some(1.0)
    );
    assert!(sample(&samples, "itsm_build_info", "version=\"0.1.0\"").is_some());
    // Ids and session tokens never become labels.
    assert!(!r.body.contains(&fx.ws.to_string()));

    // The metrics router serves nothing else; the API router does not serve metrics (the
    // ingress forwards /api and /health only, and this listener is separate).
    assert_eq!(
        call(observe::metrics_app(st.clone()), get("/health/live", &[]))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(app(st.clone()), get("/metrics", &[])).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7a_worker_heartbeat_is_exported(pool: PgPool) {
    let st = state(&pool);
    fixture(&pool).await;
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as f64;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker = outbox::spawn(st.clone(), stopped);
    let mut last = 0.0;
    for _ in 0..50 {
        let r = call(observe::metrics_app(st.clone()), get("/metrics", &[])).await;
        last = sample(
            &parse_exposition(&r.body),
            "itsm_outbox_worker_last_tick_timestamp_seconds",
            "",
        )
        .unwrap_or(0.0);
        if last >= start {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = stop.send(true);
    let _ = worker.await;
    assert!(last >= start, "worker heartbeat {last} < {start}");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7a_liveness_needs_no_database_readiness_and_outbox_metrics_do(pool: PgPool) {
    let st = state(&pool);
    fixture(&pool).await;
    assert_eq!(
        call(app(st.clone()), get("/health/ready", &[]))
            .await
            .status,
        StatusCode::OK
    );
    let before = parse_exposition(
        &call(observe::metrics_app(st.clone()), get("/metrics", &[]))
            .await
            .body,
    );
    let errors_before = sample(&before, "itsm_metrics_scrape_errors_total", "").unwrap_or(-1.0);

    pool.close().await;
    let r = call(app(st.clone()), get("/health/live", &[])).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(app(st.clone()), get("/health/ready", &[])).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(r.body.contains("not_ready"));

    // Database gone: the scrape still answers, without outbox gauges, and counts the failure.
    let r = call(observe::metrics_app(st.clone()), get("/metrics", &[])).await;
    assert_eq!(r.status, StatusCode::OK);
    let after = parse_exposition(&r.body);
    assert!(!r.body.contains("itsm_outbox_intents{"));
    assert!(sample(&after, "itsm_process_start_time_seconds", "").is_some());
    assert!(
        sample(&after, "itsm_metrics_scrape_errors_total", "").is_some_and(|n| n > errors_before)
    );
}

/// `deploy/observability/alerts.yml` refers only to exported metrics and every rule carries
/// severity, summary and runbook (no YAML crate: line-based, the file keeps one key per line).
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7a_alert_rules_use_only_exported_metrics(pool: PgPool) {
    let st = state(&pool);
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../deploy/observability/alerts.yml"
    );
    let rules = ok(std::fs::read_to_string(path), path);
    let exposition = call(observe::metrics_app(st), get("/metrics", &[]))
        .await
        .body;
    let declared: BTreeSet<&str> = exposition
        .lines()
        .filter_map(|l| l.strip_prefix("# TYPE "))
        .filter_map(|l| l.split(' ').next())
        .collect();

    let mut used = BTreeSet::new();
    for token in rules.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if token.starts_with("itsm_") {
            let family = ["_bucket", "_sum", "_count"]
                .iter()
                .find_map(|s| token.strip_suffix(s))
                .unwrap_or(token);
            used.insert(family.to_owned());
        }
    }
    assert!(used.len() >= 5, "{used:?}");
    for m in &used {
        assert!(
            declared.contains(m.as_str()),
            "alert uses unknown metric {m}"
        );
    }

    let blocks: Vec<&str> = rules.split("- alert:").skip(1).collect();
    assert!(blocks.len() >= 8, "{} alerts", blocks.len());
    for b in &blocks {
        let name = b.lines().next().unwrap_or_default().trim();
        for key in ["expr:", "for:", "severity:", "summary:", "runbook:"] {
            assert!(b.contains(key), "alert {name} lacks {key}");
        }
        // CP7b: the runbook link resolves to a "### Heading" in a file that exists.
        let link = b
            .lines()
            .find_map(|l| l.trim().strip_prefix("runbook:"))
            .unwrap_or_default()
            .trim();
        let (file, anchor) = link.split_once('#').unwrap_or((link, ""));
        let doc = ok(
            std::fs::read_to_string(format!("{}/../{file}", env!("CARGO_MANIFEST_DIR"))),
            file,
        );
        assert!(
            doc.lines().any(|l| l
                .strip_prefix("### ")
                .is_some_and(|h| h.to_lowercase().replace(' ', "") == anchor)),
            "alert {name}: no heading for {link}"
        );
    }
}
