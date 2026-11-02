//! C32 saved views (D06 rework) against a real PostgreSQL: create → reload (new session) →
//! apply, canonical storage without search text, user and workspace isolation (404),
//! refusals (unknown keys, `q`, paging keys, foreign IDs, names, CSRF), optimistic rename,
//! delete, the per-user cap, List/Board parity for an applied view, and that a stored view
//! grants nothing once access is gone. Role tests use DB fixtures and issued sessions (no
//! impersonation endpoint). Fixtures are synthetic and labelled dummy.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
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

/// Inserts a dummy ticket (status `new`) with the given channel/priority codes.
async fn ticket(
    pool: &PgPool,
    ws: Uuid,
    by: Uuid,
    owner: Option<Uuid>,
    channel: &str,
    priority: &str,
) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, \
               owner_user_id, intake_channel_id, priority_id) \
             VALUES ($1, 'Pengaduan uji (dummy)', 'Kronologi uji (dummy)', now(), $2, $3, \
               (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = $4), \
               (SELECT id FROM priorities WHERE workspace_id = $1 AND code = $5)) RETURNING id",
        )
        .bind(ws)
        .bind(by)
        .bind(owner)
        .bind(channel)
        .bind(priority)
        .fetch_one(pool)
        .await,
        "insert ticket",
    )
}

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    json: Value,
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

async fn send(
    pool: &PgPool,
    method: &str,
    uri: &str,
    t: &Tok,
    csrf: bool,
    body: Option<&Value>,
) -> Resp {
    let mut r = Request::builder().method(method).uri(uri).header(
        header::COOKIE,
        format!("{}={}", session::cookie_name(false), t.token),
    );
    if csrf {
        r = r.header(session::CSRF_HEADER, &t.csrf);
    }
    let req = match body {
        Some(b) => ok(
            r.header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            "request",
        ),
        None => ok(r.body(Body::empty()), "request"),
    };
    let res = match app(state(pool)).oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn views_uri(ws: Uuid) -> String {
    format!("/api/v1/workspaces/{ws}/saved-views")
}

fn view_uri(ws: Uuid, id: &str) -> String {
    format!("/api/v1/workspaces/{ws}/saved-views/{id}")
}

async fn list(pool: &PgPool, ws: Uuid, t: &Tok) -> Vec<Value> {
    let r = send(pool, "GET", &views_uri(ws), t, false, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json["items"].as_array().cloned().unwrap_or_default()
}

async fn create(pool: &PgPool, ws: Uuid, t: &Tok, body: Value) -> Resp {
    send(pool, "POST", &views_uri(ws), t, true, Some(&body)).await
}

fn names(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|v| v["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn s(v: &Value) -> String {
    match v.as_str() {
        Some(x) => x.to_owned(),
        None => panic!("not a string: {v}"),
    }
}

fn query_string(q: &Value) -> String {
    q.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}={}", s(v).replace(':', "%3A").replace('+', "%2B")))
                .collect::<Vec<_>>()
                .join("&")
        })
        .unwrap_or_default()
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn saved_view_persists_canonically_and_reloads(pool: PgPool) {
    let f = fixture(&pool).await;
    let email = code_id(&pool, "intake_channels", f.a, "email").await;
    let t = tok(&pool, f.admin).await;

    let r = create(
        &pool,
        f.a,
        &t,
        json!({
            "name": "  Email aktif  ",
            "view": "board",
            "query": {
                "status": "open,new,open",
                "channel": email.to_string(),
                "owner": "unassigned",
                "priority": "none",
                "receivedFrom": "2026-10-01T00:00:00+07:00",
                "sort": "number",
                "order": "asc"
            }
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["name"], "Email aktif", "name trimmed");
    assert_eq!(r.json["view"], "board");
    assert_eq!(r.json["version"], 1);
    let q = &r.json["query"];
    assert_eq!(q["status"], "open,new", "statuses de-duplicated");
    assert_eq!(q["channel"], email.to_string());
    assert_eq!(q["owner"], "unassigned");
    assert_eq!(q["priority"], "none");
    assert_eq!(q["sort"], "number");
    assert_eq!(q["order"], "asc");
    assert!(q.get("q").is_none());

    // Defaults are stored explicitly so a view always restores the same order.
    let d = create(&pool, f.a, &t, json!({ "name": "Semua", "view": "list" })).await;
    assert_eq!(d.status, StatusCode::CREATED, "{}", d.json);
    assert_eq!(
        d.json["query"],
        json!({ "sort": "updated_at", "order": "desc" })
    );

    // Reload: a new session of the same user sees both, ordered by name.
    let t2 = tok(&pool, f.admin).await;
    let items = list(&pool, f.a, &t2).await;
    assert_eq!(names(&items), vec!["Email aktif", "Semua"]);
    assert_eq!(items[0]["query"], r.json["query"]);

    // The row holds only preferences: no session token, CSRF token or search text.
    let raw: String = ok(
        sqlx::query_scalar("SELECT query::text || name || view FROM saved_views WHERE id = $1")
            .bind(Uuid::parse_str(&s(&r.json["id"])).ok())
            .fetch_one(&pool)
            .await,
        "row",
    );
    assert!(!raw.contains(&t.token) && !raw.contains(&t.csrf));
    assert!(!raw.contains("\"q\""));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn views_are_private_per_user_and_workspace(pool: PgPool) {
    let f = fixture(&pool).await;
    let admin = tok(&pool, f.admin).await;
    let cs = tok(&pool, f.cs).await;
    let lead = tok(&pool, f.lead).await;

    let mine = create(
        &pool,
        f.a,
        &cs,
        json!({ "name": "Antrian saya", "view": "list" }),
    )
    .await;
    assert_eq!(mine.status, StatusCode::CREATED, "{}", mine.json);
    let id = s(&mine.json["id"]);
    let theirs = create(
        &pool,
        f.a,
        &lead,
        json!({ "name": "Antrian saya", "view": "board" }),
    )
    .await;
    assert_eq!(
        theirs.status,
        StatusCode::CREATED,
        "same name, other user: {}",
        theirs.json
    );
    let in_b = create(
        &pool,
        f.b,
        &admin,
        json!({ "name": "Beta", "view": "list" }),
    )
    .await;
    assert_eq!(in_b.status, StatusCode::CREATED, "{}", in_b.json);

    assert_eq!(names(&list(&pool, f.a, &cs).await), vec!["Antrian saya"]);
    assert_eq!(list(&pool, f.a, &lead).await.len(), 1);
    assert_eq!(
        list(&pool, f.a, &admin).await.len(),
        0,
        "superadmin sees no one else's"
    );
    assert_eq!(names(&list(&pool, f.b, &admin).await), vec!["Beta"]);

    // Another user's view: 404 for rename and delete, nothing changes.
    let rename = json!({ "name": "Dibajak", "version": 1 });
    for t in [&lead, &admin] {
        let r = send(&pool, "PATCH", &view_uri(f.a, &id), t, true, Some(&rename)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
        let r = send(&pool, "DELETE", &view_uri(f.a, &id), t, true, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    }
    // Own view addressed through another workspace: 404.
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &s(&in_b.json["id"])),
        &admin,
        true,
        Some(&json!({ "name": "Pindah", "version": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    let r = send(
        &pool,
        "DELETE",
        &view_uri(f.a, &s(&in_b.json["id"])),
        &admin,
        true,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // A workspace the user is not a member of: 404 like any unknown workspace.
    let r = send(&pool, "GET", &views_uri(f.b), &cs, false, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = create(&pool, f.b, &cs, json!({ "name": "x", "view": "list" })).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let name: String = ok(
        sqlx::query_scalar("SELECT name FROM saved_views WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await,
        "name",
    );
    assert_eq!(name, "Antrian saya");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn invalid_saved_views_are_refused(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = tok(&pool, f.admin).await;
    let foreign = code_id(&pool, "intake_channels", f.b, "email").await;
    let long = "x".repeat(61);
    let bad = [
        json!({ "name": "a", "view": "list", "query": { "q": "Budi 0812" } }),
        json!({ "name": "a", "view": "list", "query": { "cursor": "abc" } }),
        json!({ "name": "a", "view": "list", "query": { "limit": "10" } }),
        json!({ "name": "a", "view": "list", "query": { "status": "lost" } }),
        json!({ "name": "a", "view": "list", "query": { "sort": "subject" } }),
        json!({ "name": "a", "view": "list", "query": { "channel": foreign.to_string() } }),
        json!({ "name": "a", "view": "list", "query": { "owner": Uuid::new_v4().to_string() } }),
        json!({ "name": "a", "view": "list", "query": { "receivedFrom": "2026-13-45T00:00:00Z" } }),
        json!({ "name": "a", "view": "list", "query": { "status": ["open"] } }),
        json!({ "name": "a", "view": "table" }),
        json!({ "name": "a", "view": "list", "shared": true }),
        json!({ "name": "   ", "view": "list" }),
        json!({ "name": long, "view": "list" }),
        json!({ "name": "a\u{0007}", "view": "list" }),
        json!({ "view": "list" }),
    ];
    for body in bad {
        let r = create(&pool, f.a, &t, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "invalid_request", "{body}");
    }
    assert_eq!(list(&pool, f.a, &t).await.len(), 0, "nothing stored");

    // CSRF is required for every write.
    let r = send(
        &pool,
        "POST",
        &views_uri(f.a),
        &t,
        false,
        Some(&json!({ "name": "a", "view": "list" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Duplicate names (case-insensitive) are a 422 rule, not a 500.
    assert_eq!(
        create(
            &pool,
            f.a,
            &t,
            json!({ "name": "Prioritas", "view": "list" })
        )
        .await
        .status,
        StatusCode::CREATED
    );
    let dup = create(
        &pool,
        f.a,
        &t,
        json!({ "name": "prioritas ", "view": "board" }),
    )
    .await;
    assert_eq!(dup.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", dup.json);
    assert_eq!(dup.json["error"]["code"], "rule_violation");

    // The per-user cap holds.
    ok(
        sqlx::query(
            "INSERT INTO saved_views (workspace_id, user_id, name, view, query) \
             SELECT $1, $2, 'Isi ' || g, 'list', '{}'::jsonb FROM generate_series(1, 49) g",
        )
        .bind(f.a)
        .bind(f.admin)
        .execute(&pool)
        .await,
        "fill",
    );
    let over = create(&pool, f.a, &t, json!({ "name": "Ke-51", "view": "list" })).await;
    assert_eq!(
        over.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        over.json
    );
    assert_eq!(list(&pool, f.a, &t).await.len(), 50);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn rename_is_optimistic_and_delete_removes(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = tok(&pool, f.cs).await;
    let a = create(&pool, f.a, &t, json!({ "name": "Satu", "view": "list" })).await;
    let b = create(&pool, f.a, &t, json!({ "name": "Dua", "view": "board" })).await;
    let (ida, idb) = (s(&a.json["id"]), s(&b.json["id"]));

    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &ida),
        &t,
        true,
        Some(&json!({ "name": " Satu baru ", "version": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["name"], "Satu baru");
    assert_eq!(r.json["version"], 2);
    assert_eq!(r.json["query"], a.json["query"], "rename keeps filters");

    // Stale version: 409, name unchanged.
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &ida),
        &t,
        true,
        Some(&json!({ "name": "Basi", "version": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // Rename onto another own name: 422.
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &idb),
        &t,
        true,
        Some(&json!({ "name": "SATU BARU", "version": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    // Unknown fields and missing CSRF are refused.
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &idb),
        &t,
        true,
        Some(&json!({ "name": "x", "version": 1, "view": "list" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(&pool, "DELETE", &view_uri(f.a, &idb), &t, false, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(names(&list(&pool, f.a, &t).await), vec!["Dua", "Satu baru"]);

    let r = send(&pool, "DELETE", &view_uri(f.a, &idb), &t, true, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(&pool, "DELETE", &view_uri(f.a, &idb), &t, true, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let reload = tok(&pool, f.cs).await;
    assert_eq!(names(&list(&pool, f.a, &reload).await), vec!["Satu baru"]);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn applied_view_gives_list_board_parity_and_grants_nothing(pool: PgPool) {
    let f = fixture(&pool).await;
    for (owner, ch, pr) in [
        (None, "email", "high"),
        (Some(f.cs), "email", "high"),
        (None, "email", "normal"),
        (None, "whatsapp", "high"),
    ] {
        ticket(&pool, f.a, f.admin, owner, ch, pr).await;
    }
    let email = code_id(&pool, "intake_channels", f.a, "email").await;
    let high = code_id(&pool, "priorities", f.a, "high").await;
    let t = tok(&pool, f.cs).await;
    let v = create(
        &pool,
        f.a,
        &t,
        json!({ "name": "Email high", "view": "board",
                "query": { "channel": email.to_string(), "priority": high.to_string() } }),
    )
    .await;
    assert_eq!(v.status, StatusCode::CREATED, "{}", v.json);

    // Applying = opening the List/Board with the stored parameters.
    let qs = query_string(&v.json["query"]);
    let l = send(
        &pool,
        "GET",
        &format!("/api/v1/workspaces/{}/tickets?{qs}", f.a),
        &t,
        false,
        None,
    )
    .await;
    assert_eq!(l.status, StatusCode::OK, "{}", l.json);
    assert_eq!(l.json["total"], 2);
    let b = send(
        &pool,
        "GET",
        &format!("/api/v1/workspaces/{}/tickets/board?{qs}", f.a),
        &t,
        false,
        None,
    )
    .await;
    assert_eq!(b.status, StatusCode::OK, "{}", b.json);
    let board_total: i64 = b.json["columns"]
        .as_array()
        .map(|c| c.iter().filter_map(|col| col["total"].as_i64()).sum())
        .unwrap_or(-1);
    assert_eq!(board_total, 2, "{}", b.json);
    assert_eq!(l.json["filters"], b.json["filters"], "same applied filters");

    // A stored view grants nothing: once the membership is gone, the view and the tickets
    // are both out of reach (404), even though the row still exists.
    ok(
        sqlx::query("UPDATE workspace_memberships SET active = false WHERE workspace_id = $1 AND user_id = $2")
            .bind(f.a)
            .bind(f.cs)
            .execute(&pool)
            .await,
        "deactivate",
    );
    let r = send(&pool, "GET", &views_uri(f.a), &t, false, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &pool,
        "GET",
        &format!("/api/v1/workspaces/{}/tickets?{qs}", f.a),
        &t,
        false,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // A stale ID inside a stored view is re-validated by the List (400), not trusted.
    ok(
        sqlx::query(
            "UPDATE saved_views SET query = jsonb_set(query, '{channel}', to_jsonb($1::text))",
        )
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await,
        "stale",
    );
    let admin = tok(&pool, f.admin).await;
    let stale: String = ok(
        sqlx::query_scalar("SELECT query->>'channel' FROM saved_views LIMIT 1")
            .fetch_one(&pool)
            .await,
        "stale id",
    );
    let r = send(
        &pool,
        "GET",
        &format!("/api/v1/workspaces/{}/tickets?channel={stale}", f.a),
        &admin,
        false,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn columns_are_allowlisted_persisted_and_private(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = tok(&pool, f.cs).await;
    let all = json!([
        "status", "priority", "owner", "channel", "received", "updated"
    ]);

    // No columns stored = the default (all optional columns).
    let d = create(&pool, f.a, &t, json!({ "name": "Default", "view": "list" })).await;
    assert_eq!(d.status, StatusCode::CREATED, "{}", d.json);
    assert_eq!(d.json["columns"], all);
    // Duplicates dropped, canonical order.
    let r = create(
        &pool,
        f.a,
        &t,
        json!({ "name": "Ringkas", "view": "list", "columns": ["updated", "status", "status"] }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["columns"], json!(["status", "updated"]));
    let id = s(&r.json["id"]);
    // Outside the allowlist (title/number are always shown), empty, or wrong type: 400.
    for bad in [
        json!(["title"]),
        json!(["number"]),
        json!([]),
        json!(["status", "secret"]),
        json!("status"),
        json!([1]),
    ] {
        let r = create(
            &pool,
            f.a,
            &t,
            json!({ "name": "Tolak", "view": "list", "columns": bad }),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.json);
    }
    assert_eq!(
        names(&list(&pool, f.a, &t).await),
        vec!["Default", "Ringkas"]
    );

    // Update columns only: optimistic, keeps name/query; stale = 409; nothing to change = 400.
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &id),
        &t,
        true,
        Some(&json!({ "columns": ["owner", "priority"], "version": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["columns"], json!(["priority", "owner"]));
    assert_eq!(r.json["name"], "Ringkas");
    assert_eq!(r.json["version"], 2);
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &id),
        &t,
        true,
        Some(&json!({ "columns": ["status"], "version": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &id),
        &t,
        true,
        Some(&json!({ "version": 2 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &id),
        &t,
        true,
        Some(&json!({ "columns": ["title"], "version": 2 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);

    // Reload in a new session: the stored choice is back.
    let reload = tok(&pool, f.cs).await;
    let items = list(&pool, f.a, &reload).await;
    let got = items.iter().find(|v| v["id"] == id.as_str()).cloned();
    assert_eq!(
        got.map(|v| v["columns"].clone()),
        Some(json!(["priority", "owner"]))
    );
    // Another user, or the same view through another workspace: 404, unchanged.
    let other = tok(&pool, f.lead).await;
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.a, &id),
        &other,
        true,
        Some(&json!({ "columns": ["status"], "version": 2 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let admin = tok(&pool, f.admin).await;
    let r = send(
        &pool,
        "PATCH",
        &view_uri(f.b, &id),
        &admin,
        true,
        Some(&json!({ "columns": ["status"], "version": 2 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let stored: Vec<String> = ok(
        sqlx::query_scalar("SELECT columns FROM saved_views WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await,
        "stored columns",
    );
    assert_eq!(stored, vec!["priority", "owner"]);
    // The database refuses columns outside the allowlist even without the API.
    let raw = sqlx::query("UPDATE saved_views SET columns = ARRAY['title'] WHERE id = $1::uuid")
        .bind(&id)
        .execute(&pool)
        .await;
    assert!(raw.is_err());
}
