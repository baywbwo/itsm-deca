//! D05 dashboard against a real PostgreSQL: every number is checked against an independent
//! query over the whole workspace (more tickets than one list page), the Jakarta period
//! bounds are half-open, resolve history survives a reopen, and the scope is isolated per
//! workspace for CS / Lead CS / UAT Superadmin. Fixtures are synthetic and labelled dummy.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::{AppState, app, seed, session};
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

/// Inserts a dummy ticket received at `received` (SQL expression) and moves it to `status`.
async fn ticket(
    pool: &PgPool,
    ws: Uuid,
    by: Uuid,
    owner: Option<Uuid>,
    received: &str,
    channel: Option<&str>,
    priority: &str,
) -> Uuid {
    let sql = format!(
        "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, owner_user_id, \
           intake_channel_id, priority_id) \
         VALUES ($1, 'Pengaduan uji (dummy)', 'Kronologi uji (dummy)', {received}, $2, $3, \
           (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = $4), \
           (SELECT id FROM priorities WHERE workspace_id = $1 AND code = $5)) RETURNING id"
    );
    ok(
        sqlx::query_scalar(&sql)
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

async fn resolve(pool: &PgPool, ws: Uuid, t: Uuid, at: &str, by: Uuid) {
    event(pool, ws, t, 1, "resolve", at, by).await;
    set_status(pool, t, "resolved", 1).await;
}

async fn count(pool: &PgPool, sql: &str, ws: Uuid) -> i64 {
    ok(sqlx::query_scalar(sql).bind(ws).fetch_one(pool).await, sql)
}

struct Resp {
    status: StatusCode,
    json: Value,
}

async fn get(pool: &PgPool, uri: &str, token: Option<&str>) -> Resp {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        b = b.header(
            header::COOKIE,
            format!("{}={t}", session::cookie_name(false)),
        );
    }
    let request = ok(b.body(Body::empty()), "request");
    let res = match app(state(pool)).oneshot(request).await {
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

async fn token_for(pool: &PgPool, uid: Uuid) -> String {
    ok(session::issue(pool, uid, "uat_superadmin").await, "issue").token
}

async fn dashboard(pool: &PgPool, ws: Uuid, period: &str, token: &str) -> Value {
    let r = get(
        pool,
        &format!("/api/v1/workspaces/{ws}/dashboard?period={period}"),
        Some(token),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{period}: {}", r.json);
    r.json
}

fn n(v: &Value) -> i64 {
    match v.as_i64() {
        Some(x) => x,
        None => panic!("not a number: {v}"),
    }
}

fn sum(v: &Value, field: &str) -> i64 {
    v.as_array()
        .map(|a| a.iter().map(|x| n(&x[field])).sum())
        .unwrap_or(-1)
}

const JKT_TODAY: &str =
    "(date_trunc('day', now() AT TIME ZONE 'Asia/Jakarta') AT TIME ZONE 'Asia/Jakarta')";

// ---------- numbers vs independent queries ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn numbers_cover_the_whole_scope_and_match_independent_queries(pool: PgPool) {
    let f = fixture(&pool).await;
    let channels = [Some("whatsapp"), Some("email"), Some("website"), None];
    let priorities = ["low", "normal", "high", "urgent"];
    // 64 tickets: more than one list page (50). Spread over owners, channels, ages, statuses.
    for i in 0..64_usize {
        let owner = match i % 3 {
            0 => None,
            1 => Some(f.cs),
            _ => Some(f.lead),
        };
        let received = format!("now() - interval '{} hours'", (i * 7) % 90);
        let t = ticket(
            &pool,
            f.a,
            f.admin,
            owner,
            &received,
            channels[i % 4],
            priorities[i % 4],
        )
        .await;
        match i % 6 {
            1 => set_status(&pool, t, "open", 1).await,
            2 => set_status(&pool, t, "pending_customer", 1).await,
            3 => set_status(&pool, t, "pending_engineering", 1).await,
            4 => resolve(&pool, f.a, t, "now() - interval '30 minutes'", f.admin).await,
            _ => {}
        }
    }
    // Other workspace: must never leak into DUMA numbers.
    for _ in 0..5 {
        let t = ticket(&pool, f.b, f.admin, None, "now()", Some("email"), "urgent").await;
        resolve(&pool, f.b, t, "now()", f.admin).await;
    }
    let token = token_for(&pool, f.cs).await;

    for period in ["today", "last_7_days", "this_month"] {
        let d = dashboard(&pool, f.a, period, &token).await;
        let t = &d["totals"];
        let active_sql =
            "workflow_status IN ('new','open','pending_customer','pending_engineering')";
        assert_eq!(n(&t["tickets"]), 64);
        assert_eq!(
            n(&t["tickets"]),
            count(
                &pool,
                "SELECT count(*) FROM tickets WHERE workspace_id = $1",
                f.a
            )
            .await
        );
        assert_eq!(
            n(&t["active"]),
            count(
                &pool,
                &format!("SELECT count(*) FROM tickets WHERE workspace_id = $1 AND {active_sql}"),
                f.a
            )
            .await
        );
        assert_eq!(
            n(&t["unassigned"]),
            count(
                &pool,
                &format!(
                    "SELECT count(*) FROM tickets WHERE workspace_id = $1 AND {active_sql} AND owner_user_id IS NULL"
                ),
                f.a
            )
            .await
        );
        assert_eq!(
            n(&t["pendingCustomer"]),
            count(
                &pool,
                "SELECT count(*) FROM tickets WHERE workspace_id = $1 AND workflow_status = 'pending_customer'",
                f.a
            )
            .await
        );
        let start = d["period"]["startAt"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let end = d["period"]["endAt"].as_str().unwrap_or_default().to_owned();
        let in_period =
            |col: &str| format!("{col} >= '{start}'::timestamptz AND {col} < '{end}'::timestamptz");
        assert_eq!(
            n(&t["receivedInPeriod"]),
            count(
                &pool,
                &format!(
                    "SELECT count(*) FROM tickets WHERE workspace_id = $1 AND {}",
                    in_period("received_at")
                ),
                f.a
            )
            .await
        );
        assert_eq!(
            n(&t["resolvedInPeriod"]),
            count(
                &pool,
                &format!(
                    "SELECT count(*) FROM ticket_resolution_events WHERE workspace_id = $1 AND action = 'resolve' AND {}",
                    in_period("occurred_at")
                ),
                f.a
            )
            .await
        );
        // Internal consistency: every breakdown adds up to its total.
        assert_eq!(sum(&d["statusDistribution"], "count"), n(&t["tickets"]));
        assert_eq!(sum(&d["picLoad"], "activeTickets"), n(&t["active"]));
        assert_eq!(sum(&d["age"], "count"), n(&t["active"]));
        assert_eq!(sum(&d["channels"], "count"), n(&t["receivedInPeriod"]));
        assert_eq!(sum(&d["trend"], "received"), n(&t["receivedInPeriod"]));
        assert_eq!(sum(&d["trend"], "resolved"), n(&t["resolvedInPeriod"]));
        let unassigned = d["picLoad"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["userId"].is_null()))
            .map(|p| n(&p["activeTickets"]));
        assert_eq!(unassigned, Some(n(&t["unassigned"])));
        // A channel-less ticket is reported, not dropped.
        assert!(
            d["channels"]
                .as_array()
                .is_some_and(|a| a.iter().any(|c| c["channelId"].is_null())),
            "{period}: unspecified channel row"
        );
        // Perlu perhatian: active high/urgent, total over the scope, list capped.
        let attention_total = count(
            &pool,
            &format!(
                "SELECT count(*) FROM tickets t JOIN priorities p ON p.id = t.priority_id \
                 WHERE t.workspace_id = $1 AND t.{active_sql} AND p.code IN ('high', 'urgent')"
            ),
            f.a,
        )
        .await;
        assert_eq!(n(&d["attention"]["total"]), attention_total);
        let items = d["attention"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(items.len() as i64, attention_total.min(10));
        assert_eq!(items[0]["priority"]["code"], "urgent", "urgent first");
        assert!(items.iter().all(|i| {
            i["displayNumber"]
                .as_str()
                .is_some_and(|s| s.starts_with("DUMA-"))
        }));
        assert_eq!(d["slaConfigured"], false);
        assert_eq!(d["period"]["basis"], "received");
        assert_eq!(d["period"]["timezone"], "Asia/Jakarta");
    }
}

// ---------- period bounds ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn jakarta_period_bounds_are_half_open_and_reopen_keeps_resolution(pool: PgPool) {
    let f = fixture(&pool).await;
    let start = JKT_TODAY;
    let end = format!("({JKT_TODAY} + interval '1 day')");
    // Just before / at the start, just before / at the end of today (Jakarta).
    for at in [
        format!("{start} - interval '1 microsecond'"),
        start.to_owned(),
        format!("{end} - interval '1 microsecond'"),
        end.clone(),
    ] {
        ticket(&pool, f.a, f.admin, None, &at, Some("email"), "normal").await;
    }
    // Resolve events on both sides of the start.
    let before = ticket(
        &pool,
        f.a,
        f.admin,
        None,
        "now() - interval '3 days'",
        None,
        "low",
    )
    .await;
    resolve(
        &pool,
        f.a,
        before,
        &format!("{start} - interval '1 second'"),
        f.admin,
    )
    .await;
    let at_start = ticket(
        &pool,
        f.a,
        f.admin,
        None,
        "now() - interval '3 days'",
        None,
        "low",
    )
    .await;
    resolve(&pool, f.a, at_start, start, f.admin).await;
    // Resolved today, then reopened: the resolution still counts, the ticket is active again.
    event(&pool, f.a, at_start, 2, "reopen", "now()", f.admin).await;
    set_status(&pool, at_start, "open", 2).await;

    let token = token_for(&pool, f.lead).await;
    let d = dashboard(&pool, f.a, "today", &token).await;
    assert_eq!(
        n(&d["totals"]["receivedInPeriod"]),
        2,
        "start and end-1µs only"
    );
    assert_eq!(n(&d["totals"]["resolvedInPeriod"]), 1);
    assert_eq!(n(&d["totals"]["resolvedTicketsInPeriod"]), 1);
    assert_eq!(d["trend"].as_array().map(Vec::len), Some(1));
    let reopened_open = d["statusDistribution"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["status"]["code"] == "open"))
        .map(|s| n(&s["count"]));
    assert_eq!(reopened_open, Some(1));

    let today: String = ok(
        sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Asia/Jakarta')::date::text")
            .fetch_one(&pool)
            .await,
        "today",
    );
    assert_eq!(d["period"]["startDate"], today.as_str());
    assert_eq!(d["period"]["endDate"], today.as_str());
    // 00:00 Jakarta is 17:00 UTC the day before.
    assert!(
        d["period"]["startAt"]
            .as_str()
            .is_some_and(|s| s.ends_with("T17:00:00.000000Z"))
    );
    assert_eq!(
        d["drilldown"]["receivedInPeriod"]["receivedFrom"],
        d["period"]["startAt"]
    );
    assert_eq!(
        d["drilldown"]["resolvedInPeriod"]["resolvedBefore"],
        d["period"]["endAt"]
    );

    let week = dashboard(&pool, f.a, "last_7_days", &token).await;
    let week_start: String = ok(
        sqlx::query_scalar("SELECT ((now() AT TIME ZONE 'Asia/Jakarta')::date - 6)::text")
            .fetch_one(&pool)
            .await,
        "week start",
    );
    assert_eq!(week["period"]["startDate"], week_start.as_str());
    assert_eq!(week["period"]["endDate"], today.as_str());
    assert_eq!(week["trend"].as_array().map(Vec::len), Some(7));
    assert_eq!(n(&week["totals"]["resolvedInPeriod"]), 2);

    let month = dashboard(&pool, f.a, "this_month", &token).await;
    let (first, last): (String, String) = ok(
        sqlx::query_as(
            "SELECT date_trunc('month', (now() AT TIME ZONE 'Asia/Jakarta'))::date::text, \
                    (date_trunc('month', (now() AT TIME ZONE 'Asia/Jakarta')) + interval '1 month - 1 day')::date::text",
        )
        .fetch_one(&pool)
        .await,
        "month",
    );
    assert_eq!(month["period"]["startDate"], first.as_str());
    assert_eq!(month["period"]["endDate"], last.as_str());
}

// ---------- scope / access ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn scope_is_isolated_per_workspace_and_role(pool: PgPool) {
    let f = fixture(&pool).await;
    for _ in 0..3 {
        ticket(
            &pool,
            f.a,
            f.admin,
            Some(f.cs),
            "now()",
            Some("email"),
            "high",
        )
        .await;
    }
    for _ in 0..2 {
        ticket(
            &pool,
            f.b,
            f.admin,
            None,
            "now()",
            Some("website"),
            "urgent",
        )
        .await;
    }
    let cs = token_for(&pool, f.cs).await;
    let lead = token_for(&pool, f.lead).await;
    let admin = token_for(&pool, f.admin).await;
    let (a, b) = (f.a, f.b);

    // CS and Lead CS see the same whole-workspace numbers (no per-owner narrowing).
    let as_cs = dashboard(&pool, a, "today", &cs).await;
    let as_lead = dashboard(&pool, a, "today", &lead).await;
    assert_eq!(as_cs["totals"], as_lead["totals"]);
    assert_eq!(as_cs["picLoad"], as_lead["picLoad"]);
    assert_eq!(n(&as_cs["totals"]["tickets"]), 3);
    assert_eq!(as_cs["workspaceCode"], "DUMA");

    // UAT Superadmin reads DUMB; DUMB numbers contain only DUMB tickets.
    let other = dashboard(&pool, b, "today", &admin).await;
    assert_eq!(n(&other["totals"]["tickets"]), 2);
    assert_eq!(n(&other["totals"]["unassigned"]), 2);
    assert_eq!(other["workspaceCode"], "DUMB");
    assert!(
        other["picLoad"]
            .as_array()
            .is_some_and(|p| p.len() == 1 && p[0]["userId"].is_null()),
        "DUMB has no members: only the unassigned row"
    );

    // Members of DUMA cannot see DUMB at all: same 404 as unknown/malformed IDs.
    let unknown = Uuid::new_v4();
    for (uri, token) in [
        (format!("/api/v1/workspaces/{b}/dashboard"), &cs),
        (format!("/api/v1/workspaces/{b}/dashboard"), &lead),
        (format!("/api/v1/workspaces/{unknown}/dashboard"), &cs),
        ("/api/v1/workspaces/not-a-uuid/dashboard".to_owned(), &cs),
    ] {
        let r = get(&pool, &uri, Some(token)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(r.json["error"]["code"], "not_found");
    }
    let r = get(&pool, &format!("/api/v1/workspaces/{a}/dashboard"), None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    for bad in ["7d", "TODAY", "", "this_month%20"] {
        let r = get(
            &pool,
            &format!("/api/v1/workspaces/{a}/dashboard?period={bad}"),
            Some(&cs),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert_eq!(r.json["error"]["code"], "invalid_request");
    }
    // Default period is today.
    let r = get(
        &pool,
        &format!("/api/v1/workspaces/{a}/dashboard"),
        Some(&cs),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["period"]["key"], "today");

    // A deactivated membership loses the dashboard on the next request.
    ok(
        sqlx::query(
            "UPDATE workspace_memberships SET active = false, version = version + 1 \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(a)
        .bind(f.lead)
        .execute(&pool)
        .await,
        "deactivate",
    );
    let r = get(
        &pool,
        &format!("/api/v1/workspaces/{a}/dashboard"),
        Some(&lead),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// ---------- UAT dummy seed ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn uat_ticket_seed_is_labelled_once_only_and_needs_the_marker(pool: PgPool) {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await,
        "db name",
    );
    // Without the dummy marker (seed_uat not run) the ticket seed refuses.
    assert!(seed::seed_uat_tickets(&pool, &name).await.is_err());
    let f = fixture(&pool).await;
    assert!(seed::seed_uat_tickets(&pool, "other_db").await.is_err());
    let first = ok(seed::seed_uat_tickets(&pool, &name).await, "seed tickets");
    assert_eq!(first, 17);
    assert_eq!(
        ok(seed::seed_uat_tickets(&pool, &name).await, "again"),
        0,
        "idempotent"
    );
    let unlabelled: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets WHERE subject NOT LIKE '%(dummy)'")
            .fetch_one(&pool)
            .await,
        "labels",
    );
    assert_eq!(unlabelled, 0);
    let token = token_for(&pool, f.cs).await;
    let d = dashboard(&pool, f.a, "this_month", &token).await;
    assert_eq!(n(&d["totals"]["tickets"]), 14);
    assert_eq!(n(&d["totals"]["unassigned"]), 4);
    // The reopened ticket keeps its resolution; three resolved tickets plus it = 4 events.
    let events = count(
        &pool,
        "SELECT count(*) FROM ticket_resolution_events WHERE workspace_id = $1 AND action = 'resolve'",
        f.a,
    )
    .await;
    assert_eq!(events, 4);
}

// ---------- contract shape ----------

/// Minimal structural check of a JSON value against the committed OpenAPI schema: required
/// keys present, no undocumented keys, enums respected, nullability as documented.
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
async fn response_matches_the_openapi_schema(pool: PgPool) {
    let spec: Value = ok(
        serde_json::from_str(include_str!("../../openapi/openapi.json")),
        "openapi",
    );
    let f = fixture(&pool).await;
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat_tickets(&pool, &name).await, "seed tickets");
    // An inactive member still holding active tickets exercises the rare PIC row shape.
    ok(
        sqlx::query(
            "UPDATE workspace_memberships SET active = false, version = version + 1 \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(f.a)
        .bind(f.lead)
        .execute(&pool)
        .await,
        "deactivate lead",
    );
    let token = token_for(&pool, f.admin).await;
    let schema = serde_json::json!({ "$ref": "#/components/schemas/Dashboard" });
    for (ws, period) in [(f.a, "today"), (f.a, "this_month"), (f.b, "last_7_days")] {
        let d = dashboard(&pool, ws, period, &token).await;
        conforms(&d, &schema, &spec, "dashboard");
        if ws == f.a {
            let inactive = d["picLoad"].as_array().and_then(|p| {
                p.iter()
                    .find(|r| r["userId"] == serde_json::json!(f.lead.to_string()))
            });
            assert_eq!(
                inactive.map(|r| (r["memberActive"].clone(), n(&r["activeTickets"]) > 0)),
                Some((Value::Bool(false), true)),
                "inactive PIC still holding tickets stays visible"
            );
        }
    }
    let r = get(
        &pool,
        &format!("/api/v1/workspaces/{}/dashboard?period=x", f.a),
        Some(&token),
    )
    .await;
    conforms(
        &r.json,
        &serde_json::json!({ "$ref": "#/components/schemas/Error" }),
        &spec,
        "error",
    );
}
