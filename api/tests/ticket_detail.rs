//! D08a ticket detail against a real PostgreSQL: the detail read is scoped (another
//! workspace = 404), property edits go API → DB → reload with a 409 that changes nothing,
//! the closed archive refuses every write, related work is completed only by a person with
//! summary + evidence (a Jira "Done" status never completes it nor unblocks resolve), and
//! activity is paginated from the audit log without echoing free text. Fixtures are
//! synthetic and labelled dummy.

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

async fn new_ticket(pool: &PgPool, ws: Uuid, by: Uuid) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, \
               intake_channel_id, priority_id) \
             VALUES ($1, 'Pengaduan detail uji (dummy)', 'Kronologi uji (dummy)', now(), $2, \
               (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'), \
               (SELECT id FROM priorities WHERE workspace_id = $1 AND code = 'normal')) RETURNING id",
        )
        .bind(ws)
        .bind(by)
        .fetch_one(pool)
        .await,
        "insert ticket",
    )
}

/// Historical archive row written with triggers disabled, like an imported legacy row.
async fn archive(pool: &PgPool, t: Uuid) {
    let mut c = ok(pool.acquire().await, "acquire");
    for sql in [
        "SET session_replication_role = replica",
        "UPDATE tickets SET workflow_status = 'closed', version = version + 1 WHERE id = $1",
        "SET session_replication_role = origin",
    ] {
        let q = sqlx::query(sql);
        let q = if sql.contains("$1") { q.bind(t) } else { q };
        ok(q.execute(&mut *c).await, sql);
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

async fn write(pool: &PgPool, method: &str, uri: &str, t: &Tok, csrf: bool, body: &Value) -> Resp {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie(t))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-request-id", "uji-d08-korelasi");
    if csrf {
        r = r.header(session::CSRF_HEADER, &t.csrf);
    }
    send(pool, ok(r.body(Body::from(body.to_string())), "request")).await
}

fn base(ws: Uuid, t: Uuid) -> String {
    format!("/api/v1/workspaces/{ws}/tickets/{t}")
}

async fn detail(pool: &PgPool, ws: Uuid, t: Uuid, tk: &Tok) -> Value {
    let r = get(pool, &format!("{}/detail", base(ws, t)), tk).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    r.json
}

fn n(v: &Value) -> i64 {
    match v.as_i64() {
        Some(x) => x,
        None => panic!("not a number: {v}"),
    }
}

async fn audit_count(pool: &PgPool, t: Uuid, action: &str) -> i64 {
    ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = $2",
        )
        .bind(t)
        .bind(action)
        .fetch_one(pool)
        .await,
        "audit count",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn properties_persist_reload_and_stale_version_changes_nothing(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let tk = tok(&pool, f.cs).await;
    let d = detail(&pool, f.a, t, &tk).await;
    assert_eq!(n(&d["ticket"]["version"]), 1);
    assert_eq!(d["can"]["editProperties"], true);
    assert_eq!(d["can"]["readOnly"], false);
    assert!(d["properties"]["intakeChannel"]["label"].is_string());

    let high = code_id(&pool, "priorities", f.a, "high").await;
    let r = write(
        &pool,
        "PATCH",
        &base(f.a, t),
        &tk,
        true,
        &json!({"version": 1, "subject": "  Subjek baru (dummy) ", "priorityId": high,
                "cause": "Penyebab uji (dummy)", "categoryId": null}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    assert!(r.json["auditEventId"].is_string());
    let d = &r.json["detail"];
    assert_eq!(n(&d["ticket"]["version"]), 2);
    assert_eq!(d["properties"]["subject"], "Subjek baru (dummy)");
    assert_eq!(d["properties"]["priority"]["id"], json!(high));
    // Reload from DB through a fresh read.
    let again = detail(&pool, f.a, t, &tk).await;
    assert_eq!(again["properties"]["cause"], "Penyebab uji (dummy)");
    assert_eq!(n(&again["ticket"]["version"]), 2);
    let (subject, version): (String, i32) = ok(
        sqlx::query_as("SELECT subject, version FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(&pool)
            .await,
        "row",
    );
    assert_eq!((subject.as_str(), version), ("Subjek baru (dummy)", 2));
    // Audit keeps field names only, never values.
    let change: Value = ok(
        sqlx::query_scalar(
            "SELECT safe_change FROM audit_events WHERE entity_id = $1 AND action = 'ticket.update'",
        )
        .bind(t)
        .fetch_one(&pool)
        .await,
        "audit",
    );
    assert_eq!(change["fields"], json!(["subject", "priorityId", "cause"]));
    assert!(!change.to_string().contains("dummy"));

    // Stale version: 409 and nothing changes.
    let r = write(
        &pool,
        "PATCH",
        &base(f.a, t),
        &tk,
        true,
        &json!({"version": 1, "subject": "Tidak boleh tersimpan"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.raw);
    let again = detail(&pool, f.a, t, &tk).await;
    assert_eq!(again["properties"]["subject"], "Subjek baru (dummy)");
    assert_eq!(audit_count(&pool, t, "ticket.update").await, 1);

    // Same values: no version bump, no audit.
    let r = write(
        &pool,
        "PATCH",
        &base(f.a, t),
        &tk,
        true,
        &json!({"version": 2, "subject": "Subjek baru (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json["auditEventId"].is_null());
    assert_eq!(n(&r.json["detail"]["ticket"]["version"]), 2);

    // Field errors name the field; unknown fields and empty required text are refused.
    for (body, code, field) in [
        (
            json!({"version": 2, "subject": "   "}),
            422,
            Some("subject"),
        ),
        (
            json!({"version": 2, "priorityId": Uuid::new_v4()}),
            422,
            Some("priorityId"),
        ),
        (json!({"version": 2, "cause": ""}), 422, Some("cause")),
        (json!({"version": 2, "mystery": 1}), 400, None),
        (json!({"version": 0}), 400, None),
    ] {
        let r = write(&pool, "PATCH", &base(f.a, t), &tk, true, &body).await;
        assert_eq!(r.status.as_u16(), code, "{body} → {}", r.raw);
        if let Some(field) = field {
            assert_eq!(r.json["error"]["field"], field, "{}", r.raw);
        }
    }
    // Another workspace's catalog value is not active here.
    let foreign = code_id(&pool, "priorities", f.b, "high").await;
    let r = write(
        &pool,
        "PATCH",
        &base(f.a, t),
        &tk,
        true,
        &json!({"version": 2, "priorityId": foreign}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422, "{}", r.raw);
    assert_eq!(n(&detail(&pool, f.a, t, &tk).await["ticket"]["version"]), 2);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn scope_csrf_and_archive_refuse_without_side_effects(pool: PgPool) {
    let f = fixture(&pool).await;
    let other = new_ticket(&pool, f.b, f.admin).await;
    let mine = new_ticket(&pool, f.a, f.admin).await;
    for uid in [f.cs, f.lead] {
        let tk = tok(&pool, uid).await;
        // DUMB ticket through either path: not found, never forbidden-with-details.
        for ws in [f.a, f.b] {
            for uri in [
                format!("{}/detail", base(ws, other)),
                format!("{}/activity", base(ws, other)),
            ] {
                assert_eq!(
                    get(&pool, &uri, &tk).await.status,
                    StatusCode::NOT_FOUND,
                    "{uri}"
                );
            }
            let r = write(
                &pool,
                "PATCH",
                &base(ws, other),
                &tk,
                true,
                &json!({"version": 1, "subject": "x"}),
            )
            .await;
            assert_eq!(r.status, StatusCode::NOT_FOUND);
            let r = write(
                &pool,
                "POST",
                &format!("{}/related-work", base(ws, other)),
                &tk,
                true,
                &json!({"title": "x", "blocking": false}),
            )
            .await;
            assert_eq!(r.status, StatusCode::NOT_FOUND);
        }
        // Missing CSRF header: refused before anything is written.
        let r = write(
            &pool,
            "PATCH",
            &base(f.a, mine),
            &tk,
            false,
            &json!({"version": 1, "subject": "x"}),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.raw);
    }
    let v: i32 = ok(
        sqlx::query_scalar("SELECT version FROM tickets WHERE id = $1")
            .bind(other)
            .fetch_one(&pool)
            .await,
        "v",
    );
    assert_eq!(v, 1);

    // Closed archive: read-only for every write, even with the current version.
    archive(&pool, mine).await;
    let tk = tok(&pool, f.lead).await;
    let d = detail(&pool, f.a, mine, &tk).await;
    assert_eq!(d["can"]["readOnly"], true);
    assert_eq!(d["can"]["editProperties"], false);
    assert_eq!(d["can"]["manageRelatedWork"], false);
    let r = write(
        &pool,
        "PATCH",
        &base(f.a, mine),
        &tk,
        true,
        &json!({"version": 2, "subject": "x"}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    let r = write(
        &pool,
        "POST",
        &format!("{}/related-work", base(f.a, mine)),
        &tk,
        true,
        &json!({"title": "x", "blocking": true}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(audit_count(&pool, mine, "ticket.update").await, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn related_work_completion_is_explicit_and_jira_done_never_unblocks(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let tk = tok(&pool, f.cs).await;
    let r = write(
        &pool,
        "POST",
        &format!("{}/related-work", base(f.a, t)),
        &tk,
        true,
        &json!({"title": " Perbaikan modul uji (dummy) ", "blocking": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    let rw = &r.json["detail"]["relatedWork"][0];
    assert_eq!(rw["title"], "Perbaikan modul uji (dummy)");
    assert_eq!(rw["completionState"], "open");
    assert_eq!(
        n(&r.json["detail"]["ticket"]["relatedWork"]["blockingOpen"]),
        1
    );
    let rw_id: Uuid = ok(
        Uuid::parse_str(rw["id"].as_str().unwrap_or_default()),
        "rw id",
    );

    // Link it to a simulated Jira issue and mark that issue Done at the provider.
    ok(
        sqlx::query(
            "UPDATE related_work SET jira_issue_id = (SELECT id FROM jira_issues WHERE workspace_id = $1 \
               ORDER BY issue_key LIMIT 1), version = version + 1 WHERE id = $2",
        )
        .bind(f.a)
        .bind(rw_id)
        .execute(&pool)
        .await,
        "link jira",
    );
    ok(
        sqlx::query("UPDATE jira_issues SET external_status = 'Done' WHERE workspace_id = $1")
            .bind(f.a)
            .execute(&pool)
            .await,
        "jira done",
    );
    let d = detail(&pool, f.a, t, &tk).await;
    let rw = &d["relatedWork"][0];
    assert_eq!(rw["jira"]["externalStatus"], "Done");
    assert_eq!(
        rw["completionState"], "open",
        "Jira Done must not complete work"
    );
    let resolve = d["ticket"]["allowedActions"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["kind"] == "resolve"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(resolve["blockedBy"], "blocking_related_work");
    let r = write(
        &pool,
        "POST",
        &format!("{}/transitions", base(f.a, t)),
        &tk,
        true,
        &json!({"to": "resolved", "version": 1, "summary": "Ringkasan uji (dummy)"}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422, "{}", r.raw);

    // Completion needs summary + evidence and the current version.
    let uri = format!("{}/related-work/{rw_id}/completion", base(f.a, t));
    let r = write(
        &pool,
        "POST",
        &uri,
        &tk,
        true,
        &json!({"version": 2, "summary": "ok", "evidence": " "}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(r.json["error"]["field"], "evidence");
    let r = write(
        &pool,
        "POST",
        &uri,
        &tk,
        true,
        &json!({"version": 1, "summary": "ok", "evidence": "bukti"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = write(
        &pool,
        "POST",
        &uri,
        &tk,
        true,
        &json!({"version": 2, "summary": "Sudah diperbaiki (dummy)", "evidence": "Uji ulang lulus (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    let rw = &r.json["detail"]["relatedWork"][0];
    assert_eq!(rw["completionState"], "completed");
    assert!(rw["completedBy"]["label"].is_string());
    assert_eq!(
        n(&r.json["detail"]["ticket"]["relatedWork"]["blockingOpen"]),
        0
    );
    // Completing twice is a rule error, not a silent success.
    let r = write(
        &pool,
        "POST",
        &uri,
        &tk,
        true,
        &json!({"version": 3, "summary": "a", "evidence": "b"}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    // Now resolve goes through.
    let r = write(
        &pool,
        "POST",
        &format!("{}/transitions", base(f.a, t)),
        &tk,
        true,
        &json!({"to": "resolved", "version": 1, "summary": "Ringkasan uji (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    // Resolved: new related work needs a reopen first.
    let r = write(
        &pool,
        "POST",
        &format!("{}/related-work", base(f.a, t)),
        &tk,
        true,
        &json!({"title": "Terlambat", "blocking": true}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    let d = detail(&pool, f.a, t, &tk).await;
    assert_eq!(d["resolutions"][0]["action"], "resolve");
    assert_eq!(audit_count(&pool, t, "related_work.create").await, 1);
    assert_eq!(audit_count(&pool, t, "related_work.complete").await, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn activity_pages_cover_audit_without_free_text(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let tk = tok(&pool, f.lead).await;
    for i in 1..=5 {
        let r = write(
            &pool,
            "PATCH",
            &base(f.a, t),
            &tk,
            true,
            &json!({"version": i, "cause": format!("Rahasia {i} (dummy)")}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!(
                "?limit=2&cursor={}",
                c.replace('+', "%2B").replace(':', "%3A")
            ),
            None => "?limit=2".to_owned(),
        };
        let r = get(&pool, &format!("{}/activity{q}", base(f.a, t)), &tk).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
        assert!(!r.raw.contains("Rahasia"));
        for it in r.json["items"].as_array().into_iter().flatten() {
            assert_eq!(it["label"], "Mengubah penyebab");
            seen.push(it["id"].as_str().unwrap_or_default().to_owned());
        }
        cursor = r.json["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 5);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5);
    for bad in ["?limit=0", "?limit=101", "?cursor=zzz", "?other=1"] {
        let r = get(&pool, &format!("{}/activity{bad}", base(f.a, t)), &tk).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn detail_responses_conform_to_openapi(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let tk = tok(&pool, f.lead).await;
    let root: Value = ok(
        serde_json::from_str(include_str!("../../openapi/openapi.json")),
        "openapi",
    );
    let schemas = &root["components"]["schemas"];
    let r = write(
        &pool,
        "POST",
        &format!("{}/related-work", base(f.a, t)),
        &tk,
        true,
        &json!({"title": "Uji kontrak (dummy)", "safeDescription": "Deskripsi (dummy)", "blocking": false}),
    )
    .await;
    conform(&r.json, &schemas["TicketDetailMutation"], &root, "related");
    let r = write(
        &pool,
        "PATCH",
        &base(f.a, t),
        &tk,
        true,
        &json!({"version": 1, "regionId": null}),
    )
    .await;
    conform(
        &r.json,
        &schemas["TicketDetailMutation"],
        &root,
        "noop patch",
    );
    let d = detail(&pool, f.a, t, &tk).await;
    conform(&d, &schemas["TicketDetail"], &root, "detail");
    let a = get(&pool, &format!("{}/activity", base(f.a, t)), &tk).await;
    conform(&a.json, &schemas["ActivityPage"], &root, "activity");
}

fn conform(v: &Value, schema: &Value, root: &Value, at: &str) {
    if let Some(r) = schema["$ref"].as_str() {
        let name = r.trim_start_matches("#/components/schemas/");
        return conform(v, &root["components"]["schemas"][name], root, at);
    }
    if let Some(alts) = schema["oneOf"].as_array() {
        if v.is_null() && alts.iter().any(|a| a["type"] == "null") {
            return;
        }
        let first = alts.iter().find(|a| a["type"] != "null");
        return conform(v, first.unwrap_or(&Value::Null), root, at);
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
                conform(x, p.unwrap_or(&Value::Null), root, &format!("{at}.{k}"));
            }
        }
        Value::Array(items) => {
            assert!(types.contains(&"array"), "{at}: unexpected array");
            for (i, x) in items.iter().enumerate() {
                conform(x, &schema["items"], root, &format!("{at}[{i}]"));
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
