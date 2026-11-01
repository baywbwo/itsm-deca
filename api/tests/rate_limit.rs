//! HTTP-level rate limiting (D11 rework): 429 contract, user/peer identity, isolation,
//! concurrency, recovery, and `X-Forwarded-For` spoofing. One router per test so the
//! limiter is shared across its requests (clones share the same limiter).

use std::collections::HashMap;
use std::fmt::Display;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::{AppState, app, seed, session};
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

fn router(pool: &PgPool, extra: &[(&str, &str)]) -> Router {
    let mut vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
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
    admin: Uuid,
    cs: Uuid,
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
    Fx {
        ws: id("SELECT id FROM workspaces WHERE code = $1", "DUMA").await,
        admin: id(
            "SELECT id FROM users WHERE external_subject = $1",
            "dummy:uat-superadmin",
        )
        .await,
        cs: id(
            "SELECT id FROM users WHERE external_subject = $1",
            "dummy:cs-alfa",
        )
        .await,
    }
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

struct Resp {
    status: StatusCode,
    retry_after: Option<String>,
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
        .map(str::to_owned);
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        retry_after,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
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
    match body {
        Some(b) => ok(
            r.header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            "request",
        ),
        None => ok(r.body(Body::empty()), "request"),
    }
}

fn create_view(ws: Uuid, t: &Tok, name: &str) -> Request<Body> {
    authed(
        "POST",
        &format!("/api/v1/workspaces/{ws}/saved-views"),
        t,
        Some(json!({ "name": name, "view": "list" })),
    )
}

/// The unauthenticated UAT entry, arriving from `peer`, optionally with a forwarded header.
fn entry(peer: &str, xff: Option<&str>) -> Request<Body> {
    let mut r = Request::builder()
        .method("POST")
        .uri(session::UAT_ENTRY_PATH)
        .header(session::UAT_ENTRY_HEADER, "1");
    if let Some(v) = xff {
        r = r.header("x-forwarded-for", v);
    }
    let mut req = ok(r.body(Body::empty()), "request");
    let addr: SocketAddr = ok(peer.parse(), "peer");
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn count(pool: &PgPool, sql: &str, ws: Uuid) -> i64 {
    ok(sqlx::query_scalar(sql).bind(ws).fetch_one(pool).await, sql)
}

#[sqlx::test]
async fn write_limit_returns_contract_429_and_does_not_run_the_mutation(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = router(&pool, &[("ITSM_RATE_LIMIT_WRITE_PER_MIN", "3")]);
    let admin = tok(&pool, f.admin).await;
    let views = "SELECT count(*) FROM saved_views WHERE workspace_id = $1";
    let audits = "SELECT count(*) FROM audit_events WHERE workspace_id = $1";
    for i in 0..3 {
        let r = run(&app, create_view(f.ws, &admin, &format!("RL {i}"))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    }
    let (v0, a0) = (
        count(&pool, views, f.ws).await,
        count(&pool, audits, f.ws).await,
    );

    let r = run(&app, create_view(f.ws, &admin, "RL over")).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["error"]["code"], "rate_limited");
    assert!(
        r.json["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );
    let secs = r.json["error"]["retryAfterSeconds"].as_u64();
    assert_eq!(secs, Some(20), "3/min refills one token every 20 s");
    assert_eq!(r.retry_after.as_deref(), Some("20"));
    // Nothing ran: no view, no audit row (429 is not a denied-permission event either).
    assert_eq!(count(&pool, views, f.ws).await, v0);
    assert_eq!(count(&pool, audits, f.ws).await, a0);

    // Reads use their own bucket; the same user can still load pages.
    let r = run(
        &app,
        authed(
            "GET",
            &format!("/api/v1/workspaces/{}/saved-views", f.ws),
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

#[sqlx::test]
async fn users_are_isolated_and_extra_sessions_share_the_user_bucket(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = router(&pool, &[("ITSM_RATE_LIMIT_WRITE_PER_MIN", "2")]);
    let first = tok(&pool, f.admin).await;
    let second = tok(&pool, f.admin).await;
    assert_eq!(
        run(&app, create_view(f.ws, &first, "U1")).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        run(&app, create_view(f.ws, &second, "U2")).await.status,
        StatusCode::CREATED
    );
    // A new session for the same user does not reset the limit.
    let third = tok(&pool, f.admin).await;
    let r = run(&app, create_view(f.ws, &third, "U3")).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);

    // Another user is unaffected (CS has no saved-view rights in other spaces, but its own
    // bucket admits the request: the handler answers, not the limiter).
    let cs = tok(&pool, f.cs).await;
    let r = run(&app, create_view(f.ws, &cs, "CS1")).await;
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.json);
}

#[sqlx::test]
async fn parallel_writes_never_exceed_the_limit(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = router(&pool, &[("ITSM_RATE_LIMIT_WRITE_PER_MIN", "5")]);
    let admin = Arc::new(tok(&pool, f.admin).await);
    let mut tasks = Vec::new();
    for i in 0..30 {
        let app = app.clone();
        let admin = Arc::clone(&admin);
        let ws = f.ws;
        tasks.push(tokio::spawn(async move {
            run(&app, create_view(ws, &admin, &format!("P{i}")))
                .await
                .status
        }));
    }
    let mut passed = 0;
    let mut limited = 0;
    for t in tasks {
        match ok(t.await, "join") {
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            StatusCode::CREATED => passed += 1,
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!((passed, limited), (5, 25));
    let views = count(
        &pool,
        "SELECT count(*) FROM saved_views WHERE workspace_id = $1 AND name LIKE 'P%'",
        f.ws,
    )
    .await;
    assert_eq!(views, 5);
}

#[sqlx::test]
async fn limit_recovers_after_refill(pool: PgPool) {
    let f = fixture(&pool).await;
    // 60/min = one token per second. The CSRF refresh is a cheap write with no row cap.
    let app = router(&pool, &[("ITSM_RATE_LIMIT_WRITE_PER_MIN", "60")]);
    let admin = tok(&pool, f.admin).await;
    let refresh = || {
        let mut req = authed("POST", session::CSRF_REFRESH_PATH, &admin, None);
        req.headers_mut().insert(
            session::CSRF_REFRESH_HEADER,
            axum::http::HeaderValue::from_static("1"),
        );
        req
    };
    let mut passed = 0;
    let mut first_limited = None;
    for _ in 0..70 {
        let r = run(&app, refresh()).await;
        if r.status == StatusCode::TOO_MANY_REQUESTS {
            first_limited = Some(r);
            break;
        }
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        passed += 1;
    }
    let Some(limited) = first_limited else {
        panic!("never limited after {passed}");
    };
    assert!((60..=62).contains(&passed), "{passed}");
    assert_eq!(limited.retry_after.as_deref(), Some("1"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let r = run(&app, refresh()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[sqlx::test]
async fn unauthenticated_entry_is_keyed_by_peer_and_ignores_spoofed_forwarded_for(pool: PgPool) {
    let _ = fixture(&pool).await;
    let app = router(&pool, &[("ITSM_RATE_LIMIT_ANON_PER_MIN", "2")]);
    let peer = "203.0.113.9:40000";
    for _ in 0..2 {
        assert_eq!(
            run(&app, entry(peer, None)).await.status,
            StatusCode::CREATED
        );
    }
    let r = run(&app, entry(peer, None)).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["error"]["code"], "rate_limited");
    assert!(r.retry_after.is_some());
    // A forged header from an untrusted peer does not buy a fresh bucket.
    for fake in ["1.1.1.1", "2.2.2.2, 3.3.3.3", "127.0.0.1"] {
        let r = run(&app, entry(peer, Some(fake))).await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "xff {fake}");
    }
    // Another peer (another source port is the same peer; another address is not).
    let r = run(&app, entry("203.0.113.9:40001", None)).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = run(&app, entry("198.51.100.7:40000", None)).await;
    assert_eq!(r.status, StatusCode::CREATED);
}

#[sqlx::test]
async fn trusted_proxy_forwarded_for_separates_clients_but_not_forged_prefixes(pool: PgPool) {
    let _ = fixture(&pool).await;
    let app = router(
        &pool,
        &[
            ("ITSM_RATE_LIMIT_ANON_PER_MIN", "1"),
            ("ITSM_TRUSTED_PROXIES", "127.0.0.1"),
        ],
    );
    let proxy = "127.0.0.1:5000";
    assert_eq!(
        run(&app, entry(proxy, Some("192.0.2.10"))).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        run(&app, entry(proxy, Some("192.0.2.10"))).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // The client prepends a fake hop; the proxy appended the real one, which is what counts.
    assert_eq!(
        run(&app, entry(proxy, Some("9.9.9.9, 192.0.2.10")))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        run(&app, entry(proxy, Some("192.0.2.11"))).await.status,
        StatusCode::CREATED
    );
}
