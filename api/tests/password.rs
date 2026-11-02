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
    let response = app(state(pool, mode))
        .oneshot(r.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
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
