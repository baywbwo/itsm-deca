//! D17 editable workspace code against a real PostgreSQL: a superadmin renames the business
//! code (DUMA → DECA) through Edit perusahaan; internal ids stay, old tickets keep their
//! number in list, detail, search and report exports (stored artifacts and their file names
//! unchanged), new tickets use the new code with the counter continuing, every code a workspace
//! ever used stays reserved for it, the reporter code is untouched, non-superadmins and stale
//! versions change nothing, concurrent renames to one code serialise, and migration 0014 only
//! rolls back once no ticket carries a code other than its workspace's. Fixtures are dummy.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::{AppState, app, db, seed, session};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
    bravo: Uuid,
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
        bravo: scalar_uuid(pool, user, "dummy:cs-bravo").await,
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

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    json: Value,
    bytes: Vec<u8>,
}

async fn send(
    pool: &PgPool,
    method: &str,
    uri: &str,
    t: &Tok,
    key: Option<&str>,
    body: Option<&Value>,
) -> Resp {
    let mut r = Request::builder().method(method).uri(uri).header(
        header::COOKIE,
        format!("{}={}", session::cookie_name(false), t.token),
    );
    if method != "GET" {
        r = r.header(session::CSRF_HEADER, &t.csrf);
    }
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
    let headers = res.headers().clone();
    let bytes = ok(res.into_body().collect().await, "body")
        .to_bytes()
        .to_vec();
    Resp {
        status,
        headers,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        bytes,
    }
}

async fn get(pool: &PgPool, uri: &str, t: &Tok) -> Resp {
    send(pool, "GET", uri, t, None, None).await
}

fn ws_uri(ws: Uuid, rest: &str) -> String {
    format!("/api/v1/workspaces/{ws}/{rest}")
}

fn s(v: &Value) -> String {
    match v.as_str() {
        Some(x) => x.to_owned(),
        None => panic!("not a string: {v}"),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Ticket received inside October 2026 (Jakarta), so it is in the report period.
async fn ticket(pool: &PgPool, ws: Uuid, by: Uuid, subject: &str) -> (Uuid, i64) {
    ok(
        sqlx::query_as(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, \
               intake_channel_id, priority_id, category_id) \
             VALUES ($1, $2, 'Kronologi uji (dummy)', '2026-10-15T03:00:00Z', $3, \
               (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'), \
               (SELECT id FROM priorities WHERE workspace_id = $1 AND code = 'normal'), \
               (SELECT id FROM categories WHERE workspace_id = $1 AND code = 'kategori_uji')) \
             RETURNING id, number",
        )
        .bind(ws)
        .bind(subject)
        .bind(by)
        .fetch_one(pool)
        .await,
        "insert ticket",
    )
}

/// The company row of a workspace as the superadmin's company list shows it.
async fn company(pool: &PgPool, t: &Tok, ws: Uuid) -> Value {
    let list = get(pool, "/api/v1/companies", t).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    list.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["workspaceId"] == json!(ws)))
        .cloned()
        .unwrap_or_else(|| panic!("no company for {ws}: {}", list.json))
}

fn profile(c: &Value, code: &str) -> Value {
    json!({"code": code, "workspaceName": c["workspaceName"], "companyName": c["companyName"],
           "timezone": c["timezone"], "reporterCode": c["reporterCode"],
           "brandKey": c["brandKey"], "brandName": c["brandName"], "version": c["version"]})
}

async fn rename(pool: &PgPool, t: &Tok, c: &Value, code: &str) -> Resp {
    let uri = format!("/api/v1/companies/{}/profile", s(&c["companyId"]));
    send(pool, "PATCH", &uri, t, None, Some(&profile(c, code))).await
}

async fn code_of(pool: &PgPool, ws: Uuid) -> (String, String) {
    ok(
        sqlx::query_as(
            "SELECT w.code, c.code FROM workspaces w JOIN companies c ON c.id = w.company_id \
             WHERE w.id = $1",
        )
        .bind(ws)
        .fetch_one(pool)
        .await,
        "codes",
    )
}

async fn numbers(pool: &PgPool, ws: Uuid, t: &Tok, q: &str) -> Vec<String> {
    let r = get(pool, &ws_uri(ws, &format!("tickets?{q}")), t).await;
    assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.json);
    let mut v: Vec<String> = r.json["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| s(&i["displayNumber"]))
        .collect();
    v.sort();
    v
}

async fn export(pool: &PgPool, ws: Uuid, t: &Tok, key: &str) -> Value {
    let body = json!({"reportType": "operational", "periodStart": "2026-10-01",
                      "periodEnd": "2026-10-31", "basis": "received"});
    let r = send(
        pool,
        "POST",
        &ws_uri(ws, "report-runs"),
        t,
        Some(key),
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    r.json["run"].clone()
}

async fn download(pool: &PgPool, ws: Uuid, t: &Tok, run: &str) -> Resp {
    let r = get(pool, &ws_uri(ws, &format!("report-runs/{run}/content")), t).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r
}

fn disposition(r: &Resp) -> String {
    r.headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn rename_keeps_old_numbers_everywhere_and_new_tickets_use_the_new_code(pool: PgPool) {
    let f = fixture(&pool).await;
    let (t1, n1) = ticket(&pool, f.a, f.admin, "Tiket lama satu (dummy)").await;
    let (_, n2) = ticket(&pool, f.a, f.admin, "Tiket lama dua (dummy)").await;
    ticket(&pool, f.b, f.admin, "Tiket Beta (dummy)").await;
    assert_eq!((n1, n2), (1, 2));
    let admin = tok(&pool, f.admin).await;
    let cs = tok(&pool, f.cs).await;

    // An export made before the rename.
    let before = export(&pool, f.a, &cs, "d17-before-00001").await;
    let before_id = s(&before["id"]);
    let before_name = s(&before["fileName"]);
    assert!(before_name.starts_with("DUMA-"), "{before_name}");
    let before_dl = download(&pool, f.a, &cs, &before_id).await;
    let before_sha = hex(&Sha256::digest(&before_dl.bytes));

    // Rename DUMA → DECA. Ids stay; workspace and company code move together.
    let duma = company(&pool, &admin, f.a).await;
    assert_eq!(duma["workspaceCode"], "DUMA");
    let r = rename(&pool, &admin, &duma, "DECA").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["workspaceId"], json!(f.a));
    assert_eq!(r.json["item"]["companyId"], duma["companyId"]);
    assert_eq!(r.json["item"]["workspaceCode"], "DECA");
    assert_eq!(r.json["item"]["companyCode"], "DECA");
    assert_eq!(
        r.json["item"]["reporterCode"], duma["reporterCode"],
        "reporter code is separate and never derived from the workspace code"
    );
    assert_eq!(code_of(&pool, f.a).await, ("DECA".into(), "DECA".into()));
    let audit: Value = ok(
        sqlx::query_scalar(
            "SELECT safe_change FROM audit_events WHERE action = 'company.update' AND id = $1",
        )
        .bind(ok(Uuid::parse_str(&s(&r.json["auditEventId"])), "audit id"))
        .fetch_one(&pool)
        .await,
        "audit",
    );
    assert_eq!(audit["changed"], json!(["code"]));
    assert_eq!(audit["previousCode"], "DUMA");
    assert_eq!(audit["code"], "DECA");

    // Settings (reload) show the current code and the reserved earlier one.
    let st = get(&pool, &ws_uri(f.a, "settings"), &cs).await;
    assert_eq!(st.status, StatusCode::OK, "{}", st.json);
    assert_eq!(st.json["settings"]["workspaceCode"], "DECA");
    assert_eq!(st.json["settings"]["previousCodes"], json!(["DUMA"]));

    // Old numbers: list, detail (deep link by id), search by old number.
    assert_eq!(numbers(&pool, f.a, &cs, "").await, ["DUMA-1", "DUMA-2"]);
    let d = get(&pool, &ws_uri(f.a, &format!("tickets/{t1}")), &cs).await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.json);
    assert_eq!(d.json["displayNumber"], "DUMA-1");
    assert_eq!(numbers(&pool, f.a, &cs, "q=DUMA-1").await, ["DUMA-1"]);
    assert_eq!(numbers(&pool, f.a, &cs, "q=duma-2").await, ["DUMA-2"]);
    assert_eq!(numbers(&pool, f.a, &cs, "q=1").await, ["DUMA-1"]);
    assert!(
        numbers(&pool, f.a, &cs, "q=DECA-1").await.is_empty(),
        "an old ticket is not renumbered under the new code"
    );

    // A new ticket through intake: new code, counter continues.
    let lead = tok(&pool, f.lead).await;
    let ch: Uuid = ok(
        sqlx::query_scalar(
            "SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'",
        )
        .bind(f.a)
        .fetch_one(&pool)
        .await,
        "channel",
    );
    let dir = get(&pool, &ws_uri(f.a, "intake-directory"), &lead).await;
    assert_eq!(dir.status, StatusCode::OK, "{}", dir.json);
    let body = json!({"subject": "Pengaduan baru D17 (dummy)",
                      "chronology": "Kronologi uji (dummy) setelah ganti kode.",
                      "receivedAt": "2026-10-30T09:15:00+07:00", "intakeChannelId": ch,
                      "jira": {"mode": "create", "title": "Tindak lanjut uji (dummy)",
                               "assigneeId": dir.json["jira"]["options"][0]["id"]}});
    let c = send(
        &pool,
        "POST",
        &ws_uri(f.a, "tickets"),
        &lead,
        Some("d17-create-00001"),
        Some(&body),
    )
    .await;
    assert_eq!(c.status, StatusCode::CREATED, "{}", c.json);
    assert_eq!(c.json["ticket"]["number"], 3, "no reset, no duplicate");
    let jira = c.json["integrations"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["provider"] == "jira"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        jira["issueKey"], "SIM-DECA-3",
        "simulated Jira key follows the ticket"
    );
    let new_id = s(&c.json["ticket"]["id"]);
    let d = get(&pool, &ws_uri(f.a, &format!("tickets/{new_id}")), &cs).await;
    assert_eq!(d.json["displayNumber"], "DECA-3");
    assert_eq!(
        numbers(&pool, f.a, &cs, "").await,
        ["DECA-3", "DUMA-1", "DUMA-2"]
    );
    assert_eq!(numbers(&pool, f.a, &cs, "q=DECA-3").await, ["DECA-3"]);
    assert!(numbers(&pool, f.a, &cs, "q=DUMA-3").await.is_empty());

    // A new export lists each ticket under its own number; the old run is untouched.
    let after = export(&pool, f.a, &cs, "d17-after-000001").await;
    assert!(s(&after["fileName"]).starts_with("DECA-"), "{after}");
    let after_dl = download(&pool, f.a, &cs, &s(&after["id"])).await;
    let text = String::from_utf8_lossy(&after_dl.bytes).into_owned();
    for n in ["\"DUMA-1\"", "\"DUMA-2\"", "\"DECA-3\""] {
        assert!(text.contains(n), "{n} in {text}");
    }
    let runs = get(&pool, &ws_uri(f.a, "report-runs"), &cs).await;
    let old = runs.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == json!(before_id)))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        old["fileName"],
        json!(before_name),
        "old file keeps its name"
    );
    let again = download(&pool, f.a, &cs, &before_id).await;
    assert_eq!(hex(&Sha256::digest(&again.bytes)), before_sha);
    assert!(disposition(&again).contains(&before_name));

    // Company switch: Beta is unaffected, a member of Alfa cannot read Beta, and Beta cannot
    // take either code.
    assert_eq!(numbers(&pool, f.b, &admin, "").await, ["DUMB-1"]);
    assert_eq!(
        numbers(&pool, f.a, &admin, "").await,
        ["DECA-3", "DUMA-1", "DUMA-2"]
    );
    let other = get(&pool, &ws_uri(f.b, "tickets"), &cs).await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.json);
    let dumb = company(&pool, &admin, f.b).await;
    for code in ["DECA", "DUMA"] {
        let r = rename(&pool, &admin, &dumb, code).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{code}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["field"], "code");
    }
    // A new company cannot take a historical code either.
    let r = send(
        &pool,
        "POST",
        "/api/v1/companies",
        &admin,
        None,
        Some(&json!({"code": "DUMA", "name": "PT Peniru (dummy)",
                     "timezone": "Asia/Jakarta", "reporterCode": null})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(code_of(&pool, f.b).await, ("DUMB".into(), "DUMB".into()));

    // Format is validated by the server; the UI uppercasing is only a convenience.
    let deca = company(&pool, &admin, f.a).await;
    for bad in ["deca", "D", "DE-CA", "ABCDEFGHIJK", " "] {
        let r = rename(&pool, &admin, &deca, bad).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad:?}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["field"], "code");
    }
    // The owner may return to its own earlier code; the next number still continues.
    let r = rename(&pool, &admin, &deca, "DUMA").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let (_, n4) = ticket(&pool, f.a, f.admin, "Kembali ke kode lama (dummy)").await;
    assert_eq!(n4, 4);
    assert_eq!(
        numbers(&pool, f.a, &cs, "").await,
        ["DECA-3", "DUMA-1", "DUMA-2", "DUMA-4"]
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn only_a_superadmin_with_the_current_version_changes_the_code(pool: PgPool) {
    let f = fixture(&pool).await;
    ticket(&pool, f.a, f.admin, "Tiket (dummy)").await;
    // A workspace admin grant is not platform administration.
    let m: Uuid = ok(
        sqlx::query_scalar(
            "SELECT id FROM workspace_memberships WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(f.a)
        .bind(f.lead)
        .fetch_one(&pool)
        .await,
        "membership",
    );
    ok(
        sqlx::query(
            "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by) \
             VALUES ($1, $2, 'admin.settings', $3)",
        )
        .bind(f.a)
        .bind(m)
        .bind(f.admin)
        .execute(&pool)
        .await,
        "grant",
    );
    let admin = tok(&pool, f.admin).await;
    let duma = company(&pool, &admin, f.a).await;
    for (who, uid) in [
        ("cs", f.cs),
        ("lead admin.settings", f.lead),
        ("bravo", f.bravo),
    ] {
        let t = tok(&pool, uid).await;
        let r = rename(&pool, &t, &duma, "DECA").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who}: {}", r.json);
    }
    let mut stale = duma.clone();
    stale["version"] = json!(duma["version"].as_i64().unwrap_or_default() + 5);
    let r = rename(&pool, &admin, &stale, "DECA").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(code_of(&pool, f.a).await, ("DUMA".into(), "DUMA".into()));
    let reserved: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspace_code_history WHERE code = 'DECA'")
            .fetch_one(&pool)
            .await,
        "history",
    );
    assert_eq!(reserved, 0, "a refused change reserves nothing");
    let audits: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'company.update'")
            .fetch_one(&pool)
            .await,
        "audits",
    );
    assert_eq!(audits, 0);
    // The workspace settings endpoint cannot change the code at all.
    let cs = tok(&pool, f.cs).await;
    let st = get(&pool, &ws_uri(f.a, "settings"), &cs).await;
    assert_eq!(st.json["settings"]["previousCodes"], json!([]));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn concurrent_renames_to_one_code_have_one_winner(pool: PgPool) {
    let f = fixture(&pool).await;
    let admin = tok(&pool, f.admin).await;
    let (a, b) = (
        company(&pool, &admin, f.a).await,
        company(&pool, &admin, f.b).await,
    );
    let (ra, rb) = tokio::join!(
        rename(&pool, &admin, &a, "SAMA"),
        rename(&pool, &admin, &b, "SAMA")
    );
    let mut st = [ra.status, rb.status];
    st.sort();
    assert_eq!(
        st,
        [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY],
        "{} / {}",
        ra.json,
        rb.json
    );
    let owners: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE code = 'SAMA'")
            .fetch_one(&pool)
            .await,
        "owners",
    );
    assert_eq!(owners, 1);
    let audits: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'company.update'")
            .fetch_one(&pool)
            .await,
        "audits",
    );
    assert_eq!(audits, 1, "the loser rolled back its audit row too");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0014_rolls_back_only_without_renamed_history(pool: PgPool) {
    let f = fixture(&pool).await;
    ticket(&pool, f.a, f.admin, "Tiket (dummy)").await;
    let admin = tok(&pool, f.admin).await;
    let r = rename(&pool, &admin, &company(&pool, &admin, f.a).await, "DECA").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(
        db::MIGRATOR.undo(&pool, 13).await.is_err(),
        "rollback would renumber DUMA-1 as DECA-1"
    );
    let still: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspace_code_history")
            .fetch_one(&pool)
            .await,
        "history kept",
    );
    assert!(still >= 3);
    // The refused undo already reverted 0015+; restore the full schema before using the API.
    ok(
        db::MIGRATOR.run(&pool).await,
        "reapply after refused rollback",
    );

    // Back to the tickets' code: the rollback is allowed and the D02 freeze returns.
    let r = rename(&pool, &admin, &company(&pool, &admin, f.a).await, "DUMA").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    ok(db::MIGRATOR.undo(&pool, 13).await, "undo to 0013");
    let frozen = sqlx::query("UPDATE workspaces SET code = 'DECA' WHERE id = $1")
        .bind(f.a)
        .execute(&pool)
        .await;
    assert!(frozen.is_err(), "D02 rule back after rollback");
    let tickets: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&pool)
            .await,
        "tickets",
    );
    assert_eq!(tickets, 1, "rows kept");

    ok(db::MIGRATOR.run(&pool).await, "reapply");
    let prefix: String = ok(
        sqlx::query_scalar("SELECT number_prefix FROM tickets WHERE workspace_id = $1")
            .bind(f.a)
            .fetch_one(&pool)
            .await,
        "prefix",
    );
    assert_eq!(prefix, "DUMA");
    let cs = tok(&pool, f.cs).await;
    assert_eq!(numbers(&pool, f.a, &cs, "").await, ["DUMA-1"]);
    let r = rename(&pool, &admin, &company(&pool, &admin, f.a).await, "DECA").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "editable again after reapply: {}",
        r.json
    );
}
