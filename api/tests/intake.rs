//! D07 intake + assignment against a real PostgreSQL: create persists once (idempotency key,
//! same payload = replay, other payload = 409, concurrent submits = one ticket), every
//! reference is scoped to the workspace, PIC rules for CS vs Lead CS, simulated Jira/Lark
//! delivery (success / failed / unknown) that never undoes the ticket, permissioned retry and
//! evidence-based reconcile (unknown is never retried), assignment history, and the 0006
//! rollback. Fixtures are synthetic and labelled dummy; no real provider is involved.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
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
    b: Uuid,
    admin: Uuid,
    cs: Uuid,
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
        b: uuid_of(pool, ws, "DUMB").await,
        admin: uuid_of(pool, user, "dummy:uat-superadmin").await,
        cs: uuid_of(pool, user, "dummy:cs-alfa").await,
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

async fn count(pool: &PgPool, sql: &str, ws: Uuid) -> i64 {
    ok(sqlx::query_scalar(sql).bind(ws).fetch_one(pool).await, sql)
}

async fn tickets_by_key(pool: &PgPool, ws: Uuid, key: &str) -> i64 {
    ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM tickets WHERE workspace_id = $1 AND create_idempotency_key = $2",
        )
        .bind(ws)
        .bind(key)
        .fetch_one(pool)
        .await,
        "count by key",
    )
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

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn create_persists_once_replays_and_refuses_another_payload(pool: PgPool) {
    let fx = fixture(&pool).await;
    let t = tok(&pool, fx.lead).await;
    let ch = channel(&pool, fx.a).await;
    let dir = directory(&pool, fx.a, &t).await;
    assert_eq!(dir["jira"]["mode"], "simulation");
    assert_eq!(dir["merchants"][0]["source"], "dummy");
    let mut body = full(&dir, ch);
    body["ownerUserId"] = json!(fx.cs);
    body["merchantId"] = dir["merchants"][0]["id"].clone();

    let r = create(&pool, fx.a, &t, "d07-create-0001", &body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["replayed"], false);
    let id = s(&r.json["ticket"]["id"]);
    let number = r.json["ticket"]["number"].as_i64().unwrap_or_default();
    assert!(number > 0);
    assert_eq!(r.json["ticket"]["status"]["code"], "new");
    assert_eq!(r.json["ticket"]["owner"]["id"], json!(fx.cs));
    // Simulated delivery settled after commit: Jira got a SIM issue, Lark was delivered.
    let jira = sync_of(&r.json, "jira");
    assert_eq!(jira["state"], "success");
    assert_eq!(jira["mode"], "simulation");
    assert_eq!(jira["issueKey"], format!("SIM-DUMA-{number}"));
    assert_eq!(sync_of(&r.json, "lark")["state"], "success");

    // Same key + same payload (even re-ordered / re-spaced) = the same ticket, nothing new.
    let mut again = body.clone();
    again["subject"] = json!("  Pengaduan uji D07 (dummy)  ");
    let r2 = create(&pool, fx.a, &t, "d07-create-0001", &again).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.json);
    assert_eq!(r2.json["replayed"], true);
    assert_eq!(s(&r2.json["ticket"]["id"]), id);
    // Same key + other payload = 409, nothing created.
    let mut other = body.clone();
    other["chronology"] = json!("Kronologi berbeda (dummy)");
    let r3 = create(&pool, fx.a, &t, "d07-create-0001", &other).await;
    assert_eq!(r3.status, StatusCode::CONFLICT, "{}", r3.json);
    assert_eq!(r3.json["error"]["code"], "idempotency_conflict");
    assert_eq!(tickets_by_key(&pool, fx.a, "d07-create-0001").await, 1);

    // One ticket, one related work, two intents, one create audit without free text.
    let tid: Uuid = ok(id.parse(), "uuid");
    let one = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            ok(
                sqlx::query_scalar::<_, i64>(sql)
                    .bind(tid)
                    .fetch_one(&pool)
                    .await,
                sql,
            )
        }
    };
    assert_eq!(
        one("SELECT count(*) FROM related_work WHERE ticket_id = $1").await,
        1
    );
    assert_eq!(
        one("SELECT count(*) FROM integration_outbox WHERE ticket_id = $1").await,
        2
    );
    assert_eq!(
        one("SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = 'ticket.create'")
            .await,
        1
    );
    let leaked: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE safe_change::text ILIKE '%Kronologi%' \
             OR safe_change::text ILIKE '%Pengaduan uji D07%'",
        )
        .fetch_one(&pool)
        .await,
        "audit leak",
    );
    assert_eq!(leaked, 0);

    // Visible in List with the same number.
    let l = send(
        &pool,
        "GET",
        &format!("{}?q={number}", tickets_uri(fx.a)),
        &t,
        None,
        None,
    )
    .await;
    assert_eq!(l.status, StatusCode::OK);
    assert!(
        l.json["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|i| i["id"] == json!(id)))
    );

    // Keys are per user: another user may reuse the same key string.
    let t2 = tok(&pool, fx.admin).await;
    let r4 = create(&pool, fx.a, &t2, "d07-create-0001", &base(ch)).await;
    assert_eq!(r4.status, StatusCode::CREATED, "{}", r4.json);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn concurrent_submits_with_one_key_create_one_ticket(pool: PgPool) {
    let fx = fixture(&pool).await;
    let ch = channel(&pool, fx.a).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        let t = tok(&pool, fx.cs).await;
        let body = base(ch);
        let ws = fx.a;
        tasks.push(tokio::spawn(async move {
            create(&pool, ws, &t, "d07-race-key-01", &body).await
        }));
    }
    let mut created = 0;
    let mut replayed = 0;
    let mut ids = std::collections::BTreeSet::new();
    for task in tasks {
        let r = ok(task.await, "join");
        match r.status {
            StatusCode::CREATED => created += 1,
            StatusCode::OK => replayed += 1,
            other => panic!("unexpected {other}: {}", r.json),
        }
        ids.insert(s(&r.json["ticket"]["id"]));
    }
    assert_eq!((created, replayed), (1, 7));
    assert_eq!(ids.len(), 1);
    assert_eq!(tickets_by_key(&pool, fx.a, "d07-race-key-01").await, 1);
}

// ---------- validation and scope ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn invalid_or_foreign_references_create_nothing(pool: PgPool) {
    let fx = fixture(&pool).await;
    let t = tok(&pool, fx.admin).await;
    let ch = channel(&pool, fx.a).await;
    let ch_b = channel(&pool, fx.b).await;
    let dir_b = directory(&pool, fx.b, &t).await;
    let before = count(
        &pool,
        "SELECT count(*) FROM tickets WHERE workspace_id = $1",
        fx.a,
    )
    .await;

    // Missing / malformed idempotency key.
    let r = send(&pool, "POST", &tickets_uri(fx.a), &t, None, Some(&base(ch))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = create(&pool, fx.a, &t, "bad key!", &base(ch)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Unknown field (e.g. a company id) is refused.
    let mut b = base(ch);
    b["companyId"] = json!(fx.b);
    assert_eq!(
        create(&pool, fx.a, &t, "d07-val-0000", &b).await.status,
        StatusCode::BAD_REQUEST
    );

    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut b = base(ch);
    b["subject"] = json!("   ");
    cases.push(("subject", b));
    let mut b = base(ch);
    b["chronology"] = json!("");
    cases.push(("chronology", b));
    let mut b = base(ch);
    b["intakeChannelId"] = Value::Null;
    cases.push(("intakeChannelId", b));
    cases.push(("intakeChannelId", base(ch_b)));
    let mut b = base(ch);
    b["receivedAt"] = json!("2999-01-01T00:00:00+07:00");
    cases.push(("receivedAt", b));
    let mut b = base(ch);
    b["receivedAt"] = json!("2026-02-30T10:00:00+07:00");
    cases.push(("receivedAt", b));
    let mut b = base(ch);
    b["receivedAt"] = json!("kemarin");
    cases.push(("receivedAt", b));
    let mut b = base(ch);
    b["merchantId"] = dir_b["merchants"][0]["id"].clone();
    cases.push(("merchantId", b));
    let mut b = base(ch);
    b["jira"] = json!({"mode": "link", "issueKey": "SIM-DUMB-101"});
    cases.push(("jira.issueKey", b));
    let mut b = base(ch);
    b["jira"] =
        json!({"mode": "create", "title": "t", "assigneeId": dir_b["jira"]["options"][0]["id"]});
    cases.push(("jira.assigneeId", b));
    let mut b = base(ch);
    b["larkDestinationId"] = dir_b["lark"]["options"][0]["id"].clone();
    cases.push(("larkDestinationId", b));
    let mut b = base(ch);
    b["ownerUserId"] = json!(Uuid::from_u128(7));
    cases.push(("ownerUserId", b));
    for (i, (field, body)) in cases.iter().enumerate() {
        let r = create(&pool, fx.a, &t, &format!("d07-val-{i:04}x"), body).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["code"], "validation_failed", "{field}");
        assert_eq!(r.json["error"]["field"], *field, "{}", r.json);
    }
    // A link to this workspace's own simulated issue works.
    let mut b = base(ch);
    b["jira"] = json!({"mode": "link", "issueKey": "SIM-DUMA-101"});
    let r = create(&pool, fx.a, &t, "d07-val-link", &b).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(sync_of(&r.json, "jira")["issueKey"], "SIM-DUMA-101");
    let after = count(
        &pool,
        "SELECT count(*) FROM tickets WHERE workspace_id = $1",
        fx.a,
    )
    .await;
    assert_eq!(after, before + 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn pic_rules_cs_versus_lead_and_workspace_scope(pool: PgPool) {
    let fx = fixture(&pool).await;
    let ch = channel(&pool, fx.a).await;
    let cs = tok(&pool, fx.cs).await;
    let lead = tok(&pool, fx.lead).await;

    let mut b = base(ch);
    b["ownerUserId"] = json!(fx.cs);
    assert_eq!(
        create(&pool, fx.a, &cs, "d07-pic-self", &b).await.status,
        StatusCode::CREATED
    );
    let mut b = base(ch);
    b["ownerUserId"] = json!(fx.lead);
    let r = create(&pool, fx.a, &cs, "d07-pic-other", &b).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    assert_eq!(tickets_by_key(&pool, fx.a, "d07-pic-other").await, 0);
    assert_eq!(
        create(&pool, fx.a, &cs, "d07-pic-none", &base(ch))
            .await
            .status,
        StatusCode::CREATED
    );
    let mut b = base(ch);
    b["ownerUserId"] = json!(fx.cs);
    assert_eq!(
        create(&pool, fx.a, &lead, "d07-pic-lead", &b).await.status,
        StatusCode::CREATED
    );
    // CS is not a member of workspace B: the workspace does not exist for them.
    let ch_b = channel(&pool, fx.b).await;
    let r = create(&pool, fx.b, &cs, "d07-pic-foreign", &base(ch_b)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // CS cannot read the directory of B nor switch simulation outcomes anywhere.
    let r = send(
        &pool,
        "GET",
        &format!("/api/v1/workspaces/{}/intake-directory", fx.b),
        &cs,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let d = directory(&pool, fx.a, &cs).await;
    let r = send(
        &pool,
        "PUT",
        &format!(
            "/api/v1/workspaces/{}/integrations/{}/simulation",
            fx.a,
            s(&d["jira"]["connectionId"])
        ),
        &cs,
        None,
        Some(&json!({"outcome": "failed", "version": d["jira"]["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

// ---------- simulated failures and recovery ----------

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

fn outbox_uri(ws: Uuid, item: &Value, action: &str) -> String {
    format!(
        "/api/v1/workspaces/{ws}/integration-outbox/{}/{action}",
        s(&item["id"])
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn failed_delivery_keeps_ticket_and_retry_is_permissioned(pool: PgPool) {
    let fx = fixture(&pool).await;
    let admin = tok(&pool, fx.admin).await;
    let lead = tok(&pool, fx.lead).await;
    let ch = channel(&pool, fx.a).await;
    simulate(&pool, fx.a, &admin, "jira", "failed").await;
    let dir = directory(&pool, fx.a, &lead).await;
    assert_eq!(dir["jira"]["simulatedOutcome"], "failed");

    let r = create(&pool, fx.a, &lead, "d07-fail-0001", &full(&dir, ch)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let jira = sync_of(&r.json, "jira").clone();
    assert_eq!(jira["state"], "failed");
    assert!(s(&jira["safeError"]).starts_with("Simulasi:"));
    assert_eq!(sync_of(&r.json, "lark")["state"], "success");
    let tid = s(&r.json["ticket"]["id"]);
    let read = send(
        &pool,
        "GET",
        &format!("{}/{tid}/integrations", tickets_uri(fx.a)),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(read.json["canRecover"], false);

    // Without integration.recovery: 403. With it: a stale attempt is 409.
    let body = json!({"attempt": jira["attempt"]});
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "retry"),
        &lead,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    grant_recovery(&pool, &fx).await;
    let read = send(
        &pool,
        "GET",
        &format!("{}/{tid}/integrations", tickets_uri(fx.a)),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(read.json["canRecover"], true);
    let stale = json!({"attempt": 0});
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "retry"),
        &lead,
        None,
        Some(&stale),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // Still failing in simulation: retry records a new attempt, still failed.
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "retry"),
        &lead,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "failed");
    assert_eq!(r.json["item"]["attempt"], 2);
    simulate(&pool, fx.a, &admin, "jira", "success").await;
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "retry"),
        &lead,
        None,
        Some(&json!({"attempt": 2})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "success");
    assert!(s(&r.json["item"]["issueKey"]).starts_with("SIM-DUMA-"));
    // Success is final.
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "retry"),
        &lead,
        None,
        Some(&json!({"attempt": 3})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // The ticket itself was never touched by the failures.
    let t: i32 = ok(
        sqlx::query_scalar("SELECT version FROM tickets WHERE id = $1::uuid")
            .bind(&tid)
            .fetch_one(&pool)
            .await,
        "version",
    );
    assert_eq!(t, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn unknown_delivery_is_reconciled_with_evidence_not_retried(pool: PgPool) {
    let fx = fixture(&pool).await;
    let admin = tok(&pool, fx.admin).await;
    let ch = channel(&pool, fx.a).await;
    simulate(&pool, fx.a, &admin, "jira", "unknown").await;
    simulate(&pool, fx.a, &admin, "lark", "unknown").await;
    let dir = directory(&pool, fx.a, &admin).await;
    let r = create(&pool, fx.a, &admin, "d07-unknown-01", &full(&dir, ch)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let jira = sync_of(&r.json, "jira").clone();
    let lark = sync_of(&r.json, "lark").clone();
    assert_eq!(jira["state"], "unknown");
    assert!(jira["issueKey"].is_null());

    let at = json!({"attempt": jira["attempt"]});
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "retry"),
        &admin,
        None,
        Some(&at),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown must not be retried"
    );
    let bad = [
        json!({"attempt": 1, "outcome": "success", "evidence": "  "}),
        json!({"attempt": 1, "outcome": "success", "evidence": "Dicek di Jira simulasi"}),
    ];
    for b in &bad {
        let r = send(
            &pool,
            "POST",
            &outbox_uri(fx.a, &jira, "reconcile"),
            &admin,
            None,
            Some(b),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    }
    let good = json!({"attempt": 1, "outcome": "success", "evidence": "Issue ditemukan di Jira simulasi", "issueKey": "SIM-DUMA-900"});
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &jira, "reconcile"),
        &admin,
        None,
        Some(&good),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "success");
    assert_eq!(r.json["item"]["reconciled"], true);
    assert_eq!(r.json["item"]["issueKey"], "SIM-DUMA-900");
    // Lark: reconciled as not delivered → failed → may then be retried.
    let no =
        json!({"attempt": 1, "outcome": "failed", "evidence": "Pesan tidak ada di grup simulasi"});
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &lark, "reconcile"),
        &admin,
        None,
        Some(&no),
    )
    .await;
    assert_eq!(r.json["item"]["state"], "failed", "{}", r.json);
    simulate(&pool, fx.a, &admin, "lark", "success").await;
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.a, &lark, "retry"),
        &admin,
        None,
        Some(&json!({"attempt": 1})),
    )
    .await;
    assert_eq!(r.json["item"]["state"], "success", "{}", r.json);
    let audits: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action IN ('integration.reconcile', 'integration.retry')",
        )
        .fetch_one(&pool)
        .await,
        "audits",
    );
    assert_eq!(audits, 3);
    // Cross-workspace outbox ids are not found.
    let r = send(
        &pool,
        "POST",
        &outbox_uri(fx.b, &lark, "retry"),
        &admin,
        None,
        Some(&json!({"attempt": 2})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// ---------- assignment history ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn assignment_history_lists_intake_pic_and_reassign(pool: PgPool) {
    let fx = fixture(&pool).await;
    let lead = tok(&pool, fx.lead).await;
    let ch = channel(&pool, fx.a).await;
    let mut b = base(ch);
    b["ownerUserId"] = json!(fx.cs);
    let r = create(&pool, fx.a, &lead, "d07-hist-0001", &b).await;
    let tid = s(&r.json["ticket"]["id"]);
    let r = send(
        &pool,
        "POST",
        &format!("{}/{tid}/assignment", tickets_uri(fx.a)),
        &lead,
        None,
        Some(&json!({"ownerUserId": fx.lead, "version": 1, "reason": "Dialihkan ke lead (uji)"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let h = send(
        &pool,
        "GET",
        &format!("{}/{tid}/assignment-history", tickets_uri(fx.a)),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(h.status, StatusCode::OK);
    let items = h.json["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 2, "{}", h.json);
    assert_eq!(items[0]["kind"], "reassign");
    assert_eq!(items[0]["from"]["id"], json!(fx.cs));
    assert_eq!(items[0]["to"]["id"], json!(fx.lead));
    assert_eq!(items[0]["reason"], "Dialihkan ke lead (uji)");
    assert_eq!(items[1]["kind"], "initial");
    assert_eq!(items[1]["to"]["id"], json!(fx.cs));
    // Another workspace's ticket is not found.
    let admin = tok(&pool, fx.admin).await;
    let h = send(
        &pool,
        "GET",
        &format!("{}/{tid}/assignment-history", tickets_uri(fx.b)),
        &admin,
        None,
        None,
    )
    .await;
    assert_eq!(h.status, StatusCode::NOT_FOUND);
}

// ---------- rollback ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0006_rolls_back_and_reapplies(pool: PgPool) {
    let fx = fixture(&pool).await;
    let t = tok(&pool, fx.cs).await;
    let ch = channel(&pool, fx.a).await;
    assert_eq!(
        create(&pool, fx.a, &t, "d07-rollback-1", &base(ch))
            .await
            .status,
        StatusCode::CREATED
    );
    // Fingerprint is fixed once written.
    let changed = sqlx::query("UPDATE tickets SET create_request_hash = NULL, version = version + 1 WHERE create_idempotency_key = 'd07-rollback-1'")
        .execute(&pool)
        .await;
    assert!(changed.is_err());
    ok(db::MIGRATOR.undo(&pool, 5).await, "undo to 0005");
    let has: bool = ok(
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_name = 'tickets' AND column_name = 'create_request_hash')",
        )
        .fetch_one(&pool)
        .await,
        "column",
    );
    assert!(!has);
    assert_eq!(tickets_by_key(&pool, fx.a, "d07-rollback-1").await, 1);
    ok(db::MIGRATOR.run(&pool).await, "reapply");
}
