//! CP5b live Jira/Lark integration against a real PostgreSQL and an in-process HTTP stub that
//! speaks the Jira Cloud REST v3 / Lark Open Platform shapes the adapters use. Covers: a real
//! connection test (ok, 401, timeout), the write-only credential (never in responses, logs, audit
//! or DB plaintext), delivery from intake (issue + chat message), transient failures that end in a
//! permanent failure after the automatic budget, idempotency (a retried create finds the issue by
//! its label instead of creating a second one), the SSRF guard without the lab flag, the keyring
//! requirement, and migration 0024 rolling back. All data is synthetic; no real provider is used.

use std::collections::HashMap;
use std::fmt::Display;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::outbox;
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

const JIRA_TOKEN: &str = "jira-token-SINTETIS-7f3a9c2e51";
const LARK_SECRET: &str = "lark-secret-SINTETIS-b81d44e0c6";
const JIRA_EMAIL: &str = "svc-itsm@example.test";
const LARK_APP: &str = "cli_sintetis01";

// ---------- provider stub ----------

#[derive(Default)]
struct StubState {
    /// (method, path with query)
    calls: Vec<(String, String)>,
    /// Bodies of every request (to prove no ticket chronology leaves the app).
    bodies: Vec<String>,
    /// `/myself` sleeps this long (timeout test).
    slow: Option<Duration>,
    /// `/search/jql` answers with this status instead of searching.
    search_status: Option<u16>,
    /// The next create stores the issue but answers this status (accepted, reply lost).
    create_then_status: Option<u16>,
    /// Created issues: (id, key, labels).
    issues: Vec<(String, String, Vec<String>)>,
    /// Lark messages: (chat_id, text, uuid).
    messages: Vec<(String, String, String)>,
}

type Stub = Arc<Mutex<StubState>>;

fn json_resp(status: u16, v: Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        code,
        [(header::CONTENT_TYPE, "application/json")],
        v.to_string(),
    )
        .into_response()
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
    let slow = {
        let mut s = st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s.calls.push((method.to_string(), full.clone()));
        s.bodies.push(body.to_string());
        s.slow
    };
    if path.starts_with("/rest/api/3/") {
        let basic = format!(
            "Basic {}",
            STANDARD.encode(format!("{JIRA_EMAIL}:{JIRA_TOKEN}"))
        );
        if auth != basic {
            return json_resp(401, json!({"errorMessages": ["unauthorized"]}));
        }
        return jira(&st, &method, &path, &body, slow).await;
    }
    if path.starts_with("/open-apis/") {
        return lark(&st, &method, &path, &auth, &body);
    }
    json_resp(404, json!({}))
}

async fn jira(
    st: &Stub,
    method: &Method,
    path: &str,
    body: &Value,
    slow: Option<Duration>,
) -> Response {
    match (method.as_str(), path) {
        ("GET", "/rest/api/3/myself") => {
            if let Some(d) = slow {
                tokio::time::sleep(d).await;
            }
            json_resp(
                200,
                json!({"accountId": "svc-1", "displayName": "Akun layanan", "active": true}),
            )
        }
        ("GET", "/rest/api/3/project/OPS") => json_resp(
            200,
            json!({"key": "OPS", "issueTypes": [{"id": "10001", "name": "Task"}, {"id": "10002", "name": "Bug"}]}),
        ),
        ("GET", "/rest/api/3/priority") => json_resp(
            200,
            json!([{"id": "1", "name": "Highest"}, {"id": "3", "name": "Medium"}]),
        ),
        ("GET", "/rest/api/3/project/OPS/statuses") => json_resp(
            200,
            json!([{"statuses": [{"id": "10000", "name": "To Do"}, {"id": "10003", "name": "Done"}]}]),
        ),
        ("GET", "/rest/api/3/user/assignable/search") => json_resp(
            200,
            json!([
                {"accountId": "acc-budi", "displayName": "Budi Jira (sintetis)", "active": true},
                {"accountId": "acc-off", "displayName": "Nonaktif", "active": false}
            ]),
        ),
        ("POST", "/rest/api/3/search/jql") => {
            let s = st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(code) = s.search_status {
                return json_resp(code, json!({"errorMessages": ["busy"]}));
            }
            let jql = body["jql"].as_str().unwrap_or_default();
            let found: Vec<Value> = s
                .issues
                .iter()
                .filter(|(_, _, labels)| labels.iter().any(|l| jql == format!("labels = \"{l}\"")))
                .map(|(id, key, _)| json!({"id": id, "key": key, "fields": {"status": {"id": "10000", "name": "To Do"}}}))
                .collect();
            json_resp(200, json!({"issues": found}))
        }
        ("POST", "/rest/api/3/issue") => {
            let mut s = st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let n = s.issues.len() + 1;
            let (id, key) = (format!("{}", 20000 + n), format!("OPS-{n}"));
            let labels: Vec<String> = body["fields"]["labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            s.issues.push((id.clone(), key.clone(), labels));
            if let Some(code) = s.create_then_status.take() {
                return json_resp(code, json!({"errorMessages": ["busy"]}));
            }
            json_resp(201, json!({"id": id, "key": key}))
        }
        _ => json_resp(404, json!({"errorMessages": ["not found"]})),
    }
}

fn lark(st: &Stub, method: &Method, path: &str, auth: &str, body: &Value) -> Response {
    if (method.as_str(), path) == ("POST", "/open-apis/auth/v3/tenant_access_token/internal") {
        if body["app_id"] == LARK_APP && body["app_secret"] == LARK_SECRET {
            return json_resp(
                200,
                json!({"code": 0, "tenant_access_token": "t-sintetis", "expire": 7200}),
            );
        }
        return json_resp(200, json!({"code": 10014, "msg": "app secret invalid"}));
    }
    if auth != "Bearer t-sintetis" {
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
                {"chat_id": "oc_eskalasi", "name": "Eskalasi (sintetis)"},
                {"chat_id": "oc_ops", "name": "Ops (sintetis)"}
            ], "has_more": false}}),
        ),
        ("POST", "/open-apis/im/v1/messages") => {
            let mut s = st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let uuid = body["uuid"].as_str().unwrap_or_default().to_owned();
            if !s.messages.iter().any(|m| m.2 == uuid) {
                let text = body["content"].as_str().unwrap_or_default().to_owned();
                let chat = body["receive_id"].as_str().unwrap_or_default().to_owned();
                s.messages.push((chat, text, uuid));
            }
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

fn calls(st: &Stub, method: &str, path_prefix: &str) -> usize {
    st.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .calls
        .iter()
        .filter(|(m, p)| m == method && p.starts_with(path_prefix))
        .count()
}

// ---------- app under test ----------

#[derive(Clone, Copy)]
struct Opts {
    lab: bool,
    keyring: bool,
}

const LIVE: Opts = Opts {
    lab: true,
    keyring: true,
};

fn state(pool: &PgPool, o: Opts) -> AppState {
    state_with(pool, o, Vec::new())
}

fn state_with(pool: &PgPool, o: Opts, extra: Vec<(&str, String)>) -> AppState {
    let key = STANDARD.encode([7u8; 32]);
    let keyring = format!("k1:{key}");
    let mut vars: Vec<(&str, String)> = vec![
        ("ITSM_ENV", "uat".into()),
        ("ITSM_ACCESS_MODE", "uat_superadmin".into()),
        ("DATABASE_URL", "postgres://unused-in-tests".into()),
        ("ITSM_COOKIE_SECURE", "false".into()),
        ("ITSM_INTEGRATIONS", "live".into()),
        ("ITSM_INTEGRATIONS_TIMEOUT_MS", "800".into()),
        (
            "ITSM_INTEGRATIONS_LAB",
            if o.lab { "on" } else { "off" }.into(),
        ),
    ];
    if o.keyring {
        vars.push(("ITSM_KEYRING", keyring));
        vars.push(("ITSM_KEYRING_ACTIVE", "k1".into()));
    }
    vars.extend(extra);
    let vars: HashMap<String, String> = vars.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
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
    assert_eq!(r.json["integrationsMode"], "live");
    match r.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["provider"] == provider))
    {
        Some(c) => c.clone(),
        None => panic!("no {provider} connection in {}", r.json),
    }
}

/// Settings + credential for one provider; returns the connection after the last write.
async fn configure(
    st: &AppState,
    ws: Uuid,
    t: &Tok,
    provider: &str,
    base: &str,
    secret: &str,
) -> Value {
    let c = connection(st, ws, t, provider).await;
    let id = s(&c["id"]);
    let body = if provider == "jira" {
        json!({"baseUrl": base, "email": JIRA_EMAIL, "projectKey": "OPS", "issueTypeId": "10001",
               "priorityMap": {"urgent": "1"}, "statusMap": {"resolved": "10003"}, "version": c["version"]})
    } else {
        json!({"appId": LARK_APP, "baseUrl": base, "version": c["version"]})
    };
    let r = send(
        st,
        "PUT",
        &ws_uri(ws, &format!("integrations/{id}/config")),
        t,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["mode"], "live");
    assert_eq!(
        r.json["item"]["state"], "draft",
        "a new target goes back to draft"
    );
    let v = r.json["item"]["version"].clone();
    let r = send(
        st,
        "PUT",
        &ws_uri(ws, &format!("integrations/{id}/credential")),
        t,
        None,
        Some(&json!({"secret": secret, "version": v})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["credential"]["state"], "stored");
    assert!(!r.raw.contains(secret), "credential echoed: {}", r.raw);
    r.json["item"].clone()
}

async fn step(st: &AppState, ws: Uuid, t: &Tok, c: &Value, step: &str) -> Resp {
    send(
        st,
        "POST",
        &ws_uri(ws, &format!("integrations/{}/{step}", s(&c["id"]))),
        t,
        None,
        Some(&json!({"version": c["version"]})),
    )
    .await
}

/// Configures, tests and activates Jira and Lark against the stub.
async fn go_live(st: &AppState, fx: &Fx, base: &str) -> (Value, Value) {
    let admin = tok(&st.pool, fx.admin).await;
    let mut out = Vec::new();
    for (p, secret) in [("jira", JIRA_TOKEN), ("lark", LARK_SECRET)] {
        let c = configure(st, fx.a, &admin, p, base, secret).await;
        let r = step(st, fx.a, &admin, &c, "test").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(r.json["item"]["lastTestOk"], true, "{}", r.json);
        assert_eq!(r.json["item"]["state"], "tested");
        let r = step(st, fx.a, &admin, &r.json["item"], "activate").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(r.json["item"]["state"], "active");
        out.push(r.json["item"].clone());
    }
    let lark = out.pop().unwrap_or_default();
    let jira = out.pop().unwrap_or_default();
    (jira, lark)
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

const CHRONOLOGY: &str = "Kronologi uji CP5b (sintetis): nasabah fiktif 0812-0000-0000.";

/// Intake with a Jira issue (assignee from the real user search) and a Lark chat (from the synced list).
async fn intake(st: &AppState, fx: &Fx, lark: &Value, key: &str) -> Resp {
    intake_titled(st, fx, lark, key, "Tindak lanjut uji CP5b", None).await
}

async fn intake_titled(
    st: &AppState,
    fx: &Fx,
    lark: &Value,
    key: &str,
    title: &str,
    category: Option<Uuid>,
) -> Resp {
    let admin = tok(&st.pool, fx.admin).await;
    let lead = tok(&st.pool, fx.lead).await;
    let r = send(
        st,
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
    assert_eq!(r.json["synced"], 2);
    let users = send(
        st,
        "GET",
        &ws_uri(fx.a, "intake/jira-users?query=bu"),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(users.status, StatusCode::OK, "{}", users.json);
    let items = users.json["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        items.len(),
        1,
        "inactive Jira users are not offered: {}",
        users.json
    );
    assert_eq!(items[0]["source"], "jira");
    let dir = send(
        st,
        "GET",
        &ws_uri(fx.a, "intake-directory"),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(dir.status, StatusCode::OK, "{}", dir.json);
    assert_eq!(dir.json["integrationsMode"], "live");
    assert_eq!(dir.json["jira"]["usable"], true);
    for p in ["jira", "lark"] {
        let opts = dir.json[p]["options"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            opts.iter().all(|o| o["source"] != "dummy"),
            "dummy offered in live: {}",
            dir.json
        );
    }
    let chat = dir.json["lark"]["options"]
        .as_array()
        .and_then(|a| a.iter().find(|o| o["displayName"] == "Eskalasi (sintetis)"))
        .map(|o| o["id"].clone())
        .unwrap_or(Value::Null);
    let body = json!({
        "subject": "Pengaduan uji CP5b (sintetis)",
        "chronology": CHRONOLOGY,
        "receivedAt": "2026-11-10T09:15:00+07:00",
        "intakeChannelId": channel(&st.pool, fx.a).await,
        "jira": {"mode": "create", "title": title, "assigneeId": items[0]["id"]},
        "larkDestinationId": chat,
        "categoryId": category,
    });
    send(
        st,
        "POST",
        &ws_uri(fx.a, "tickets"),
        &lead,
        Some(key),
        Some(&body),
    )
    .await
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

async fn outbox_state(pool: &PgPool, id: Uuid) -> (String, i32, Option<String>) {
    ok(
        sqlx::query_as(
            "SELECT state, auto_attempts, safe_error FROM integration_outbox WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await,
        "outbox row",
    )
}

// ---------- log capture ----------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One process-wide subscriber for this binary (a scoped one misses cached callsites).
fn capture() -> Captured {
    static CAPTURED: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
    CAPTURED
        .get_or_init(|| {
            let captured = Captured::default();
            let writer = captured.clone();
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .init();
            captured
        })
        .clone()
}

// ---------- tests ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn connection_test_ok_then_401_then_timeout(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (stub, base) = stub().await;
    let admin = tok(&pool, fx.admin).await;

    let c = configure(&st, fx.a, &admin, "jira", &base, JIRA_TOKEN).await;
    let r = step(&st, fx.a, &admin, &c, "test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let item = &r.json["item"];
    assert_eq!(item["lastTestOk"], true, "{item}");
    assert_eq!(item["state"], "tested");
    assert!(s(&item["lastTestNote"]).contains("Proyek OPS"), "{item}");
    assert_eq!(calls(&stub, "GET", "/rest/api/3/myself"), 1);
    assert_eq!(
        calls(&stub, "GET", "/rest/api/3/priority"),
        1,
        "mapped priorities are checked"
    );

    // Options for the mapping form come from Jira.
    let r = send(
        &st,
        "GET",
        &ws_uri(fx.a, &format!("integrations/{}/options", s(&c["id"]))),
        &admin,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["issueTypes"][1]["name"], "Bug");
    assert_eq!(r.json["statuses"][1]["id"], "10003");

    // Wrong token: Jira answers 401, the test fails with a safe note and the state is not tested.
    let r = send(
        &st,
        "PUT",
        &ws_uri(fx.a, &format!("integrations/{}/credential", s(&c["id"]))),
        &admin,
        None,
        Some(&json!({"secret": "token-salah-sintetis", "version": item["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        r.json["item"]["state"], "draft",
        "a replaced credential needs a new test"
    );
    let r = step(&st, fx.a, &admin, &r.json["item"], "test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["lastTestOk"], false, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "draft");
    let note = s(&r.json["item"]["lastTestNote"]);
    assert!(
        note.contains("401") || note.to_lowercase().contains("kredensial"),
        "{note}"
    );
    let r = step(&st, fx.a, &admin, &r.json["item"], "activate").await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no activation without a passing test: {}",
        r.json
    );

    // Slow provider: the test answers within the budget instead of hanging.
    let c = connection(&st, fx.a, &admin, "jira").await;
    let r = send(
        &st,
        "PUT",
        &ws_uri(fx.a, &format!("integrations/{}/credential", s(&c["id"]))),
        &admin,
        None,
        Some(&json!({"secret": JIRA_TOKEN, "version": c["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    stub.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .slow = Some(Duration::from_secs(3));
    let started = std::time::Instant::now();
    let r = step(&st, fx.a, &admin, &r.json["item"], "test").await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "test waited {:?}",
        started.elapsed()
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["lastTestOk"], false);
    assert!(
        s(&r.json["item"]["lastTestNote"]).contains("batas waktu"),
        "{}",
        r.json
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn lark_test_checks_app_secret_bot_and_chats(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (_stub, base) = stub().await;
    let admin = tok(&pool, fx.admin).await;
    let c = configure(&st, fx.a, &admin, "lark", &base, "secret-salah-sintetis").await;
    let r = step(&st, fx.a, &admin, &c, "test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["lastTestOk"], false, "{}", r.json);
    let c = configure(&st, fx.a, &admin, "lark", &base, LARK_SECRET).await;
    let r = step(&st, fx.a, &admin, &c, "test").await;
    assert_eq!(r.json["item"]["lastTestOk"], true, "{}", r.json);
    let note = s(&r.json["item"]["lastTestNote"]);
    assert!(
        note.contains("ITSM Bot (sintetis)") && note.contains("2 grup"),
        "{note}"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn delivers_issue_and_message_without_leaking_secrets(pool: PgPool) {
    let captured = capture();
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (stub, base) = stub().await;
    let (jira, _lark) = go_live(&st, &fx, &base).await;
    let lark = connection(&st, fx.a, &tok(&pool, fx.admin).await, "lark").await;

    let r = intake(&st, &fx, &lark, "cp5b-live-0001").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let j = sync_of(&r.json, "jira");
    assert_eq!(j["state"], "success", "{j}");
    assert_eq!(j["mode"], "live");
    assert_eq!(j["issueKey"], "OPS-1");
    assert_eq!(sync_of(&r.json, "lark")["state"], "success");

    let (issues, messages, bodies) = {
        let g = stub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (g.issues.clone(), g.messages.clone(), g.bodies.join("\n"))
    };
    assert_eq!(issues.len(), 1);
    assert_eq!(
        issues[0].2[0],
        format!("itsm-{}", s(&j["id"])),
        "idempotency label"
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0, "oc_eskalasi");
    assert_eq!(
        messages[0].2,
        s(&sync_of(&r.json, "lark")["id"]),
        "Lark dedupe uuid = outbox id"
    );
    assert!(
        !bodies.contains("0812-0000-0000"),
        "chronology left the app"
    );
    assert!(
        bodies.contains("acc-budi"),
        "assignee from Jira user search"
    );

    // Secrets: never in API responses, logs, audit details or DB plaintext.
    let admin = tok(&pool, fx.admin).await;
    let mut texts = vec![r.raw.clone()];
    for uri in [
        ws_uri(fx.a, "integrations"),
        ws_uri(fx.a, "intake-directory"),
        ws_uri(fx.a, &format!("integrations/{}/options", s(&jira["id"]))),
    ] {
        texts.push(send(&st, "GET", &uri, &admin, None, None).await.raw);
    }
    let log = String::from_utf8_lossy(
        &captured
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .into_owned();
    assert!(
        log.contains("outbox") || log.contains("integration"),
        "log capture is empty"
    );
    texts.push(log);
    let audit: Vec<String> = ok(
        sqlx::query_scalar("SELECT safe_change::text FROM audit_events WHERE workspace_id = $1")
            .bind(fx.a)
            .fetch_all(&pool)
            .await,
        "audit",
    );
    texts.extend(audit);
    let configs: Vec<String> = ok(
        sqlx::query_scalar(
            "SELECT config::text FROM integration_connections WHERE workspace_id = $1",
        )
        .bind(fx.a)
        .fetch_all(&pool)
        .await,
        "configs",
    );
    texts.extend(configs);
    let basic = STANDARD.encode(format!("{JIRA_EMAIL}:{JIRA_TOKEN}"));
    for t in &texts {
        for secret in [JIRA_TOKEN, LARK_SECRET, basic.as_str(), "t-sintetis"] {
            assert!(!t.contains(secret), "secret {secret} leaked in: {t}");
        }
    }
    let blobs: Vec<Vec<u8>> = ok(
        sqlx::query_scalar(
            "SELECT ciphertext FROM encrypted_secrets WHERE purpose IN ('jira_api_token', 'lark_app_secret')",
        )
        .fetch_all(&pool)
        .await,
        "ciphertext",
    );
    assert_eq!(blobs.len(), 2);
    for b in &blobs {
        for secret in [JIRA_TOKEN, LARK_SECRET] {
            assert!(
                !b.windows(secret.len()).any(|w| w == secret.as_bytes()),
                "plaintext secret in DB"
            );
        }
    }
    let replaced = ok(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE workspace_id = $1 AND action = 'integration.credential.replace'",
        )
        .bind(fx.a)
        .fetch_one(&pool)
        .await,
        "audit count",
    );
    assert_eq!(
        replaced, 2,
        "credential writes are audited (without values)"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn transient_failures_end_permanent_and_retry_is_idempotent(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (stub, base) = stub().await;
    let (jira, _) = go_live(&st, &fx, &base).await;
    let lark = connection(&st, fx.a, &tok(&pool, fx.admin).await, "lark").await;

    // Jira is throttling: the label search answers 429 before anything is written.
    stub.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .search_status = Some(429);
    let r = intake(&st, &fx, &lark, "cp5b-live-0002").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let j = sync_of(&r.json, "jira").clone();
    assert_eq!(j["state"], "pending", "{j}");
    assert!(j["nextAttemptAt"].is_string());
    let id = ok(Uuid::parse_str(&s(&j["id"])), "id");
    for n in 2..=outbox::MAX_AUTO_ATTEMPTS {
        make_due(&pool, id).await;
        let t = ok(outbox::run_once(&st).await, "tick");
        assert_eq!(t.settled, 1, "attempt {n}");
    }
    let (state_now, autos, err) = outbox_state(&pool, id).await;
    assert_eq!(
        (state_now.as_str(), autos),
        ("failed", outbox::MAX_AUTO_ATTEMPTS)
    );
    assert!(err.unwrap_or_default().contains("Batas 5"));
    assert_eq!(
        calls(&stub, "POST", "/rest/api/3/issue"),
        0,
        "nothing created while throttled"
    );

    // The permanent failure is visible on the ticket and a permitted officer resends it.
    let ticket = s(&r.json["ticket"]["id"]);
    let admin = tok(&pool, fx.admin).await;
    let view = send(
        &st,
        "GET",
        &ws_uri(fx.a, &format!("tickets/{ticket}/integrations")),
        &admin,
        None,
        None,
    )
    .await;
    assert_eq!(view.status, StatusCode::OK, "{}", view.json);
    assert_eq!(view.json["items"][0]["state"], "failed");
    assert_eq!(view.json["canRecover"], true);

    // Jira recovers but the create reply is lost (429 after storing): the next attempt finds the
    // issue by its label instead of creating a second one.
    {
        let mut g = stub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.search_status = None;
        g.create_then_status = Some(429);
    }
    let attempt = view.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["provider"] == "jira"))
        .map(|i| i["attempt"].clone())
        .unwrap_or(Value::Null);
    let r = send(
        &st,
        "POST",
        &ws_uri(fx.a, &format!("integration-outbox/{id}/retry")),
        &admin,
        None,
        Some(&json!({"attempt": attempt})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "pending", "{}", r.json);
    make_due(&pool, id).await;
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.settled, 1);
    let (state_now, _, _) = outbox_state(&pool, id).await;
    assert_eq!(state_now, "success");
    assert_eq!(
        calls(&stub, "POST", "/rest/api/3/issue"),
        1,
        "one create only"
    );
    assert_eq!(
        stub.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .issues
            .len(),
        1
    );
    // Seeded simulation links stay on the connection; only one real issue was linked.
    let keys: Vec<String> = ok(
        sqlx::query_scalar(
            "SELECT issue_key FROM jira_issues WHERE connection_id = $1::uuid AND issue_key NOT LIKE 'SIM-%'",
        )
        .bind(s(&jira["id"]))
        .fetch_all(&pool)
        .await,
        "linked key",
    );
    assert_eq!(keys, vec!["OPS-1".to_owned()]);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn private_targets_are_refused_without_the_lab_flag(pool: PgPool) {
    let fx = fixture(&pool).await;
    let (stub, base) = stub().await;
    let prod_like = state(
        &pool,
        Opts {
            lab: false,
            keyring: true,
        },
    );
    let admin = tok(&pool, fx.admin).await;
    let c = connection(&prod_like, fx.a, &admin, "jira").await;
    for bad in [
        base.as_str(),
        "https://127.0.0.1",
        "https://10.0.0.5",
        "https://localhost",
        "https://[::1]",
    ] {
        let r = send(
            &prod_like,
            "PUT",
            &ws_uri(fx.a, &format!("integrations/{}/config", s(&c["id"]))),
            &admin,
            None,
            Some(&json!({"baseUrl": bad, "email": JIRA_EMAIL, "projectKey": "OPS", "version": c["version"]})),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["field"], "baseUrl");
    }

    // Saved while the lab flag was on, then the server runs without it: the call is refused
    // before any request leaves (also for names that resolve to private addresses after DNS).
    let lab = state(&pool, LIVE);
    let c = configure(&lab, fx.a, &admin, "jira", &base, JIRA_TOKEN).await;
    let r = step(&prod_like, fx.a, &admin, &c, "test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["lastTestOk"], false);
    assert!(
        s(&r.json["item"]["lastTestNote"]).contains("ditolak"),
        "{}",
        r.json
    );
    assert_eq!(
        stub.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .calls
            .len(),
        0,
        "no request reached the stub"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn credentials_and_activation_need_the_keyring(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(
        &pool,
        Opts {
            lab: true,
            keyring: false,
        },
    );
    let admin = tok(&pool, fx.admin).await;
    let list = send(
        &st,
        "GET",
        &ws_uri(fx.a, "integrations"),
        &admin,
        None,
        None,
    )
    .await;
    assert_eq!(list.json["keyringConfigured"], false);
    let c = connection(&st, fx.a, &admin, "jira").await;
    let r = send(
        &st,
        "PUT",
        &ws_uri(fx.a, &format!("integrations/{}/credential", s(&c["id"]))),
        &admin,
        None,
        Some(&json!({"secret": JIRA_TOKEN, "version": c["version"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "keyring_missing");
    assert!(s(&r.json["error"]["message"]).contains("ITSM_KEYRING"));
    assert!(!r.raw.contains(JIRA_TOKEN));

    // The seeded simulation connection is not usable for intake in live mode.
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
    assert_eq!(dir.json["jira"]["usable"], false, "{}", dir.json);
    assert_eq!(dir.json["jira"]["options"], json!([]));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0024_rolls_back_and_reapplies(pool: PgPool) {
    let shape = "SELECT count(*) FROM information_schema.columns \
                 WHERE table_name = 'integration_connections' \
                   AND column_name IN ('credential_secret_id', 'credential_updated_at', 'last_test_ok')";
    let up: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "up");
    assert_eq!(up, 3);
    ok(db::MIGRATOR.undo(&pool, 23).await, "undo to 0023");
    let down: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "down");
    assert_eq!(down, 0);
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let again: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "again");
    assert_eq!(again, 3);

    // Once a live connection exists, down is refused instead of dropping it.
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (_stub, base) = stub().await;
    let admin = tok(&pool, fx.admin).await;
    configure(&st, fx.a, &admin, "jira", &base, JIRA_TOKEN).await;
    assert!(db::MIGRATOR.undo(&pool, 23).await.is_err());
    let kept: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM integration_connections WHERE mode = 'live'")
            .fetch_one(&pool)
            .await,
        "kept",
    );
    assert_eq!(kept, 1);
}

async fn put_config(st: &AppState, ws: Uuid, t: &Tok, c: &Value, body: Value) -> Resp {
    let mut body = body;
    body["version"] = c["version"].clone();
    send(
        st,
        "PUT",
        &ws_uri(ws, &format!("integrations/{}/config", s(&c["id"]))),
        t,
        None,
        Some(&body),
    )
    .await
}

async fn grant_manage_only(pool: &PgPool, fx: &Fx) {
    ok(
        sqlx::query(
            "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by) \
             SELECT workspace_id, id, 'integration.manage', $3 FROM workspace_memberships \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(fx.a)
        .bind(fx.lead)
        .bind(fx.admin)
        .execute(pool)
        .await,
        "grant integration.manage",
    );
}

async fn secrets_in(pool: &PgPool, ws: Uuid) -> i64 {
    ok(
        sqlx::query_scalar("SELECT count(*) FROM encrypted_secrets WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(pool)
            .await,
        "secrets",
    )
}

async fn last_configure_audit(pool: &PgPool, ws: Uuid) -> Value {
    ok(
        sqlx::query_scalar(
            "SELECT safe_change FROM audit_events WHERE workspace_id = $1 \
             AND action = 'integration.configure' ORDER BY occurred_at DESC LIMIT 1",
        )
        .bind(ws)
        .fetch_one(pool)
        .await,
        "configure audit",
    )
}

/// CP5B-01: a holder of `integration.manage` without `integration.credentials` points Jira at
/// another host. The stored token is deleted in the same transaction, so Test, options and the
/// intake user search send nothing anywhere until a credential holder enters it again.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5b01_new_target_deletes_the_stored_credential(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (evil, evil_base) = stub().await;
    let (stub, base) = stub().await;
    let admin = tok(&pool, fx.admin).await;
    let c = configure(&st, fx.a, &admin, "jira", &base, JIRA_TOKEN).await;
    let before = secrets_in(&pool, fx.a).await;
    let calls_before = stub.lock().map(|g| g.calls.len()).unwrap_or_default();

    grant_manage_only(&pool, &fx).await;
    let lead = tok(&pool, fx.lead).await;
    let list = send(&st, "GET", &ws_uri(fx.a, "integrations"), &lead, None, None).await;
    assert_eq!(list.json["canManageCredentials"], false, "{}", list.json);
    let jira = json!({"baseUrl": evil_base, "email": JIRA_EMAIL, "projectKey": "OPS",
                      "issueTypeId": "10001", "priorityMap": {"urgent": "1"}, "statusMap": {"resolved": "10003"}});
    let r = put_config(&st, fx.a, &lead, &c, jira.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let item = r.json["item"].clone();
    assert_eq!(item["state"], "draft");
    assert_eq!(item["credential"]["state"], "missing", "{item}");
    assert_eq!(
        secrets_in(&pool, fx.a).await,
        before - 1,
        "old ciphertext deleted"
    );
    let audit = last_configure_audit(&pool, fx.a).await;
    assert_eq!(audit["credentialCleared"], true, "{audit}");

    // Nothing leaves for the new host (nor the old one) without a credential.
    let r = step(&st, fx.a, &lead, &item, "test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["lastTestOk"], false);
    assert!(
        s(&r.json["item"]["lastTestNote"]).contains("Kredensial belum"),
        "{}",
        r.json
    );
    let r = send(
        &st,
        "GET",
        &ws_uri(fx.a, &format!("integrations/{}/options", s(&c["id"]))),
        &lead,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(evil.lock().map(|g| g.calls.len()).unwrap_or(1), 0);
    assert_eq!(
        stub.lock().map(|g| g.calls.len()).unwrap_or_default(),
        calls_before
    );

    // A credential holder enters it again for the new target; a mapping-only change keeps it,
    // a different account (email) deletes it again.
    let c = configure(&st, fx.a, &admin, "jira", &base, JIRA_TOKEN).await;
    let mut mapping = jira.clone();
    mapping["baseUrl"] = json!(base);
    mapping["issueTypeId"] = json!("10002");
    let r = put_config(&st, fx.a, &lead, &c, mapping.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["credential"]["state"], "stored");
    assert_eq!(
        last_configure_audit(&pool, fx.a).await["credentialCleared"],
        false
    );
    let mut account = mapping;
    account["email"] = json!("lain@example.test");
    let r = put_config(&st, fx.a, &lead, &r.json["item"], account).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["credential"]["state"], "missing");

    // Lark: the App ID is part of the target; an empty address equals the international default.
    let l = configure(&st, fx.a, &admin, "lark", "", LARK_SECRET).await;
    let r = put_config(
        &st,
        fx.a,
        &lead,
        &l,
        json!({"appId": LARK_APP, "baseUrl": "https://open.larksuite.com"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        r.json["item"]["credential"]["state"], "stored",
        "same effective address"
    );
    let r = put_config(
        &st,
        fx.a,
        &lead,
        &r.json["item"],
        json!({"appId": "cli_lain02"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["credential"]["state"], "missing");
    assert!(!r.raw.contains(LARK_SECRET) && !r.raw.contains(JIRA_TOKEN));
}

/// CP5B-02: outside lab mode only the providers' own hosts on port 443 are accepted.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5b02_configure_accepts_only_provider_hosts_on_443(pool: PgPool) {
    let fx = fixture(&pool).await;
    let prod_like = state(
        &pool,
        Opts {
            lab: false,
            keyring: true,
        },
    );
    let admin = tok(&pool, fx.admin).await;
    let jira = connection(&prod_like, fx.a, &admin, "jira").await;
    for bad in [
        "https://evil.example",
        "https://acme.atlassian.net:8443",
        "https://acme.atlassian.net.evil.example",
        "https://a.b.atlassian.net",
        "https://open.larksuite.com",
        "https://8.8.8.8",
    ] {
        let r = put_config(
            &prod_like,
            fx.a,
            &admin,
            &jira,
            json!({"baseUrl": bad, "email": JIRA_EMAIL, "projectKey": "OPS"}),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["field"], "baseUrl");
    }
    let r = put_config(&prod_like, fx.a, &admin, &jira,
        json!({"baseUrl": "https://Kantor.atlassian.net/", "email": JIRA_EMAIL, "projectKey": "OPS"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        r.json["item"]["settings"]["baseUrl"],
        "https://kantor.atlassian.net"
    );

    let lark = connection(&prod_like, fx.a, &admin, "lark").await;
    for bad in [
        "https://open.larksuite.com:8443",
        "https://kantor.atlassian.net",
        "https://evil.example",
    ] {
        let r = put_config(
            &prod_like,
            fx.a,
            &admin,
            &lark,
            json!({"appId": LARK_APP, "baseUrl": bad}),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["field"], "baseUrl");
    }
    let r = put_config(
        &prod_like,
        fx.a,
        &admin,
        &lark,
        json!({"appId": LARK_APP, "baseUrl": "https://open.feishu.cn"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

fn issue_bodies(st: &Stub) -> Vec<Value> {
    st.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .bodies
        .iter()
        .filter_map(|b| serde_json::from_str::<Value>(b).ok())
        .filter(|b| b["fields"]["summary"].is_string())
        .collect()
}

/// CP5B-05: the officer's free-text Jira title (here full of synthetic customer data) stays in
/// ITSM; the issue summary is the server template "[ITSM] <ticket> — <category>".
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5b05_jira_summary_never_carries_the_free_text_title(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (stub, base) = stub().await;
    let (_, lark) = go_live(&st, &fx, &base).await;
    let (category_id, category): (Uuid, String) = ok(
        sqlx::query_as(
            "SELECT id, label FROM categories WHERE workspace_id = $1 AND active ORDER BY sort_order, code LIMIT 1",
        )
        .bind(fx.a)
        .fetch_one(&pool)
        .await,
        "category",
    );
    let pii = [
        "Budi Santoso",
        "3201234567890123",
        "081234567890",
        "1234567890123",
    ];
    let title = format!(
        "Nasabah {} NIK {} HP {} rek {}",
        pii[0], pii[1], pii[2], pii[3]
    );
    let r = intake_titled(&st, &fx, &lark, "cp5b05-0001", &title, Some(category_id)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(sync_of(&r.json, "jira")["state"], "success", "{}", r.json);
    let number = s(&r.json["ticket"]["displayNumber"]);
    let bodies = issue_bodies(&stub);
    assert_eq!(bodies.len(), 1);
    let summary = s(&bodies[0]["fields"]["summary"]);
    assert_eq!(summary, format!("[ITSM] {number} — {category}"));
    let all = stub
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .bodies
        .join("\n");
    for p in pii {
        assert!(!all.contains(p), "{p} left the app: {all}");
    }
    // The title is kept locally for officers.
    let local: String = ok(
        sqlx::query_scalar(
            "SELECT title FROM related_work WHERE workspace_id = $1 AND title IS NOT NULL",
        )
        .bind(fx.a)
        .fetch_one(&pool)
        .await,
        "local title",
    );
    assert_eq!(local, title);
}

/// CP5B-06: Jira stores the issue but a balancer answers 503: the outcome is unknown (no
/// automatic retry that could create a second issue while the label search lags).
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5b06_503_after_the_create_is_unknown_not_retried(pool: PgPool) {
    let fx = fixture(&pool).await;
    let st = state(&pool, LIVE);
    let (stub, base) = stub().await;
    let (_, lark) = go_live(&st, &fx, &base).await;
    stub.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .create_then_status = Some(503);
    let r = intake(&st, &fx, &lark, "cp5b06-0001").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let j = sync_of(&r.json, "jira").clone();
    assert_eq!(j["state"], "unknown", "{j}");
    let id = ok(Uuid::parse_str(&s(&j["id"])), "id");
    make_due(&pool, id).await;
    let t = ok(outbox::run_once(&st).await, "tick");
    assert_eq!(t.settled, 0, "unknown is never claimed again");
    let (state_now, _, err) = outbox_state(&pool, id).await;
    assert_eq!(state_now, "unknown");
    assert!(err.unwrap_or_default().contains("rekonsiliasi"));
    assert_eq!(
        calls(&stub, "POST", "/rest/api/3/issue"),
        1,
        "one create only"
    );
}

/// CP6b: with ITSM_EGRESS_PROXY set, a live call leaves only as a CONNECT to the provider host on
/// 443 through the proxy, with the proxy credentials; the provider path and the provider token
/// never reach the proxy, and a proxy refusal reads as "provider unreachable" (retried later).
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp6b_live_calls_leave_through_the_egress_proxy(pool: PgPool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fx = fixture(&pool).await;
    let heads: Arc<Mutex<Vec<String>>> = Arc::default();
    let listener = ok(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
        "bind proxy",
    );
    let proxy = format!("http://{}", ok(listener.local_addr(), "proxy addr"));
    let seen = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(String::from_utf8_lossy(&head).into_owned());
            let _ = sock
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });
    let auth = std::env::temp_dir().join(format!("itsm-cp6b-proxy-{}", Uuid::new_v4()));
    ok(
        std::fs::write(&auth, "itsm-api:synthetic-egress-pass-01\n"),
        "auth file",
    );
    let st = state_with(
        &pool,
        Opts {
            lab: false,
            keyring: true,
        },
        vec![
            ("ITSM_EGRESS_PROXY", proxy),
            (
                "ITSM_EGRESS_PROXY_AUTH_FILE",
                auth.to_string_lossy().into_owned(),
            ),
        ],
    );
    let _ = std::fs::remove_file(&auth);
    let admin = tok(&pool, fx.admin).await;
    let c = configure(
        &st,
        fx.a,
        &admin,
        "jira",
        "https://acme.atlassian.net",
        JIRA_TOKEN,
    )
    .await;
    let r = step(&st, fx.a, &admin, &c, "test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["lastTestOk"], false);
    assert!(
        s(&r.json["item"]["lastTestNote"]).contains("tidak dapat dihubungi"),
        "{}",
        r.json
    );
    let heads = heads
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(!heads.is_empty(), "the call went to the proxy");
    let basic = STANDARD.encode("itsm-api:synthetic-egress-pass-01");
    for h in &heads {
        assert!(
            h.starts_with("CONNECT acme.atlassian.net:443 HTTP/1.1\r\n"),
            "{h}"
        );
        assert!(
            h.to_ascii_lowercase().contains(&format!(
                "proxy-authorization: basic {}",
                basic.to_ascii_lowercase()
            )),
            "{h}"
        );
        assert!(
            !h.contains("/rest/") && !h.contains(JIRA_TOKEN) && !h.contains(JIRA_EMAIL),
            "{h}"
        );
    }
}
