//! CP7d: rate limits shared by every replica (Postgres fixed windows). Two routers built from
//! the same pool stand for two API replicas: each has its own per-process limiter, so only
//! the shared table can make them agree.

use std::collections::HashMap;
use std::fmt::Display;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::routes::password::hash_password;
use itsm_api::{AppState, app, db, seed, session, shared_limit};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

/// One "replica". Per-process limits are set high so only the shared limits under test bite.
fn replica(pool: &PgPool, mode: &str, extra: &[(&str, &str)]) -> Router {
    let mut vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", mode),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        (
            "ITSM_COOKIE_SECURE",
            if mode == "password" { "true" } else { "false" },
        ),
        ("ITSM_RATE_LIMIT_ANON_PER_MIN", "100000"),
        ("ITSM_RATE_LIMIT_READ_PER_MIN", "100000"),
        ("ITSM_RATE_LIMIT_WRITE_PER_MIN", "100000"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (k, v) in extra {
        vars.insert((*k).to_owned(), (*v).to_owned());
    }
    app(AppState {
        config: Arc::new(ok(Config::from_map(&vars), "test config")),
        pool: pool.clone(),
    })
}

struct Fx {
    ws: Uuid,
    cs: Uuid,
    lead: Uuid,
    outsider: Uuid,
}

async fn fixture(pool: &PgPool) -> Fx {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    let id = |sql: &'static str, key: &'static str| async move {
        let v: Uuid = ok(sqlx::query_scalar(sql).bind(key).fetch_one(pool).await, key);
        v
    };
    let user = "SELECT id FROM users WHERE external_subject = $1";
    Fx {
        ws: id("SELECT id FROM workspaces WHERE code = $1", "DUMA").await,
        cs: id(user, "dummy:cs-alfa").await,
        lead: id(user, "dummy:lead-alfa").await,
        outsider: id(user, "dummy:cs-bravo").await,
    }
}

struct Resp {
    status: StatusCode,
    retry_after: Option<u64>,
    cookie: bool,
    headers: axum::http::HeaderMap,
    json: Value,
}

async fn run(app: &Router, req: Request<Body>) -> Resp {
    let res = match app.clone().oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let retry_after = res
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let cookie = res.headers().get(header::SET_COOKIE).is_some();
    let headers = res.headers().clone();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        retry_after,
        cookie,
        headers,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn from_peer(mut req: Request<Body>, peer: &str) -> Request<Body> {
    let addr: SocketAddr = ok(peer.parse(), "peer");
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

fn uat_entry(peer: &str, xff: Option<&str>) -> Request<Body> {
    let mut r = Request::builder()
        .method("POST")
        .uri(session::UAT_ENTRY_PATH)
        .header(session::UAT_ENTRY_HEADER, "1");
    if let Some(v) = xff {
        r = r.header("x-forwarded-for", v);
    }
    from_peer(ok(r.body(Body::empty()), "request"), peer)
}

fn login(peer: &str, username: &str, password: &str) -> Request<Body> {
    let r = Request::builder()
        .method("POST")
        .uri("/api/v1/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-itsm-login", "1")
        .header("sec-fetch-site", "same-origin")
        .body(Body::from(
            json!({ "username": username, "password": password }).to_string(),
        ));
    from_peer(ok(r, "request"), peer)
}

struct Tok {
    token: String,
    csrf: String,
}

async fn tok(pool: &PgPool, uid: Uuid) -> Tok {
    let s = ok(session::issue(pool, uid, "uat_superadmin").await, "issue");
    Tok {
        token: s.token,
        csrf: s.csrf_token,
    }
}

fn authed(method: &str, uri: &str, t: &Tok, body: Option<Value>) -> Request<Body> {
    let r = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            header::COOKIE,
            format!("{}={}", session::cookie_name(false), t.token),
        )
        .header(session::CSRF_HEADER, &t.csrf);
    let req = match body {
        Some(b) => r
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => r.body(Body::empty()),
    };
    from_peer(ok(req, "request"), "198.51.100.20:5000")
}

async fn scalar(pool: &PgPool, sql: &str) -> i64 {
    ok(sqlx::query_scalar(sql).fetch_one(pool).await, sql)
}

fn assert_limited(r: &Resp, window: u64) {
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "rate_limited");
    let secs = r.json["error"]["retryAfterSeconds"].as_u64();
    assert!(
        secs.is_some_and(|s| (1..=window).contains(&s)),
        "retry after {secs:?} within the {window} s window"
    );
    assert_eq!(r.retry_after, secs, "header matches body");
    assert!(!r.cookie, "a refused request never sets a cookie");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_sign_in_limit_per_address_is_shared_by_replicas(pool: PgPool) {
    fixture(&pool).await;
    let limit = [("ITSM_LIMIT_AUTH_PER_IP_10MIN", "4")];
    let (a, b) = (
        replica(&pool, "uat_superadmin", &limit),
        replica(&pool, "uat_superadmin", &limit),
    );
    let peer = "203.0.113.7:40000";
    for i in 0..4 {
        let app = if i % 2 == 0 { &a } else { &b };
        let r = run(app, uat_entry(peer, None)).await;
        assert_eq!(r.status, StatusCode::CREATED, "attempt {i}: {}", r.json);
    }
    let sessions = scalar(&pool, "SELECT count(*) FROM sessions").await;
    // Either replica now refuses this address: per-process limits alone would allow 4 more.
    for app in [&a, &b] {
        let r = run(app, uat_entry(peer, None)).await;
        assert_limited(&r, 600);
    }
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM sessions").await,
        sessions,
        "no session behind a 429"
    );
    // A forged X-Forwarded-For from an untrusted peer does not buy a new identity.
    let r = run(&a, uat_entry(peer, Some("192.0.2.99"))).await;
    assert_limited(&r, 600);
    // Another address is unaffected.
    let r = run(&b, uat_entry("203.0.113.8:40000", None)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    // Only digests are stored: no address in clear.
    let clear = scalar(
        &pool,
        "SELECT count(*) FROM rate_limit_windows WHERE encode(key, 'escape') LIKE '%203.0.113%'",
    )
    .await;
    assert_eq!(clear, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_password_spraying_from_one_address_stops_before_the_password_check(pool: PgPool) {
    let f = fixture(&pool).await;
    ok(
        sqlx::query(
            "INSERT INTO local_credentials(user_id, username, password_hash) VALUES ($1, 'admin@example.test', $2)",
        )
        .bind(f.cs)
        .bind(ok(hash_password("synthetic-password-123"), "hash"))
        .execute(&pool)
        .await,
        "credentials",
    );
    let limit = [("ITSM_LIMIT_AUTH_PER_IP_10MIN", "3")];
    let (a, b) = (
        replica(&pool, "password", &limit),
        replica(&pool, "password", &limit),
    );
    let peer = "203.0.113.50:1234";
    // Spraying: a different account each time, so no lockout ever triggers.
    for (i, user) in ["x1@example.test", "x2@example.test", "x3@example.test"]
        .iter()
        .enumerate()
    {
        let app = if i % 2 == 0 { &a } else { &b };
        let r = run(app, login(peer, user, "guess-123456")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    }
    let events = "SELECT count(*) FROM auth_events WHERE event = 'login'";
    let before = scalar(&pool, events).await;
    // Even the right password is refused from this address until the window ends, and the
    // attempt is not processed (no auth event, no session, no lockout counter change).
    let r = run(
        &b,
        login(peer, "admin@example.test", "synthetic-password-123"),
    )
    .await;
    assert_limited(&r, 600);
    assert_eq!(scalar(&pool, events).await, before);
    assert_eq!(scalar(&pool, "SELECT count(*) FROM sessions").await, 0);
    // The account owner on another address signs in normally.
    let r = run(
        &a,
        login(
            "198.51.100.9:999",
            "admin@example.test",
            "synthetic-password-123",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert!(r.cookie);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_ai_limits_count_per_user_and_per_workspace_after_authorisation(pool: PgPool) {
    let f = fixture(&pool).await;
    let limits = [
        ("ITSM_LIMIT_AI_PER_USER_MIN", "2"),
        ("ITSM_LIMIT_AI_PER_WORKSPACE_DAY", "3"),
    ];
    let (a, b) = (
        replica(&pool, "uat_superadmin", &limits),
        replica(&pool, "uat_superadmin", &limits),
    );
    let uri = format!("/api/v1/workspaces/{}/ai-suggestions", f.ws);
    let body = || Some(json!({ "mode": "reply", "text": "Mohon bantu cek transaksi." }));
    let (cs, lead, outsider) = (
        tok(&pool, f.cs).await,
        tok(&pool, f.lead).await,
        tok(&pool, f.outsider).await,
    );
    let ws_hits = "SELECT coalesce(sum(hits), 0)::bigint FROM rate_limit_windows WHERE scope = 'ai_workspace_day'";

    // Someone without access to the workspace gets 404 (no enumeration) and spends none of its quota.
    for _ in 0..5 {
        let r = run(&a, authed("POST", &uri, &outsider, body())).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    }
    // Invalid input is not counted either.
    let r = run(
        &a,
        authed("POST", &uri, &cs, Some(json!({ "mode": "x", "text": "a" }))),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(scalar(&pool, ws_hits).await, 0);

    // No provider key in tests: an accepted request ends 503 after it was counted.
    for app in [&a, &b] {
        let r = run(app, authed("POST", &uri, &cs, body())).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.json);
    }
    let r = run(&a, authed("POST", &uri, &cs, body())).await;
    assert_limited(&r, 60);
    // The user limit refused it before the workspace quota was touched.
    assert_eq!(scalar(&pool, ws_hits).await, 2);

    // A colleague has their own user limit but shares the workspace quota (3 per day).
    let r = run(&b, authed("POST", &uri, &lead, body())).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.json);
    let r = run(&a, authed("POST", &uri, &lead, body())).await;
    assert_limited(&r, 86_400);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_outbound_calls_and_lookups_are_limited_per_user(pool: PgPool) {
    let f = fixture(&pool).await;
    let limits = [
        ("ITSM_LIMIT_OUTBOUND_PER_USER_10MIN", "2"),
        ("ITSM_LIMIT_LOOKUP_PER_USER_MIN", "1"),
    ];
    let (a, b) = (
        replica(&pool, "uat_superadmin", &limits),
        replica(&pool, "uat_superadmin", &limits),
    );
    let (cs, lead) = (tok(&pool, f.cs).await, tok(&pool, f.lead).await);
    let missing = Uuid::new_v4();
    let test = format!("/api/v1/workspaces/{}/integrations/{missing}/test", f.ws);
    let provider = format!(
        "/api/v1/workspaces/{}/ai-settings/providers/openai/test",
        f.ws
    );
    // Different outbound routes share one budget per user, across replicas.
    let r = run(&a, authed("POST", &test, &cs, None)).await;
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.json);
    let r = run(&b, authed("POST", &provider, &cs, Some(json!({})))).await;
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.json);
    let r = run(&a, authed("POST", &test, &cs, None)).await;
    assert_limited(&r, 600);
    let r = run(&b, authed("POST", &test, &lead, None)).await;
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.json);

    // Live look-ups have their own, per-minute budget.
    let options = format!("/api/v1/workspaces/{}/integrations/{missing}/options", f.ws);
    let r = run(&a, authed("GET", &options, &cs, None)).await;
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.json);
    let users = format!("/api/v1/workspaces/{}/intake/jira-users?q=ab", f.ws);
    let r = run(&b, authed("GET", &users, &cs, None)).await;
    assert_limited(&r, 60);

    // Ordinary reads are not counted in Postgres at all.
    let before = scalar(&pool, "SELECT count(*) FROM rate_limit_windows").await;
    for _ in 0..3 {
        let r = run(
            &a,
            authed(
                "GET",
                &format!("/api/v1/workspaces/{}/tickets", f.ws),
                &cs,
                None,
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    }
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rate_limit_windows").await,
        before
    );
    // Without a session nothing is counted: a look-up (GET passes the session layer) gets 401.
    for _ in 0..3 {
        let anon = from_peer(
            ok(
                Request::builder()
                    .method("GET")
                    .uri(&options)
                    .body(Body::empty()),
                "request",
            ),
            "198.51.100.30:1",
        );
        let r = run(&a, anon).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    }
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rate_limit_windows").await,
        before
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_webhook_deliveries_are_limited_per_address_before_any_lookup(pool: PgPool) {
    fixture(&pool).await;
    let limit = [("ITSM_LIMIT_WEBHOOK_PER_IP_MIN", "2")];
    let (a, b) = (
        replica(&pool, "uat_superadmin", &limit),
        replica(&pool, "uat_superadmin", &limit),
    );
    let deliver = |peer: &str| {
        let r = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/v1/integrations/jira/{}/webhook",
                Uuid::new_v4()
            ))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-hub-signature", format!("sha256={}", "0".repeat(64)))
            .body(Body::from("{}"));
        from_peer(ok(r, "request"), peer)
    };
    for app in [&a, &b] {
        let r = run(app, deliver("203.0.113.70:1")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    }
    let r = run(&a, deliver("203.0.113.70:2")).await;
    assert_limited(&r, 60);
    let r = run(&b, deliver("203.0.113.71:1")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_parallel_hits_on_two_replicas_never_exceed_the_limit(pool: PgPool) {
    fixture(&pool).await;
    let limit = [("ITSM_LIMIT_AUTH_PER_IP_10MIN", "7")];
    let (a, b) = (
        replica(&pool, "uat_superadmin", &limit),
        replica(&pool, "uat_superadmin", &limit),
    );
    let mut tasks = Vec::new();
    for i in 0..24 {
        let app = if i % 2 == 0 { a.clone() } else { b.clone() };
        tasks.push(tokio::spawn(async move {
            run(&app, uat_entry("203.0.113.90:7", None)).await.status
        }));
    }
    let mut created = 0;
    let mut limited = 0;
    for t in tasks {
        match ok(t.await, "join") {
            StatusCode::CREATED => created += 1,
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!((created, limited), (7, 17));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_prune_drops_only_expired_windows(pool: PgPool) {
    let key = |b: u8| vec![b; 32];
    for (k, age) in [(1u8, "3 days"), (2, "47 hours"), (3, "0 seconds")] {
        ok(
            sqlx::query(
                "INSERT INTO rate_limit_windows (scope, key, window_start, hits) \
                 VALUES ('auth_ip', $1, now() - $2::interval, 1)",
            )
            .bind(key(k))
            .bind(age)
            .execute(&pool)
            .await,
            "insert",
        );
    }
    assert_eq!(ok(shared_limit::prune(&pool).await, "prune"), 1);
    let left: Vec<Vec<u8>> = ok(
        sqlx::query_scalar("SELECT key FROM rate_limit_windows ORDER BY key")
            .fetch_all(&pool)
            .await,
        "left",
    );
    assert_eq!(left, vec![key(2), key(3)]);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_migration_0031_rolls_back_and_reapplies(pool: PgPool) {
    let exists = "SELECT count(*) FROM pg_class WHERE relname = 'rate_limit_windows' AND relpersistence = 'u'";
    assert_eq!(scalar(&pool, exists).await, 1, "unlogged table");
    ok(db::MIGRATOR.undo(&pool, 30).await, "undo to 0030");
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM pg_class WHERE relname = 'rate_limit_windows'"
        )
        .await,
        0
    );
    ok(db::MIGRATOR.run(&pool).await, "reapply");
    assert_eq!(scalar(&pool, exists).await, 1);
}

fn assert_defensive_headers(r: &Resp) {
    let h = |name: &str| {
        r.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(
        h("content-security-policy"),
        itsm_api::API_CSP,
        "{}",
        r.status
    );
    assert!(h("content-security-policy").starts_with("default-src 'none';"));
    assert_eq!(h("x-frame-options"), "DENY");
    assert_eq!(h("referrer-policy"), "no-referrer");
    assert_eq!(h("cross-origin-resource-policy"), "same-origin");
    assert_eq!(h("x-content-type-options"), "nosniff");
    assert_eq!(h("cache-control"), "no-store");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7d_every_api_answer_carries_the_defensive_headers(pool: PgPool) {
    fixture(&pool).await;
    let app = replica(
        &pool,
        "uat_superadmin",
        &[("ITSM_LIMIT_WEBHOOK_PER_IP_MIN", "1")],
    );
    let get = |uri: &str| {
        from_peer(
            ok(Request::builder().uri(uri).body(Body::empty()), "request"),
            "198.51.100.40:1",
        )
    };
    // Success, refusal, unknown route.
    for uri in ["/health/live", "/api/v1/session", "/api/v1/nope"] {
        let r = run(&app, get(uri)).await;
        assert_defensive_headers(&r);
    }
    // Webhook: over 1 MiB is refused (413) before anything is counted or read; the headers are
    // on that answer and on the 429 of the shared limit too.
    let path = format!("/api/v1/integrations/jira/{}/webhook", Uuid::new_v4());
    let big = vec![b' '; 1024 * 1024 + 1];
    let r = run(
        &app,
        from_peer(
            ok(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, big.len())
                    .body(Body::from(big)),
                "request",
            ),
            "203.0.113.80:1",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_defensive_headers(&r);
    assert_eq!(
        scalar(&pool, "SELECT count(*) FROM rate_limit_windows").await,
        0
    );
    for expected in [StatusCode::UNAUTHORIZED, StatusCode::TOO_MANY_REQUESTS] {
        let r = run(
            &app,
            from_peer(
                ok(
                    Request::builder()
                        .method("POST")
                        .uri(&path)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}")),
                    "request",
                ),
                "203.0.113.80:2",
            ),
        )
        .await;
        assert_eq!(r.status, expected, "{}", r.json);
        assert_defensive_headers(&r);
    }
}
