//! D08b follow-up against a real PostgreSQL: operational notes (idempotent, closed = 422,
//! scoped, no free text in audit), the read-only conversation panel (no adapter, simulation
//! rows only), idempotent related-work create (replay, conflict, concurrent = one row), CS and
//! Lead CS editing per the frozen matrix, and attachments quarantine → scan → bind with
//! ACL-checked download.
//!
//! The scanner here is an in-test stand-in speaking the clamd INSTREAM protocol on a local
//! port. It is NOT real clamd and proves only this API's handling of each verdict; a real
//! clamd run is still an open gap. Fixtures are synthetic and labelled dummy.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::{AppState, app, db, seed, session};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use uuid::Uuid;

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

fn state_with(pool: &PgPool, extra: &[(&str, String)]) -> AppState {
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
        vars.insert((*k).to_owned(), v.clone());
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
             VALUES ($1, 'Tindak lanjut uji (dummy)', 'Kronologi uji (dummy)', now(), $2, \
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

/// Moves a ticket to `status` with triggers disabled (like an imported legacy row).
async fn force_status(pool: &PgPool, t: Uuid, status: &str) {
    let mut c = ok(pool.acquire().await, "acquire");
    ok(
        sqlx::query("SET session_replication_role = replica")
            .execute(&mut *c)
            .await,
        "replica",
    );
    ok(
        sqlx::query("UPDATE tickets SET workflow_status = $2, version = version + 1 WHERE id = $1")
            .bind(t)
            .bind(status)
            .execute(&mut *c)
            .await,
        "status",
    );
    ok(
        sqlx::query("SET session_replication_role = origin")
            .execute(&mut *c)
            .await,
        "origin",
    );
}

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    json: Value,
    bytes: Vec<u8>,
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

async fn send_with(st: &AppState, request: Request<Body>) -> Resp {
    let res = match app(st.clone()).oneshot(request).await {
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
        raw: String::from_utf8_lossy(&bytes).into_owned(),
        bytes,
    }
}

fn cookie(t: &Tok) -> String {
    format!("{}={}", session::cookie_name(false), t.token)
}

async fn get(pool: &PgPool, uri: &str, t: &Tok) -> Resp {
    get_with(&state_with(pool, &[]), uri, t).await
}

async fn get_with(st: &AppState, uri: &str, t: &Tok) -> Resp {
    let r = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, cookie(t));
    send_with(st, ok(r.body(Body::empty()), "request")).await
}

async fn write_key(
    pool: &PgPool,
    method: &str,
    uri: &str,
    t: &Tok,
    key: Option<&str>,
    body: &Value,
) -> Resp {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie(t))
        .header(header::CONTENT_TYPE, "application/json")
        .header(session::CSRF_HEADER, &t.csrf);
    if let Some(k) = key {
        r = r.header("idempotency-key", k);
    }
    send_with(
        &state_with(pool, &[]),
        ok(r.body(Body::from(body.to_string())), "request"),
    )
    .await
}

fn base(ws: Uuid, t: Uuid) -> String {
    format!("/api/v1/workspaces/{ws}/tickets/{t}")
}

async fn count(pool: &PgPool, sql: &str, id: Uuid) -> i64 {
    ok(sqlx::query_scalar(sql).bind(id).fetch_one(pool).await, sql)
}

// ---------------- notes ----------------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn notes_are_idempotent_scoped_and_closed_is_read_only(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let cs = tok(&pool, f.cs).await;
    let uri = format!("{}/notes", base(f.a, t));
    let body = json!({"body": "  Sudah konfirmasi ke merchant (dummy).  "});

    let r = write_key(&pool, "POST", &uri, &cs, Some("note-key-0001"), &body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    assert_eq!(r.json["replayed"], false);
    assert_eq!(
        r.json["note"]["body"],
        "Sudah konfirmasi ke merchant (dummy)."
    );
    assert_eq!(r.json["note"]["author"]["id"], f.cs.to_string());
    let note_id = r.json["note"]["id"].clone();

    // Same key + same text: the original note, nothing new, no extra audit.
    let r = write_key(&pool, "POST", &uri, &cs, Some("note-key-0001"), &body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    assert_eq!(r.json["replayed"], true);
    assert_eq!(r.json["note"]["id"], note_id);
    assert!(r.json["auditEventId"].is_null());
    // Same key + other text: refused, nothing stored.
    let r = write_key(
        &pool,
        "POST",
        &uri,
        &cs,
        Some("note-key-0001"),
        &json!({"body": "lain"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "idempotency_conflict");
    // The key is per user: Lead CS may reuse the same string for its own note.
    let lead = tok(&pool, f.lead).await;
    let r = write_key(&pool, "POST", &uri, &lead, Some("note-key-0001"), &body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM ticket_activities WHERE ticket_id = $1",
            t
        )
        .await,
        2
    );
    // Audit names the note only, never its text.
    let changes: Vec<Value> = ok(
        sqlx::query_scalar(
            "SELECT safe_change FROM audit_events WHERE entity_id = $1 AND action = 'ticket.note'",
        )
        .bind(t)
        .fetch_all(&pool)
        .await,
        "audit",
    );
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|c| !c.to_string().contains("merchant")));

    // Validation and transport refusals.
    let r = write_key(&pool, "POST", &uri, &cs, None, &body).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = write_key(
        &pool,
        "POST",
        &uri,
        &cs,
        Some("note-key-0002"),
        &json!({"body": "   "}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(r.json["error"]["field"], "body");
    let r = write_key(
        &pool,
        "POST",
        &uri,
        &cs,
        Some("note-key-0003"),
        &json!({"body": "x".repeat(5001)}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    let mut req = Request::builder()
        .method("POST")
        .uri(&uri)
        .header(header::COOKIE, cookie(&cs))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", "note-key-0004");
    req = req.header("x-request-id", "uji-d08b");
    let r = send_with(
        &state_with(&pool, &[]),
        ok(req.body(Body::from(body.to_string())), "req"),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "missing CSRF");

    // Another workspace's ticket path: 404, both read and write.
    let other = new_ticket(&pool, f.b, f.admin).await;
    let r = write_key(
        &pool,
        "POST",
        &format!("{}/notes", base(f.a, other)),
        &cs,
        Some("note-key-0005"),
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&pool, &format!("{}/notes", base(f.a, other)), &cs).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Resolved still takes follow-up notes; the closed archive refuses them.
    force_status(&pool, t, "resolved").await;
    let r = write_key(
        &pool,
        "POST",
        &uri,
        &cs,
        Some("note-key-0006"),
        &json!({"body": "Tindak lanjut setelah selesai (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    force_status(&pool, t, "closed").await;
    let r = write_key(
        &pool,
        "POST",
        &uri,
        &cs,
        Some("note-key-0007"),
        &json!({"body": "Tidak boleh (dummy)"}),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(r.json["error"]["code"], "rule_violation");
    let d = get(&pool, &format!("{}/detail", base(f.a, t)), &cs).await;
    assert_eq!(d.json["can"]["addNote"], false);
    assert_eq!(d.json["can"]["uploadAttachment"], false);

    // Pagination: newest first, cursor walks the rest without duplicates.
    let p1 = get(&pool, &format!("{uri}?limit=2"), &cs).await;
    assert_eq!(p1.status, StatusCode::OK, "{}", p1.raw);
    assert_eq!(p1.json["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        p1.json["items"][0]["body"],
        "Tindak lanjut setelah selesai (dummy)"
    );
    let cursor = p1.json["nextCursor"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(!cursor.is_empty());
    let p2 = get(
        &pool,
        &format!("{uri}?limit=2&cursor={}", cursor.replace('+', "%2B")),
        &cs,
    )
    .await;
    assert_eq!(p2.status, StatusCode::OK, "{}", p2.raw);
    assert_eq!(p2.json["items"].as_array().map(Vec::len), Some(1));
    assert!(p2.json["nextCursor"].is_null());
    for bad in ["?limit=0", "?limit=101", "?cursor=zzz", "?other=1"] {
        let r = get(&pool, &format!("{uri}{bad}"), &cs).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

// ---------------- conversations ----------------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn conversation_panel_is_read_only_and_scoped(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let cs = tok(&pool, f.cs).await;
    let uri = format!("{}/conversations", base(f.a, t));
    let r = get(&pool, &uri, &cs).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    assert_eq!(r.json["adapterConfigured"], false);
    assert_eq!(r.json["conversations"], json!([]));

    let conv: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO conversations (workspace_id, ticket_id, channel_label, external_thread_id) \
             VALUES ($1, $2, 'WhatsApp (simulasi)', 'sim-thread-1') RETURNING id",
        )
        .bind(f.a)
        .bind(t)
        .fetch_one(&pool)
        .await,
        "conv",
    );
    for (i, (dir, state)) in [
        ("inbound", "received"),
        ("outbound", "queued"),
        ("outbound", "unknown"),
    ]
    .iter()
    .enumerate()
    {
        ok(
            sqlx::query(
                "INSERT INTO messages (workspace_id, conversation_id, direction, \
                   external_message_id, body, delivery_state, occurred_at) \
                 VALUES ($1, $2, $3, $4, 'Pesan simulasi (dummy)', $5, \
                   now() + make_interval(secs => $6))",
            )
            .bind(f.a)
            .bind(conv)
            .bind(dir)
            .bind(format!("sim-msg-{i}"))
            .bind(state)
            .bind(i as f64)
            .execute(&pool)
            .await,
            "msg",
        );
    }
    // An outbound message can never be "received", an inbound one never "sent".
    let bad = sqlx::query(
        "INSERT INTO messages (workspace_id, conversation_id, direction, external_message_id, \
           body, delivery_state, occurred_at) VALUES ($1, $2, 'inbound', 'x', 'x', 'sent', now())",
    )
    .bind(f.a)
    .bind(conv)
    .execute(&pool)
    .await;
    assert!(bad.is_err());
    // A message can't be attached to a conversation of another workspace.
    let bad = sqlx::query(
        "INSERT INTO messages (workspace_id, conversation_id, direction, external_message_id, \
           body, delivery_state, occurred_at) VALUES ($1, $2, 'inbound', 'y', 'y', 'received', now())",
    )
    .bind(f.b)
    .bind(conv)
    .execute(&pool)
    .await;
    assert!(bad.is_err());

    let r = get(&pool, &uri, &cs).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    let c = &r.json["conversations"][0];
    assert_eq!(c["source"], "simulation");
    let states: Vec<&str> = c["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["deliveryState"].as_str())
        .collect();
    assert_eq!(states, ["received", "queued", "unknown"]);
    // No send path exists.
    let r = write_key(&pool, "POST", &uri, &cs, None, &json!({"body": "x"})).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    let other = new_ticket(&pool, f.b, f.admin).await;
    let r = get(&pool, &format!("{}/conversations", base(f.a, other)), &cs).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// ---------------- related work idempotency + role matrix ----------------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn related_work_create_is_idempotent_even_when_concurrent(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let cs = tok(&pool, f.cs).await;
    let uri = format!("{}/related-work", base(f.a, t));
    let body = json!({"title": "Cek mutasi bank (dummy)", "blocking": true});

    let r = write_key(&pool, "POST", &uri, &cs, Some("rw-key-00001"), &body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    assert_eq!(r.json["replayed"], false);
    let id = r.json["relatedWorkId"].clone();
    let r = write_key(&pool, "POST", &uri, &cs, Some("rw-key-00001"), &body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    assert_eq!(r.json["replayed"], true);
    assert_eq!(r.json["relatedWorkId"], id);
    let r = write_key(
        &pool,
        "POST",
        &uri,
        &cs,
        Some("rw-key-00001"),
        &json!({"title": "Cek mutasi bank (dummy)", "blocking": false}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.raw);
    let r = write_key(&pool, "POST", &uri, &cs, None, &body).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Eight parallel submits of one draft: exactly one row and one audit event.
    let body2 = json!({"title": "Eskalasi paralel (dummy)", "blocking": false});
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (pool, uri, body2) = (pool.clone(), uri.clone(), body2.clone());
        let tk = Tok {
            token: cs.token.clone(),
            csrf: cs.csrf.clone(),
        };
        set.spawn(async move {
            write_key(&pool, "POST", &uri, &tk, Some("rw-key-par01"), &body2)
                .await
                .status
        });
    }
    let mut statuses = Vec::new();
    while let Some(s) = set.join_next().await {
        statuses.push(ok(s, "join").as_u16());
    }
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 200, 200, 200, 200, 200, 200, 201]);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM related_work WHERE ticket_id = $1",
            t
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = 'related_work.create'",
            t
        )
        .await,
        2
    );
    // The key binding of a row can't be rewritten.
    let changed = sqlx::query(
        "UPDATE related_work SET create_idempotency_key = 'rw-key-other' WHERE ticket_id = $1",
    )
    .bind(t)
    .execute(&pool)
    .await;
    assert!(changed.is_err());
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cs_and_lead_both_edit_properties_and_related_work_per_matrix(pool: PgPool) {
    // The frozen matrix gives CS and Lead CS the same ticket.update and related_work.manage;
    // only assign_any differs. No extra role restriction is invented here.
    let f = fixture(&pool).await;
    for (i, who) in [f.cs, f.lead].into_iter().enumerate() {
        let t = new_ticket(&pool, f.a, f.admin).await;
        let tk = tok(&pool, who).await;
        let d = get(&pool, &format!("{}/detail", base(f.a, t)), &tk).await;
        assert_eq!(d.json["can"]["editProperties"], true);
        assert_eq!(d.json["can"]["manageRelatedWork"], true);
        assert_eq!(d.json["can"]["addNote"], true);
        assert_eq!(d.json["can"]["uploadAttachment"], true);
        let r = write_key(
            &pool,
            "PATCH",
            &base(f.a, t),
            &tk,
            None,
            &json!({"version": 1, "subject": format!("Subjek baru {i} (dummy)")}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
        // Stale version: 409, stored subject unchanged.
        let r = write_key(
            &pool,
            "PATCH",
            &base(f.a, t),
            &tk,
            None,
            &json!({"version": 1, "subject": "Basi (dummy)"}),
        )
        .await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.raw);
        let subject: String = ok(
            sqlx::query_scalar("SELECT subject FROM tickets WHERE id = $1")
                .bind(t)
                .fetch_one(&pool)
                .await,
            "subject",
        );
        assert_eq!(subject, format!("Subjek baru {i} (dummy)"));
        let r = write_key(
            &pool,
            "POST",
            &format!("{}/related-work", base(f.a, t)),
            &tk,
            Some(&format!("rw-role-{i}-0001")),
            &json!({"title": "Pekerjaan peran (dummy)", "blocking": false}),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    }
}

// ---------------- attachments ----------------

/// In-test clamd stand-in (NOT real clamd): answers per marker found in the stream.
async fn fake_clamd() -> String {
    let l = ok(tokio::net::TcpListener::bind("127.0.0.1:0").await, "bind");
    let addr = ok(l.local_addr(), "addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut cmd = Vec::new();
                let mut b = [0u8; 1];
                while s.read_exact(&mut b).await.is_ok() && b[0] != 0 {
                    cmd.push(b[0]);
                }
                if cmd != b"zINSTREAM" {
                    return;
                }
                let mut data = Vec::new();
                loop {
                    let mut len = [0u8; 4];
                    if s.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let n = u32::from_be_bytes(len) as usize;
                    if n == 0 {
                        break;
                    }
                    let mut chunk = vec![0u8; n];
                    if s.read_exact(&mut chunk).await.is_err() {
                        return;
                    }
                    data.extend_from_slice(&chunk);
                }
                let has = |m: &[u8]| data.windows(m.len()).any(|w| w == m);
                let reply: &[u8] = if has(b"DUMMY-INFECTED") {
                    b"stream: Dummy.Test.Signature FOUND\0"
                } else if has(b"DUMMY-SCANERR") {
                    b"INSTREAM size limit exceeded. ERROR\0"
                } else {
                    b"stream: OK\0"
                };
                let _ = s.write_all(reply).await;
            });
        }
    });
    format!("tcp://{addr}")
}

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!("itsm-d08b-test-{}", Uuid::new_v4()))
}

fn files_in(p: &Path) -> usize {
    std::fs::read_dir(p).map(|d| d.count()).unwrap_or(0)
}

fn pdf(marker: &str) -> Vec<u8> {
    format!("%PDF-1.4\n% dokumen uji (dummy) {marker}\n%%EOF\n").into_bytes()
}

async fn upload(
    st: &AppState,
    ws: Uuid,
    t: Uuid,
    tk: &Tok,
    name: &str,
    bytes: Vec<u8>,
    with_len: bool,
) -> Resp {
    let mut r = Request::builder()
        .method("POST")
        .uri(format!("{}/attachments?visibility=internal", base(ws, t)))
        .header(header::COOKIE, cookie(tk))
        .header(session::CSRF_HEADER, &tk.csrf)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header("x-file-name", name);
    if with_len {
        r = r.header(header::CONTENT_LENGTH, bytes.len());
    }
    send_with(st, ok(r.body(Body::from(bytes)), "upload")).await
}

async fn scan_status(pool: &PgPool, t: Uuid) -> Vec<(String, Option<Uuid>)> {
    ok(
        sqlx::query_as(
            "SELECT scan_status, ticket_id FROM attachments WHERE target_ticket_id = $1 \
             ORDER BY created_at, id",
        )
        .bind(t)
        .fetch_all(pool)
        .await,
        "status",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn clean_upload_binds_and_download_rechecks_scope(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let root = temp_root();
    let st = state_with(
        &pool,
        &[
            ("ITSM_ATTACHMENT_DIR", root.display().to_string()),
            ("ITSM_CLAMD_ADDR", fake_clamd().await),
        ],
    );
    let cs = tok(&pool, f.cs).await;
    let bytes = pdf("bersih");
    let r = upload(
        &st,
        f.a,
        t,
        &cs,
        "bukti%20transfer%C3%A9.pdf",
        bytes.clone(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    let att = r.json["attachmentId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let meta = &r.json["detail"]["attachments"][0];
    assert_eq!(meta["id"], att.as_str());
    assert_eq!(meta["fileName"], "bukti transferé.pdf");
    assert_eq!(meta["inspectedMime"], "application/pdf");
    assert_eq!(meta["scanStatus"], "clean");
    assert_eq!(files_in(&root.join("quarantine")), 0);
    assert_eq!(files_in(&root.join("store")), 1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = 'attachment.upload' AND outcome = 'success'",
            t
        )
        .await,
        1
    );

    // Download by another member with access: same bytes, safe headers, audited.
    let lead = tok(&pool, f.lead).await;
    let uri = format!("{}/attachments/{att}/content", base(f.a, t));
    let r = get_with(&st, &uri, &lead).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.raw);
    assert_eq!(r.bytes, bytes);
    let h = |k: &str| {
        r.headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(h("content-type"), "application/pdf");
    assert!(h("content-disposition").starts_with("attachment; filename=\"bukti transfer_.pdf\""));
    assert!(h("content-disposition").contains("filename*=UTF-8''bukti%20transfer%C3%A9.pdf"));
    assert_eq!(h("content-security-policy"), "default-src 'none'; sandbox");
    assert_eq!(h("x-content-type-options"), "nosniff");
    assert_eq!(h("cache-control"), "no-store");
    let att_id = ok(Uuid::parse_str(&att), "uuid");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_events WHERE entity_id = $1 AND action = 'attachment.download'",
            att_id
        )
        .await,
        1
    );
    // Wrong ticket, wrong workspace, unknown id: 404 without bytes.
    let t2 = new_ticket(&pool, f.a, f.admin).await;
    for bad in [
        format!("{}/attachments/{att}/content", base(f.a, t2)),
        format!("{}/attachments/{att}/content", base(f.b, t)),
        format!("{}/attachments/{}/content", base(f.a, t), Uuid::new_v4()),
    ] {
        let r = get_with(&st, &bad, &lead).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad}");
        assert_ne!(r.bytes, bytes);
    }
    // Tampered stored bytes are never served.
    ok(
        std::fs::write(root.join("store").join(&att), b"%PDF-1.4 diubah"),
        "tamper",
    );
    let r = get_with(&st, &uri, &lead).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    // Bound rows can't be moved or deleted.
    let moved =
        sqlx::query("UPDATE attachments SET ticket_id = $2, target_ticket_id = $2 WHERE id = $1")
            .bind(att_id)
            .bind(t2)
            .execute(&pool)
            .await;
    assert!(moved.is_err());
    let gone = sqlx::query("DELETE FROM attachments WHERE id = $1")
        .bind(att_id)
        .execute(&pool)
        .await;
    assert!(gone.is_err());

    // Larger than the 64 KiB JSON limit: the upload route has its own cap.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.resize(300 * 1024, 7);
    let r = upload(&st, f.a, t, &cs, "layar.png", png, true).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    assert_eq!(
        r.json["detail"]["attachments"][1]["inspectedMime"],
        "image/png"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn every_non_clean_outcome_fails_closed(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let root = temp_root();
    let dir = ("ITSM_ATTACHMENT_DIR", root.display().to_string());
    let st = state_with(
        &pool,
        &[
            dir.clone(),
            ("ITSM_CLAMD_ADDR", fake_clamd().await),
            ("ITSM_ATTACHMENT_MAX_BYTES", "4096".to_owned()),
        ],
    );
    let cs = tok(&pool, f.cs).await;

    let r = upload(&st, f.a, t, &cs, "a.pdf", pdf("DUMMY-INFECTED"), true).await;
    assert_eq!(r.status.as_u16(), 422, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "attachment_infected");
    assert!(!r.raw.contains("Dummy.Test.Signature"));
    let r = upload(&st, f.a, t, &cs, "b.pdf", pdf("DUMMY-SCANERR"), true).await;
    assert_eq!(r.status.as_u16(), 502, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "scan_failed");
    // No scanner configured, and a configured but unreachable scanner: both 503.
    let none = state_with(&pool, std::slice::from_ref(&dir));
    let r = upload(&none, f.a, t, &cs, "c.pdf", pdf("x"), true).await;
    assert_eq!(r.status.as_u16(), 503, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "scanner_unavailable");
    let closed_port = {
        let l = ok(std::net::TcpListener::bind("127.0.0.1:0"), "bind");
        ok(l.local_addr(), "addr")
    };
    let down = state_with(
        &pool,
        &[
            dir.clone(),
            ("ITSM_CLAMD_ADDR", format!("tcp://{closed_port}")),
        ],
    );
    let r = upload(&down, f.a, t, &cs, "d.pdf", pdf("x"), true).await;
    assert_eq!(r.status.as_u16(), 503, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "scanner_unavailable");
    assert_eq!(
        scan_status(&pool, t).await,
        [
            ("infected".to_owned(), None),
            ("scan_error".to_owned(), None),
            ("scanner_unavailable".to_owned(), None),
            ("scanner_unavailable".to_owned(), None),
        ]
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_events WHERE action = 'attachment.upload' AND outcome = 'failed' AND safe_change->>'ticketId' = $1::text",
            t
        )
        .await,
        4
    );
    // Nothing kept on disk, nothing visible on the ticket, nothing downloadable.
    assert_eq!(files_in(&root.join("quarantine")), 0);
    assert_eq!(files_in(&root.join("store")), 0);
    let d = get_with(&st, &format!("{}/detail", base(f.a, t)), &cs).await;
    assert_eq!(d.json["attachments"], json!([]));
    let infected: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM attachments WHERE scan_status = 'infected'")
            .fetch_one(&pool)
            .await,
        "id",
    );
    let r = get_with(
        &st,
        &format!("{}/attachments/{infected}/content", base(f.a, t)),
        &cs,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // A final verdict can't be flipped to clean afterwards.
    let flip = sqlx::query("UPDATE attachments SET scan_status = 'clean' WHERE id = $1")
        .bind(infected)
        .execute(&pool)
        .await;
    assert!(flip.is_err());

    let before = count(
        &pool,
        "SELECT count(*) FROM attachments WHERE target_ticket_id = $1",
        t,
    )
    .await;
    // Too large: declared, and streamed without a length.
    let big = {
        let mut v = pdf("x");
        v.resize(5000, b' ');
        v
    };
    let r = upload(&st, f.a, t, &cs, "e.pdf", big.clone(), true).await;
    assert_eq!(r.status.as_u16(), 413, "{}", r.raw);
    let r = upload(&st, f.a, t, &cs, "e.pdf", big, false).await;
    assert_eq!(r.status.as_u16(), 413, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "attachment_too_large");
    // Type sniffed from bytes, not from the name or Content-Type.
    let r = upload(
        &st,
        f.a,
        t,
        &cs,
        "f.pdf",
        b"<html>bukan pdf</html>".to_vec(),
        true,
    )
    .await;
    assert_eq!(r.status.as_u16(), 415, "{}", r.raw);
    assert_eq!(r.json["error"]["code"], "attachment_type_refused");
    for bad in ["", "..", "a%2Fb.pdf", "%0Aa.pdf"] {
        let r = upload(&st, f.a, t, &cs, bad, pdf("x"), true).await;
        assert_eq!(r.status.as_u16(), 422, "{bad}: {}", r.raw);
        assert_eq!(r.json["error"]["field"], "fileName");
    }
    let r = upload(&st, f.a, t, &cs, "g.pdf", Vec::new(), true).await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(r.json["error"]["field"], "file");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM attachments WHERE target_ticket_id = $1",
            t
        )
        .await,
        before
    );
    assert_eq!(files_in(&root.join("quarantine")), 0);

    // Scope, CSRF, closed, storage not configured.
    let other = new_ticket(&pool, f.b, f.admin).await;
    let r = upload(&st, f.a, other, &cs, "h.pdf", pdf("x"), true).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let req = Request::builder()
        .method("POST")
        .uri(format!("{}/attachments", base(f.a, t)))
        .header(header::COOKIE, cookie(&cs))
        .header("x-file-name", "i.pdf");
    let r = send_with(&st, ok(req.body(Body::from(pdf("x"))), "req")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send_with(
        &st,
        ok(
            Request::builder()
                .method("POST")
                .uri(format!("{}/attachments?visibility=semua", base(f.a, t)))
                .header(header::COOKIE, cookie(&cs))
                .header(session::CSRF_HEADER, &cs.csrf)
                .header("x-file-name", "i.pdf")
                .body(Body::from(pdf("x"))),
            "req",
        ),
    )
    .await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(r.json["error"]["field"], "visibility");
    let nodir = state_with(&pool, &[("ITSM_CLAMD_ADDR", fake_clamd().await)]);
    let r = upload(&nodir, f.a, t, &cs, "j.pdf", pdf("x"), true).await;
    assert_eq!(r.status.as_u16(), 503);
    assert_eq!(r.json["error"]["code"], "dependency_unavailable");
    force_status(&pool, t, "closed").await;
    let r = upload(&st, f.a, t, &cs, "k.pdf", pdf("x"), true).await;
    assert_eq!(r.status.as_u16(), 422);
    assert_eq!(r.json["error"]["code"], "rule_violation");
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------- contract + rollback ----------------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn followup_responses_conform_to_openapi(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let root_dir = temp_root();
    let st = state_with(
        &pool,
        &[
            ("ITSM_ATTACHMENT_DIR", root_dir.display().to_string()),
            ("ITSM_CLAMD_ADDR", fake_clamd().await),
        ],
    );
    let tk = tok(&pool, f.lead).await;
    let root: Value = ok(
        serde_json::from_str(include_str!("../../openapi/openapi.json")),
        "openapi",
    );
    let s = &root["components"]["schemas"];
    let r = write_key(
        &pool,
        "POST",
        &format!("{}/notes", base(f.a, t)),
        &tk,
        Some("conform-note-1"),
        &json!({"body": "Catatan kontrak (dummy)"}),
    )
    .await;
    conform(&r.json, &s["NoteCreated"], &root, "note");
    let r = get(&pool, &format!("{}/notes", base(f.a, t)), &tk).await;
    conform(&r.json, &s["NotePage"], &root, "notes");
    let r = get(&pool, &format!("{}/conversations", base(f.a, t)), &tk).await;
    conform(&r.json, &s["ConversationPanel"], &root, "conversations");
    let r = write_key(
        &pool,
        "POST",
        &format!("{}/related-work", base(f.a, t)),
        &tk,
        Some("conform-rw-1"),
        &json!({"title": "Kontrak (dummy)", "blocking": false}),
    )
    .await;
    conform(&r.json, &s["RelatedWorkCreated"], &root, "related");
    let r = upload(&st, f.a, t, &tk, "kontrak.pdf", pdf("x"), true).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.raw);
    conform(&r.json, &s["AttachmentUploaded"], &root, "upload");
    let r = upload(&st, f.a, t, &tk, "kontrak.pdf", pdf("DUMMY-INFECTED"), true).await;
    conform(&r.json, &s["Error"], &root, "infected");
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0007_rolls_back_and_reapplies(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = new_ticket(&pool, f.a, f.admin).await;
    let tk = tok(&pool, f.cs).await;
    let r = write_key(
        &pool,
        "POST",
        &format!("{}/notes", base(f.a, t)),
        &tk,
        Some("rollback-note"),
        &json!({"body": "Sebelum rollback (dummy)"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    ok(db::MIGRATOR.undo(&pool, 6).await, "undo to 0006");
    for table in ["ticket_activities", "conversations", "messages"] {
        let has: bool = ok(
            sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(table)
                .fetch_one(&pool)
                .await,
            table,
        );
        assert!(!has, "{table}");
    }
    let has: bool = ok(
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_name = 'attachments' AND column_name = 'target_ticket_id')",
        )
        .fetch_one(&pool)
        .await,
        "column",
    );
    assert!(!has);
    ok(db::MIGRATOR.run(&pool).await, "reapply");
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
