//! CP6a two-way Jira sync and the Lark token cache, against a real PostgreSQL and an in-process
//! HTTP stub speaking the Jira Cloud REST v3 / Lark Open Platform shapes the adapters use.
//! Covers: the inbound webhook (HMAC over the raw body, one generic 401, replay without effect,
//! Jira timestamp order, stale/unknown/ignored outcomes, comments and status/assignee entries on
//! open tickets only, the ticket status never changed by Jira), the webhook secret (shown once,
//! rotation invalidates the old one, credential grant required), the outbound ITSM → Jira status
//! transition through the outbox (moved, already there, superseded, workflow refuses), the Lark
//! tenant token reused across operations and dropped when Lark refuses it, and migration 0027
//! down/up. All data is synthetic; no real provider is used.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::outbox;
use itsm_api::{AppState, app, db, seed, session};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

const JIRA_TOKEN: &str = "jira-token-SINTETIS-cp6a-4be1";
const LARK_SECRET: &str = "lark-secret-SINTETIS-cp6a-90d2";
const JIRA_EMAIL: &str = "svc-itsm@example.test";
const LARK_APP: &str = "cli_sintetis_cp6a";
const IN_PROGRESS: &str = "10002";
const WAITING: &str = "10004";
const DONE: &str = "10003";

// ---------- provider stub ----------

#[derive(Default)]
struct StubState {
    calls: Vec<(String, String)>,
    bodies: Vec<String>,
    /// Created issues: (id, key, status id).
    issues: Vec<(String, String, String)>,
    /// Tenant tokens handed out so far (each exchange issues a new one).
    tokens_issued: u32,
    /// Lark refuses this token as invalid (code 99991663).
    revoked: Option<String>,
    lark_messages: u32,
}

type Stub = Arc<Mutex<StubState>>;

fn lock(st: &Stub) -> std::sync::MutexGuard<'_, StubState> {
    st.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn json_resp(status: u16, v: Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        code,
        [(header::CONTENT_TYPE, "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn status_name(id: &str) -> &'static str {
    match id {
        IN_PROGRESS => "In Progress",
        WAITING => "Waiting",
        DONE => "Done",
        _ => "To Do",
    }
}

async fn stub_handler(
    State(st): State<Stub>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let full = uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| path.clone());
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    {
        let mut s = lock(&st);
        s.calls.push((method.to_string(), full));
        s.bodies.push(body.to_string());
    }
    if path.starts_with("/rest/api/3/") {
        let basic = format!(
            "Basic {}",
            STANDARD.encode(format!("{JIRA_EMAIL}:{JIRA_TOKEN}"))
        );
        if auth != basic {
            return json_resp(401, json!({"errorMessages": ["unauthorized"]}));
        }
        return jira(&st, &method, &path, &body);
    }
    if path.starts_with("/open-apis/") {
        return lark(&st, &method, &path, &auth, &body);
    }
    json_resp(404, json!({}))
}

fn jira(st: &Stub, method: &Method, path: &str, body: &Value) -> Response {
    let mut s = lock(st);
    match (method.as_str(), path) {
        ("GET", "/rest/api/3/myself") => json_resp(
            200,
            json!({"accountId": "svc-1", "displayName": "Akun layanan", "active": true}),
        ),
        ("GET", "/rest/api/3/project/OPS") => json_resp(
            200,
            json!({"key": "OPS", "issueTypes": [{"id": "10001", "name": "Task"}]}),
        ),
        ("GET", "/rest/api/3/priority") => json_resp(200, json!([{"id": "1", "name": "Highest"}])),
        ("GET", "/rest/api/3/project/OPS/statuses") => json_resp(
            200,
            json!([{"statuses": [
                {"id": "10000", "name": "To Do"}, {"id": IN_PROGRESS, "name": "In Progress"},
                {"id": WAITING, "name": "Waiting"}, {"id": DONE, "name": "Done"}
            ]}]),
        ),
        ("POST", "/rest/api/3/search/jql") => json_resp(200, json!({"issues": []})),
        ("POST", "/rest/api/3/issue") => {
            let n = s.issues.len() + 1;
            let (id, key) = (format!("{}", 30000 + n), format!("OPS-{n}"));
            s.issues.push((id.clone(), key.clone(), "10000".to_owned()));
            json_resp(201, json!({"id": id, "key": key}))
        }
        _ => {
            // /rest/api/3/issue/{key}?fields=status and /rest/api/3/issue/{key}/transitions
            let rest = path.trim_start_matches("/rest/api/3/issue/");
            let (key, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let Some(i) = s.issues.iter().position(|(_, k, _)| k == key) else {
                return json_resp(404, json!({"errorMessages": ["not found"]}));
            };
            match (method.as_str(), tail) {
                ("GET", "") => {
                    let (id, key, status) = s.issues[i].clone();
                    json_resp(
                        200,
                        json!({"id": id, "key": key, "fields": {"status": {"id": status, "name": status_name(&status)}}}),
                    )
                }
                // The workflow allows In Progress and Done from anywhere, never Waiting.
                ("GET", "transitions") => json_resp(
                    200,
                    json!({"transitions": [
                        {"id": "21", "name": "Mulai", "to": {"id": IN_PROGRESS, "name": "In Progress"}},
                        {"id": "31", "name": "Selesai", "to": {"id": DONE, "name": "Done"}}
                    ]}),
                ),
                ("POST", "transitions") => {
                    let to = match body["transition"]["id"].as_str() {
                        Some("21") => IN_PROGRESS,
                        Some("31") => DONE,
                        _ => return json_resp(400, json!({"errorMessages": ["bad transition"]})),
                    };
                    s.issues[i].2 = to.to_owned();
                    (StatusCode::NO_CONTENT, "").into_response()
                }
                _ => json_resp(404, json!({"errorMessages": ["not found"]})),
            }
        }
    }
}

fn lark(st: &Stub, method: &Method, path: &str, auth: &str, body: &Value) -> Response {
    let mut s = lock(st);
    if (method.as_str(), path) == ("POST", "/open-apis/auth/v3/tenant_access_token/internal") {
        if body["app_id"] == LARK_APP && body["app_secret"] == LARK_SECRET {
            s.tokens_issued += 1;
            let token = format!("t-sintetis-{}", s.tokens_issued);
            return json_resp(
                200,
                json!({"code": 0, "tenant_access_token": token, "expire": 7200}),
            );
        }
        return json_resp(200, json!({"code": 10014, "msg": "app secret invalid"}));
    }
    let token = auth.strip_prefix("Bearer ").unwrap_or_default();
    let issued = token
        .strip_prefix("t-sintetis-")
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| n >= 1 && n <= s.tokens_issued);
    if !issued || s.revoked.as_deref() == Some(token) {
        return json_resp(200, json!({"code": 99991663, "msg": "invalid token"}));
    }
    match (method.as_str(), path) {
        ("GET", "/open-apis/bot/v3/info") => json_resp(
            200,
            json!({"code": 0, "bot": {"app_name": "ITSM Bot (sintetis)"}}),
        ),
        ("GET", "/open-apis/im/v1/chats") => json_resp(
            200,
            json!({"code": 0, "data": {"items": [
                {"chat_id": "oc_eskalasi", "name": "Eskalasi (sintetis)"}
            ], "has_more": false}}),
        ),
        ("POST", "/open-apis/im/v1/messages") => {
            s.lark_messages += 1;
            json_resp(200, json!({"code": 0, "data": {"message_id": "om_1"}}))
        }
        _ => json_resp(200, json!({"code": 404, "msg": "not found"})),
    }
}

async fn stub() -> (Stub, String) {
    let st: Stub = Arc::default();
    let router = Router::new().fallback(stub_handler).with_state(st.clone());
    let listener = ok(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
        "bind stub",
    );
    let addr = ok(listener.local_addr(), "stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (st, format!("http://{addr}"))
}

fn calls(st: &Stub, method: &str, needle: &str) -> usize {
    lock(st)
        .calls
        .iter()
        .filter(|(m, p)| m == method && p.contains(needle))
        .count()
}

// ---------- app under test ----------

fn state(pool: &PgPool) -> AppState {
    let key = STANDARD.encode([9u8; 32]);
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat".to_owned()),
        ("ITSM_ACCESS_MODE", "uat_superadmin".to_owned()),
        ("DATABASE_URL", "postgres://unused-in-tests".to_owned()),
        ("ITSM_COOKIE_SECURE", "false".to_owned()),
        ("ITSM_INTEGRATIONS", "live".to_owned()),
        ("ITSM_INTEGRATIONS_TIMEOUT_MS", "800".to_owned()),
        ("ITSM_INTEGRATIONS_LAB", "on".to_owned()),
        ("ITSM_KEYRING", format!("k1:{key}")),
        ("ITSM_KEYRING_ACTIVE", "k1".to_owned()),
        ("ITSM_PUBLIC_URL", "https://itsm.example.test".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
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

async fn fixture(pool: &PgPool) -> Fx {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    let id = |sql: &'static str, key: &'static str| async move {
        let v: Uuid = ok(sqlx::query_scalar(sql).bind(key).fetch_one(pool).await, key);
        v
    };
    Fx {
        a: id("SELECT id FROM workspaces WHERE code = $1", "DUMA").await,
        admin: id(
            "SELECT id FROM users WHERE external_subject = $1",
            "dummy:uat-superadmin",
        )
        .await,
        lead: id(
            "SELECT id FROM users WHERE external_subject = $1",
            "dummy:lead-alfa",
        )
        .await,
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
    json: Value,
    raw: String,
}

async fn call(st: &AppState, req: Request<Body>) -> Resp {
    let res = match app(st.clone()).oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        raw: String::from_utf8_lossy(&bytes).into_owned(),
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
    call(st, req).await
}

fn s(v: &Value) -> String {
    match v.as_str() {
        Some(x) => x.to_owned(),
        None => panic!("not a string: {v}"),
    }
}

fn ws_uri(ws: Uuid, rest: &str) -> String {
    format!("/api/v1/workspaces/{ws}/{rest}")
}

async fn connection(st: &AppState, ws: Uuid, t: &Tok, provider: &str) -> Value {
    let r = send(st, "GET", &ws_uri(ws, "integrations"), t, None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    match r.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["provider"] == provider))
    {
        Some(c) => c.clone(),
        None => panic!("no {provider} connection in {}", r.json),
    }
}

/// Configure, store the credential, test and activate one provider against the stub.
async fn go_live(st: &AppState, fx: &Fx, base: &str, provider: &str) -> Value {
    let admin = tok(&st.pool, fx.admin).await;
    let c = connection(st, fx.a, &admin, provider).await;
    let id = s(&c["id"]);
    let (body, secret) = if provider == "jira" {
        (
            json!({"baseUrl": base, "email": JIRA_EMAIL, "projectKey": "OPS", "issueTypeId": "10001",
                   "priorityMap": {}, "version": c["version"],
                   "statusMap": {"open": IN_PROGRESS, "pending_engineering": WAITING}}),
            JIRA_TOKEN,
        )
    } else {
        (
            json!({"appId": LARK_APP, "baseUrl": base, "version": c["version"]}),
            LARK_SECRET,
        )
    };
    let r = send(
        st,
        "PUT",
        &ws_uri(fx.a, &format!("integrations/{id}/config")),
        &admin,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = send(
        st,
        "PUT",
        &ws_uri(fx.a, &format!("integrations/{id}/credential")),
        &admin,
        None,
        Some(&json!({"secret": secret, "version": r.json["item"]["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let mut item = r.json["item"].clone();
    for step in ["test", "activate"] {
        let r = send(
            st,
            "POST",
            &ws_uri(fx.a, &format!("integrations/{id}/{step}")),
            &admin,
            None,
            Some(&json!({"version": item["version"]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{step}: {}", r.json);
        item = r.json["item"].clone();
    }
    assert_eq!(item["state"], "active", "{item}");
    item
}

async fn channel(pool: &PgPool, ws: Uuid) -> Uuid {
    ok(
        sqlx::query_scalar(
            "SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'",
        )
        .bind(ws)
        .fetch_one(pool)
        .await,
        "channel",
    )
}

/// New ticket with a new Jira issue (and optionally a Lark message); returns the ticket id.
async fn intake(st: &AppState, fx: &Fx, key: &str, lark_chat: Option<&Value>) -> Uuid {
    let lead = tok(&st.pool, fx.lead).await;
    let body = json!({
        "subject": "Pengaduan uji CP6a (sintetis)",
        "chronology": "Kronologi uji CP6a (sintetis).",
        "receivedAt": "2026-11-12T09:15:00+07:00",
        "intakeChannelId": channel(&st.pool, fx.a).await,
        "jira": {"mode": "create", "title": "Tindak lanjut uji CP6a"},
        "larkDestinationId": lark_chat,
    });
    let r = send(
        st,
        "POST",
        &ws_uri(fx.a, "tickets"),
        &lead,
        Some(key),
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let jira = r.json["integrations"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["provider"] == "jira"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(jira["state"], "success", "{}", r.json);
    ok(Uuid::parse_str(&s(&r.json["ticket"]["id"])), "ticket id")
}

fn now_ms() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = ok(
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()),
        "hmac key",
    );
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

/// Posts a webhook delivery exactly as Jira does: no session, signed raw body.
async fn deliver(
    st: &AppState,
    connection: &str,
    delivery: Option<&str>,
    signature: Option<String>,
    body: &Value,
) -> Resp {
    let raw = body.to_string();
    let mut r = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/integrations/jira/{connection}/webhook"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(sig) = signature {
        r = r.header("x-hub-signature", sig);
    }
    if let Some(d) = delivery {
        r = r.header("x-atlassian-webhook-identifier", d);
    }
    call(st, ok(r.body(Body::from(raw)), "request")).await
}

async fn signed(
    st: &AppState,
    connection: &str,
    secret: &str,
    delivery: &str,
    body: &Value,
) -> Resp {
    let sig = sign(secret, body.to_string().as_bytes());
    deliver(st, connection, Some(delivery), Some(sig), body).await
}

fn issue_updated(issue_id: &str, ts: i64, status: Option<&str>, assignee: Option<Value>) -> Value {
    let mut fields = serde_json::Map::new();
    if let Some(id) = status {
        fields.insert("status".into(), json!({"id": id, "name": status_name(id)}));
    }
    if let Some(a) = assignee {
        fields.insert("assignee".into(), a);
    }
    json!({
        "webhookEvent": "jira:issue_updated",
        "timestamp": ts,
        "user": {"displayName": "Rina Jira (sintetis)"},
        "issue": {"id": issue_id, "key": "OPS-1", "fields": fields},
    })
}

fn comment_created(issue_id: &str, ts: i64, body: Value) -> Value {
    json!({
        "webhookEvent": "comment_created",
        "timestamp": ts,
        "issue": {"id": issue_id, "key": "OPS-1"},
        "comment": {"id": "c-1", "author": {"displayName": "Dewi Jira (sintetis)"}, "body": body},
    })
}

async fn notes(st: &AppState, fx: &Fx, ticket: Uuid) -> Vec<Value> {
    let lead = tok(&st.pool, fx.lead).await;
    let r = send(
        st,
        "GET",
        &ws_uri(fx.a, &format!("tickets/{ticket}/notes")),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json["items"].as_array().cloned().unwrap_or_default()
}

async fn outcomes(pool: &PgPool) -> Vec<String> {
    ok(
        sqlx::query_scalar("SELECT outcome FROM jira_webhook_events ORDER BY received_at, id")
            .fetch_all(pool)
            .await,
        "outcomes",
    )
}

async fn rotate(st: &AppState, fx: &Fx, t: &Tok, conn: &Value) -> Resp {
    send(
        st,
        "POST",
        &ws_uri(
            fx.a,
            &format!("integrations/{}/webhook-secret", s(&conn["id"])),
        ),
        t,
        None,
        Some(&json!({"version": conn["version"]})),
    )
    .await
}

async fn ticket_status(pool: &PgPool, ticket: Uuid) -> (String, i32) {
    ok(
        sqlx::query_as("SELECT workflow_status, version FROM tickets WHERE id = $1")
            .bind(ticket)
            .fetch_one(pool)
            .await,
        "ticket",
    )
}

/// `closed` cannot be produced by updates (guards), so the fixture writes it with triggers
/// disabled on one connection, like an imported legacy row (same as tests/tickets_list.rs).
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

// ---------- webhook ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp6a_webhook_is_signed_idempotent_and_applied_in_jira_order(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let (_stub, base) = stub().await;
    let jira = go_live(&st, &fx, &base, "jira").await;
    let conn = s(&jira["id"]);
    assert_eq!(jira["webhook"]["state"], "missing", "{jira}");
    let ticket = intake(&st, &fx, "cp6a-wh-0001", None).await;
    let before = ticket_status(&pool, ticket).await;
    let issue_id = "30001";

    // No secret yet: every delivery is the same 401.
    let ev = issue_updated(issue_id, now_ms(), Some(IN_PROGRESS), None);
    let r = signed(&st, &conn, &"0".repeat(64), "dlv-00000001", &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.raw);

    // Without integration.credentials the secret cannot be made.
    let lead = tok(&pool, fx.lead).await;
    let r = rotate(&st, &fx, &lead, &jira).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);

    // Generate: the secret is in this response only.
    let admin = tok(&pool, fx.admin).await;
    let r = rotate(&st, &fx, &admin, &jira).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let secret = s(&r.json["secret"]);
    assert_eq!(secret.len(), 64);
    assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        s(&r.json["url"]),
        format!("https://itsm.example.test/api/v1/integrations/jira/{conn}/webhook")
    );
    assert_eq!(r.json["item"]["webhook"]["state"], "stored");
    assert_eq!(r.json["item"]["state"], "active", "outbound is untouched");
    let listing = send(
        &st,
        "GET",
        &ws_uri(fx.a, "integrations"),
        &admin,
        None,
        None,
    )
    .await;
    assert!(!listing.raw.contains(&secret), "secret shown again");
    let audit: Vec<String> = ok(
        sqlx::query_scalar("SELECT safe_change::text FROM audit_events WHERE workspace_id = $1")
            .bind(fx.a)
            .fetch_all(&pool)
            .await,
        "audit",
    );
    assert!(audit.iter().all(|a| !a.contains(&secret)), "secret audited");
    let blobs: Vec<Vec<u8>> = ok(
        sqlx::query_scalar(
            "SELECT ciphertext FROM encrypted_secrets WHERE purpose = 'jira_webhook_secret'",
        )
        .fetch_all(&pool)
        .await,
        "ciphertext",
    );
    assert_eq!(blobs.len(), 1);
    assert!(!blobs[0].windows(64).any(|w| w == secret.as_bytes()));

    // Refused alike: no signature, wrong signature, body changed after signing, wrong secret,
    // unknown connection, malformed id.
    let r = deliver(&st, &conn, Some("dlv-00000002"), None, &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json["error"]["code"], "unauthenticated");
    let r = deliver(&st, &conn, None, Some("sha256=zz".into()), &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let tampered = issue_updated(issue_id, now_ms(), Some(DONE), None);
    let r = deliver(
        &st,
        &conn,
        None,
        Some(sign(&secret, ev.to_string().as_bytes())),
        &tampered,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = signed(&st, &conn, &"a".repeat(64), "dlv-00000003", &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = signed(
        &st,
        &Uuid::new_v4().to_string(),
        &secret,
        "dlv-00000004",
        &ev,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = signed(&st, "bukan-uuid", &secret, "dlv-00000005", &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        outcomes(&pool).await.is_empty(),
        "refused deliveries are not recorded"
    );

    // Signed but not JSON / no event name: 400.
    let r = deliver(&st, &conn, None, Some(sign(&secret, b"\"x\"")), &json!("x")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.raw);

    // Status + assignee change: two timeline entries, written by Jira, ticket status unchanged.
    let t1 = now_ms();
    let ev = issue_updated(
        issue_id,
        t1,
        Some(IN_PROGRESS),
        Some(json!({"accountId": "acc-budi", "displayName": "Budi Jira (sintetis)"})),
    );
    let r = signed(&st, &conn, &secret, "dlv-00000010", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.raw);
    let n = notes(&st, &fx, ticket).await;
    let jira_notes: Vec<&Value> = n.iter().filter(|x| x["kind"] != "note").collect();
    assert_eq!(jira_notes.len(), 2, "{n:?}");
    for x in &jira_notes {
        assert!(x["author"].is_null(), "{x}");
        assert_eq!(x["externalAuthor"], "Rina Jira (sintetis)");
    }
    assert!(
        jira_notes
            .iter()
            .any(|x| x["kind"] == "jira_status" && s(&x["body"]).contains("In Progress"))
    );
    assert!(jira_notes.iter().any(|x| x["kind"] == "jira_assignee"
        && s(&x["body"]).contains("Budi Jira (sintetis)")));
    assert_eq!(
        ticket_status(&pool, ticket).await,
        before,
        "Jira never moves the ticket"
    );
    let (ext, assignee): (Option<String>, Option<String>) = ok(
        sqlx::query_as(
            "SELECT external_status, external_assignee_name FROM jira_issues WHERE external_issue_id = $1",
        )
        .bind(issue_id)
        .fetch_one(&pool)
        .await,
        "issue",
    );
    assert_eq!(ext.as_deref(), Some("In Progress"));
    assert_eq!(assignee.as_deref(), Some("Budi Jira (sintetis)"));

    // Replay (same delivery id): 204, nothing new. Same body without an id: deduped by hash.
    let r = signed(&st, &conn, &secret, "dlv-00000010", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let sig = sign(&secret, ev.to_string().as_bytes());
    for _ in 0..2 {
        let r = deliver(&st, &conn, None, Some(sig.clone()), &ev).await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(notes(&st, &fx, ticket).await.len(), n.len());
    // The body-hash delivery is a second event, but it carries no change: ignored.
    assert_eq!(outcomes(&pool).await, ["applied", "ignored"]);

    // Older than the applied state (Jira retried late): stale, nothing changes.
    let ev = issue_updated(issue_id, t1 - 60_000, Some(DONE), None);
    let r = signed(&st, &conn, &secret, "dlv-00000011", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // Outside the freshness window: stale.
    let ev = issue_updated(issue_id, t1 - 73 * 3_600_000, Some(DONE), None);
    let r = signed(&st, &conn, &secret, "dlv-00000012", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // Unknown issue and an event type that is not synced.
    let ev = issue_updated("99999", now_ms(), Some(DONE), None);
    let r = signed(&st, &conn, &secret, "dlv-00000013", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let ev = json!({"webhookEvent": "jira:issue_deleted", "timestamp": now_ms(), "issue": {"id": issue_id}});
    let r = signed(&st, &conn, &secret, "dlv-00000014", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        outcomes(&pool).await[2..],
        ["stale", "stale", "unknown_issue", "ignored"]
    );
    let ext: Option<String> = ok(
        sqlx::query_scalar("SELECT external_status FROM jira_issues WHERE external_issue_id = $1")
            .bind(issue_id)
            .fetch_one(&pool)
            .await,
        "status",
    );
    assert_eq!(ext.as_deref(), Some("In Progress"));

    // Unassigned (assignee: null) is a change.
    let ev = issue_updated(issue_id, now_ms(), None, Some(Value::Null));
    let r = signed(&st, &conn, &secret, "dlv-00000015", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let n = notes(&st, &fx, ticket).await;
    assert!(n.iter().any(|x| {
        x["kind"] == "jira_assignee"
            && s(&x["body"])
                .contains("tidak lagi punya penerima tugas (sebelumnya Budi Jira (sintetis))")
    }));

    // Comment (ADF) becomes a timeline entry with the Jira author.
    let adf = json!({"type": "doc", "version": 1, "content": [
        {"type": "paragraph", "content": [{"type": "text", "text": "Sudah dicek tim Jira."}]}
    ]});
    let r = signed(
        &st,
        &conn,
        &secret,
        "dlv-00000016",
        &comment_created(issue_id, now_ms(), adf),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let n = notes(&st, &fx, ticket).await;
    let c = n
        .iter()
        .find(|x| x["kind"] == "jira_comment")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(c["body"], "Sudah dicek tim Jira.", "{n:?}");
    assert_eq!(c["externalAuthor"], "Dewi Jira (sintetis)");
    let count = n.len();

    // Webhook audit rows carry ids and flags only.
    let wh: Vec<String> = ok(
        sqlx::query_scalar(
            "SELECT safe_change::text FROM audit_events WHERE action = 'integration.webhook'",
        )
        .fetch_all(&pool)
        .await,
        "webhook audit",
    );
    assert_eq!(wh.len(), 3, "{wh:?}");
    assert!(
        wh.iter()
            .all(|a| !a.contains("Sudah dicek") && !a.contains("Budi"))
    );

    // A closed ticket is an archive: no new entries.
    archive(&pool, ticket).await;
    let r = signed(
        &st,
        &conn,
        &secret,
        "dlv-00000017",
        &comment_created(issue_id, now_ms(), json!("Komentar baru")),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(notes(&st, &fx, ticket).await.len(), count);
    assert_eq!(
        outcomes(&pool).await.last().map(String::as_str),
        Some("ignored")
    );

    // Rotation: the old secret stops working at once; delete refuses everything.
    let jira = connection(&st, fx.a, &admin, "jira").await;
    let r = rotate(&st, &fx, &admin, &jira).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let fresh = s(&r.json["secret"]);
    assert_ne!(fresh, secret);
    let ev = issue_updated(issue_id, now_ms(), Some(DONE), None);
    let r = signed(&st, &conn, &secret, "dlv-00000020", &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = signed(&st, &conn, &fresh, "dlv-00000021", &ev).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let secrets: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM encrypted_secrets WHERE purpose = 'jira_webhook_secret'",
        )
        .fetch_one(&pool)
        .await,
        "secret rows",
    );
    assert_eq!(secrets, 1, "the replaced secret is deleted");
    let jira = connection(&st, fx.a, &admin, "jira").await;
    let r = send(
        &st,
        "DELETE",
        &ws_uri(fx.a, &format!("integrations/{conn}/webhook-secret")),
        &admin,
        None,
        Some(&json!({"version": jira["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["webhook"]["state"], "missing");
    let r = signed(&st, &conn, &fresh, "dlv-00000022", &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &st,
        "DELETE",
        &ws_uri(fx.a, &format!("integrations/{conn}/webhook-secret")),
        &admin,
        None,
        Some(&r.json["item"]["version"].clone()),
    )
    .await;
    assert_ne!(r.status, StatusCode::OK);

    // Lark has no webhook.
    let lark = connection(&st, fx.a, &admin, "lark").await;
    assert!(lark["webhook"].is_null());
    let r = rotate(&st, &fx, &admin, &lark).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp6a_disabled_connection_refuses_webhooks(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let (_stub, base) = stub().await;
    let jira = go_live(&st, &fx, &base, "jira").await;
    let admin = tok(&pool, fx.admin).await;
    let r = rotate(&st, &fx, &admin, &jira).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let secret = s(&r.json["secret"]);
    let r = send(
        &st,
        "POST",
        &ws_uri(fx.a, &format!("integrations/{}/disable", s(&jira["id"]))),
        &admin,
        None,
        Some(&json!({"version": r.json["item"]["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let ev = issue_updated("30001", now_ms(), Some(DONE), None);
    let r = signed(&st, &s(&jira["id"]), &secret, "dlv-00000100", &ev).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

// ---------- outbound transition ----------

async fn transition(st: &AppState, fx: &Fx, ticket: Uuid, to: &str) -> Resp {
    let lead = tok(&st.pool, fx.lead).await;
    let (_, version) = ticket_status(&st.pool, ticket).await;
    send(
        st,
        "POST",
        &ws_uri(fx.a, &format!("tickets/{ticket}/transitions")),
        &lead,
        None,
        Some(&json!({"to": to, "version": version})),
    )
    .await
}

/// Makes every waiting intent due (skips the inline grace period), then runs the worker once.
async fn run_worker(st: &AppState) {
    ok(
        sqlx::query(
            "UPDATE integration_outbox SET next_attempt_at = now() - interval '1 second' \
             WHERE state = 'pending' AND claim_token IS NULL",
        )
        .execute(&st.pool)
        .await,
        "make due",
    );
    ok(outbox::run_once(st).await, "tick");
}

async fn transitions_of(pool: &PgPool, ticket: Uuid) -> Vec<(String, String, Option<String>)> {
    ok(
        sqlx::query_as(
            "SELECT target_ticket_status, state, safe_error FROM integration_outbox \
             WHERE ticket_id = $1 AND operation = 'jira_transition_issue' ORDER BY created_at, id",
        )
        .bind(ticket)
        .fetch_all(pool)
        .await,
        "transitions",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp6a_ticket_transition_moves_the_jira_issue_through_the_outbox(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let (stub, base) = stub().await;
    go_live(&st, &fx, &base, "jira").await;
    let ticket = intake(&st, &fx, "cp6a-tr-0001", None).await;

    // new → open (mapped to In Progress): queued with the ticket change, moved by the worker.
    let r = transition(&st, &fx, ticket, "open").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        transitions_of(&pool, ticket).await,
        [("open".to_owned(), "pending".to_owned(), None)]
    );
    run_worker(&st).await;
    assert_eq!(transitions_of(&pool, ticket).await[0].1, "success");
    assert_eq!(calls(&stub, "POST", "/issue/OPS-1/transitions"), 1);
    assert!(lock(&stub).bodies.iter().any(|b| b.contains("\"21\"")));
    assert_eq!(lock(&stub).issues[0].2, IN_PROGRESS);
    let lead = tok(&pool, fx.lead).await;
    let r = send(
        &st,
        "GET",
        &ws_uri(fx.a, &format!("tickets/{ticket}/integrations")),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let item = r.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["operation"] == "jira_transition_issue"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(item["targetStatus"], "open", "{}", r.json);
    assert_eq!(item["state"], "success");

    // open → pending_engineering (Waiting): the Jira workflow has no such transition → failed
    // with a fixed sentence, never retried automatically.
    let r = transition(&st, &fx, ticket, "pending_engineering").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    run_worker(&st).await;
    let t = transitions_of(&pool, ticket).await;
    assert_eq!(t[1].1, "failed", "{t:?}");
    assert!(
        t[1].2
            .as_deref()
            .is_some_and(|e| e.contains("Ubah manual di Jira")),
        "{t:?}"
    );
    assert_eq!(calls(&stub, "POST", "/issue/OPS-1/transitions"), 1);
    // The failure is visible to monitoring (CP7a alert ItsmOutboxFailed).
    let metrics = itsm_api::observe::render(&st).await;
    assert!(
        metrics
            .lines()
            .any(|l| l == "itsm_outbox_intents{provider=\"jira\",state=\"failed\"} 1"),
        "{metrics}"
    );

    // pending_engineering → pending_customer: unmapped, nothing queued.
    let r = transition(&st, &fx, ticket, "pending_customer").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(transitions_of(&pool, ticket).await.len(), 2);
    let audit: Value = ok(
        sqlx::query_scalar(
            "SELECT safe_change FROM audit_events WHERE action = 'ticket.transition' \
             ORDER BY occurred_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await,
        "audit",
    );
    assert_eq!(audit["jiraTransitions"], 0);

    // → open, then → pending_engineering before the worker runs: the first intent is superseded
    // (no Jira call at all), the second is refused by the workflow as before.
    let gets_before = calls(&stub, "GET", "/issue/OPS-1?fields=status");
    for to in ["open", "pending_engineering"] {
        let r = transition(&st, &fx, ticket, to).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    }
    run_worker(&st).await;
    let t = transitions_of(&pool, ticket).await;
    assert_eq!(t[2].0, "open");
    assert_eq!(
        t[2].1, "success",
        "superseded intent settles without a call: {t:?}"
    );
    assert_eq!(t[3].1, "failed");
    assert_eq!(
        calls(&stub, "GET", "/issue/OPS-1?fields=status"),
        gets_before + 1,
        "only the current intent reads Jira"
    );

    // → open again: Jira is already In Progress, so no transition is posted.
    let r = transition(&st, &fx, ticket, "open").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    run_worker(&st).await;
    assert_eq!(transitions_of(&pool, ticket).await[4].1, "success");
    assert_eq!(calls(&stub, "POST", "/issue/OPS-1/transitions"), 1);
    assert_eq!(ticket_status(&pool, ticket).await.0, "open");
}

// ---------- Lark token cache ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp6a_lark_token_is_reused_and_dropped_when_refused(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let (stub, base) = stub().await;
    go_live(&st, &fx, &base, "jira").await;
    let lark = go_live(&st, &fx, &base, "lark").await;
    // The connection test always proves the app secret with a fresh exchange.
    assert_eq!(lock(&stub).tokens_issued, 1);

    let admin = tok(&pool, fx.admin).await;
    let r = send(
        &st,
        "POST",
        &ws_uri(
            fx.a,
            &format!("integrations/{}/directory/sync", s(&lark["id"])),
        ),
        &admin,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        lock(&stub).tokens_issued,
        2,
        "first operation exchanges once"
    );
    let lead = tok(&pool, fx.lead).await;
    let dir = send(
        &st,
        "GET",
        &ws_uri(fx.a, "intake-directory"),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(dir.status, StatusCode::OK, "{}", dir.json);
    let chat = dir.json["lark"]["options"][0]["id"].clone();
    assert!(chat.is_string(), "{}", dir.json);

    // Two deliveries and another sync reuse the cached token.
    intake(&st, &fx, "cp6a-lk-0001", Some(&chat)).await;
    intake(&st, &fx, "cp6a-lk-0002", Some(&chat)).await;
    assert_eq!(lock(&stub).lark_messages, 2);
    assert_eq!(
        lock(&stub).tokens_issued,
        2,
        "token reused across operations"
    );

    // Lark revokes the cached token: the delivery is transient (not delivered, never unknown),
    // the cache entry is dropped and the retry exchanges a new token.
    lock(&stub).revoked = Some("t-sintetis-2".to_owned());
    let lead = tok(&pool, fx.lead).await;
    let body = json!({
        "subject": "Pengaduan uji CP6a (sintetis)",
        "chronology": "Kronologi uji CP6a (sintetis).",
        "receivedAt": "2026-11-12T09:15:00+07:00",
        "intakeChannelId": channel(&pool, fx.a).await,
        "larkDestinationId": chat,
    });
    let r = send(
        &st,
        "POST",
        &ws_uri(fx.a, "tickets"),
        &lead,
        Some("cp6a-lk-0003"),
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let sync = r.json["integrations"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["provider"] == "lark"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(sync["state"], "pending", "{sync}");
    assert!(sync["nextAttemptAt"].is_string(), "{sync}");
    assert_eq!(lock(&stub).lark_messages, 2);
    ok(
        sqlx::query(
            "UPDATE integration_outbox SET next_attempt_at = now() - interval '1 second' \
             WHERE next_attempt_at IS NOT NULL",
        )
        .execute(&pool)
        .await,
        "make due",
    );
    ok(outbox::run_once(&st).await, "tick");
    let state: String = ok(
        sqlx::query_scalar("SELECT state FROM integration_outbox WHERE id = $1")
            .bind(ok(Uuid::parse_str(&s(&sync["id"])), "id"))
            .fetch_one(&pool)
            .await,
        "state",
    );
    assert_eq!(state, "success");
    assert_eq!(
        lock(&stub).tokens_issued,
        3,
        "a refused token is exchanged again"
    );
    assert_eq!(lock(&stub).lark_messages, 3);

    // No token value reaches the API, audit or outbox.
    let listing = send(
        &st,
        "GET",
        &ws_uri(fx.a, "integrations"),
        &admin,
        None,
        None,
    )
    .await;
    let audit: Vec<String> = ok(
        sqlx::query_scalar("SELECT safe_change::text FROM audit_events")
            .fetch_all(&pool)
            .await,
        "audit",
    );
    let errors: Vec<Option<String>> = ok(
        sqlx::query_scalar("SELECT safe_error FROM integration_outbox")
            .fetch_all(&pool)
            .await,
        "errors",
    );
    let mut texts = vec![listing.raw];
    texts.extend(audit);
    texts.extend(errors.into_iter().flatten());
    for t in &texts {
        assert!(!t.contains("t-sintetis"), "token leaked: {t}");
        assert!(!t.contains(LARK_SECRET), "secret leaked: {t}");
    }
}

// ---------- migration ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0027_rolls_back_and_refuses_once_sync_data_exists(pool: PgPool) {
    let shape = "SELECT count(*) FROM information_schema.columns \
                 WHERE (table_name = 'integration_connections' AND column_name = 'webhook_secret_id') \
                    OR (table_name = 'integration_outbox' AND column_name = 'target_ticket_status') \
                    OR (table_name = 'ticket_activities' AND column_name = 'external_author')";
    let up: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "up");
    assert_eq!(up, 3);
    ok(db::MIGRATOR.undo(&pool, 26).await, "undo to 0026");
    let down: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "down");
    assert_eq!(down, 0);
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let again: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "again");
    assert_eq!(again, 3);

    // A stored webhook secret blocks the down migration instead of being dropped.
    let fx = fixture(&pool).await;
    let st = state(&pool);
    let (_stub, base) = stub().await;
    let jira = go_live(&st, &fx, &base, "jira").await;
    let admin = tok(&pool, fx.admin).await;
    let r = rotate(&st, &fx, &admin, &jira).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(db::MIGRATOR.undo(&pool, 26).await.is_err());
    let kept: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM integration_connections WHERE webhook_secret_id IS NOT NULL",
        )
        .fetch_one(&pool)
        .await,
        "kept",
    );
    assert_eq!(kept, 1);
}
