//! D10 operational reports against a real PostgreSQL: CS and Lead CS may preview and export
//! their own workspace only (foreign workspace / non-member = 404), the period is whole
//! Jakarta days (midnight boundaries), received vs resolved basis with unique tickets and
//! resolution cycles kept apart, filters narrow totals and rows identically, the export CSV
//! equals the preview scope row for row and its SHA-256 equals the stored artifact and the
//! download header, formula cells are neutralized, idempotent replay / conflict, the BI
//! regulator type is refused (422) without creating a run, too many rows = failed run (never a
//! truncated file), expired artifacts answer 410, and every export / download is audited.
//! Fixtures are synthetic and labelled dummy.

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

fn state(pool: &PgPool, max_rows: Option<&str>) -> AppState {
    let mut vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    if let Some(m) = max_rows {
        vars.insert("ITSM_REPORT_MAX_ROWS".to_owned(), m.to_owned());
    }
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

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
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
    st: &AppState,
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
    let res = match app(st.clone()).oneshot(req).await {
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

async fn get(st: &AppState, uri: &str, t: &Tok) -> Resp {
    send(st, "GET", uri, t, None, None).await
}

async fn export(st: &AppState, ws: Uuid, t: &Tok, key: &str, body: &Value) -> Resp {
    send(
        st,
        "POST",
        &format!("/api/v1/workspaces/{ws}/report-runs"),
        t,
        Some(key),
        Some(body),
    )
    .await
}

fn preview_uri(ws: Uuid, q: &str) -> String {
    format!("/api/v1/workspaces/{ws}/reports/preview?{q}")
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

/// Ticket with an explicit UTC receive instant, optional owner and subject.
async fn ticket(
    pool: &PgPool,
    ws: Uuid,
    by: Uuid,
    owner: Option<Uuid>,
    received_utc: &str,
    subject: &str,
) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, \
               owner_user_id, intake_channel_id, priority_id, category_id) \
             VALUES ($1, $2, 'Kronologi uji (dummy)', $3::timestamptz, $4, $5, \
               (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'), \
               (SELECT id FROM priorities WHERE workspace_id = $1 AND code = 'normal'), \
               (SELECT id FROM categories WHERE workspace_id = $1 AND code = 'kategori_uji')) \
             RETURNING id",
        )
        .bind(ws)
        .bind(subject)
        .bind(received_utc)
        .bind(by)
        .bind(owner)
        .fetch_one(pool)
        .await,
        "insert ticket",
    )
}

/// Applies resolve / reopen events in order and moves the ticket like the workflow does.
async fn lifecycle(pool: &PgPool, ws: Uuid, t: Uuid, steps: &[(i32, &str, &str)], by: Uuid) {
    for &(cycle, action, at) in steps {
        ok(
            sqlx::query(
                "INSERT INTO ticket_resolution_events (workspace_id, ticket_id, cycle_no, action, \
                   summary, actor_id, occurred_at) \
                 VALUES ($1, $2, $3, $4, 'Ringkasan uji (dummy)', $5, $6::timestamptz)",
            )
            .bind(ws)
            .bind(t)
            .bind(cycle)
            .bind(action)
            .bind(by)
            .bind(at)
            .execute(pool)
            .await,
            "event",
        );
        let status = if action == "resolve" {
            "resolved"
        } else {
            "open"
        };
        ok(
            sqlx::query(
                "UPDATE tickets SET workflow_status = $2, cycle_no = $3, version = version + 1 \
                 WHERE id = $1",
            )
            .bind(t)
            .bind(status)
            .bind(cycle)
            .execute(pool)
            .await,
            "status",
        );
    }
}

struct Data {
    t_first_second: Uuid,
}

/// Period under test: 2026-10-01 ..= 2026-10-31 Jakarta (UTC+7).
async fn data(pool: &PgPool, f: &Fx) -> Data {
    let (a, by) = (f.a, f.admin);
    // 1 s before the Jakarta period starts / ends → outside.
    ticket(
        pool,
        a,
        by,
        None,
        "2026-09-30T16:59:59Z",
        "Sebelum periode (dummy)",
    )
    .await;
    ticket(
        pool,
        a,
        by,
        None,
        "2026-10-31T17:00:00Z",
        "Sesudah periode (dummy)",
    )
    .await;
    // First second / last second of the period → inside. Formula-like subject.
    let t_first_second = ticket(
        pool,
        a,
        by,
        Some(f.cs),
        "2026-09-30T17:00:00Z",
        "=HYPERLINK(\"http://contoh.invalid\",\"klik\") (dummy)",
    )
    .await;
    ticket(
        pool,
        a,
        by,
        None,
        "2026-10-31T16:59:59Z",
        "@SUM(A1) detik terakhir (dummy)",
    )
    .await;
    // Received in period, resolved twice in period (two cycles).
    let two = ticket(
        pool,
        a,
        by,
        None,
        "2026-10-05T03:00:00Z",
        "Dua siklus (dummy)",
    )
    .await;
    lifecycle(
        pool,
        a,
        two,
        &[
            (1, "resolve", "2026-10-10T03:00:00Z"),
            (2, "reopen", "2026-10-11T03:00:00Z"),
            (2, "resolve", "2026-10-12T03:00:00Z"),
        ],
        by,
    )
    .await;
    // Received before the period, resolved inside it → resolved basis only.
    let old = ticket(
        pool,
        a,
        by,
        None,
        "2026-09-15T03:00:00Z",
        "Lama selesai (dummy)",
    )
    .await;
    lifecycle(pool, a, old, &[(1, "resolve", "2026-10-05T03:00:00Z")], by).await;
    // Resolved just after the period end (Nov 1 00:00 Jakarta) → not counted.
    let late = ticket(
        pool,
        a,
        by,
        None,
        "2026-09-16T03:00:00Z",
        "Selesai November (dummy)",
    )
    .await;
    lifecycle(pool, a, late, &[(1, "resolve", "2026-10-31T17:00:00Z")], by).await;
    // Other workspace, inside the period: never part of DUMA's report.
    let tb = ticket(
        pool,
        f.b,
        by,
        None,
        "2026-10-10T03:00:00Z",
        "Workspace B (dummy)",
    )
    .await;
    lifecycle(pool, f.b, tb, &[(1, "resolve", "2026-10-11T03:00:00Z")], by).await;
    Data { t_first_second }
}

const OCT: &str = "periodStart=2026-10-01&periodEnd=2026-10-31";

fn oct_body(basis: &str) -> Value {
    json!({"reportType": "operational", "periodStart": "2026-10-01", "periodEnd": "2026-10-31", "basis": basis})
}

/// Data lines of a CSV artifact (header excluded), as raw text.
fn csv_lines(bytes: &[u8]) -> Vec<String> {
    let text = ok(std::str::from_utf8(bytes), "utf8");
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or_else(|| panic!("BOM"));
    let mut lines: Vec<String> = text.split("\r\n").map(str::to_owned).collect();
    assert_eq!(lines.pop().as_deref(), Some(""), "ends with CRLF");
    assert!(lines[0].starts_with("\"jenis_laporan\",\"kode_perusahaan\""));
    lines.remove(0);
    lines
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn preview_scope_period_and_basis(pool: PgPool) {
    let f = fixture(&pool).await;
    let d = data(&pool, &f).await;
    let st = state(&pool, None);
    let cs = tok(&pool, f.cs).await;

    let r = get(&st, &preview_uri(f.a, OCT), &cs).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let p = &r.json;
    assert_eq!(p["reportType"], "operational");
    assert_eq!(p["header"]["companyCode"], "DUMA");
    assert_eq!(p["header"]["timezone"], "Asia/Jakarta");
    assert_eq!(p["header"]["startAt"], "2026-09-30T17:00:00.000000Z");
    assert_eq!(p["header"]["endAt"], "2026-10-31T17:00:00.000000Z");
    assert!(p["header"]["reporterCode"].is_null());
    let bi = &p["reportTypes"][1];
    assert_eq!(bi["code"], "bi_regulator");
    assert_eq!(bi["available"], false);
    let codes: Vec<String> = p["validation"]
        .as_array()
        .map(|v| v.iter().map(|i| s(&i["code"])).collect())
        .unwrap_or_default();
    assert!(codes.contains(&"regulator_mapping_unavailable".to_owned()));
    assert!(codes.contains(&"reporter_code_missing".to_owned()));
    assert!(p["notice"].as_str().is_some_and(|n| n.contains("bukan")));

    // received: first second, last second, two-cycle ticket.
    let t = &p["totals"];
    assert_eq!(t["tickets"], 3, "{t}");
    assert_eq!(t["resolutionCycles"], 2);
    assert_eq!(t["resolvedTickets"], 1);
    assert_eq!(t["unassigned"], 2);
    assert_eq!(p["rows"].as_array().map(Vec::len), Some(3));
    assert_eq!(p["rows"][0]["ticketId"], json!(d.t_first_second));
    assert_eq!(p["rows"][0]["receivedLocal"], "2026-10-01 00:00");
    assert_eq!(p["rows"][2]["receivedLocal"], "2026-10-31 23:59");
    let status_sum: i64 = p["byStatus"]
        .as_array()
        .map(|v| v.iter().filter_map(|b| b["count"].as_i64()).sum())
        .unwrap_or(0);
    assert_eq!(p["byStatus"].as_array().map(Vec::len), Some(6));
    assert_eq!(status_sum, 3);
    for k in ["byPriority", "byChannel", "byCategory", "byOwner"] {
        let n: i64 = p[k]
            .as_array()
            .map(|v| v.iter().filter_map(|b| b["count"].as_i64()).sum())
            .unwrap_or(0);
        assert_eq!(n, 3, "{k}");
    }

    // resolved: two-cycle ticket + old ticket; cycles 3, distinct 2; Nov-1 event excluded.
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&basis=resolved")),
        &cs,
    )
    .await;
    let t = &r.json["totals"];
    assert_eq!(
        (
            t["tickets"].as_i64(),
            t["resolutionCycles"].as_i64(),
            t["resolvedTickets"].as_i64()
        ),
        (Some(2), Some(3), Some(2)),
        "{t}"
    );
    assert_eq!(r.json["header"]["basis"], "resolved");

    // Single day = the two boundary days.
    let r = get(
        &st,
        &preview_uri(f.a, "periodStart=2026-10-01&periodEnd=2026-10-01"),
        &cs,
    )
    .await;
    assert_eq!(r.json["totals"]["tickets"], 1);
    let r = get(
        &st,
        &preview_uri(f.a, "periodStart=2026-10-31&periodEnd=2026-10-31"),
        &cs,
    )
    .await;
    assert_eq!(r.json["totals"]["tickets"], 1);

    // Filters: status, owner (unassigned / id), totals = rows.
    let r = get(&st, &preview_uri(f.a, &format!("{OCT}&status=new")), &cs).await;
    assert_eq!(r.json["totals"]["tickets"], 2);
    assert_eq!(r.json["filters"]["status"], json!(["new"]));
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&status=resolved,new")),
        &cs,
    )
    .await;
    assert_eq!(r.json["totals"]["tickets"], 3);
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&owner=unassigned")),
        &cs,
    )
    .await;
    assert_eq!(r.json["totals"]["tickets"], 2);
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&owner={}", f.cs)),
        &cs,
    )
    .await;
    assert_eq!(r.json["totals"]["tickets"], 1);
    assert_eq!(r.json["rows"].as_array().map(Vec::len), Some(1));

    // Independent SQL count equals the preview (received basis, Jakarta days).
    let n: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM tickets WHERE workspace_id = $1 \
               AND (received_at AT TIME ZONE 'Asia/Jakarta')::date BETWEEN '2026-10-01' AND '2026-10-31'",
        )
        .bind(f.a)
        .fetch_one(&pool)
        .await,
        "independent count",
    );
    assert_eq!(n, 3);

    // Lead CS sees the same.
    let lead = tok(&pool, f.lead).await;
    let r = get(&st, &preview_uri(f.a, OCT), &lead).await;
    assert_eq!(r.json["totals"]["tickets"], 3);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn preview_input_and_scope_are_validated(pool: PgPool) {
    let f = fixture(&pool).await;
    data(&pool, &f).await;
    let st = state(&pool, None);
    let cs = tok(&pool, f.cs).await;
    let field = |r: &Resp| r.json["error"]["field"].as_str().map(str::to_owned);

    for (q, want) in [
        ("periodEnd=2026-10-31", "periodStart"),
        ("periodStart=2026-10-01", "periodEnd"),
        ("periodStart=2026-10-31&periodEnd=2026-10-01", "periodEnd"),
        ("periodStart=2025-01-01&periodEnd=2026-10-31", "periodEnd"),
        ("periodStart=2026-02-30&periodEnd=2026-03-01", "periodStart"),
        ("periodStart=01-10-2026&periodEnd=2026-10-31", "periodStart"),
    ] {
        let r = get(&st, &preview_uri(f.a, q), &cs).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{q}");
        assert_eq!(field(&r).as_deref(), Some(want), "{q}");
    }
    for (extra, want) in [
        ("basis=updated", "basis"),
        ("status=open,xyz", "status"),
        ("reportType=official", "reportType"),
        ("owner=NOT-A-UUID", "owner"),
    ] {
        let r = get(&st, &preview_uri(f.a, &format!("{OCT}&{extra}")), &cs).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{extra}");
        assert_eq!(field(&r).as_deref(), Some(want), "{extra}");
    }
    // A catalog row of the other workspace / a non-member as PIC.
    let b_cat: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM categories WHERE workspace_id = $1 LIMIT 1")
            .bind(f.b)
            .fetch_one(&pool)
            .await,
        "b category",
    );
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&category={b_cat}")),
        &cs,
    )
    .await;
    assert_eq!(field(&r).as_deref(), Some("category"));
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&owner={}", f.bravo)),
        &cs,
    )
    .await;
    assert_eq!(field(&r).as_deref(), Some("owner"));
    // Unknown parameter (e.g. a company id) = 400.
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&companyId={}", f.b)),
        &cs,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Regulator type held.
    let r = get(
        &st,
        &preview_uri(f.a, &format!("{OCT}&reportType=bi_regulator")),
        &cs,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json["error"]["code"], "regulator_mapping_unavailable");

    // CS is not a member of DUMB; cs-bravo is a member of nothing.
    let r = get(&st, &preview_uri(f.b, OCT), &cs).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let bravo = tok(&pool, f.bravo).await;
    let r = get(&st, &preview_uri(f.a, OCT), &bravo).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(
        &st,
        &format!("/api/v1/workspaces/{}/report-runs", f.a),
        &bravo,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn export_matches_preview_and_download_is_scoped(pool: PgPool) {
    let f = fixture(&pool).await;
    data(&pool, &f).await;
    let st = state(&pool, None);
    let cs = tok(&pool, f.cs).await;

    // No / malformed key = 400; regulator = 422 without a run.
    let none = send(
        &st,
        "POST",
        &format!("/api/v1/workspaces/{}/report-runs", f.a),
        &cs,
        None,
        Some(&oct_body("received")),
    )
    .await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);
    let mut bi = oct_body("received");
    bi["reportType"] = json!("bi_regulator");
    let r = export(&st, f.a, &cs, "d10-bi-000001", &bi).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json["error"]["code"], "regulator_mapping_unavailable");
    let mut extra = oct_body("received");
    extra["companyId"] = json!(f.b);
    let r = export(&st, f.a, &cs, "d10-extra-0001", &extra).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let runs: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM report_runs")
            .fetch_one(&pool)
            .await,
        "runs",
    );
    assert_eq!(runs, 0);

    let r = export(&st, f.a, &cs, "d10-exp-000001", &oct_body("received")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["replayed"], false);
    let run = r.json["run"].clone();
    assert_eq!(run["state"], "succeeded");
    assert_eq!(run["rowCount"], 3);
    assert_eq!(run["downloadable"], true);
    assert_eq!(run["hasArtifact"], true);
    assert_eq!(run["requestedBy"]["id"], json!(f.cs));
    assert!(
        s(&run["fileName"])
            .starts_with("DUMA-rekap-operasional-NON-REGULATOR-2026-10-01_sd_2026-10-31-received-")
    );
    let id = s(&run["id"]);

    // Replay with the same key + body: same run, nothing new. Other body: 409.
    let r2 = export(&st, f.a, &cs, "d10-exp-000001", &oct_body("received")).await;
    assert_eq!(r2.status, StatusCode::OK);
    assert_eq!(r2.json["replayed"], true);
    assert_eq!(r2.json["run"]["id"], json!(id));
    let r3 = export(&st, f.a, &cs, "d10-exp-000001", &oct_body("resolved")).await;
    assert_eq!(r3.status, StatusCode::CONFLICT);
    assert_eq!(r3.json["error"]["code"], "idempotency_conflict");
    // Same key, another user = independent.
    let lead = tok(&pool, f.lead).await;
    let r4 = export(&st, f.a, &lead, "d10-exp-000001", &oct_body("resolved")).await;
    assert_eq!(r4.status, StatusCode::CREATED);
    assert_eq!(r4.json["run"]["rowCount"], 2);

    // Download: bytes hash = header = run = DB artifact; rows = preview rows.
    let uri = format!("/api/v1/workspaces/{}/report-runs/{id}/content", f.a);
    let dl = get(&st, &uri, &cs).await;
    assert_eq!(dl.status, StatusCode::OK);
    let h = |k: &str| {
        dl.headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(h("content-type"), "text/csv; charset=utf-8");
    assert!(h("content-disposition").contains(&s(&run["fileName"])));
    assert_eq!(h("cache-control"), "no-store");
    let sha = hex(&Sha256::digest(&dl.bytes));
    assert_eq!(h("x-content-sha256"), sha);
    assert_eq!(run["artifactSha256"], json!(sha));
    assert_eq!(run["artifactBytes"], json!(dl.bytes.len()));
    let (db_sha, db_len): (Vec<u8>, i32) = ok(
        sqlx::query_as(
            "SELECT sha256, length(content) FROM report_artifacts WHERE run_id = $1::uuid",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await,
        "artifact",
    );
    assert_eq!(hex(&db_sha), sha);
    assert_eq!(usize::try_from(db_len).ok(), Some(dl.bytes.len()));
    let lines = csv_lines(&dl.bytes);
    assert_eq!(lines.len(), 3);
    let preview = get(&st, &preview_uri(f.a, OCT), &cs).await;
    for (line, row) in lines
        .iter()
        .zip(preview.json["rows"].as_array().into_iter().flatten())
    {
        assert!(
            line.contains(&format!("\"{}\"", s(&row["displayNumber"]))),
            "{line}"
        );
        assert!(line.starts_with("\"Rekap operasional (bukan format regulator BI)\",\"DUMA\""));
    }
    // Formula cells neutralized, quotes doubled.
    assert!(
        lines[0].contains("\"'=HYPERLINK(\"\"http://contoh.invalid\"\",\"\"klik\"\") (dummy)\""),
        "{}",
        lines[0]
    );
    assert!(
        lines[2].contains("\"'@SUM(A1) detik terakhir (dummy)\""),
        "{}",
        lines[2]
    );
    assert!(lines[0].contains("\"CS Alfa (dummy)\""));
    assert!(lines[1].contains("\"Belum ditugaskan\""));
    assert!(!dl.bytes.windows(15).any(|w| w == b"Workspace B (du"));

    // Audit: export (with sha) + download.
    let (n_exp, n_dl): (i64, i64) = ok(
        sqlx::query_as(
            "SELECT count(*) FILTER (WHERE action = 'report.export' AND safe_change->>'sha256' = $2), \
                    count(*) FILTER (WHERE action = 'report.download' AND outcome = 'success') \
             FROM audit_events WHERE workspace_id = $1 AND entity_id = $3::uuid",
        )
        .bind(f.a)
        .bind(&sha)
        .bind(&id)
        .fetch_one(&pool)
        .await,
        "audit",
    );
    assert_eq!((n_exp, n_dl), (1, 1));

    // Lead may download the CS run (same workspace, report.view); list shows both runs.
    assert_eq!(get(&st, &uri, &lead).await.status, StatusCode::OK);
    let list = get(&st, &format!("/api/v1/workspaces/{}/report-runs", f.a), &cs).await;
    assert_eq!(list.json["items"].as_array().map(Vec::len), Some(2));

    // Foreign workspace path, non-member, CS on DUMB: 404.
    let bravo = tok(&pool, f.bravo).await;
    assert_eq!(get(&st, &uri, &bravo).await.status, StatusCode::NOT_FOUND);
    let wrong = format!("/api/v1/workspaces/{}/report-runs/{id}/content", f.b);
    assert_eq!(get(&st, &wrong, &cs).await.status, StatusCode::NOT_FOUND);
    let admin = tok(&pool, f.admin).await;
    assert_eq!(get(&st, &wrong, &admin).await.status, StatusCode::NOT_FOUND);

    // Tampered artifact = 500, never served.
    ok(
        sqlx::query(
            "UPDATE report_artifacts SET content = content || 'x'::bytea WHERE run_id = $1::uuid",
        )
        .bind(&id)
        .execute(&pool)
        .await,
        "tamper",
    );
    assert_eq!(
        get(&st, &uri, &cs).await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    // Expired = 410 and not downloadable.
    let id2 = s(&r4.json["run"]["id"]);
    ok(
        sqlx::query(
            "UPDATE report_runs SET expires_at = now() - interval '1 second' WHERE id = $1::uuid",
        )
        .bind(&id2)
        .execute(&pool)
        .await,
        "expire",
    );
    let uri2 = format!("/api/v1/workspaces/{}/report-runs/{id2}/content", f.a);
    let r = get(&st, &uri2, &lead).await;
    assert_eq!(r.status, StatusCode::GONE);
    assert_eq!(r.json["error"]["code"], "report_expired");
    let meta = get(
        &st,
        &format!("/api/v1/workspaces/{}/report-runs/{id2}", f.a),
        &lead,
    )
    .await;
    assert_eq!(meta.json["expired"], true);
    assert_eq!(meta.json["downloadable"], false);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn too_many_rows_is_a_failed_run_not_a_truncated_file(pool: PgPool) {
    let f = fixture(&pool).await;
    data(&pool, &f).await;
    let st = state(&pool, Some("2"));
    let cs = tok(&pool, f.cs).await;

    let p = get(&st, &preview_uri(f.a, OCT), &cs).await;
    assert_eq!(p.json["exportable"], false);
    assert_eq!(p.json["exportMaxRows"], 2);
    assert!(p.json["validation"].as_array().is_some_and(|v| {
        v.iter()
            .any(|i| i["code"] == "row_limit" && i["level"] == "blocking")
    }));
    let r = export(&st, f.a, &cs, "d10-limit-0001", &oct_body("received")).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json["run"]["state"], "failed");
    assert_eq!(r.json["run"]["failureCode"], "row_limit");
    assert_eq!(r.json["run"]["downloadable"], false);
    let id = s(&r.json["run"]["id"]);
    let (artifacts, failed_audit): (i64, i64) = ok(
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM report_artifacts WHERE run_id = $1::uuid), \
                    (SELECT count(*) FROM audit_events WHERE entity_id = $1::uuid \
                       AND action = 'report.export' AND outcome = 'failed')",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await,
        "failed run",
    );
    assert_eq!((artifacts, failed_audit), (0, 1));
    let dl = get(
        &st,
        &format!("/api/v1/workspaces/{}/report-runs/{id}/content", f.a),
        &cs,
    )
    .await;
    assert_eq!(dl.status, StatusCode::NOT_FOUND);
    // Narrower filter fits.
    let mut b = oct_body("received");
    b["owner"] = json!("unassigned");
    let r = export(&st, f.a, &cs, "d10-limit-0002", &b).await;
    assert_eq!(r.json["run"]["state"], "succeeded");
    assert_eq!(r.json["run"]["rowCount"], 2);
    assert_eq!(r.json["run"]["filters"]["owner"], "unassigned");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0010_rolls_back_with_runs_and_reapplies(pool: PgPool) {
    let f = fixture(&pool).await;
    data(&pool, &f).await;
    let st = state(&pool, None);
    let cs = tok(&pool, f.cs).await;
    let r = export(&st, f.a, &cs, "d10-mig-000001", &oct_body("received")).await;
    assert_eq!(r.status, StatusCode::CREATED);

    // Contract: artifacts and D10 columns go; the run rows (pre-D10 table) stay.
    ok(db::MIGRATOR.undo(&pool, 9).await, "undo to 0009");
    let cols: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_name = 'report_runs' AND column_name IN \
             ('basis', 'idempotency_key', 'request_hash', 'row_count', 'artifact_sha256', \
              'artifact_bytes', 'expires_at', 'failure_code')",
        )
        .fetch_one(&pool)
        .await,
        "columns",
    );
    assert_eq!(cols, 0);
    let (artifacts, runs): (Option<String>, i64) = ok(
        sqlx::query_as(
            "SELECT to_regclass('report_artifacts')::text, (SELECT count(*) FROM report_runs)",
        )
        .fetch_one(&pool)
        .await,
        "after undo",
    );
    assert_eq!((artifacts, runs), (None, 1));

    // Expand again on top of the kept rows; a fresh export works end to end.
    ok(db::MIGRATOR.run(&pool).await, "reapply");
    let r = export(&st, f.a, &cs, "d10-mig-000002", &oct_body("received")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["run"]["state"], "succeeded");
    let id = s(&r.json["run"]["id"]);
    let dl = get(
        &st,
        &format!("/api/v1/workspaces/{}/report-runs/{id}/content", f.a),
        &cs,
    )
    .await;
    assert_eq!(dl.status, StatusCode::OK);
}

/// D11 (Arka's decision): the operational export keeps `report.view` and is never wider than
/// the preview. For every filter the exported rows are exactly the preview rows; a member whose
/// membership is revoked can neither export nor download what they exported before.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn export_is_never_wider_than_the_preview_and_stops_on_revoke(pool: PgPool) {
    let f = fixture(&pool).await;
    data(&pool, &f).await;
    let st = state(&pool, None);
    let cs = tok(&pool, f.cs).await;
    let cases: [(String, Value); 5] = [
        (String::new(), json!({})),
        ("&basis=resolved".into(), json!({"basis": "resolved"})),
        ("&status=new".into(), json!({"status": ["new"]})),
        ("&owner=unassigned".into(), json!({"owner": "unassigned"})),
        (format!("&owner={}", f.cs), json!({"owner": f.cs})),
    ];
    let mut last = String::new();
    for (i, (q, extra)) in cases.iter().enumerate() {
        let p = get(&st, &preview_uri(f.a, &format!("{OCT}{q}")), &cs).await;
        assert_eq!(p.status, StatusCode::OK, "{q}");
        let mut want: Vec<String> = p.json["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| s(&r["displayNumber"]))
            .collect();
        let mut body = oct_body("received");
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let r = export(&st, f.a, &cs, &format!("d11-wide-{i:06}"), &body).await;
        assert_eq!(r.status, StatusCode::CREATED, "{q}: {}", r.json);
        assert_eq!(r.json["run"]["rowCount"], json!(want.len()), "{q}");
        let id = s(&r.json["run"]["id"]);
        let uri = format!("/api/v1/workspaces/{}/report-runs/{id}/content", f.a);
        let dl = get(&st, &uri, &cs).await;
        assert_eq!(dl.status, StatusCode::OK);
        let mut got: Vec<String> = csv_lines(&dl.bytes)
            .iter()
            .map(|l| {
                want.iter()
                    .find(|n| l.contains(&format!("\"{n}\"")))
                    .cloned()
                    .unwrap_or_else(|| panic!("{q}: exported row not in preview: {l}"))
            })
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{q}");
        last = uri;
    }

    // Revoke CS Alfa's membership: the next export and download are refused (404, no leak).
    ok(
        sqlx::query(
            "UPDATE workspace_memberships SET active = false, version = version + 1 \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(f.a)
        .bind(f.cs)
        .execute(&pool)
        .await,
        "revoke",
    );
    assert_eq!(get(&st, &last, &cs).await.status, StatusCode::NOT_FOUND);
    let r = export(&st, f.a, &cs, "d11-wide-revoked", &oct_body("received")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
