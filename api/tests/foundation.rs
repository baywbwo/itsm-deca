//! Black-box tests for the D02a foundation against a real PostgreSQL.
//! Each `#[sqlx::test]` gets a fresh database with migrations applied.
//! Role fixtures use `session::issue` directly (test harness), never an HTTP impersonation route.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::{AppState, app, db, seed, session};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn uat_config() -> Config {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    match Config::from_map(&vars) {
        Ok(c) => c,
        Err(e) => panic!("test config invalid: {e}"),
    }
}

fn state(pool: &PgPool) -> AppState {
    AppState {
        config: Arc::new(uat_config()),
        pool: pool.clone(),
    }
}

async fn seeded(pool: &PgPool) {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .unwrap_or_default();
    if let Err(e) = seed::seed_uat(pool, &name).await {
        panic!("seed failed: {e}");
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    json: Value,
}

async fn call(pool: &PgPool, req: Request<Body>) -> Resp {
    let res = match app(state(pool)).oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = match res.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => panic!("body: {e}"),
    };
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Resp {
        status,
        headers,
        json,
    }
}

fn req(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder().method(method).uri(uri)
}

fn build(b: axum::http::request::Builder) -> Request<Body> {
    match b.body(Body::empty()) {
        Ok(r) => r,
        Err(e) => panic!("request: {e}"),
    }
}

fn cookie(token: &str) -> String {
    format!("{}={token}", session::cookie_name(false))
}

async fn user_id(pool: &PgPool, subject: &str) -> Uuid {
    match sqlx::query_scalar("SELECT id FROM users WHERE external_subject = $1")
        .bind(subject)
        .fetch_one(pool)
        .await
    {
        Ok(id) => id,
        Err(e) => panic!("user {subject}: {e}"),
    }
}

async fn workspace_id(pool: &PgPool, code: &str) -> Uuid {
    match sqlx::query_scalar("SELECT id FROM workspaces WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
    {
        Ok(id) => id,
        Err(e) => panic!("workspace {code}: {e}"),
    }
}

/// Test-harness session for a fixture account (no HTTP route involved).
async fn fixture_session(pool: &PgPool, subject: &str) -> session::IssuedSession {
    let uid = user_id(pool, subject).await;
    match session::issue(pool, uid, "uat_superadmin").await {
        Ok(s) => s,
        Err(e) => panic!("issue: {e}"),
    }
}

async fn uat_enter(pool: &PgPool) -> Resp {
    call(
        pool,
        build(req("POST", "/api/v1/uat/session").header("x-itsm-uat-entry", "1")),
    )
    .await
}

fn set_cookie_token(r: &Resp) -> String {
    let raw = r
        .headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    raw.split(';')
        .next()
        .and_then(|kv| kv.split_once('='))
        .map(|(_, v)| v.to_owned())
        .unwrap_or_default()
}

// ---------- health and runtime guard ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn live_is_ok_and_sets_request_id_and_no_store(pool: PgPool) {
    let r = call(&pool, build(req("GET", "/health/live"))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.headers.contains_key("x-request-id"));
    assert_eq!(
        r.headers.get(header::CACHE_CONTROL).map(|v| v.as_bytes()),
        Some(&b"no-store"[..])
    );
    assert_eq!(
        r.headers
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .map(|v| v.as_bytes()),
        Some(&b"nosniff"[..])
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn unmarked_database_fails_guard_and_readiness(pool: PgPool) {
    assert!(
        db::verify_runtime_guard(&uat_config(), &pool)
            .await
            .is_err()
    );
    let r = call(&pool, build(req("GET", "/health/ready"))).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn marker_for_another_database_name_is_not_accepted(pool: PgPool) {
    // e.g. a dump of a test DB restored under a different (possibly production) name.
    let inserted = sqlx::query(
        "INSERT INTO itsm_deployment_marker (kind, marked_db) VALUES ('test_dummy', 'other_db')",
    )
    .execute(&pool)
    .await;
    assert!(inserted.is_ok());
    assert!(matches!(
        db::is_marked_test_database(&pool).await,
        Ok(false)
    ));
    assert_eq!(
        uat_enter(&pool).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn marked_database_passes_guard_and_readiness(pool: PgPool) {
    seeded(&pool).await;
    assert!(db::verify_runtime_guard(&uat_config(), &pool).await.is_ok());
    let r = call(&pool, build(req("GET", "/health/ready"))).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn seed_requires_exact_database_confirmation(pool: PgPool) {
    assert!(seed::seed_uat(&pool, "wrong_name").await.is_err());
    let markers: i64 = sqlx::query_scalar("SELECT count(*) FROM itsm_deployment_marker")
        .fetch_one(&pool)
        .await
        .unwrap_or(-1);
    let companies: i64 = sqlx::query_scalar("SELECT count(*) FROM companies")
        .fetch_one(&pool)
        .await
        .unwrap_or(-1);
    assert_eq!((markers, companies), (0, 0));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn seed_is_idempotent(pool: PgPool) {
    seeded(&pool).await;
    seeded(&pool).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_memberships")
        .fetch_one(&pool)
        .await
        .unwrap_or(-1);
    assert_eq!(n, 2);
}

// ---------- UAT entry, session, CSRF ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn meta_shows_uat_banner(pool: PgPool) {
    let r = call(&pool, build(req("GET", "/api/v1/meta"))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["banner"], "Mode uji — Superadmin");
    assert_eq!(r.json["accessMode"], "uat_superadmin");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn uat_entry_refused_on_unmarked_database(pool: PgPool) {
    let r = uat_enter(&pool).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json["error"]["code"], "uat_not_available");
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap_or(-1);
    assert_eq!(sessions, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn uat_entry_requires_custom_header(pool: PgPool) {
    seeded(&pool).await;
    let r = call(&pool, build(req("POST", "/api/v1/uat/session"))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"]["code"], "csrf_failed");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn uat_entry_issues_hardened_session(pool: PgPool) {
    seeded(&pool).await;
    let r = uat_enter(&pool).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json["banner"], "Mode uji — Superadmin");
    assert_eq!(r.json["uatSuperadmin"], true);
    let csrf = r.json["csrfToken"].as_str().unwrap_or_default().to_owned();
    assert_eq!(csrf.len(), 43);

    let sc = r
        .headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(sc.contains(attr), "cookie missing {attr}");
    }
    let token = set_cookie_token(&r);
    assert_eq!(token.len(), 43);

    // Only digests are stored, never the raw token or CSRF value.
    let raw_hits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE token_hash = convert_to($1, 'UTF8') \
         OR csrf_hash = convert_to($2, 'UTF8')",
    )
    .bind(&token)
    .bind(&csrf)
    .fetch_one(&pool)
    .await
    .unwrap_or(-1);
    assert_eq!(raw_hits, 0);

    let me = call(
        &pool,
        build(req("GET", "/api/v1/session").header(header::COOKIE, cookie(&token))),
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["user"]["displayName"], seed::UAT_SUPERADMIN_NAME);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn session_endpoints_require_a_valid_cookie(pool: PgPool) {
    seeded(&pool).await;
    let none = call(&pool, build(req("GET", "/api/v1/session"))).await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert_eq!(none.json["error"]["code"], "unauthenticated");
    let bogus = call(
        &pool,
        build(req("GET", "/api/v1/session").header(header::COOKIE, cookie("forged-token"))),
    )
    .await;
    assert_eq!(bogus.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn logout_enforces_csrf_then_revokes(pool: PgPool) {
    seeded(&pool).await;
    let s = fixture_session(&pool, "dummy:uat-superadmin").await;
    let c = cookie(&s.token);

    let no_token = call(
        &pool,
        build(req("DELETE", "/api/v1/session").header(header::COOKIE, &c)),
    )
    .await;
    assert_eq!(no_token.status, StatusCode::FORBIDDEN);
    assert_eq!(no_token.json["error"]["code"], "csrf_failed");

    let wrong = call(
        &pool,
        build(
            req("DELETE", "/api/v1/session")
                .header(header::COOKIE, &c)
                .header("x-csrf-token", "not-the-token"),
        ),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);

    // CSRF token of another session must not work either.
    let other = fixture_session(&pool, "dummy:uat-superadmin").await;
    let cross = call(
        &pool,
        build(
            req("DELETE", "/api/v1/session")
                .header(header::COOKIE, &c)
                .header("x-csrf-token", &other.csrf_token),
        ),
    )
    .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);

    let ok = call(
        &pool,
        build(
            req("DELETE", "/api/v1/session")
                .header(header::COOKIE, &c)
                .header("x-csrf-token", &s.csrf_token),
        ),
    )
    .await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT);
    let cleared = ok
        .headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(cleared.contains("Max-Age=0"));

    let after = call(
        &pool,
        build(req("GET", "/api/v1/session").header(header::COOKIE, &c)),
    )
    .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn unsafe_request_without_session_is_unauthenticated(pool: PgPool) {
    let r = call(&pool, build(req("DELETE", "/api/v1/session"))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn expired_session_and_inactive_user_are_rejected(pool: PgPool) {
    seeded(&pool).await;
    let s = fixture_session(&pool, "dummy:cs-alfa").await;
    let c = cookie(&s.token);
    let _ = sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '10 hours', \
         expires_at = now() - interval '1 second'",
    )
    .execute(&pool)
    .await;
    let r = call(
        &pool,
        build(req("GET", "/api/v1/session").header(header::COOKIE, &c)),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let s2 = fixture_session(&pool, "dummy:cs-alfa").await;
    let _ = sqlx::query("UPDATE users SET active = false WHERE external_subject = 'dummy:cs-alfa'")
        .execute(&pool)
        .await;
    let r2 = call(
        &pool,
        build(req("GET", "/api/v1/session").header(header::COOKIE, cookie(&s2.token))),
    )
    .await;
    assert_eq!(r2.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn oversized_body_is_rejected(pool: PgPool) {
    seeded(&pool).await;
    let big = vec![b'a'; 70 * 1024];
    let request = match req("POST", "/api/v1/uat/session")
        .header("x-itsm-uat-entry", "1")
        .header(header::CONTENT_LENGTH, big.len())
        .body(Body::from(big))
    {
        Ok(r) => r,
        Err(e) => panic!("request: {e}"),
    };
    let r = call(&pool, request).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

// ---------- workspace scope ----------

async fn get_with(pool: &PgPool, uri: &str, token: &str) -> Resp {
    call(
        pool,
        build(req("GET", uri).header(header::COOKIE, cookie(token))),
    )
    .await
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cs_member_sees_only_own_workspace(pool: PgPool) {
    seeded(&pool).await;
    let s = fixture_session(&pool, "dummy:cs-alfa").await;
    let list = get_with(&pool, "/api/v1/workspaces", &s.token).await;
    assert_eq!(list.status, StatusCode::OK);
    let items = list.json["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["code"], "DUMA");
    assert_eq!(items[0]["myRole"], "cs");

    let a = workspace_id(&pool, "DUMA").await;
    let b = workspace_id(&pool, "DUMB").await;
    assert_eq!(
        get_with(&pool, &format!("/api/v1/workspaces/{a}"), &s.token)
            .await
            .status,
        StatusCode::OK
    );
    let members = get_with(&pool, &format!("/api/v1/workspaces/{a}/members"), &s.token).await;
    assert_eq!(members.status, StatusCode::OK);
    assert_eq!(members.json["items"].as_array().map(Vec::len), Some(2));

    // Foreign workspace: same answer as a non-existent one.
    for uri in [
        format!("/api/v1/workspaces/{b}"),
        format!("/api/v1/workspaces/{b}/members"),
        format!("/api/v1/workspaces/{}", Uuid::new_v4()),
        "/api/v1/workspaces/not-a-uuid".to_owned(),
    ] {
        let r = get_with(&pool, &uri, &s.token).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(r.json["error"]["code"], "not_found");
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn inactive_membership_or_workspace_removes_access(pool: PgPool) {
    seeded(&pool).await;
    let s = fixture_session(&pool, "dummy:lead-alfa").await;
    let a = workspace_id(&pool, "DUMA").await;
    let _ = sqlx::query(
        "UPDATE workspace_memberships SET active = false \
         WHERE user_id = (SELECT id FROM users WHERE external_subject = 'dummy:lead-alfa')",
    )
    .execute(&pool)
    .await;
    assert_eq!(
        get_with(&pool, &format!("/api/v1/workspaces/{a}"), &s.token)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let cs = fixture_session(&pool, "dummy:cs-alfa").await;
    let _ = sqlx::query("UPDATE workspaces SET active = false WHERE code = 'DUMA'")
        .execute(&pool)
        .await;
    let list = get_with(&pool, "/api/v1/workspaces", &cs.token).await;
    assert_eq!(list.json["items"].as_array().map(Vec::len), Some(0));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn user_without_membership_sees_nothing(pool: PgPool) {
    seeded(&pool).await;
    let _ =
        sqlx::query("INSERT INTO users (external_subject, display_name) VALUES ('dummy:x', 'X')")
            .execute(&pool)
            .await;
    let s = fixture_session(&pool, "dummy:x").await;
    let list = get_with(&pool, "/api/v1/workspaces", &s.token).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json["items"].as_array().map(Vec::len), Some(0));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn uat_superadmin_sees_all_active_workspaces(pool: PgPool) {
    seeded(&pool).await;
    let r = uat_enter(&pool).await;
    let token = set_cookie_token(&r);
    let list = get_with(&pool, "/api/v1/workspaces", &token).await;
    let codes: Vec<String> = list.json["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|w| w["code"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(codes, ["DUMA", "DUMB"]);
    let b = workspace_id(&pool, "DUMB").await;
    let one = get_with(&pool, &format!("/api/v1/workspaces/{b}"), &token).await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.json["myRole"], Value::Null);
}

// ---------- schema constraints ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn schema_rejects_invalid_roles_and_shapes(pool: PgPool) {
    seeded(&pool).await;
    let a = workspace_id(&pool, "DUMA").await;
    let cs = user_id(&pool, "dummy:cs-alfa").await;
    let b = workspace_id(&pool, "DUMB").await;

    let bad_role = sqlx::query(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role) VALUES ($1, $2, 'workspace_admin')",
    )
    .bind(b)
    .bind(cs)
    .execute(&pool)
    .await;
    assert!(bad_role.is_err(), "workspace_admin must not be a role");

    let dup = sqlx::query(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role) VALUES ($1, $2, 'cs')",
    )
    .bind(a)
    .bind(cs)
    .execute(&pool)
    .await;
    assert!(dup.is_err(), "duplicate membership");

    let lower = sqlx::query("INSERT INTO companies (code, name) VALUES ('dumc', 'x')")
        .execute(&pool)
        .await;
    assert!(lower.is_err(), "company code format");

    let company_a: Option<Uuid> =
        sqlx::query_scalar("SELECT company_id FROM workspaces WHERE id = $1")
            .bind(a)
            .fetch_optional(&pool)
            .await
            .unwrap_or(None);
    let second_ws =
        sqlx::query("INSERT INTO workspaces (company_id, code, name) VALUES ($1, 'DUMZ', 'z')")
            .bind(company_a)
            .execute(&pool)
            .await;
    assert!(second_ws.is_err(), "one workspace per company");

    let bad_grant = sqlx::query(
        "INSERT INTO platform_grants (user_id, grant_code) VALUES ($1, 'workspace_admin')",
    )
    .bind(cs)
    .execute(&pool)
    .await;
    assert!(bad_grant.is_err(), "unknown platform grant");

    let second_marker = sqlx::query(
        "INSERT INTO itsm_deployment_marker (singleton, kind, marked_db) VALUES (false, 'test_dummy', 'x')",
    )
    .execute(&pool)
    .await;
    assert!(second_marker.is_err(), "marker is a singleton");

    let delete_ws = sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await;
    assert!(
        delete_ws.is_err(),
        "no cascade delete of workspace with members"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn error_bodies_do_not_leak_internals(pool: PgPool) {
    let r = call(&pool, build(req("GET", "/api/v1/session"))).await;
    let text = r.json.to_string();
    for needle in ["sqlx", "SELECT", "panicked", "postgres"] {
        assert!(!text.contains(needle), "leaked {needle}");
    }
}
