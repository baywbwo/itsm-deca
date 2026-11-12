#![allow(clippy::expect_used)]
//! `ITSM_MFA` off (the default, owner 2026-11-12): plain password login. An administrator whose
//! grants would make MFA mandatory gets a full session, sensitive actions need no step-up and the
//! MFA endpoints answer 404.
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

const PASSWORD: &str = "synthetic-password-123";

fn state(pool: &PgPool) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "password"),
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

/// Returns (status, body, set-cookie pair).
async fn send(
    pool: &PgPool,
    method: &str,
    path: &str,
    body: Value,
    auth: Option<(&str, &str)>,
) -> (StatusCode, Value, Option<String>) {
    loop {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        match auth {
            Some((cookie, csrf)) => {
                r = r.header("cookie", cookie).header("x-csrf-token", csrf);
            }
            None => {
                r = r
                    .header("x-itsm-login", "1")
                    .header("sec-fetch-site", "same-origin");
            }
        }
        let req = r.body(Body::from(body.to_string())).expect("request");
        let response = app(state(pool)).oneshot(req).await.expect("response");
        // Login hashing is bounded by a process-wide semaphore; parallel tests may see 429.
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
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
        return (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
        );
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn mfa_off_admin_logs_in_with_password_only_and_needs_no_step_up(pool: PgPool) {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("db");
    seed::seed_uat(&pool, &name).await.expect("seed");
    let admin: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject='dummy:cs-alfa'")
            .fetch_one(&pool)
            .await
            .expect("admin");
    sqlx::query("INSERT INTO local_credentials(user_id,username,password_hash) VALUES($1,$2,$3)")
        .bind(admin)
        .bind("admin@example.test")
        .bind(hash_password(PASSWORD).expect("hash"))
        .execute(&pool)
        .await
        .expect("credentials");
    sqlx::query("INSERT INTO platform_grants(user_id,grant_code) VALUES($1,'platform_admin')")
        .bind(admin)
        .execute(&pool)
        .await
        .expect("grant");
    let required: bool = sqlx::query_scalar("SELECT itsm_mfa_required($1)")
        .bind(admin)
        .fetch_one(&pool)
        .await
        .expect("required");
    assert!(
        required,
        "the grant alone would make MFA mandatory with ITSM_MFA=on"
    );

    let (status, body, cookie) = send(
        &pool,
        "POST",
        "/api/v1/login",
        json!({"username":"admin@example.test","password":PASSWORD}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["mfaEnabled"], json!(false));
    assert_eq!(body["mfaRequired"], json!(false));
    let cookie = cookie.expect("session cookie");
    let csrf = body["csrfToken"].as_str().expect("csrf").to_owned();
    let auth = Some((cookie.as_str(), csrf.as_str()));

    let (status, body, _) = send(&pool, "GET", "/api/v1/session", Value::Null, auth).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["mfaEnabled"], json!(false));
    assert_eq!(body["mfaRequired"], json!(false));

    // Not a pre-MFA session: routes outside the enrolment allow-list answer.
    let (status, body, _) = send(&pool, "GET", "/api/v1/users", Value::Null, auth).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The boot request behind the deployed "Anda tidak memiliki izin" screen (PR #17): no 403.
    let (status, body, _) = send(&pool, "GET", "/api/v1/workspaces", Value::Null, auth).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A sensitive action goes through without a step-up.
    let (status, body, _) = send(
        &pool,
        "POST",
        "/api/v1/users",
        json!({"username":"new@example.test","displayName":"Synthetic User"}),
        auth,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for (method, path) in [
        ("GET", "/api/v1/me/mfa"),
        ("POST", "/api/v1/me/mfa/enrollment"),
        ("POST", "/api/v1/session/step-up"),
        ("DELETE", "/api/v1/me/mfa"),
    ] {
        let (status, body, _) = send(&pool, method, path, json!({"code":"123456"}), auth).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {body}");
    }
}
