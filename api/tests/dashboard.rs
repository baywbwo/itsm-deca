//! D05 dashboard against a real PostgreSQL: every number is checked against an independent
//! query over the whole workspace (more tickets than one list page), the Jakarta period
//! bounds are half-open, resolve history survives a reopen, and the scope is isolated per
//! workspace for CS / Lead CS / UAT Superadmin. Shared filters (PIC, kanal, kategori,
//! prioritas) are checked number by number against independent SQL. Period-boundary tests
//! pin the evaluation instant instead of reading the wall clock. Fixtures are synthetic and
//! labelled dummy.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::routes::dashboard::{self as dash, Dim, Filters, PeriodKey};
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

/// Inserts a dummy ticket (status `new`) received at `received` (SQL expression).
async fn ticket(
    pool: &PgPool,
    ws: Uuid,
    by: Uuid,
    owner: Option<Uuid>,
    received: &str,
    channel: Option<&str>,
    priority: &str,
) -> Uuid {
    ticket_in(pool, ws, by, owner, received, channel, Some(priority), None).await
}

/// Same as `ticket`, with an optional priority and a category code.
#[allow(clippy::too_many_arguments)]
async fn ticket_in(
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

/// Records lifecycle events in order, moving the ticket like the workflow does (the DB only
/// accepts a resolve for the open cycle and a reopen of a resolved ticket into cycle + 1).
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

/// Dashboard at a pinned instant (no wall clock), as JSON.
async fn at(pool: &PgPool, ws: Uuid, key: PeriodKey, filters: Filters, instant: &str) -> Value {
    match dash::load(pool, ws, "DUMA", key, filters, Some(instant)).await {
        Ok(d) => ok(serde_json::to_value(d), "json"),
        Err(e) => panic!("load at {instant}: {e:?}"),
    }
}

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
        assert_eq!(d["metricBasis"]["active"], "current_status");
        assert_eq!(d["metricBasis"]["picLoad"], "current_status");
        assert_eq!(d["metricBasis"]["receivedInPeriod"], "received_at");
        assert_eq!(d["metricBasis"]["resolvedInPeriod"], "resolution_event");
        assert_eq!(d["attention"]["rule"], "operational_rule");
        assert_eq!(d["period"]["timezone"], "Asia/Jakarta");
    }
}

// ---------- period bounds (pinned instants, no wall clock) ----------

const AT1: &str = "2026-10-14T16:59:59.999999Z"; // 14 Oct 23:59:59.999999 Jakarta
const AT2: &str = "2026-10-14T17:00:00Z"; // 15 Oct 00:00 Jakarta

fn ts(s: &str) -> String {
    format!("'{s}'::timestamptz")
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn period_bounds_resolution_events_and_reopen_at_pinned_instants(pool: PgPool) {
    let f = fixture(&pool).await;
    let any = Filters::default();
    // Received just before / at the start, just before / at the end of 14 Oct (Jakarta).
    for r in [
        "2026-10-13T16:59:59.999999Z",
        "2026-10-13T17:00:00Z",
        "2026-10-14T16:59:59.999999Z",
        "2026-10-14T17:00:00Z",
    ] {
        ticket(&pool, f.a, f.admin, None, &ts(r), Some("email"), "normal").await;
    }
    // Received yesterday (13 Oct Jakarta), resolved today: counts as resolved TODAY.
    let y = ticket(
        &pool,
        f.a,
        f.admin,
        Some(f.cs),
        &ts("2026-10-13T05:00:00Z"),
        Some("whatsapp"),
        "high",
    )
    .await;
    resolve(&pool, f.a, y, &ts("2026-10-14T02:00:00Z"), f.admin).await;
    // Multi-cycle: resolved yesterday, reopened, resolved twice today, reopened again.
    let m = ticket(
        &pool,
        f.a,
        f.admin,
        Some(f.lead),
        &ts("2026-10-01T03:00:00Z"),
        Some("website"),
        "urgent",
    )
    .await;
    let steps: Vec<(i32, &str, String)> = [
        (1, "resolve", "2026-10-13T16:59:59.999999Z"),
        (2, "reopen", "2026-10-13T20:00:00Z"),
        (2, "resolve", "2026-10-14T01:00:00Z"),
        (3, "reopen", "2026-10-14T03:00:00Z"),
        (3, "resolve", "2026-10-14T05:00:00Z"),
        (4, "reopen", "2026-10-14T06:00:00Z"),
    ]
    .into_iter()
    .map(|(c, a, w)| (c, a, ts(w)))
    .collect();
    let steps: Vec<(i32, &str, &str)> =
        steps.iter().map(|(c, a, w)| (*c, *a, w.as_str())).collect();
    lifecycle(&pool, f.a, m, &steps, f.admin).await;
    // Resolved exactly at the start of 14 Oct (inclusive).
    let s = ticket(
        &pool,
        f.a,
        f.admin,
        None,
        &ts("2026-10-01T04:00:00Z"),
        None,
        "low",
    )
    .await;
    resolve(&pool, f.a, s, &ts("2026-10-13T17:00:00Z"), f.admin).await;

    let d = at(&pool, f.a, PeriodKey::Today, any, AT1).await;
    assert_eq!(d["asOf"], "2026-10-14T16:59:59.999999Z");
    assert_eq!(d["period"]["startDate"], "2026-10-14");
    assert_eq!(d["period"]["endDate"], "2026-10-14");
    assert_eq!(d["period"]["startAt"], "2026-10-13T17:00:00.000000Z");
    assert_eq!(d["period"]["endAt"], "2026-10-14T17:00:00.000000Z");
    let t = &d["totals"];
    assert_eq!(n(&t["receivedInPeriod"]), 2, "start and end-1µs only");
    // y (received yesterday) + m cycles 2 and 3 + s at the start; m cycle 1 is yesterday.
    assert_eq!(n(&t["resolvedInPeriod"]), 4, "resolution events (cycles)");
    assert_eq!(n(&t["resolvedTicketsInPeriod"]), 3, "distinct tickets");
    assert_eq!(d["trend"].as_array().map(Vec::len), Some(1));
    assert_eq!(n(&d["trend"][0]["received"]), 2);
    assert_eq!(n(&d["trend"][0]["resolved"]), 4);
    // Reopened m is active again and stays on its PIC's load although received on 1 Oct.
    let pic = |d: &Value, user: Uuid| {
        d["picLoad"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["userId"] == user.to_string()))
            .map(|p| n(&p["activeTickets"]))
    };
    assert_eq!(pic(&d, f.lead), Some(1));
    assert_eq!(pic(&d, f.cs), Some(0), "y is resolved");
    assert_eq!(sum(&d["picLoad"], "activeTickets"), n(&t["active"]));
    assert_eq!(
        d["drilldown"]["receivedInPeriod"]["receivedFrom"],
        d["period"]["startAt"]
    );
    assert_eq!(
        d["drilldown"]["resolvedInPeriod"]["resolvedBefore"],
        d["period"]["endAt"]
    );

    // One microsecond later it is 15 Oct in Jakarta.
    let next = at(&pool, f.a, PeriodKey::Today, any, AT2).await;
    assert_eq!(next["asOf"], "2026-10-14T17:00:00.000000Z");
    assert_eq!(next["period"]["startDate"], "2026-10-15");
    assert_eq!(n(&next["totals"]["receivedInPeriod"]), 1);
    assert_eq!(n(&next["totals"]["resolvedInPeriod"]), 0);
    assert_eq!(pic(&next, f.lead), Some(1), "PIC load is not period-bound");

    let week = at(&pool, f.a, PeriodKey::Last7Days, any, AT1).await;
    assert_eq!(week["period"]["startDate"], "2026-10-08");
    assert_eq!(week["period"]["endDate"], "2026-10-14");
    assert_eq!(week["trend"].as_array().map(Vec::len), Some(7));
    assert_eq!(n(&week["totals"]["receivedInPeriod"]), 4, "r1..r3 + y");
    assert_eq!(n(&week["totals"]["resolvedInPeriod"]), 5, "m cycle 1 joins");
    assert_eq!(n(&week["totals"]["resolvedTicketsInPeriod"]), 3);

    // PIC filter: y's resolution follows y's owner; the unassigned row/drilldown go away.
    let only_cs = Filters {
        owner: Dim::Id(f.cs),
        ..Filters::default()
    };
    let c = at(&pool, f.a, PeriodKey::Today, only_cs, AT1).await;
    assert_eq!(n(&c["totals"]["resolvedInPeriod"]), 1);
    assert_eq!(n(&c["totals"]["receivedInPeriod"]), 0);
    assert_eq!(c["picLoad"].as_array().map(Vec::len), Some(1));
    assert_eq!(c["picLoad"][0]["userId"], f.cs.to_string());
    assert!(c["drilldown"]["unassigned"].is_null());
    assert_eq!(c["filters"]["owner"], f.cs.to_string());
    assert_eq!(
        c["drilldown"]["resolvedInPeriod"]["owner"],
        f.cs.to_string()
    );

    // Month boundary: 30 Sep 23:59:59.999999 vs 1 Oct 00:00 Jakarta.
    let sep = at(
        &pool,
        f.a,
        PeriodKey::ThisMonth,
        any,
        "2026-09-30T16:59:59.999999Z",
    )
    .await;
    assert_eq!(sep["period"]["startDate"], "2026-09-01");
    assert_eq!(sep["period"]["endDate"], "2026-09-30");
    assert_eq!(sep["period"]["startAt"], "2026-08-31T17:00:00.000000Z");
    assert_eq!(sep["period"]["endAt"], "2026-09-30T17:00:00.000000Z");
    assert_eq!(sep["trend"].as_array().map(Vec::len), Some(30));
    let oct = at(
        &pool,
        f.a,
        PeriodKey::ThisMonth,
        any,
        "2026-09-30T17:00:00Z",
    )
    .await;
    assert_eq!(oct["period"]["startDate"], "2026-10-01");
    assert_eq!(oct["period"]["endDate"], "2026-10-31");
    assert_eq!(oct["period"]["endAt"], "2026-10-31T17:00:00.000000Z");
    assert_eq!(oct["trend"].as_array().map(Vec::len), Some(31));
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

// ---------- shared filters vs independent SQL ----------

async fn catalog_id(pool: &PgPool, table: &str, ws: Uuid, code: &str) -> Uuid {
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

async fn filtered(pool: &PgPool, ws: Uuid, query: &str, token: &str) -> Resp {
    get(
        pool,
        &format!("/api/v1/workspaces/{ws}/dashboard?{query}"),
        Some(token),
    )
    .await
}

fn row_for<'a>(rows: &'a Value, key: &str, id: Option<String>) -> Option<&'a Value> {
    rows.as_array().and_then(|a| {
        a.iter().find(|r| match &id {
            Some(id) => r[key] == id.as_str(),
            None => r[key].is_null(),
        })
    })
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn combined_filters_narrow_every_number_like_independent_sql(pool: PgPool) {
    let f = fixture(&pool).await;
    let channels = [Some("whatsapp"), Some("email"), Some("website"), None];
    let categories = [Some("kategori_uji"), Some("kategori_uji_b"), None];
    let priorities = [
        Some("low"),
        Some("normal"),
        Some("high"),
        Some("urgent"),
        None,
    ];
    // 90 tickets, received up to ~16 days ago (many outside today/this week), spread over
    // every owner/channel/category/priority combination; some resolved recently after being
    // received long ago, some resolved twice (reopen in between), some reopened.
    for i in 0..90_usize {
        let owner = match i % 3 {
            0 => None,
            1 => Some(f.cs),
            _ => Some(f.lead),
        };
        let received = format!("now() - interval '{} hours'", (i * 13) % 400);
        let t = ticket_in(
            &pool,
            f.a,
            f.admin,
            owner,
            &received,
            channels[i % 4],
            priorities[i % 5],
            categories[(i / 3) % 3],
        )
        .await;
        match i % 6 {
            1 => set_status(&pool, t, "open", 1).await,
            2 => set_status(&pool, t, "pending_customer", 1).await,
            3 => set_status(&pool, t, "pending_engineering", 1).await,
            4 => {
                resolve(&pool, f.a, t, "now() - interval '20 minutes'", f.admin).await;
                if i % 12 == 10 {
                    // Second cycle: reopen, resolve again.
                    let steps = [
                        (2, "reopen", "now() - interval '15 minutes'"),
                        (2, "resolve", "now() - interval '10 minutes'"),
                    ];
                    lifecycle(&pool, f.a, t, &steps, f.admin).await;
                }
            }
            5 if i % 4 == 1 => {
                let steps = [
                    (1, "resolve", "now() - interval '5 minutes'"),
                    (2, "reopen", "now() - interval '1 minute'"),
                ];
                lifecycle(&pool, f.a, t, &steps, f.admin).await;
            }
            _ => {}
        }
    }
    // Other workspace noise, same codes but different catalog rows.
    for _ in 0..4 {
        let t = ticket_in(
            &pool,
            f.b,
            f.admin,
            None,
            "now()",
            Some("email"),
            Some("urgent"),
            Some("kategori_uji_b"),
        )
        .await;
        resolve(&pool, f.b, t, "now()", f.admin).await;
    }
    let token = token_for(&pool, f.lead).await;
    let ch = |c| catalog_id(&pool, "intake_channels", f.a, c);
    let (wa, email) = (ch("whatsapp").await, ch("email").await);
    let kat = catalog_id(&pool, "categories", f.a, "kategori_uji").await;
    let katb = catalog_id(&pool, "categories", f.a, "kategori_uji_b").await;
    let pr = |c| catalog_id(&pool, "priorities", f.a, c);
    let (high, urgent, low) = (pr("high").await, pr("urgent").await, pr("low").await);
    let (cs, lead) = (f.cs, f.lead);

    // (query, independent WHERE over tickets t)
    let combos: Vec<(String, String)> = vec![
        (String::new(), "true".to_owned()),
        (format!("owner={cs}"), format!("t.owner_user_id = '{cs}'")),
        (
            "owner=unassigned&channel=none".to_owned(),
            "t.owner_user_id IS NULL AND t.intake_channel_id IS NULL".to_owned(),
        ),
        (
            format!("category={katb}&priority={urgent}"),
            format!("t.category_id = '{katb}' AND t.priority_id = '{urgent}'"),
        ),
        (
            format!("owner={lead}&channel={email}&category=none&priority={high}"),
            format!(
                "t.owner_user_id = '{lead}' AND t.intake_channel_id = '{email}' \
                 AND t.category_id IS NULL AND t.priority_id = '{high}'"
            ),
        ),
        (
            format!("owner=unassigned&channel={wa}&category={kat}&priority=none"),
            format!(
                "t.owner_user_id IS NULL AND t.intake_channel_id = '{wa}' \
                 AND t.category_id = '{kat}' AND t.priority_id IS NULL"
            ),
        ),
        (
            "priority=none".to_owned(),
            "t.priority_id IS NULL".to_owned(),
        ),
        (
            format!("priority={low}"),
            format!("t.priority_id = '{low}'"),
        ),
    ];
    let active = "t.workflow_status IN ('new','open','pending_customer','pending_engineering')";
    let mut nonzero_resolved = 0;
    for (query, cond) in &combos {
        for period in ["today", "last_7_days", "this_month"] {
            let q = if query.is_empty() {
                format!("period={period}")
            } else {
                format!("period={period}&{query}")
            };
            let r = filtered(&pool, f.a, &q, &token).await;
            assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.json);
            let d = &r.json;
            let t = &d["totals"];
            let start = d["period"]["startAt"].as_str().unwrap_or_default();
            let end = d["period"]["endAt"].as_str().unwrap_or_default();
            let w = format!("t.workspace_id = $1 AND {cond}");
            let inp = |col: &str| {
                format!("{col} >= '{start}'::timestamptz AND {col} < '{end}'::timestamptz")
            };
            let c = |extra: String| {
                let sql = format!("SELECT count(*) FROM tickets t WHERE {w} AND {extra}");
                let pool = pool.clone();
                async move { count(&pool, &sql, f.a).await }
            };
            let ev = |distinct: bool| {
                let what = if distinct {
                    "count(DISTINCT e.ticket_id)"
                } else {
                    "count(*)"
                };
                let sql = format!(
                    "SELECT {what} FROM ticket_resolution_events e JOIN tickets t ON t.id = e.ticket_id \
                     WHERE {w} AND e.action = 'resolve' AND {}",
                    inp("e.occurred_at")
                );
                let pool = pool.clone();
                async move { count(&pool, &sql, f.a).await }
            };
            let ctx = q.to_string();
            assert_eq!(n(&t["tickets"]), c("true".into()).await, "{ctx} tickets");
            assert_eq!(n(&t["active"]), c(active.into()).await, "{ctx} active");
            assert_eq!(
                n(&t["unassigned"]),
                c(format!("{active} AND t.owner_user_id IS NULL")).await,
                "{ctx} unassigned"
            );
            assert_eq!(
                n(&t["pendingCustomer"]),
                c("t.workflow_status = 'pending_customer'".into()).await,
                "{ctx} pending customer"
            );
            let received = c(inp("t.received_at")).await;
            assert_eq!(n(&t["receivedInPeriod"]), received, "{ctx} received");
            let resolved = ev(false).await;
            assert_eq!(n(&t["resolvedInPeriod"]), resolved, "{ctx} resolved");
            assert_eq!(
                n(&t["resolvedTicketsInPeriod"]),
                ev(true).await,
                "{ctx} resolved tickets"
            );
            if resolved > 0 {
                nonzero_resolved += 1;
            }
            for s in d["statusDistribution"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                let code = s["status"]["code"].as_str().unwrap_or_default().to_owned();
                assert_eq!(
                    n(&s["count"]),
                    c(format!("t.workflow_status = '{code}'")).await,
                    "{ctx} status {code}"
                );
            }
            assert_eq!(sum(&d["statusDistribution"], "count"), n(&t["tickets"]));
            // PIC load: every active ticket in scope, whatever its received_at.
            for p in d["picLoad"].as_array().cloned().unwrap_or_default() {
                let owner = match p["userId"].as_str() {
                    Some(u) => format!("t.owner_user_id = '{u}'"),
                    None => "t.owner_user_id IS NULL".to_owned(),
                };
                assert_eq!(
                    n(&p["activeTickets"]),
                    c(format!("{active} AND {owner}")).await,
                    "{ctx} pic {owner}"
                );
            }
            assert_eq!(
                sum(&d["picLoad"], "activeTickets"),
                n(&t["active"]),
                "{ctx}"
            );
            let person =
                query.contains(&format!("owner={cs}")) || query.contains(&format!("owner={lead}"));
            assert_eq!(
                row_for(&d["picLoad"], "userId", None).is_some(),
                !person,
                "{ctx} unassigned row"
            );
            assert_eq!(d["drilldown"]["unassigned"].is_null(), person, "{ctx}");
            for (key, col) in [
                ("channels", "intake_channel_id"),
                ("categories", "category_id"),
            ] {
                for row in d[key].as_array().cloned().unwrap_or_default() {
                    let is = match row["id"].as_str() {
                        Some(id) => format!("t.{col} = '{id}'"),
                        None => format!("t.{col} IS NULL"),
                    };
                    assert_eq!(
                        n(&row["count"]),
                        c(format!("{} AND {is}", inp("t.received_at"))).await,
                        "{ctx} {key} {is}"
                    );
                }
                assert_eq!(sum(&d[key], "count"), received, "{ctx} {key} sum");
            }
            assert_eq!(sum(&d["trend"], "received"), received, "{ctx}");
            assert_eq!(sum(&d["trend"], "resolved"), resolved, "{ctx}");
            let attention = count(
                &pool,
                &format!(
                    "SELECT count(*) FROM tickets t JOIN priorities p ON p.id = t.priority_id \
                     WHERE {w} AND {active} AND p.code IN ('high', 'urgent')"
                ),
                f.a,
            )
            .await;
            assert_eq!(n(&d["attention"]["total"]), attention, "{ctx} attention");
            assert_eq!(
                d["attention"]["items"].as_array().map(|a| a.len() as i64),
                Some(attention.min(10))
            );
            // Filters are echoed and travel with every drilldown.
            for (param, key) in [
                ("owner", "owner"),
                ("channel", "channel"),
                ("category", "category"),
            ] {
                let want = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{param}=")));
                assert_eq!(d["filters"][key].as_str(), want, "{ctx} echo {param}");
                assert_eq!(
                    d["drilldown"]["receivedInPeriod"][key].as_str(),
                    want,
                    "{ctx} drilldown {param}"
                );
                assert_eq!(d["drilldown"]["active"][key].as_str(), want, "{ctx}");
            }
        }
    }
    assert!(nonzero_resolved > 5, "fixture must exercise resolutions");

    // Priority outside urgent/high: attention list and its drilldown are empty/absent.
    let r = filtered(&pool, f.a, &format!("priority={low}"), &token).await;
    assert_eq!(n(&r.json["attention"]["total"]), 0);
    assert!(r.json["drilldown"]["attention"].is_null());
    let r = filtered(&pool, f.a, &format!("priority={urgent}"), &token).await;
    assert_eq!(
        r.json["drilldown"]["attention"]["priority"],
        serde_json::json!([urgent.to_string()])
    );
    let r = filtered(&pool, f.a, "", &token).await;
    assert_eq!(
        r.json["drilldown"]["attention"]["priority"],
        serde_json::json!([urgent.to_string(), high.to_string()])
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn filter_values_must_belong_to_the_workspace(pool: PgPool) {
    let f = fixture(&pool).await;
    let token = token_for(&pool, f.admin).await;
    let b_channel = catalog_id(&pool, "intake_channels", f.b, "email").await;
    let b_category = catalog_id(&pool, "categories", f.b, "kategori_uji").await;
    let b_priority = catalog_id(&pool, "priorities", f.b, "urgent").await;
    let a_channel = catalog_id(&pool, "intake_channels", f.a, "email").await;
    let unknown = Uuid::new_v4();
    let upper = f.cs.to_string().to_uppercase();
    for q in [
        format!("channel={b_channel}"),
        format!("category={b_category}"),
        format!("priority={b_priority}"),
        format!("owner={unknown}"),
        format!("owner={}", f.admin),
        format!("owner={upper}"),
        "owner=none".to_owned(),
        "channel=unassigned".to_owned(),
        "category=".to_owned(),
        "priority=urgent".to_owned(),
        format!("period=today&channel={a_channel}&category={unknown}"),
    ] {
        let r = filtered(&pool, f.a, &q, &token).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "invalid_request", "{q}");
    }
    // DUMB has no members: a DUMA member is not a valid PIC filter there.
    let r = filtered(&pool, f.b, &format!("owner={}", f.cs), &token).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Options are scoped: DUMA members/catalog rows only.
    let r = filtered(&pool, f.a, "", &token).await;
    assert_eq!(r.status, StatusCode::OK);
    let ids = |key: &str| -> Vec<String> {
        r.json["filterOptions"][key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|o| o["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut owners = ids("owners");
    owners.sort();
    let mut want = vec![f.cs.to_string(), f.lead.to_string()];
    want.sort();
    assert_eq!(owners, want);
    for (key, table) in [
        ("channels", "intake_channels"),
        ("categories", "categories"),
        ("priorities", "priorities"),
    ] {
        let mut got = ids(key);
        got.sort();
        let mut db: Vec<String> = ok(
            sqlx::query_scalar::<_, Uuid>(&format!(
                "SELECT id FROM {table} WHERE workspace_id = $1"
            ))
            .bind(f.a)
            .fetch_all(&pool)
            .await,
            table,
        )
        .into_iter()
        .map(|u| u.to_string())
        .collect();
        db.sort();
        assert_eq!(got, db, "{key}");
    }
    let other = filtered(&pool, f.b, "", &token).await;
    assert_eq!(other.json["filterOptions"]["owners"], serde_json::json!([]));
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
    // Filtered shapes: a person as PIC nulls the unassigned drilldown, priority=none
    // nulls the attention drilldown; both must still conform.
    let cs_filter = format!("today&owner={}", f.cs);
    for (query, null_drill) in [
        (cs_filter.as_str(), "unassigned"),
        (
            "this_month&priority=none&channel=none&category=none",
            "attention",
        ),
        ("last_7_days&owner=unassigned", ""),
    ] {
        let d = dashboard(&pool, f.a, query, &token).await;
        conforms(&d, &schema, &spec, query);
        if !null_drill.is_empty() {
            assert!(
                d["drilldown"][null_drill].is_null(),
                "{query}: {null_drill} drilldown null"
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
