#![allow(clippy::expect_used)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use itsm_api::{AppState, app, config::Config, routes::password::hash_password, seed};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;
use uuid::Uuid;
fn state(pool: &PgPool, mode: &str) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", mode),
        ("ITSM_COOKIE_SECURE", "true"),
        ("DATABASE_URL", "postgres://unused"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    AppState {
        pool: pool.clone(),
        config: Arc::new(Config::from_map(&vars).expect("config")),
    }
}
async fn request(
    pool: &PgPool,
    mode: &str,
    path: &str,
    method: &str,
    body: Value,
    header: bool,
    cookie: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if header {
        r = r
            .header("x-itsm-login", "1")
            .header("sec-fetch-site", "same-origin");
    }
    if let Some(cookie) = cookie {
        r = r.header("cookie", cookie);
    }
    let req = r.body(Body::from(body.to_string())).expect("request");
    let (parts, body_bytes) = req.into_parts();
    let body_bytes = body_bytes.collect().await.expect("body").to_bytes();
    // Login hashing is bounded by a process-wide semaphore; parallel tests may see 429.
    let response = loop {
        let req = Request::from_parts(parts.clone(), Body::from(body_bytes.clone()));
        let response = app(state(pool, mode)).oneshot(req).await.expect("response");
        if response.status() != StatusCode::TOO_MANY_REQUESTS {
            break response;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let status = response.status();
    let cookie = response.headers().get("set-cookie").map(|v| {
        v.to_str()
            .expect("cookie")
            .split(';')
            .next()
            .expect("pair")
            .to_owned()
    });
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("bytes")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}
async fn fixture(pool: &PgPool) -> Uuid {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("db");
    seed::seed_uat(pool, &name).await.expect("seed");
    let user: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject='dummy:cs-alfa'")
            .fetch_one(pool)
            .await
            .expect("user");
    sqlx::query("INSERT INTO local_credentials(user_id,username,password_hash) VALUES($1,'admin@example.test',$2)").bind(user).bind(hash_password("synthetic-password-123").expect("hash")).execute(pool).await.expect("credentials");
    user
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn local_login_requires_header_and_valid_password_and_scopes_session(pool: PgPool) {
    fixture(&pool).await;
    let body = json!({"username":"ADMIN@example.test","password":"synthetic-password-123"});
    assert_eq!(
        request(
            &pool,
            "password",
            "/api/v1/login",
            "POST",
            body.clone(),
            false,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &pool,
            "password",
            "/api/v1/login",
            "POST",
            json!({"username":"admin@example.test","password":"wrong"}),
            true,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, result, cookie) =
        request(&pool, "password", "/api/v1/login", "POST", body, true, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(result["uatSuperadmin"], false);
    assert!(result["csrfToken"].is_string());
    let cookie = cookie.expect("session cookie");
    assert!(cookie.starts_with("__Host-itsm_session="));
    let (status, workspaces, _) = request(
        &pool,
        "password",
        "/api/v1/workspaces",
        "GET",
        Value::Null,
        false,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(workspaces["items"].as_array().expect("items").len(), 1);
    assert_eq!(
        request(
            &pool,
            "uat_superadmin",
            "/api/v1/session",
            "GET",
            Value::Null,
            false,
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let other: Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE code='DUMB'")
        .fetch_one(&pool)
        .await
        .expect("foreign workspace");
    assert_eq!(
        request(
            &pool,
            "password",
            &format!("/api/v1/workspaces/{other}"),
            "GET",
            Value::Null,
            false,
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &pool,
            "password",
            "/api/v1/uat/session",
            "POST",
            Value::Null,
            false,
            Some(&cookie)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn repeated_bad_passwords_lock_account_and_do_not_issue_sessions(pool: PgPool) {
    fixture(&pool).await;
    for _ in 0..5 {
        assert_eq!(
            request(
                &pool,
                "password",
                "/api/v1/login",
                "POST",
                json!({"username":"admin@example.test","password":"wrong"}),
                true,
                None
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        request(
            &pool,
            "password",
            "/api/v1/login",
            "POST",
            json!({"username":"admin@example.test","password":"synthetic-password-123"}),
            true,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(sessions, 0);
}
#[test]
fn password_mode_rejects_insecure_cookie_config() {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "production"),
        ("ITSM_ACCESS_MODE", "password"),
        ("ITSM_COOKIE_SECURE", "false"),
        ("DATABASE_URL", "postgres://unused"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    assert!(Config::from_map(&vars).is_err());
}
async fn login_ok(pool: &PgPool) -> String {
    let (status, _, cookie) = request(
        pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":"admin@example.test","password":"synthetic-password-123"}),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    cookie.expect("session cookie")
}
async fn workspaces(pool: &PgPool, cookie: &str) -> StatusCode {
    request(
        pool,
        "password",
        "/api/v1/workspaces",
        "GET",
        Value::Null,
        false,
        Some(cookie),
    )
    .await
    .0
}
async fn events(pool: &PgPool, event: &str, outcome: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE event=$1 AND outcome=$2")
        .bind(event)
        .bind(outcome)
        .fetch_one(pool)
        .await
        .expect("events")
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_1_lockout_after_five_failures_is_audited_and_expires_after_15_minutes(pool: PgPool) {
    let user = fixture(&pool).await;
    for _ in 0..5 {
        request(
            &pool,
            "password",
            "/api/v1/login",
            "POST",
            json!({"username":"admin@example.test","password":"wrong-password-x"}),
            true,
            None,
        )
        .await;
    }
    assert_eq!(events(&pool, "login", "failed").await, 5);
    assert_eq!(events(&pool, "lockout", "success").await, 1);
    let mins: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM locked_until-now())/60)::float8 FROM local_credentials WHERE user_id=$1",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .expect("locked");
    assert!(mins > 14.0 && mins <= 15.0);
    let (status, _, _) = request(
        &pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":"admin@example.test","password":"synthetic-password-123"}),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(events(&pool, "login", "denied").await, 1);
    // Simulate 15 minutes passing; a successful login resets the counter.
    sqlx::query(
        "UPDATE local_credentials SET locked_until=now()-interval '1 second' WHERE user_id=$1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .expect("unlock");
    login_ok(&pool).await;
    let failed: i32 =
        sqlx::query_scalar("SELECT failed_attempts FROM local_credentials WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .expect("attempts");
    assert_eq!(failed, 0);
    assert_eq!(events(&pool, "login", "success").await, 1);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_1_unknown_username_is_audited_as_digest_only(pool: PgPool) {
    fixture(&pool).await;
    request(
        &pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":"nobody@example.test","password":"wrong-password-x"}),
        true,
        None,
    )
    .await;
    let (uid, hash_len): (Option<Uuid>, i32) = sqlx::query_as("SELECT user_id, octet_length(username_hash) FROM auth_events WHERE failure_code='unknown_user'")
        .fetch_one(&pool).await.expect("event");
    assert!(uid.is_none());
    assert_eq!(hash_len, 32);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_6_session_expires_after_30_minutes_idle_and_is_revoked_and_audited(pool: PgPool) {
    fixture(&pool).await;
    let cookie = login_ok(&pool).await;
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::OK);
    let idle_mins: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM idle_expires_at-now())/60)::float8 FROM sessions",
    )
    .fetch_one(&pool)
    .await
    .expect("idle");
    assert!(idle_mins > 29.0 && idle_mins <= 30.0);
    sqlx::query("UPDATE sessions SET idle_expires_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .expect("idle");
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
    let reason: Option<String> = sqlx::query_scalar("SELECT revoked_reason FROM sessions")
        .fetch_one(&pool)
        .await
        .expect("reason");
    assert_eq!(reason.as_deref(), Some("idle"));
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE event='session_expire' AND failure_code='idle' AND actor_id IS NULL")
            .fetch_one(&pool)
            .await
            .expect("n");
    assert_eq!(n, 1);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_6_session_absolute_limit_is_12_hours(pool: PgPool) {
    fixture(&pool).await;
    let cookie = login_ok(&pool).await;
    let hours: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM expires_at-created_at)/3600)::float8 FROM sessions",
    )
    .fetch_one(&pool)
    .await
    .expect("abs");
    assert!((hours - 12.0).abs() < 0.01);
    // Activity keeps idle alive but never beyond the absolute limit.
    sqlx::query("UPDATE sessions SET created_at=now()-interval '12 hours 1 second', expires_at=now()-interval '1 second', idle_expires_at=now()+interval '30 minutes'").execute(&pool).await.expect("abs");
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
    let reason: Option<String> = sqlx::query_scalar("SELECT revoked_reason FROM sessions")
        .fetch_one(&pool)
        .await
        .expect("reason");
    assert_eq!(reason.as_deref(), Some("expired"));
    // Audited once: a second request on the revoked cookie adds no event.
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth_events WHERE event='session_expire' AND failure_code='absolute' AND actor_id IS NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("n");
    assert_eq!(n, 1);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_4_session_with_old_credential_version_is_rejected(pool: PgPool) {
    let user = fixture(&pool).await;
    let cookie = login_ok(&pool).await;
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::OK);
    sqlx::query(
        "UPDATE local_credentials SET credential_version=credential_version+1 WHERE user_id=$1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .expect("bump");
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_7_deactivated_user_cannot_login_or_use_session(pool: PgPool) {
    let user = fixture(&pool).await;
    let cookie = login_ok(&pool).await;
    sqlx::query("UPDATE users SET deactivated_at=now() WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .expect("deactivate");
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
    let (status, _, _) = request(
        &pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":"admin@example.test","password":"synthetic-password-123"}),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE failure_code='deactivated'")
            .fetch_one(&pool)
            .await
            .expect("n");
    assert_eq!(n, 1);
}
async fn login_csrf(pool: &PgPool, password: &str) -> (StatusCode, String, String, Value) {
    let (status, body, cookie) = request(
        pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":"admin@example.test","password":password}),
        true,
        None,
    )
    .await;
    let csrf = body["csrfToken"].as_str().unwrap_or_default().to_owned();
    (status, cookie.unwrap_or_default(), csrf, body)
}
async fn call(
    pool: &PgPool,
    path: &str,
    body: Value,
    cookie: &str,
    csrf: &str,
) -> (StatusCode, Value) {
    loop {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
            .body(Body::from(body.to_string()))
            .expect("request");
        let response = app(state(pool, "password"))
            .oneshot(req)
            .await
            .expect("response");
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("bytes")
            .to_bytes();
        return (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        );
    }
}
async fn revoked(pool: &PgPool, reason: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE revoked_reason=$1")
        .bind(reason)
        .fetch_one(pool)
        .await
        .expect("sessions")
}
const NEW_PASSWORD: &str = "another-synthetic-pass-456";
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_4_change_password_enforces_policy_revokes_other_sessions_and_keeps_current(
    pool: PgPool,
) {
    fixture(&pool).await;
    let other = login_ok(&pool).await;
    let (status, cookie, csrf, _) = login_csrf(&pool, "synthetic-password-123").await;
    assert_eq!(status, StatusCode::CREATED);
    let path = "/api/v1/account/password";
    for (current, new, field) in [
        ("wrong-password-000", NEW_PASSWORD, "currentPassword"),
        ("synthetic-password-123", "short", "newPassword"),
        (
            "synthetic-password-123",
            "ADMIN@example.test",
            "newPassword",
        ),
        (
            "synthetic-password-123",
            "synthetic-password-123",
            "newPassword",
        ),
    ] {
        let (s, body) = call(
            &pool,
            path,
            json!({"currentPassword":current,"newPassword":new}),
            &cookie,
            &csrf,
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{new}");
        assert_eq!(body["error"]["field"], field);
    }
    assert_eq!(events(&pool, "password_change", "failed").await, 4);
    let (s, _) = call(
        &pool,
        path,
        json!({"currentPassword":"synthetic-password-123","newPassword":NEW_PASSWORD}),
        &cookie,
        "wrong-csrf",
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "CSRF required");
    let (s, _) = call(
        &pool,
        path,
        json!({"currentPassword":"synthetic-password-123","newPassword":NEW_PASSWORD}),
        &cookie,
        &csrf,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(events(&pool, "password_change", "success").await, 1);
    assert_eq!(
        workspaces(&pool, &cookie).await,
        StatusCode::OK,
        "current session stays"
    );
    assert_eq!(
        workspaces(&pool, &other).await,
        StatusCode::UNAUTHORIZED,
        "other session revoked"
    );
    assert_eq!(revoked(&pool, "password_change").await, 1);
    let version: i32 = sqlx::query_scalar("SELECT credential_version FROM local_credentials")
        .fetch_one(&pool)
        .await
        .expect("version");
    assert_eq!(version, 2);
    assert_eq!(
        login_csrf(&pool, "synthetic-password-123").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(login_csrf(&pool, NEW_PASSWORD).await.0, StatusCode::CREATED);
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE failure_code LIKE '%synthetic%' OR failure_code LIKE '%another%'")
        .fetch_one(&pool)
        .await
        .expect("audit");
    assert_eq!(leaked, 0);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_4_must_change_password_blocks_other_routes_until_changed(pool: PgPool) {
    fixture(&pool).await;
    sqlx::query("UPDATE local_credentials SET must_change_password=true")
        .execute(&pool)
        .await
        .expect("flag");
    let (status, cookie, csrf, body) = login_csrf(&pool, "synthetic-password-123").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["mustChangePassword"], true);
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::FORBIDDEN);
    let current = request(
        &pool,
        "password",
        "/api/v1/session",
        "GET",
        Value::Null,
        false,
        Some(&cookie),
    )
    .await;
    assert_eq!(current.0, StatusCode::OK, "session read stays allowed");
    // CP3a: a reloaded tab learns the flag from the session read, not only from the login.
    assert_eq!(current.1["mustChangePassword"], true);
    let (s, _) = call(
        &pool,
        "/api/v1/account/password",
        json!({"currentPassword":"synthetic-password-123","newPassword":NEW_PASSWORD}),
        &cookie,
        &csrf,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::OK);
    let after = request(
        &pool,
        "password",
        "/api/v1/session",
        "GET",
        Value::Null,
        false,
        Some(&cookie),
    )
    .await;
    assert_eq!(after.1["mustChangePassword"], false);
    let flag: bool = sqlx::query_scalar("SELECT must_change_password FROM local_credentials")
        .fetch_one(&pool)
        .await
        .expect("flag");
    assert!(!flag);
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_6_logout_all_revokes_every_session_and_is_audited(pool: PgPool) {
    fixture(&pool).await;
    let other = login_ok(&pool).await;
    let (_, cookie, csrf, _) = login_csrf(&pool, "synthetic-password-123").await;
    let (s, _) = call(
        &pool,
        "/api/v1/session/logout-all",
        Value::Null,
        &cookie,
        "wrong-csrf",
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "CSRF required");
    let (s, _) = call(
        &pool,
        "/api/v1/session/logout-all",
        Value::Null,
        &cookie,
        &csrf,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
    assert_eq!(workspaces(&pool, &other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(revoked(&pool, "logout_all").await, 2);
    assert_eq!(events(&pool, "logout_all", "success").await, 1);
}

/// CP3c (found end to end): a plain logout ends only this session, records the reason and is
/// audited once (TM-15); the other session of the same user keeps working.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_6_logout_revokes_only_this_session_with_reason_and_is_audited(pool: PgPool) {
    let user = fixture(&pool).await;
    let other = login_ok(&pool).await;
    let (_, cookie, csrf, _) = login_csrf(&pool, "synthetic-password-123").await;
    let logout = |csrf: String| {
        let (pool, cookie) = (pool.clone(), cookie.clone());
        async move {
            let req = Request::builder()
                .method("DELETE")
                .uri("/api/v1/session")
                .header("cookie", cookie)
                .header("x-csrf-token", csrf)
                .body(Body::empty())
                .expect("request");
            app(state(&pool, "password"))
                .oneshot(req)
                .await
                .expect("response")
                .status()
        }
    };
    assert_eq!(logout(csrf.clone()).await, StatusCode::NO_CONTENT);
    assert_eq!(workspaces(&pool, &cookie).await, StatusCode::UNAUTHORIZED);
    assert_eq!(workspaces(&pool, &other).await, StatusCode::OK);
    assert_eq!(revoked(&pool, "logout").await, 1);
    let actor: Option<Uuid> = sqlx::query_scalar(
        "SELECT actor_id FROM auth_events WHERE event='logout' AND outcome='success' AND user_id=$1",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .expect("one logout event");
    assert_eq!(actor, None);
    // The ended session cannot log out again, so no second event.
    assert_eq!(logout(csrf).await, StatusCode::UNAUTHORIZED);
    assert_eq!(events(&pool, "logout", "success").await, 1);
}
