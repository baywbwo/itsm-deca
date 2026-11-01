//! D06 List/Board against a real PostgreSQL: pages are walked to the end and compared with
//! independent SQL (scope, filters, every sort with ID tie-break), counts cover the whole
//! scope (more than one page), Board columns equal List pages, cursors are bound to the
//! workspace/filter/sort, Dashboard drilldowns open equivalent Lists, and every mutation is
//! checked UI-path → API → DB → reload, including version conflicts that change nothing,
//! lifecycle guards, roles and audit rows. Role tests use DB fixtures and issued sessions
//! (no impersonation endpoint). Fixtures are synthetic and labelled dummy.

use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::error::AppError;
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

fn state(pool: &PgPool) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    AppState {
        config: Arc::new(ok(Config::from_map(&vars), "test config")),
        pool: pool.clone(),
    }
}

struct Fx {
    a: Uuid,
    b: Uuid,
    admin: Uuid,
    cs: Uuid,
    lead: Uuid,
}

async fn scalar_uuid(pool: &PgPool, sql: &str, key: &str) -> Uuid {
    ok(sqlx::query_scalar(sql).bind(key).fetch_one(pool).await, key)
}

async fn fixture(pool: &PgPool) -> Fx {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    let ws = "SELECT id FROM workspaces WHERE code = $1";
    let user = "SELECT id FROM users WHERE external_subject = $1";
    Fx {
        a: scalar_uuid(pool, ws, "DUMA").await,
        b: scalar_uuid(pool, ws, "DUMB").await,
        admin: scalar_uuid(pool, user, "dummy:uat-superadmin").await,
        cs: scalar_uuid(pool, user, "dummy:cs-alfa").await,
        lead: scalar_uuid(pool, user, "dummy:lead-alfa").await,
    }
}

/// Inserts a dummy ticket (status `new`) received at `received` (SQL expression).
#[allow(clippy::too_many_arguments)]
async fn ticket(
    pool: &PgPool,
    ws: Uuid,
    by: Uuid,
    owner: Option<Uuid>,
    received: &str,
    channel: Option<&str>,
    priority: Option<&str>,
    category: Option<&str>,
) -> Uuid {
    let sql = format!(
        "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, owner_user_id, \
           intake_channel_id, priority_id, category_id) \
         VALUES ($1, 'Pengaduan uji (dummy)', 'Kronologi uji (dummy)', {received}, $2, $3, \
           (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = $4), \
           (SELECT id FROM priorities WHERE workspace_id = $1 AND code = $5), \
           (SELECT id FROM categories WHERE workspace_id = $1 AND code = $6)) RETURNING id"
    );
    ok(
        sqlx::query_scalar(&sql)
            .bind(ws)
            .bind(by)
            .bind(owner)
            .bind(channel)
            .bind(priority)
            .bind(category)
            .fetch_one(pool)
            .await,
        "insert ticket",
    )
}

async fn simple(pool: &PgPool, ws: Uuid, by: Uuid) -> Uuid {
    ticket(
        pool,
        ws,
        by,
        None,
        "now()",
        Some("email"),
        Some("normal"),
        None,
    )
    .await
}

async fn set_status(pool: &PgPool, t: Uuid, status: &str, cycle: i32) {
    ok(
        sqlx::query(
            "UPDATE tickets SET workflow_status = $2, cycle_no = $3, version = version + 1 WHERE id = $1",
        )
        .bind(t)
        .bind(status)
        .bind(cycle)
        .execute(pool)
        .await,
        "set status",
    );
}

async fn set_subject(pool: &PgPool, t: Uuid, subject: &str) {
    ok(
        sqlx::query("UPDATE tickets SET subject = $2, version = version + 1 WHERE id = $1")
            .bind(t)
            .bind(subject)
            .execute(pool)
            .await,
        "set subject",
    );
}

async fn event(pool: &PgPool, ws: Uuid, t: Uuid, cycle: i32, action: &str, at: &str, by: Uuid) {
    let sql = format!(
        "INSERT INTO ticket_resolution_events (workspace_id, ticket_id, cycle_no, action, summary, actor_id, occurred_at) \
         VALUES ($1, $2, $3, $4, 'Ringkasan uji (dummy)', $5, {at})"
    );
    ok(
        sqlx::query(&sql)
            .bind(ws)
            .bind(t)
            .bind(cycle)
            .bind(action)
            .bind(by)
            .execute(pool)
            .await,
        "event",
    );
}

/// Records lifecycle events in order, moving the ticket like the workflow does.
async fn lifecycle(pool: &PgPool, ws: Uuid, t: Uuid, steps: &[(i32, &str, &str)], by: Uuid) {
    for &(cycle, action, at) in steps {
        event(pool, ws, t, cycle, action, at, by).await;
        let status = if action == "resolve" {
            "resolved"
        } else {
            "open"
        };
        set_status(pool, t, status, cycle).await;
    }
}

/// Historical archive row: `closed` cannot be produced by updates (guards), so the fixture
/// writes it with triggers disabled on one connection, like an imported legacy row.
async fn archive(pool: &PgPool, t: Uuid) {
    let mut c = ok(pool.acquire().await, "acquire");
    ok(
        sqlx::query("SET session_replication_role = replica")
            .execute(&mut *c)
            .await,
        "replica",
    );
    ok(
        sqlx::query(
            "UPDATE tickets SET workflow_status = 'closed', version = version + 1 WHERE id = $1",
        )
        .bind(t)
        .execute(&mut *c)
        .await,
        "archive",
    );
    ok(
        sqlx::query("SET session_replication_role = origin")
            .execute(&mut *c)
            .await,
        "origin",
    );
}

async fn related(pool: &PgPool, ws: Uuid, t: Uuid, by: Uuid, blocking: bool) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO related_work (workspace_id, ticket_id, title, blocking, requested_by) \
             VALUES ($1, $2, 'Pekerjaan terkait uji (dummy)', $3, $4) RETURNING id",
        )
        .bind(ws)
        .bind(t)
        .bind(blocking)
        .bind(by)
        .fetch_one(pool)
        .await,
        "related work",
    )
}

async fn count(pool: &PgPool, sql: &str, id: Uuid) -> i64 {
    ok(sqlx::query_scalar(sql).bind(id).fetch_one(pool).await, sql)
}

async fn ids(pool: &PgPool, sql: &str, ws: Uuid) -> Vec<Uuid> {
    ok(sqlx::query_scalar(sql).bind(ws).fetch_all(pool).await, sql)
}

async fn code_id(pool: &PgPool, table: &str, ws: Uuid, code: &str) -> Uuid {
    let sql = format!("SELECT id FROM {table} WHERE workspace_id = $1 AND code = $2");
    ok(
        sqlx::query_scalar(&sql)
            .bind(ws)
            .bind(code)
            .fetch_one(pool)
            .await,
        code,
    )
}

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    json: Value,
    raw: String,
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

async fn send(pool: &PgPool, request: Request<Body>) -> Resp {
    let res = match app(state(pool)).oneshot(request).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    Resp {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        raw,
    }
}

fn cookie(t: &Tok) -> String {
    format!("{}={}", session::cookie_name(false), t.token)
}

async fn get(pool: &PgPool, uri: &str, t: &Tok) -> Resp {
    let r = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, cookie(t));
    send(pool, ok(r.body(Body::empty()), "request")).await
}

async fn post_with(pool: &PgPool, uri: &str, t: &Tok, csrf: Option<&str>, body: &Value) -> Resp {
    let mut r = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::COOKIE, cookie(t))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-request-id", "uji-d06-korelasi");
    if let Some(c) = csrf {
        r = r.header(session::CSRF_HEADER, c);
    }
    send(pool, ok(r.body(Body::from(body.to_string())), "request")).await
}

async fn post(pool: &PgPool, uri: &str, t: &Tok, body: &Value) -> Resp {
    post_with(pool, uri, t, Some(&t.csrf), body).await
}

fn n(v: &Value) -> i64 {
    match v.as_i64() {
        Some(x) => x,
        None => panic!("not a number: {v}"),
    }
}

fn uuid(v: &Value) -> Uuid {
    ok(Uuid::parse_str(v.as_str().unwrap_or_default()), "uuid")
}

/// Query-string encoding for the few characters our values can contain.
fn enc(s: &str) -> String {
    s.replace('%', "%25")
        .replace('+', "%2B")
        .replace(':', "%3A")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('&', "%26")
}

/// A Dashboard `TicketFilter` as List query parameters (arrays comma-joined).
fn qs(filter: &Value) -> String {
    let mut out = Vec::new();
    if let Some(map) = filter.as_object() {
        for (k, v) in map {
            let val = match v {
                Value::Array(a) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
                Value::String(s) => s.clone(),
                other => panic!("unexpected filter value {other}"),
            };
            out.push(format!("{k}={}", enc(&val)));
        }
    }
    out.join("&")
}

fn list_uri(ws: Uuid, query: &str) -> String {
    format!("/api/v1/workspaces/{ws}/tickets?{query}")
}

async fn list(pool: &PgPool, ws: Uuid, query: &str, t: &Tok) -> Value {
    let r = get(pool, &list_uri(ws, query), t).await;
    assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.json);
    r.json
}

/// Walks every page; returns the items in order and the first page.
async fn walk(pool: &PgPool, ws: Uuid, query: &str, t: &Tok) -> (Vec<Value>, Value) {
    walk_from(pool, ws, query, None, t).await
}

/// Same, starting from a cursor handed out elsewhere (a Board column).
async fn walk_from(
    pool: &PgPool,
    ws: Uuid,
    query: &str,
    start: Option<&str>,
    t: &Tok,
) -> (Vec<Value>, Value) {
    let first_query = match start {
        Some(c) => format!("{query}&cursor={c}"),
        None => query.to_owned(),
    };
    let first = list(pool, ws, &first_query, t).await;
    let mut items: Vec<Value> = first["items"].as_array().cloned().unwrap_or_default();
    let mut next = first["nextCursor"].as_str().map(str::to_owned);
    let mut guard = 0;
    while let Some(c) = next {
        guard += 1;
        assert!(guard < 200, "too many pages for {query}");
        let page = list(pool, ws, &format!("{query}&cursor={c}"), t).await;
        // Counts stay the whole-scope numbers on every page.
        assert_eq!(page["total"], first["total"], "{query}: total changed");
        items.extend(page["items"].as_array().cloned().unwrap_or_default());
        next = page["nextCursor"].as_str().map(str::to_owned);
    }
    (items, first)
}

fn item_ids(items: &[Value]) -> Vec<Uuid> {
    items.iter().map(|i| uuid(&i["id"])).collect()
}

fn as_set(v: &[Uuid]) -> HashSet<Uuid> {
    v.iter().copied().collect()
}

// ---------- scope, paging and sort ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn pages_cover_whole_scope_in_stable_order_for_every_sort(pool: PgPool) {
    let f = fixture(&pool).await;
    let statuses = ["new", "open", "pending_customer", "pending_engineering"];
    // 75 tickets in DUMA; received_at repeats every 5 tickets so the ID tie-break matters.
    for i in 0..75_usize {
        let received = format!(
            "timestamptz '2026-10-20T08:00:00Z' + interval '{} hours'",
            i / 5
        );
        let t = ticket(
            &pool,
            f.a,
            f.admin,
            None,
            &received,
            Some("email"),
            Some("normal"),
            None,
        )
        .await;
        if i % 4 != 0 {
            set_status(&pool, t, statuses[i % 4], 1).await;
        }
    }
    for _ in 0..6 {
        simple(&pool, f.b, f.admin).await;
    }
    let t = tok(&pool, f.admin).await;
    let total = count(
        &pool,
        "SELECT count(*) FROM tickets WHERE workspace_id = $1",
        f.a,
    )
    .await;
    assert_eq!(total, 75);

    for (sort, col) in [
        ("updated_at", "updated_at"),
        ("received_at", "received_at"),
        ("number", "number"),
    ] {
        for order in ["asc", "desc"] {
            let query = format!("sort={sort}&order={order}&limit=7");
            let (items, first) = walk(&pool, f.a, &query, &t).await;
            assert_eq!(n(&first["total"]), total, "{query}");
            assert_eq!(first["items"].as_array().map(Vec::len), Some(7));
            assert!(first["nextCursor"].is_string());
            let got = item_ids(&items);
            assert_eq!(
                as_set(&got).len(),
                got.len(),
                "{query}: duplicate across pages"
            );
            let expected = ids(
                &pool,
                &format!(
                    "SELECT id FROM tickets WHERE workspace_id = $1 ORDER BY {col} {order}, id {order}"
                ),
                f.a,
            )
            .await;
            assert_eq!(got, expected, "{query}: order differs from SQL");
            // Count per status is over the whole scope, not the page.
            let by_status: i64 = first["countsByStatus"]
                .as_array()
                .map(|a| a.iter().map(|c| n(&c["count"])).sum())
                .unwrap_or(-1);
            assert_eq!(by_status, total);
        }
    }
    // Default: updated_at desc, 50 per page.
    let d = list(&pool, f.a, "", &t).await;
    assert_eq!(d["items"].as_array().map(Vec::len), Some(50));
    assert_eq!(d["filters"]["sort"], "updated_at");
    assert_eq!(d["filters"]["order"], "desc");
    assert_eq!(d["basis"]["unit"], "ticket");
    assert_eq!(d["basis"]["dimensions"], "current_ticket_attributes");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn filters_match_independent_queries(pool: PgPool) {
    let f = fixture(&pool).await;
    let owners = [None, Some(f.cs), Some(f.lead)];
    let channels = [Some("whatsapp"), Some("email"), Some("website"), None];
    let priorities = [
        Some("low"),
        Some("normal"),
        Some("high"),
        Some("urgent"),
        None,
    ];
    let categories = [Some("kategori_uji"), Some("kategori_uji_b"), None];
    let statuses = ["new", "open", "pending_customer", "pending_engineering"];
    let mut all = Vec::new();
    for i in 0..60_usize {
        let received = format!(
            "timestamptz '2026-10-01T00:00:00Z' + interval '{} hours'",
            i * 13
        );
        let t = ticket(
            &pool,
            f.a,
            f.admin,
            owners[i % 3],
            &received,
            channels[i % 4],
            priorities[i % 5],
            categories[i % 3],
        )
        .await;
        if i % 4 != 0 {
            set_status(&pool, t, statuses[i % 4], 1).await;
        }
        all.push(t);
    }
    // Search fixtures: LIKE wildcards are literal.
    set_subject(&pool, all[1], "Diskon 50% tidak masuk (dummy)").await;
    set_subject(&pool, all[2], "Diskon 50 persen (dummy)").await;
    set_subject(&pool, all[3], "kode_promo salah (dummy)").await;
    // Resolution history: one ticket resolved twice (multi-cycle) inside the window, one
    // resolved outside, one resolved inside then reopened (still counted as an event).
    let multi = simple(&pool, f.a, f.admin).await;
    lifecycle(
        &pool,
        f.a,
        multi,
        &[
            (1, "resolve", "timestamptz '2026-10-10T03:00:00Z'"),
            (2, "reopen", "timestamptz '2026-10-11T03:00:00Z'"),
            (2, "resolve", "timestamptz '2026-10-12T03:00:00Z'"),
        ],
        f.admin,
    )
    .await;
    let outside = simple(&pool, f.a, f.admin).await;
    lifecycle(
        &pool,
        f.a,
        outside,
        &[(1, "resolve", "timestamptz '2026-09-10T03:00:00Z'")],
        f.admin,
    )
    .await;
    let reopened = simple(&pool, f.a, f.admin).await;
    lifecycle(
        &pool,
        f.a,
        reopened,
        &[
            (1, "resolve", "timestamptz '2026-10-15T03:00:00Z'"),
            (2, "reopen", "timestamptz '2026-10-16T03:00:00Z'"),
        ],
        f.admin,
    )
    .await;
    // Noise in DUMB never leaks into DUMA.
    for _ in 0..5 {
        simple(&pool, f.b, f.admin).await;
    }

    let t = tok(&pool, f.admin).await;
    let email = code_id(&pool, "intake_channels", f.a, "email").await;
    let high = code_id(&pool, "priorities", f.a, "high").await;
    let urgent = code_id(&pool, "priorities", f.a, "urgent").await;
    let cat = code_id(&pool, "categories", f.a, "kategori_uji").await;
    let number: i64 = ok(
        sqlx::query_scalar("SELECT number FROM tickets WHERE id = $1")
            .bind(all[7])
            .fetch_one(&pool)
            .await,
        "number",
    );

    let base = "SELECT id FROM tickets t WHERE workspace_id = $1";
    let cases: Vec<(String, String)> = vec![
        (
            "owner=unassigned".into(),
            format!("{base} AND owner_user_id IS NULL"),
        ),
        (
            format!("owner={}", f.cs),
            format!("{base} AND owner_user_id = '{}'", f.cs),
        ),
        (
            "channel=none".into(),
            format!("{base} AND intake_channel_id IS NULL"),
        ),
        (
            format!("channel={email}"),
            format!("{base} AND intake_channel_id = '{email}'"),
        ),
        (
            format!("category={cat}"),
            format!("{base} AND category_id = '{cat}'"),
        ),
        (
            "category=none".into(),
            format!("{base} AND category_id IS NULL"),
        ),
        (
            format!("priority={high},{urgent}"),
            format!("{base} AND priority_id IN ('{high}', '{urgent}')"),
        ),
        (
            "priority=none".into(),
            format!("{base} AND priority_id IS NULL"),
        ),
        (
            "status=new,pending_customer".into(),
            format!("{base} AND workflow_status IN ('new', 'pending_customer')"),
        ),
        (
            "status=resolved".into(),
            format!("{base} AND workflow_status = 'resolved'"),
        ),
        (
            format!("status=open&owner={}&channel={email}", f.lead),
            format!(
                "{base} AND workflow_status = 'open' AND owner_user_id = '{}' AND intake_channel_id = '{email}'",
                f.lead
            ),
        ),
        (
            format!(
                "receivedFrom={}&receivedBefore={}",
                enc("2026-10-05T00:00:00+07:00"),
                enc("2026-10-15T00:00:00Z")
            ),
            format!(
                "{base} AND received_at >= '2026-10-05T00:00:00+07:00' AND received_at < '2026-10-15T00:00:00Z'"
            ),
        ),
        (
            format!("q={}", enc("50%")),
            format!("{base} AND subject LIKE '%50\\%%'"),
        ),
        (
            "q=kode_promo".into(),
            format!("{base} AND subject LIKE '%kode\\_promo%'"),
        ),
        (
            format!("q={}", enc(&format!("#{number}"))),
            format!("{base} AND number = {number}"),
        ),
        (
            format!("q=DUMA-{number}"),
            format!("{base} AND number = {number}"),
        ),
        (
            format!("q=diskon&owner={}", f.cs),
            format!(
                "{base} AND subject ILIKE '%diskon%' AND owner_user_id = '{}'",
                f.cs
            ),
        ),
    ];
    for (query, sql) in &cases {
        let (items, first) = walk(&pool, f.a, &format!("{query}&limit=9"), &t).await;
        let expected = ids(&pool, sql, f.a).await;
        assert!(
            !expected.is_empty(),
            "{query}: fixture should match something"
        );
        assert_eq!(n(&first["total"]), expected.len() as i64, "{query}: total");
        assert_eq!(
            as_set(&item_ids(&items)),
            as_set(&expected),
            "{query}: rows"
        );
        assert_eq!(items.len(), expected.len(), "{query}: duplicates");
        assert!(
            first["resolutionEvents"].is_null(),
            "{query}: no events without range"
        );
    }

    // countsByStatus ignores the status filter but keeps the others.
    let r = list(&pool, f.a, &format!("status=open&owner={}", f.cs), &t).await;
    let open_cs = count(
        &pool,
        &format!(
            "SELECT count(*) FROM tickets WHERE workspace_id = $1 AND owner_user_id = '{}'",
            f.cs
        ),
        f.a,
    )
    .await;
    let counted: i64 = r["countsByStatus"]
        .as_array()
        .map(|a| a.iter().map(|c| n(&c["count"])).sum())
        .unwrap_or(-1);
    assert_eq!(counted, open_cs);

    // Resolved range: tickets are unique, events are counted separately.
    let range = format!(
        "resolvedFrom={}&resolvedBefore={}",
        enc("2026-10-01T00:00:00+07:00"),
        enc("2026-11-01T00:00:00+07:00")
    );
    let (items, first) = walk(&pool, f.a, &format!("{range}&limit=1"), &t).await;
    let got = as_set(&item_ids(&items));
    assert_eq!(got, as_set(&[multi, reopened]));
    assert_eq!(items.len(), 2, "multi-cycle ticket listed once");
    assert_eq!(n(&first["total"]), 2);
    assert_eq!(n(&first["resolutionEvents"]), 3);
    assert_eq!(first["basis"]["resolved"], "resolve_event_in_range");
    // The reopened ticket is currently open: status filter still applies on top.
    let r = list(&pool, f.a, &format!("{range}&status=resolved"), &t).await;
    assert_eq!(n(&r["total"]), 1);
    assert_eq!(n(&r["resolutionEvents"]), 2);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn invalid_parameters_and_cursors_are_refused(pool: PgPool) {
    let f = fixture(&pool).await;
    for _ in 0..5 {
        simple(&pool, f.a, f.admin).await;
        simple(&pool, f.b, f.admin).await;
    }
    let t = tok(&pool, f.admin).await;
    let first = list(&pool, f.a, "limit=2&sort=number&order=asc", &t).await;
    let cursor = first["nextCursor"].as_str().unwrap_or_default().to_owned();
    assert!(!cursor.is_empty());
    // Same cursor keeps working with the same scope/filter/sort (limit may change).
    let again = list(
        &pool,
        f.a,
        &format!("limit=3&sort=number&order=asc&cursor={cursor}"),
        &t,
    )
    .await;
    assert_eq!(again["items"].as_array().map(Vec::len), Some(3));

    let foreign_cat = code_id(&pool, "categories", f.b, "kategori_uji").await;
    let bad = [
        format!(
            "/api/v1/workspaces/{}/tickets?limit=2&sort=number&order=asc&cursor={cursor}",
            f.b
        ),
        list_uri(
            f.a,
            &format!("limit=2&sort=number&order=desc&cursor={cursor}"),
        ),
        list_uri(
            f.a,
            &format!("limit=2&sort=received_at&order=asc&cursor={cursor}"),
        ),
        list_uri(
            f.a,
            &format!("limit=2&sort=number&order=asc&status=new&cursor={cursor}"),
        ),
        list_uri(
            f.a,
            &format!("limit=2&sort=number&order=asc&q=x&cursor={cursor}"),
        ),
        list_uri(f.a, "cursor=bm90LWEtY3Vyc29y"),
        list_uri(f.a, "cursor=%%%"),
        list_uri(f.a, "limit=0"),
        list_uri(f.a, "limit=101"),
        list_uri(f.a, "limit=abc"),
        list_uri(f.a, "sort=subject"),
        list_uri(f.a, "order=up"),
        list_uri(f.a, "status=done"),
        list_uri(f.a, "owner=someone"),
        list_uri(f.a, "priority=none,none"),
        list_uri(f.a, &format!("category={foreign_cat}")),
        list_uri(f.a, "receivedFrom=yesterday"),
        list_uri(
            f.a,
            &format!("receivedFrom={}", enc("2026-13-45T00:00:00Z")),
        ),
        list_uri(f.a, &format!("q={}", "x".repeat(101))),
        format!("/api/v1/workspaces/{}/tickets/board?cursor={cursor}", f.a),
        format!("/api/v1/workspaces/{}/tickets/board?limit=51", f.a),
    ];
    for uri in &bad {
        let r = get(&pool, uri, &t).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "invalid_request");
        assert!(
            !r.raw.contains("SQL") && !r.raw.contains("22"),
            "{uri}: {}",
            r.raw
        );
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn scope_is_isolated_per_workspace_and_role(pool: PgPool) {
    let f = fixture(&pool).await;
    let mine = simple(&pool, f.a, f.admin).await;
    let other = simple(&pool, f.b, f.admin).await;
    for uid in [f.cs, f.lead] {
        let t = tok(&pool, uid).await;
        let r = list(&pool, f.a, "", &t).await;
        assert_eq!(
            item_ids(r["items"].as_array().map(Vec::as_slice).unwrap_or(&[])),
            vec![mine]
        );
        for uri in [
            format!("/api/v1/workspaces/{}/tickets", f.b),
            format!("/api/v1/workspaces/{}/tickets/board", f.b),
        ] {
            let r = get(&pool, &uri, &t).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        }
        // A DUMB ticket through the DUMA path is simply not found.
        let r = post(
            &pool,
            &format!("/api/v1/workspaces/{}/tickets/{other}/transitions", f.a),
            &t,
            &json!({"to": "open", "version": 1}),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        let r = post(
            &pool,
            &format!("/api/v1/workspaces/{}/tickets/{other}/transitions", f.b),
            &t,
            &json!({"to": "open", "version": 1}),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    let still: String = ok(
        sqlx::query_scalar("SELECT workflow_status FROM tickets WHERE id = $1")
            .bind(other)
            .fetch_one(&pool)
            .await,
        "status",
    );
    assert_eq!(still, "new");
    // No session: 401.
    let r = send(
        &pool,
        ok(
            Request::builder()
                .uri(list_uri(f.a, ""))
                .body(Body::empty()),
            "req",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

// ---------- Board ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn board_columns_equal_list_pages_and_counts(pool: PgPool) {
    let f = fixture(&pool).await;
    let statuses = ["new", "open", "pending_customer", "pending_engineering"];
    for i in 0..70_usize {
        let owner = if i % 2 == 0 { Some(f.cs) } else { None };
        let t = ticket(
            &pool,
            f.a,
            f.admin,
            owner,
            "now()",
            Some("email"),
            Some("normal"),
            None,
        )
        .await;
        // Most land in `open` so that column has more than one page.
        let s = if i % 5 == 0 { statuses[i % 4] } else { "open" };
        if s != "new" {
            set_status(&pool, t, s, 1).await;
        }
    }
    let r = simple(&pool, f.a, f.admin).await;
    lifecycle(&pool, f.a, r, &[(1, "resolve", "now()")], f.admin).await;
    let t = tok(&pool, f.admin).await;

    for filter in [String::new(), format!("owner={}", f.cs)] {
        let b = get(
            &pool,
            &format!("/api/v1/workspaces/{}/tickets/board?{filter}", f.a),
            &t,
        )
        .await;
        assert_eq!(b.status, StatusCode::OK, "{}", b.json);
        let l = list(&pool, f.a, &filter, &t).await;
        let counts: HashMap<String, i64> = l["countsByStatus"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                (
                    c["status"]["code"].as_str().unwrap_or_default().to_owned(),
                    n(&c["count"]),
                )
            })
            .collect();
        let columns = b.json["columns"].as_array().cloned().unwrap_or_default();
        assert!(!columns.is_empty());
        assert_eq!(
            n(&b.json["total"]),
            n(&l["total"]),
            "{filter}: board vs list total"
        );
        let mut seen = 0;
        for col in &columns {
            let code = col["status"]["code"].as_str().unwrap_or_default();
            assert_eq!(
                Some(&n(&col["total"])),
                counts.get(code),
                "{filter}: {code}"
            );
            seen += 1;
            let sep = if filter.is_empty() { "" } else { "&" };
            let cq = format!("{filter}{sep}status={code}&limit=20");
            let lp = list(&pool, f.a, &cq, &t).await;
            assert_eq!(
                col["items"], lp["items"],
                "{filter}: column {code} = list page"
            );
            assert_eq!(n(&col["total"]), n(&lp["total"]));
            assert!(col["items"].as_array().map(Vec::len).unwrap_or(99) <= 20);
            // Load more: the column cursor continues through List with the same status.
            if let Some(c) = col["nextCursor"].as_str() {
                assert!(n(&col["total"]) > 20);
                let (rest, _) = walk_from(&pool, f.a, &cq, Some(c), &t).await;
                let mut all = item_ids(col["items"].as_array().map(Vec::as_slice).unwrap_or(&[]));
                all.extend(item_ids(&rest));
                assert_eq!(all.len() as i64, n(&col["total"]), "{code}: all loaded");
                assert_eq!(as_set(&all).len(), all.len(), "{code}: no duplicates");
                // A column cursor cannot be reused for another column.
                let other = get(
                    &pool,
                    &list_uri(f.a, &format!("{filter}{sep}status=new&limit=20&cursor={c}")),
                    &t,
                )
                .await;
                assert_eq!(other.status, StatusCode::BAD_REQUEST);
            }
        }
        assert_eq!(
            seen,
            counts.len(),
            "{filter}: one column per status in counts"
        );
        // Closed is not a default column when no archive rows exist.
        assert!(!counts.contains_key("closed"));
    }
    // Explicit status selection limits the columns.
    let b = get(
        &pool,
        &format!(
            "/api/v1/workspaces/{}/tickets/board?status=open,new&limit=5",
            f.a
        ),
        &t,
    )
    .await;
    let codes: Vec<&str> = b.json["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["status"]["code"].as_str())
        .collect();
    assert_eq!(codes, vec!["new", "open"]);
}

// ---------- Dashboard drilldown parity ----------

async fn dashboard(pool: &PgPool, ws: Uuid, query: &str, t: &Tok) -> Value {
    let r = get(
        pool,
        &format!("/api/v1/workspaces/{ws}/dashboard?{query}"),
        t,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.json);
    r.json
}

async fn list_total(pool: &PgPool, ws: Uuid, filter: &Value, t: &Tok) -> (i64, Value) {
    let l = list(pool, ws, &qs(filter), t).await;
    (n(&l["total"]), l)
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn dashboard_drilldowns_open_equivalent_lists(pool: PgPool) {
    let f = fixture(&pool).await;
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await,
        "db",
    );
    ok(seed::seed_uat_tickets(&pool, &name).await, "seed tickets");
    // Extra multi-cycle history inside every period so events > tickets.
    let multi = ticket(
        &pool,
        f.a,
        f.admin,
        Some(f.cs),
        "now() - interval '2 hours'",
        Some("email"),
        Some("high"),
        None,
    )
    .await;
    lifecycle(
        &pool,
        f.a,
        multi,
        &[
            (1, "resolve", "now() - interval '90 minutes'"),
            (2, "reopen", "now() - interval '80 minutes'"),
            (2, "resolve", "now() - interval '70 minutes'"),
        ],
        f.admin,
    )
    .await;
    let t = tok(&pool, f.lead).await;
    let email = code_id(&pool, "intake_channels", f.a, "email").await;
    let cat = code_id(&pool, "categories", f.a, "kategori_uji").await;
    let mut checked = 0;
    for period in ["today", "last_7_days", "this_month"] {
        for extra in [
            String::new(),
            format!("&channel={email}"),
            format!("&owner={}", f.cs),
            "&owner=unassigned".to_owned(),
            format!("&category={cat}"),
            "&priority=none".to_owned(),
        ] {
            let d = dashboard(&pool, f.a, &format!("period={period}{extra}"), &t).await;
            let tot = &d["totals"];
            let dd = &d["drilldown"];
            let label = format!("{period}{extra}");
            let pairs = [
                ("active", n(&tot["active"])),
                ("pendingCustomer", n(&tot["pendingCustomer"])),
                ("receivedInPeriod", n(&tot["receivedInPeriod"])),
            ];
            for (key, expected) in pairs {
                let (got, _) = list_total(&pool, f.a, &dd[key], &t).await;
                assert_eq!(got, expected, "{label}: {key}");
                checked += 1;
            }
            if !dd["unassigned"].is_null() {
                let (got, _) = list_total(&pool, f.a, &dd["unassigned"], &t).await;
                assert_eq!(got, n(&tot["unassigned"]), "{label}: unassigned");
                checked += 1;
            }
            // Resolved: unique tickets vs resolve events, both from the same List call.
            let (got, l) = list_total(&pool, f.a, &dd["resolvedInPeriod"], &t).await;
            assert_eq!(
                got,
                n(&tot["resolvedTicketsInPeriod"]),
                "{label}: resolved tickets"
            );
            assert_eq!(
                n(&l["resolutionEvents"]),
                n(&tot["resolvedInPeriod"]),
                "{label}: events"
            );
            checked += 2;
            if !dd["attention"].is_null() {
                let (got, _) = list_total(&pool, f.a, &dd["attention"], &t).await;
                assert_eq!(got, n(&d["attention"]["total"]), "{label}: attention");
                checked += 1;
            }
            for (arr, field) in [
                ("statusDistribution", "count"),
                ("picLoad", "activeTickets"),
                ("channels", "count"),
                ("categories", "count"),
            ] {
                for row in d[arr].as_array().into_iter().flatten() {
                    let (got, _) = list_total(&pool, f.a, &row["filter"], &t).await;
                    assert_eq!(got, n(&row[field]), "{label}: {arr} {}", row["filter"]);
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 150, "only {checked} checks");
    // The multi-cycle ticket is one List row for this month's resolved drilldown.
    let d = dashboard(&pool, f.a, "period=this_month", &t).await;
    let (items, _) = walk(
        &pool,
        f.a,
        &format!("{}&limit=5", qs(&d["drilldown"]["resolvedInPeriod"])),
        &t,
    )
    .await;
    assert_eq!(item_ids(&items).iter().filter(|i| **i == multi).count(), 1);
}

// ---------- transitions ----------

fn transition_uri(ws: Uuid, t: Uuid) -> String {
    format!("/api/v1/workspaces/{ws}/tickets/{t}/transitions")
}

fn assign_uri(ws: Uuid, t: Uuid) -> String {
    format!("/api/v1/workspaces/{ws}/tickets/{t}/assignment")
}

#[derive(Debug, PartialEq, Eq)]
struct Snap {
    status: String,
    cycle: i32,
    version: i32,
    owner: Option<Uuid>,
    events: i64,
    audits: i64,
}

async fn snap(pool: &PgPool, t: Uuid) -> Snap {
    let (status, cycle, version, owner): (String, i32, i32, Option<Uuid>) = ok(
        sqlx::query_as(
            "SELECT workflow_status, cycle_no, version, owner_user_id FROM tickets WHERE id = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await,
        "snap",
    );
    Snap {
        status,
        cycle,
        version,
        owner,
        events: count(
            pool,
            "SELECT count(*) FROM ticket_resolution_events WHERE ticket_id = $1",
            t,
        )
        .await,
        audits: count(
            pool,
            "SELECT count(*) FROM audit_events WHERE entity_id = $1",
            t,
        )
        .await,
    }
}

async fn reloaded(pool: &PgPool, ws: Uuid, id: Uuid, t: &Tok) -> Value {
    let (items, _) = walk(pool, ws, "limit=100", t).await;
    match items.into_iter().find(|i| uuid(&i["id"]) == id) {
        Some(i) => i,
        None => panic!("{id} not in list"),
    }
}

fn action<'a>(item: &'a Value, to: &str) -> Option<&'a Value> {
    item["allowedActions"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["to"]["code"] == to))
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn transitions_persist_atomically_and_conflicts_change_nothing(pool: PgPool) {
    let f = fixture(&pool).await;
    let id = simple(&pool, f.a, f.admin).await;
    let t = tok(&pool, f.cs).await;
    let uri = transition_uri(f.a, id);

    // List offers the moves the server will accept.
    let item = reloaded(&pool, f.a, id, &t).await;
    assert_eq!(item["version"], 1);
    for to in [
        "open",
        "pending_customer",
        "pending_engineering",
        "resolved",
    ] {
        assert!(action(&item, to).is_some(), "{to} offered");
    }
    assert!(action(&item, "closed").is_none());
    assert_eq!(
        action(&item, "resolved").map(|a| a["requires"].clone()),
        Some(json!("summary"))
    );

    // Move new → open: version + 1, audit row in the same transaction, visible on reload.
    let before = snap(&pool, id).await;
    let r = post(&pool, &uri, &t, &json!({"to": "open", "version": 1})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["ticket"]["status"]["code"], "open");
    assert_eq!(r.json["ticket"]["version"], 2);
    let after = snap(&pool, id).await;
    assert_eq!(
        (after.status.as_str(), after.version, after.audits),
        ("open", 2, before.audits + 1)
    );
    let (action_name, change, actor, corr): (String, Value, Uuid, Option<String>) = ok(
        sqlx::query_as(
            "SELECT action, safe_change, actor_id, correlation_id FROM audit_events WHERE id = $1",
        )
        .bind(uuid(&r.json["auditEventId"]))
        .fetch_one(&pool)
        .await,
        "audit",
    );
    assert_eq!(action_name, "ticket.transition");
    assert_eq!(actor, f.cs);
    assert_eq!(corr.as_deref(), Some("uji-d06-korelasi"));
    assert_eq!(change["from"], "new");
    assert_eq!(change["to"], "open");
    assert_eq!(change["kind"], "move");
    assert_eq!(change["version"], 2);
    let item = reloaded(&pool, f.a, id, &t).await;
    assert_eq!(
        (item["status"]["code"].clone(), item["version"].clone()),
        (json!("open"), json!(2))
    );

    // Stale version: 409 and nothing changes (no event, no audit, same version).
    let still = snap(&pool, id).await;
    let r = post(
        &pool,
        &uri,
        &t,
        &json!({"to": "pending_customer", "version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["error"]["code"], "version_conflict");
    assert_eq!(snap(&pool, id).await, still);
    // Same request replayed after success also conflicts (no double apply).
    let r = post(&pool, &uri, &t, &json!({"to": "open", "version": 1})).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // Input rules: nothing persisted on refusal.
    let refused = [
        (
            json!({"to": "pending_customer", "version": 2, "summary": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"to": "pending_customer", "version": 2, "reason": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"to": "resolved", "version": 2}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"to": "resolved", "version": 2, "summary": "   "}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"to": "resolved", "version": 2, "reason": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"to": "resolved", "version": 2, "summary": "x".repeat(5001)}),
            StatusCode::BAD_REQUEST,
        ),
        (json!({"to": "done", "version": 2}), StatusCode::BAD_REQUEST),
        (json!({"to": "open", "version": 0}), StatusCode::BAD_REQUEST),
        (
            json!({"to": "pending_customer", "version": 2, "extra": 1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"to": "open", "version": 2}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"to": "closed", "version": 2}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (body, code) in &refused {
        let r = post(&pool, &uri, &t, body).await;
        assert_eq!(r.status, *code, "{body}: {}", r.json);
        assert_eq!(snap(&pool, id).await, still, "{body}");
    }
    // Missing or wrong CSRF: 403 before anything runs.
    for csrf in [None, Some("salah")] {
        let r = post_with(
            &pool,
            &uri,
            &t,
            csrf,
            &json!({"to": "pending_customer", "version": 2}),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.json["error"]["code"], "csrf_failed");
    }
    let r = send(
        &pool,
        ok(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::COOKIE, cookie(&t))
                .header(session::CSRF_HEADER, &t.csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json")),
            "req",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(snap(&pool, id).await, still);

    // Blocking related work: resolve is offered as blocked and refused with nothing persisted.
    let rw = related(&pool, f.a, id, f.admin, true).await;
    let item = reloaded(&pool, f.a, id, &t).await;
    assert_eq!(item["relatedWork"]["blockingOpen"], 1);
    assert_eq!(
        action(&item, "resolved").map(|a| a["blockedBy"].clone()),
        Some(json!("blocking_related_work"))
    );
    let r = post(
        &pool,
        &uri,
        &t,
        &json!({"to": "resolved", "version": 2, "summary": "Selesai (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json["error"]["code"], "rule_violation");
    assert!(
        !r.raw.contains("related") && !r.raw.contains("IT422"),
        "{}",
        r.raw
    );
    assert_eq!(
        snap(&pool, id).await,
        still,
        "event rolled back with the refused resolve"
    );
    ok(
        sqlx::query(
            "UPDATE related_work SET completion_state = 'completed', completion_summary = 'Selesai (dummy)', \
               completion_evidence = 'Bukti (dummy)', completed_by = $2, completed_at = now(), version = version + 1 \
             WHERE id = $1",
        )
        .bind(rw)
        .bind(f.admin)
        .execute(&pool)
        .await,
        "complete related",
    );

    // Resolve with summary: event + status in one transaction.
    let r = post(
        &pool,
        &uri,
        &t,
        &json!({"to": "resolved", "version": 2, "summary": "Saldo dikembalikan (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let s = snap(&pool, id).await;
    assert_eq!(
        (s.status.as_str(), s.cycle, s.version, s.events),
        ("resolved", 1, 3, 1)
    );
    let item = reloaded(&pool, f.a, id, &t).await;
    let offered: Vec<&str> = item["allowedActions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["to"]["code"].as_str())
        .collect();
    assert_eq!(offered, vec!["open"]);
    assert_eq!(
        action(&item, "open").map(|a| a["kind"].clone()),
        Some(json!("reopen"))
    );
    for body in [
        json!({"to": "pending_customer", "version": 3}),
        json!({"to": "open", "version": 3}),
        json!({"to": "open", "version": 3, "summary": "x"}),
    ] {
        let r = post(&pool, &uri, &t, &body).await;
        assert!(
            r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::BAD_REQUEST,
            "{body}"
        );
        assert_eq!(snap(&pool, id).await, s);
    }
    // Reopen with reason starts cycle 2; a second resolve is a second event of one ticket.
    let r = post(
        &pool,
        &uri,
        &t,
        &json!({"to": "open", "version": 3, "reason": "Pelanggan komplain lagi (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["ticket"]["cycleNo"], 2);
    let r = post(
        &pool,
        &uri,
        &t,
        &json!({"to": "resolved", "version": 4, "summary": "Selesai lagi (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let s = snap(&pool, id).await;
    assert_eq!((s.cycle, s.version, s.events), (2, 5, 3));
    let resolves = count(
        &pool,
        "SELECT count(*) FROM ticket_resolution_events WHERE ticket_id = $1 AND action = 'resolve'",
        id,
    )
    .await;
    assert_eq!(resolves, 2);
    let l = list(
        &pool,
        f.a,
        &format!("resolvedFrom={}", enc("2026-01-01T00:00:00Z")),
        &t,
    )
    .await;
    assert_eq!((n(&l["total"]), n(&l["resolutionEvents"])), (1, 2));
    let audits = count(
        &pool,
        "SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = 'ticket.transition'",
        id,
    )
    .await;
    assert_eq!(audits, 4);

    // Closed archive: read-only, no actions, every move refused.
    let old = simple(&pool, f.a, f.admin).await;
    archive(&pool, old).await;
    let item = reloaded(&pool, f.a, old, &t).await;
    assert_eq!(item["allowedActions"], json!([]));
    let s = snap(&pool, old).await;
    for to in ["open", "resolved", "new"] {
        let r = post(
            &pool,
            &transition_uri(f.a, old),
            &t,
            &json!({"to": to, "version": s.version, "summary": "x"}),
        )
        .await;
        assert!(
            r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::BAD_REQUEST,
            "{to}"
        );
        let r = post(
            &pool,
            &transition_uri(f.a, old),
            &t,
            &json!({"to": "open", "version": s.version}),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(snap(&pool, old).await, s);
    // Unknown ticket in my workspace: 404.
    let r = post(
        &pool,
        &transition_uri(f.a, Uuid::new_v4()),
        &t,
        &json!({"to": "open", "version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn concurrent_transitions_let_exactly_one_win(pool: PgPool) {
    let f = fixture(&pool).await;
    let id = simple(&pool, f.a, f.admin).await;
    let t = tok(&pool, f.lead).await;
    let uri = transition_uri(f.a, id);
    let (ba, bb) = (
        json!({"to": "open", "version": 1}),
        json!({"to": "pending_customer", "version": 1}),
    );
    let a = post(&pool, &uri, &t, &ba);
    let b = post(&pool, &uri, &t, &bb);
    let (a, b) = tokio::join!(a, b);
    let mut codes = [a.status, b.status];
    codes.sort();
    assert_eq!(codes, [StatusCode::OK, StatusCode::CONFLICT]);
    let s = snap(&pool, id).await;
    assert_eq!((s.version, s.audits), (2, 1));
}

// ---------- assignment and roles ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn assignment_follows_role_permissions_and_is_audited(pool: PgPool) {
    let f = fixture(&pool).await;
    let id = simple(&pool, f.a, f.admin).await;
    let cs = tok(&pool, f.cs).await;
    let lead = tok(&pool, f.lead).await;
    let uri = assign_uri(f.a, id);

    // CS takes an unassigned ticket for self (no reason needed or accepted).
    let r = post(
        &pool,
        &uri,
        &cs,
        &json!({"ownerUserId": f.cs, "version": 1, "reason": "x"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = post(
        &pool,
        &uri,
        &cs,
        &json!({"ownerUserId": f.cs, "version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["ticket"]["owner"]["id"], json!(f.cs));
    let (change,): (Value,) = ok(
        sqlx::query_as("SELECT safe_change FROM audit_events WHERE id = $1")
            .bind(uuid(&r.json["auditEventId"]))
            .fetch_one(&pool)
            .await,
        "audit",
    );
    assert_eq!(change["kind"], "take_for_self");

    // CS cannot hand it to someone else or release it.
    let s = snap(&pool, id).await;
    for body in [
        json!({"ownerUserId": f.lead, "version": 2, "reason": "Alih tugas (dummy)"}),
        json!({"ownerUserId": null, "version": 2, "reason": "Lepas (dummy)"}),
    ] {
        let r = post(&pool, &uri, &cs, &body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(snap(&pool, id).await, s);
    }
    // CS cannot take a ticket another person owns.
    let owned = ticket(&pool, f.a, f.admin, Some(f.lead), "now()", None, None, None).await;
    let r = post(
        &pool,
        &assign_uri(f.a, owned),
        &cs,
        &json!({"ownerUserId": f.cs, "version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Lead CS reassigns with a mandatory reason; stale version is a conflict.
    let r = post(
        &pool,
        &uri,
        &lead,
        &json!({"ownerUserId": f.lead, "version": 2}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = post(
        &pool,
        &uri,
        &lead,
        &json!({"ownerUserId": f.lead, "version": 1, "reason": "Alih tugas (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(snap(&pool, id).await, s);
    let r = post(
        &pool,
        &uri,
        &lead,
        &json!({"ownerUserId": f.lead, "version": 2, "reason": "Alih tugas (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let (change, actor): (Value, Uuid) = ok(
        sqlx::query_as("SELECT safe_change, actor_id FROM audit_events WHERE id = $1")
            .bind(uuid(&r.json["auditEventId"]))
            .fetch_one(&pool)
            .await,
        "audit",
    );
    assert_eq!(actor, f.lead);
    assert_eq!(change["kind"], "reassign");
    assert_eq!(change["reason"], "Alih tugas (dummy)");
    assert_eq!(change["to"], json!(f.lead));
    // Unchanged owner is a rule error.
    let r = post(
        &pool,
        &uri,
        &lead,
        &json!({"ownerUserId": f.lead, "version": 3, "reason": "Sama (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Owner must be an active member of this workspace; unknown user is the same generic
    // rule error (FK/guard text never reaches the client).
    let s = snap(&pool, id).await;
    for owner in [f.admin, Uuid::new_v4()] {
        let r = post(
            &pool,
            &uri,
            &lead,
            &json!({"ownerUserId": owner, "version": 3, "reason": "Uji (dummy)"}),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{owner}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["code"], "rule_violation");
        for leak in [
            "fkey",
            "constraint",
            "owner_user_id",
            "IT422",
            "23503",
            "tickets",
        ] {
            assert!(!r.raw.contains(leak), "{leak} leaked: {}", r.raw);
        }
        assert_eq!(snap(&pool, id).await, s);
    }
    // Closed archive cannot be reassigned.
    let old = simple(&pool, f.a, f.admin).await;
    archive(&pool, old).await;
    let r = post(
        &pool,
        &assign_uri(f.a, old),
        &lead,
        &json!({"ownerUserId": f.lead, "version": 2, "reason": "Uji (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // CS and Lead CS both transition their workspace tickets; the list reflects owner filter.
    let l = list(&pool, f.a, &format!("owner={}", f.lead), &lead).await;
    assert!(item_ids(l["items"].as_array().map(Vec::as_slice).unwrap_or(&[])).contains(&id));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn foreign_key_violation_maps_to_generic_rule_error(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = simple(&pool, f.a, f.admin).await;
    let err = sqlx::query(
        "INSERT INTO related_work (workspace_id, ticket_id, title, requested_by) VALUES ($1, $2, 'x', $3)",
    )
    .bind(f.a)
    .bind(t)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    let db_err = match err {
        Err(e) => e,
        Ok(_) => panic!("FK must refuse"),
    };
    let code = match &db_err {
        sqlx::Error::Database(d) => d.code().map(|c| c.into_owned()),
        _ => None,
    };
    assert_eq!(code.as_deref(), Some("23503"));
    let app_err = AppError::from(db_err);
    assert!(matches!(app_err, AppError::RuleViolation));
    let res = app_err.into_response();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(raw.contains("rule_violation"));
    for leak in ["fkey", "related_work", "requested_by", "23503", "violates"] {
        assert!(!raw.contains(leak), "{leak} leaked: {raw}");
    }
}

// ---------- contract shape ----------

/// Minimal structural check of a JSON value against the committed OpenAPI schema.
fn conforms(v: &Value, schema: &Value, root: &Value, at: &str) {
    if let Some(r) = schema["$ref"].as_str() {
        let name = r.trim_start_matches("#/components/schemas/");
        return conforms(v, &root["components"]["schemas"][name], root, at);
    }
    if let Some(alts) = schema["oneOf"].as_array() {
        if v.is_null() && alts.iter().any(|a| a["type"] == "null") {
            return;
        }
        let first = alts.iter().find(|a| a["type"] != "null");
        return conforms(v, first.unwrap_or(&Value::Null), root, at);
    }
    let types: Vec<&str> = match &schema["type"] {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    if v.is_null() {
        assert!(types.contains(&"null"), "{at}: null not allowed");
        return;
    }
    if let Some(e) = schema["enum"].as_array() {
        assert!(e.contains(v), "{at}: {v} not in enum");
    }
    match v {
        Value::Object(map) => {
            assert!(types.contains(&"object"), "{at}: unexpected object");
            let props = schema["properties"].as_object();
            for req in schema["required"].as_array().into_iter().flatten() {
                let k = req.as_str().unwrap_or_default();
                assert!(map.contains_key(k), "{at}: missing {k}");
            }
            for (k, x) in map {
                let p = props.and_then(|p| p.get(k));
                assert!(p.is_some(), "{at}: undocumented key {k}");
                conforms(x, p.unwrap_or(&Value::Null), root, &format!("{at}.{k}"));
            }
        }
        Value::Array(items) => {
            assert!(types.contains(&"array"), "{at}: unexpected array");
            for (i, x) in items.iter().enumerate() {
                conforms(x, &schema["items"], root, &format!("{at}[{i}]"));
            }
        }
        Value::String(_) => assert!(types.contains(&"string"), "{at}: unexpected string"),
        Value::Number(_) => assert!(
            types.contains(&"integer") || types.contains(&"number"),
            "{at}: unexpected number"
        ),
        Value::Bool(_) => assert!(types.contains(&"boolean"), "{at}: unexpected bool"),
        Value::Null => {}
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn responses_conform_to_openapi(pool: PgPool) {
    let f = fixture(&pool).await;
    let id = ticket(
        &pool,
        f.a,
        f.admin,
        Some(f.cs),
        "now()",
        Some("email"),
        Some("high"),
        Some("kategori_uji"),
    )
    .await;
    related(&pool, f.a, id, f.admin, true).await;
    let bare = ticket(&pool, f.a, f.admin, None, "now()", None, None, None).await;
    let r = simple(&pool, f.a, f.admin).await;
    lifecycle(&pool, f.a, r, &[(1, "resolve", "now()")], f.admin).await;
    let old = simple(&pool, f.a, f.admin).await;
    archive(&pool, old).await;
    let root: Value = ok(
        serde_json::from_str(include_str!("../../openapi/openapi.json")),
        "openapi",
    );
    let schemas = &root["components"]["schemas"];
    let t = tok(&pool, f.lead).await;
    let l = list(
        &pool,
        f.a,
        &format!("limit=2&resolvedFrom={}", enc("2026-01-01T00:00:00Z")),
        &t,
    )
    .await;
    conforms(&l, &schemas["TicketList"], &root, "list(resolved)");
    let l = list(&pool, f.a, "limit=2&q=dummy", &t).await;
    conforms(&l, &schemas["TicketList"], &root, "list");
    let all = list(&pool, f.a, "", &t).await;
    conforms(&all, &schemas["TicketList"], &root, "list(all)");
    let b = get(
        &pool,
        &format!("/api/v1/workspaces/{}/tickets/board?limit=1", f.a),
        &t,
    )
    .await;
    conforms(&b.json, &schemas["TicketBoard"], &root, "board");
    let m = post(
        &pool,
        &transition_uri(f.a, bare),
        &t,
        &json!({"to": "open", "version": 1}),
    )
    .await;
    conforms(
        &m.json,
        &schemas["TicketMutationResult"],
        &root,
        "transition",
    );
    let m = post(
        &pool,
        &assign_uri(f.a, bare),
        &t,
        &json!({"ownerUserId": f.lead, "version": 2}),
    )
    .await;
    conforms(&m.json, &schemas["TicketMutationResult"], &root, "assign");
    let e = post(
        &pool,
        &transition_uri(f.a, bare),
        &t,
        &json!({"to": "open", "version": 1}),
    )
    .await;
    conforms(&e.json, &schemas["Error"], &root, "error");
}
