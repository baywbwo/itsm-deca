//! D08c simulated outbox worker against a real PostgreSQL: claims are exclusive under
//! concurrency (one delivery, one issue), transient failures retry automatically with bounded
//! attempts and backoff and then fail, a claim that outlives its lease becomes unknown and is
//! never retried (reconcile only), worker audits carry no user, manual retry resets the
//! automatic budget, and migration 0008 rolls back. Fixtures are synthetic and labelled dummy;
//! no real provider is involved.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::outbox::{self, Settled};
use itsm_api::{AppState, app, db, seed, session};
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
    admin: Uuid,
    lead: Uuid,
}

async fn uuid_of(pool: &PgPool, sql: &str, key: &str) -> Uuid {
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
        a: uuid_of(pool, ws, "DUMA").await,
        admin: uuid_of(pool, user, "dummy:uat-superadmin").await,
        lead: uuid_of(pool, user, "dummy:lead-alfa").await,
    }
}

async fn ref_id(pool: &PgPool, sql: &str, ws: Uuid) -> Uuid {
    ok(sqlx::query_scalar(sql).bind(ws).fetch_one(pool).await, sql)
}

async fn channel(pool: &PgPool, ws: Uuid) -> Uuid {
    ref_id(
        pool,
        "SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'",
        ws,
    )
    .await
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

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    json: Value,
}

async fn send(
    pool: &PgPool,
    method: &str,
    uri: &str,
    t: &Tok,
    key: Option<&str>,
    body: Option<&Value>,
) -> Resp {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            header::COOKIE,
            format!("{}={}", session::cookie_name(false), t.token),
        )
        .header(session::CSRF_HEADER, &t.csrf);
    if let Some(k) = key {
        r = r.header("idempotency-key", k);
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

fn tickets_uri(ws: Uuid) -> String {
    format!("/api/v1/workspaces/{ws}/tickets")
}

async fn create(pool: &PgPool, ws: Uuid, t: &Tok, key: &str, body: &Value) -> Resp {
    send(pool, "POST", &tickets_uri(ws), t, Some(key), Some(body)).await
}

fn base(ch: Uuid) -> Value {
    json!({
        "subject": "Pengaduan uji D07 (dummy)",
        "chronology": "Kronologi uji (dummy): nasabah fiktif melapor via email.",
        "receivedAt": "2026-10-30T09:15:00+07:00",
        "intakeChannelId": ch,
    })
}

fn s(v: &Value) -> String {
    match v.as_str() {
        Some(x) => x.to_owned(),
        None => panic!("not a string: {v}"),
    }
}

async fn directory(pool: &PgPool, ws: Uuid, t: &Tok) -> Value {
    let r = send(
        pool,
        "GET",
        &format!("/api/v1/workspaces/{ws}/intake-directory"),
        t,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json
}

async fn simulate(pool: &PgPool, ws: Uuid, t: &Tok, provider: &str, outcome: &str) {
    let d = directory(pool, ws, t).await;
    let conn = &d[provider];
    let r = send(
        pool,
        "PUT",
        &format!(
            "/api/v1/workspaces/{ws}/integrations/{}/simulation",
            s(&conn["connectionId"])
        ),
        t,
        None,
        Some(&json!({"outcome": outcome, "version": conn["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

fn sync_of<'a>(created: &'a Value, provider: &str) -> &'a Value {
    match created["integrations"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["provider"] == provider))
    {
        Some(v) => v,
        None => panic!("no {provider} intent in {created}"),
    }
}

fn full(pool_dir: &Value, ch: Uuid) -> Value {
    let mut b = base(ch);
    b["jira"] = json!({
        "mode": "create",
        "title": "Tindak lanjut uji (dummy)",
        "assigneeId": pool_dir["jira"]["options"][0]["id"],
    });
    b["larkDestinationId"] = pool_dir["lark"]["options"][0]["id"].clone();
    b
}

// ---------- create, replay, conflict ----------

async fn grant_recovery(pool: &PgPool, fx: &Fx) {
    ok(
        sqlx::query(
            "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by) \
             SELECT $1, id, 'integration.recovery', $3 FROM workspace_memberships \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(fx.a)
        .bind(fx.lead)
        .bind(fx.admin)
        .execute(pool)
        .await,
        "grant recovery",
    );
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    state: String,
    attempt: i32,
    auto_attempts: i32,
    in_flight: bool,
    /// Seconds from the last update to the next automatic attempt.
    wait_secs: Option<f64>,
    safe_error: Option<String>,
}

async fn row(pool: &PgPool, id: Uuid) -> Row {
    ok(
        sqlx::query_as(
            "SELECT state, attempt, auto_attempts, claim_token IS NOT NULL AS in_flight, \
               EXTRACT(EPOCH FROM next_attempt_at - updated_at)::float8 AS wait_secs, safe_error \
             FROM integration_outbox WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await,
        "outbox row",
    )
}

async fn make_due(pool: &PgPool, id: Uuid) {
    ok(
        sqlx::query(
            "UPDATE integration_outbox SET next_attempt_at = now() - interval '1 second' \
             WHERE id = $1 AND next_attempt_at IS NOT NULL",
        )
        .bind(id)
        .execute(pool)
        .await,
        "make due",
    );
}

async fn audits(pool: &PgPool, id: Uuid, action: &str, worker: bool) -> i64 {
    ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = $2 \
             AND (actor_id IS NULL) = $3",
        )
        .bind(id)
        .bind(action)
        .bind(worker)
        .fetch_one(pool)
        .await,
        "audits",
    )
}

/// Creates a ticket whose Jira intent hit a transient failure on the inline attempt.
async fn transient_jira(pool: &PgPool, fx: &Fx, key: &str) -> Uuid {
    let admin = tok(pool, fx.admin).await;
    let lead = tok(pool, fx.lead).await;
    simulate(pool, fx.a, &admin, "jira", "transient").await;
    let dir = directory(pool, fx.a, &lead).await;
    assert_eq!(dir["jira"]["simulatedOutcome"], "transient");
    let r = create(
        pool,
        fx.a,
        &lead,
        key,
        &full(&dir, channel(pool, fx.a).await),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let jira = sync_of(&r.json, "jira");
    assert_eq!(jira["state"], "pending", "{jira}");
    assert_eq!(jira["autoAttempts"], 1);
    assert_eq!(jira["inFlight"], false);
    assert!(jira["nextAttemptAt"].is_string(), "{jira}");
    assert_eq!(sync_of(&r.json, "lark")["state"], "success");
    ok(Uuid::parse_str(&s(&jira["id"])), "id")
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn transient_failures_back_off_then_fail_and_retry_resets_budget(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let id = transient_jira(&pool, &fx, "d08c-transient-1").await;
    let first = row(&pool, id).await;
    assert_eq!((first.attempt, first.auto_attempts), (1, 1));
    assert!(
        (first.wait_secs.unwrap_or(0.0) - 2.0).abs() < 0.5,
        "{first:?}"
    );

    // Not due yet: the worker leaves it alone.
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.claimed, 0);
    assert_eq!(row(&pool, id).await.attempt, 1);

    // Each due tick is one more attempt; waits double (2s, 4s, 8s, 16s) until the budget ends.
    let mut waits = vec![first.wait_secs.unwrap_or(0.0).round()];
    for n in 2..=outbox::MAX_AUTO_ATTEMPTS {
        make_due(&pool, id).await;
        let t = ok(outbox::run_once(&st).await, "tick");
        assert_eq!((t.claimed, t.settled), (1, 1), "attempt {n}");
        let r = row(&pool, id).await;
        assert_eq!((r.attempt, r.auto_attempts), (n, n));
        assert!(!r.in_flight);
        if n < outbox::MAX_AUTO_ATTEMPTS {
            assert_eq!(r.state, "pending");
            waits.push(r.wait_secs.unwrap_or(0.0).round());
        } else {
            assert_eq!(r.state, "failed", "{r:?}");
            assert!(r.wait_secs.is_none());
            assert!(r.safe_error.unwrap_or_default().contains("Batas 5"));
        }
    }
    assert_eq!(waits, vec![2.0, 4.0, 8.0, 16.0]);
    // Inline attempt audited with the user, worker attempts without one.
    assert_eq!(audits(&pool, id, "integration.sync", false).await, 1);
    assert_eq!(audits(&pool, id, "integration.sync", true).await, 4);
    // Failed is final for the worker.
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.claimed, 0);

    // A permitted manual retry starts a fresh automatic budget.
    grant_recovery(&pool, &fx).await;
    let lead = tok(&pool, fx.lead).await;
    let r = send(
        &pool,
        "POST",
        &format!("/api/v1/workspaces/{}/integration-outbox/{id}/retry", fx.a),
        &lead,
        None,
        Some(&json!({ "attempt": 5 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "pending");
    assert_eq!(r.json["item"]["attempt"], 6);
    assert_eq!(r.json["item"]["autoAttempts"], 1);

    // Provider recovers: the next due tick delivers once and links one issue.
    let admin = tok(&pool, fx.admin).await;
    simulate(&pool, fx.a, &admin, "jira", "success").await;
    make_due(&pool, id).await;
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.settled, 1);
    let r = row(&pool, id).await;
    assert_eq!((r.state.as_str(), r.attempt), ("success", 7));
    assert!(r.safe_error.is_none());
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn concurrent_claimers_deliver_exactly_once(pool: PgPool) {
    let fx = fixture(&pool).await;
    let id = transient_jira(&pool, &fx, "d08c-race-0001").await;
    let admin = tok(&pool, fx.admin).await;
    simulate(&pool, fx.a, &admin, "jira", "success").await;
    make_due(&pool, id).await;

    let mut tasks = Vec::new();
    for i in 0..12 {
        let st = state(&pool);
        let ws = fx.a;
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                outbox::run_once(&st).await.map(|t| t.settled)
            } else {
                let claim = {
                    let mut conn = st.pool.acquire().await?;
                    outbox::claim_one(&mut conn, ws, id).await?
                };
                match claim {
                    Some(c) => outbox::settle(&st, c, None, &None)
                        .await
                        .map(|s| usize::from(s == Settled::Success)),
                    None => Ok(0),
                }
            }
        }));
    }
    let mut settled = 0;
    for t in tasks {
        settled += ok(ok(t.await, "join"), "worker");
    }
    assert_eq!(settled, 1, "exactly one claimer delivers");
    let r = row(&pool, id).await;
    assert_eq!((r.state.as_str(), r.attempt), ("success", 2));
    let issues: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM jira_issues WHERE external_issue_id = $1")
            .bind(format!("sim-{id}"))
            .fetch_one(&pool)
            .await,
        "issues",
    );
    assert_eq!(issues, 1);
    assert_eq!(audits(&pool, id, "integration.sync", true).await, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn expired_lease_becomes_unknown_and_is_never_retried(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let id = transient_jira(&pool, &fx, "d08c-lease-001").await;
    let admin = tok(&pool, fx.admin).await;
    simulate(&pool, fx.a, &admin, "jira", "success").await;
    make_due(&pool, id).await;

    // A claimer takes the intent and "crashes" before settling.
    let claim = {
        let mut conn = ok(pool.acquire().await, "conn");
        ok(outbox::claim_one(&mut conn, fx.a, id).await, "claim")
    };
    let Some(claim) = claim else {
        panic!("due intent was not claimed")
    };
    let r = row(&pool, id).await;
    assert!(r.in_flight);
    assert_eq!(r.attempt, 2);
    // While the lease holds nobody else takes it, and the sweeper leaves it alone.
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!((t.expired, t.claimed), (0, 0));
    // A claimed row cannot leave pending without clearing the claim (DB guard).
    let raw = sqlx::query("UPDATE integration_outbox SET state = 'failed' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await;
    assert!(raw.is_err());

    ok(
        sqlx::query(
            "UPDATE integration_outbox SET claimed_until = now() - interval '1 second' WHERE id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await,
        "expire lease",
    );
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!((t.expired, t.claimed), (1, 0));
    let r = row(&pool, id).await;
    assert_eq!(r.state, "unknown");
    assert!(!r.in_flight);
    assert_eq!(r.safe_error.as_deref(), Some(outbox::LEASE_EXPIRED_ERROR));
    assert_eq!(
        audits(&pool, id, "integration.lease_expired", true).await,
        1
    );

    // The late claimer cannot settle any more.
    let late = ok(outbox::settle(&st, claim, None, &None).await, "late settle");
    assert_eq!(late, Settled::LeaseLost);
    assert_eq!(row(&pool, id).await.state, "unknown");
    // Unknown is never picked up again or retried; only reconcile with evidence.
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.claimed, 0);
    grant_recovery(&pool, &fx).await;
    let lead = tok(&pool, fx.lead).await;
    let r = send(
        &pool,
        "POST",
        &format!("/api/v1/workspaces/{}/integration-outbox/{id}/retry", fx.a),
        &lead,
        None,
        Some(&json!({ "attempt": 2 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let r = send(
        &pool,
        "POST",
        &format!(
            "/api/v1/workspaces/{}/integration-outbox/{id}/reconcile",
            fx.a
        ),
        &lead,
        None,
        Some(&json!({
            "attempt": 2,
            "outcome": "success",
            "evidence": "Dicek di Jira simulasi: issue ada (dummy).",
            "issueKey": "SIM-DUMA-RECON"
        })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "success");
    assert_eq!(r.json["item"]["reconciled"], true);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn worker_ignores_fresh_intents_left_to_inline_dispatch(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let admin = tok(&pool, fx.admin).await;
    let lead = tok(&pool, fx.lead).await;
    // Unknown: inline settles it; the worker must not touch settled rows.
    simulate(&pool, fx.a, &admin, "jira", "unknown").await;
    let dir = directory(&pool, fx.a, &lead).await;
    let r = create(
        &pool,
        fx.a,
        &lead,
        "d08c-fresh-001",
        &full(&dir, channel(&pool, fx.a).await),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(sync_of(&r.json, "jira")["state"], "unknown");
    // A pending intent younger than the inline grace is not claimed by the worker; an older
    // one (inline dispatch lost, e.g. process restart) is.
    let tid = s(&r.json["ticket"]["id"]);
    let fresh = extra_lark(&pool, &tid, "fresh", 0).await;
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.claimed, 0);
    assert_eq!(row(&pool, fresh).await.state, "pending");
    let old = extra_lark(&pool, &tid, "stale", 10).await;
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!((t.claimed, t.settled), (1, 1));
    let r = row(&pool, old).await;
    assert_eq!((r.state.as_str(), r.attempt), ("success", 1));
    assert_eq!(audits(&pool, old, "integration.sync", true).await, 1);
    assert_eq!(row(&pool, fresh).await.state, "pending");
}

async fn extra_lark(pool: &PgPool, ticket: &str, tag: &str, age_secs: i32) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, \
               lark_destination_id, idempotency_key, created_at) \
             SELECT workspace_id, ticket_id, connection_id, provider, operation, lark_destination_id, \
               ticket_id::text || ':lark-' || $2, now() - make_interval(secs => $3) \
             FROM integration_outbox WHERE ticket_id = $1::uuid AND provider = 'lark' \
             ORDER BY created_at LIMIT 1 RETURNING id",
        )
        .bind(ticket)
        .bind(tag)
        .bind(f64::from(age_secs))
        .fetch_one(pool)
        .await,
        "extra intent",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0008_rolls_back_and_reapplies(pool: PgPool) {
    let fx = fixture(&pool).await;
    let id = transient_jira(&pool, &fx, "d08c-rollback1").await;
    ok(db::MIGRATOR.undo(&pool, 7).await, "undo to 0007");
    let has: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns WHERE \
             (table_name = 'integration_outbox' AND column_name IN ('claim_token', 'claimed_until', 'auto_attempts')) \
             OR (table_name = 'saved_views' AND column_name = 'columns')",
        )
        .fetch_one(&pool)
        .await,
        "columns",
    );
    assert_eq!(has, 0);
    assert_eq!(row_state(&pool, id).await, "pending");
    ok(db::MIGRATOR.run(&pool).await, "reapply");
    assert_eq!(row(&pool, id).await.auto_attempts, 0);
}

async fn row_state(pool: &PgPool, id: Uuid) -> String {
    ok(
        sqlx::query_scalar("SELECT state FROM integration_outbox WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await,
        "state",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn disabled_connection_settles_pending_intent_as_failed(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let id = transient_jira(&pool, &fx, "d09-disabled-1").await;
    // D09: an administrator disables the connection while the intent waits for its retry.
    let admin = tok(&pool, fx.admin).await;
    let list = send(
        &pool,
        "GET",
        &format!("/api/v1/workspaces/{}/integrations", fx.a),
        &admin,
        None,
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    let jira = match list.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["provider"] == "jira"))
    {
        Some(c) => c.clone(),
        None => panic!("no jira in {}", list.json),
    };
    let r = send(
        &pool,
        "POST",
        &format!(
            "/api/v1/workspaces/{}/integrations/{}/disable",
            fx.a,
            s(&jira["id"])
        ),
        &admin,
        None,
        Some(&json!({"version": jira["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let issues = || async {
        let n: i64 = ok(
            sqlx::query_scalar("SELECT count(*) FROM jira_issues WHERE workspace_id = $1")
                .bind(fx.a)
                .fetch_one(&pool)
                .await,
            "issues",
        );
        n
    };
    let before = issues().await;
    make_due(&pool, id).await;
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.settled, 1, "{t:?}");
    let after = row(&pool, id).await;
    assert_eq!(after.state, "failed", "{after:?}");
    assert!(
        after
            .safe_error
            .as_deref()
            .is_some_and(|e| e.contains("tidak aktif")),
        "{after:?}"
    );
    assert_eq!(
        issues().await,
        before,
        "nothing sent through a disabled connection"
    );
}

/// CP7b: several worker processes (two pools = two instances, three workers each) drain a
/// backlog together. Every intent is delivered once: one worker attempt, one issue, one audit.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7b_worker_instances_share_a_backlog_without_double_delivery(pool: PgPool) {
    const INTENTS: usize = 24;
    let fx = fixture(&pool).await;
    let mut ids = Vec::new();
    for i in 0..INTENTS {
        ids.push(transient_jira(&pool, &fx, &format!("cp7b-scale-{i:04}")).await);
    }
    let admin = tok(&pool, fx.admin).await;
    simulate(&pool, fx.a, &admin, "jira", "success").await;
    for id in &ids {
        make_due(&pool, *id).await;
    }

    let second_instance = ok(
        db::pool_options(&itsm_api::config::DbPoolConfig {
            max_connections: 3,
            acquire_timeout: std::time::Duration::from_secs(3),
        })
        .connect_with(
            (*pool.connect_options())
                .clone()
                .application_name("itsm-worker"),
        )
        .await,
        "second pool",
    );
    assert_eq!(second_instance.options().get_max_connections(), 3);

    // All workers claim at the same moment: the race SKIP LOCKED has to win.
    let start = Arc::new(tokio::sync::Barrier::new(6));
    let mut tasks = Vec::new();
    for i in 0..6 {
        let st = state(if i % 2 == 0 { &pool } else { &second_instance });
        let start = start.clone();
        tasks.push(tokio::spawn(async move {
            // Warm a connection first so the claims, not the connects, start together.
            drop(st.pool.acquire().await?);
            start.wait().await;
            let mut settled = 0;
            let mut idle = 0;
            while idle < 2 {
                let t = outbox::run_once(&st).await?;
                settled += t.settled;
                idle = if t.claimed == 0 { idle + 1 } else { 0 };
            }
            Ok::<usize, itsm_api::error::AppError>(settled)
        }));
    }
    let mut settled = Vec::new();
    for t in tasks {
        settled.push(ok(ok(t.await, "join"), "worker"));
    }
    assert_eq!(settled.iter().sum::<usize>(), INTENTS, "{settled:?}");

    for id in &ids {
        let r = row(&pool, *id).await;
        // attempt 1 was the inline transient failure, attempt 2 the single worker delivery.
        assert_eq!(
            (r.state.as_str(), r.attempt, r.in_flight),
            ("success", 2, false)
        );
        assert_eq!(audits(&pool, *id, "integration.sync", true).await, 1);
    }
    let issues: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(DISTINCT external_issue_id) FROM jira_issues \
             WHERE external_issue_id = ANY($1)",
        )
        .bind(ids.iter().map(|id| format!("sim-{id}")).collect::<Vec<_>>())
        .fetch_one(&pool)
        .await,
        "issues",
    );
    assert_eq!(issues, INTENTS as i64);
    let rows: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM jira_issues WHERE external_issue_id = ANY($1)")
            .bind(ids.iter().map(|id| format!("sim-{id}")).collect::<Vec<_>>())
            .fetch_one(&pool)
            .await,
        "issue rows",
    );
    assert_eq!(rows, INTENTS as i64, "one issue row per intent");
    second_instance.close().await;
}

/// CP7b: a tick holds one claim at a time, so every lease starts right before its own attempt.
/// A whole batch claimed up front would age while earlier items are delivered, and another
/// instance's sweeper would turn the last claims into `unknown` although they were never sent.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7b_worker_tick_holds_one_claim_at_a_time(pool: PgPool) {
    const INTENTS: usize = 20;
    let fx = fixture(&pool).await;
    let mut ids = Vec::new();
    for i in 0..INTENTS {
        ids.push(transient_jira(&pool, &fx, &format!("cp7b-lease-{i:04}")).await);
    }
    let admin = tok(&pool, fx.admin).await;
    simulate(&pool, fx.a, &admin, "jira", "success").await;
    for id in &ids {
        make_due(&pool, *id).await;
    }

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observer = {
        let pool = pool.clone();
        let done = done.clone();
        tokio::spawn(async move {
            let mut max = 0i64;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let n: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM integration_outbox WHERE claim_token IS NOT NULL",
                )
                .fetch_one(&pool)
                .await?;
                max = max.max(n);
            }
            Ok::<i64, sqlx::Error>(max)
        })
    };
    let st = state(&pool);
    let t = ok(outbox::run_once(&st).await, "tick");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let max_in_flight = ok(ok(observer.await, "join"), "observer");
    assert_eq!((t.claimed, t.settled), (INTENTS, INTENTS));
    assert!(max_in_flight <= 1, "claims held at once: {max_in_flight}");
    for id in &ids {
        assert_eq!(row(&pool, *id).await.state, "success");
    }
}
