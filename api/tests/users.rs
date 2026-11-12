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
const ADMIN_PASSWORD: &str = "synthetic-password-123";
/// Admin (`platform_admin`) and a plain user, both with local credentials.
async fn fixture(pool: &PgPool) -> (Uuid, Uuid) {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("db");
    seed::seed_uat(pool, &name).await.expect("seed");
    let admin: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject='dummy:cs-alfa'")
            .fetch_one(pool)
            .await
            .expect("admin");
    let plain: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE id<>$1 ORDER BY id LIMIT 1")
        .bind(admin)
        .fetch_one(pool)
        .await
        .expect("plain");
    for (id, username) in [(admin, "admin@example.test"), (plain, "plain@example.test")] {
        sqlx::query(
            "INSERT INTO local_credentials(user_id,username,password_hash) VALUES($1,$2,$3)",
        )
        .bind(id)
        .bind(username)
        .bind(hash_password(ADMIN_PASSWORD).expect("hash"))
        .execute(pool)
        .await
        .expect("credentials");
    }
    sqlx::query("INSERT INTO platform_grants(user_id,grant_code) VALUES($1,'platform_admin')")
        .bind(admin)
        .execute(pool)
        .await
        .expect("grant");
    (admin, plain)
}
/// Returns (status, cookie, csrf).
async fn login(pool: &PgPool, username: &str, password: &str) -> (StatusCode, String, String) {
    let (status, body, cookie) = request(
        pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":username,"password":password}),
        true,
        None,
    )
    .await;
    let csrf = body["csrfToken"].as_str().unwrap_or_default().to_owned();
    (status, cookie.unwrap_or_default(), csrf)
}
async fn call(
    pool: &PgPool,
    method: &str,
    path: &str,
    body: Value,
    (cookie, csrf): (&str, &str),
) -> (StatusCode, Value) {
    loop {
        let req = Request::builder()
            .method(method)
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
async fn admin(pool: &PgPool) -> (String, String) {
    let (status, cookie, csrf) = login(pool, "admin@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    (cookie, csrf)
}
async fn create_user(pool: &PgPool, a: &(String, String), username: &str) -> (Uuid, String) {
    let (status, body) = call(
        pool,
        "POST",
        "/api/v1/users",
        json!({"username":username,"displayName":"Synthetic User"}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["user"]["id"]
        .as_str()
        .expect("id")
        .parse()
        .expect("uuid");
    (
        id,
        body["temporaryPassword"].as_str().expect("temp").to_owned(),
    )
}
async fn count(pool: &PgPool, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count")
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_3_admin_creates_user_with_one_time_temporary_password(pool: PgPool) {
    let (admin_id, _) = fixture(&pool).await;
    let a = admin(&pool).await;
    let (status, body) = call(
        &pool,
        "POST",
        "/api/v1/users",
        json!({"username":"  New.User@Example.Test ","displayName":" Synthetic User ","email":"New@Example.Test"}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["user"]["username"], "new.user@example.test");
    assert_eq!(body["user"]["email"], "new@example.test");
    assert_eq!(body["user"]["mustChangePassword"], true);
    let temp = body["temporaryPassword"].as_str().expect("temp").to_owned();
    assert!(temp.len() >= 16);
    let id: Uuid = body["user"]["id"]
        .as_str()
        .expect("id")
        .parse()
        .expect("uuid");
    let (hash, must, by): (String, bool, Option<Uuid>) = sqlx::query_as(
        "SELECT c.password_hash, c.must_change_password, u.created_by FROM local_credentials c \
         JOIN users u ON u.id=c.user_id WHERE c.user_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("user");
    assert!(must && hash.starts_with("$argon2id$") && !hash.contains(&temp));
    assert_eq!(by, Some(admin_id));
    let (status, _, _) = login(&pool, "new.user@example.test", &temp).await;
    assert_eq!(status, StatusCode::CREATED);
    for (body, code) in [
        (
            json!({"username":"NEW.USER@example.test","displayName":"X"}),
            "username_taken",
        ),
        (
            json!({"username":"other@example.test","displayName":"X","email":"new@example.test"}),
            "email_taken",
        ),
    ] {
        let (status, err) = call(&pool, "POST", "/api/v1/users", body, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(err["error"]["code"].as_str(), Some(code), "{err}");
    }
    for body in [
        json!({"username":"x","displayName":"X"}),
        json!({"username":"ok@example.test","displayName":"  "}),
    ] {
        let (status, _) = call(&pool, "POST", "/api/v1/users", body, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // Unknown fields (role/workspace not in scope yet) follow the project body convention: 400.
    let role = json!({"username":"ok@example.test","displayName":"X","role":"lead_cs"});
    let (status, _) = call(&pool, "POST", "/api/v1/users", role, (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event='user_create' AND outcome='success' AND actor_id=(SELECT created_by FROM users WHERE id=$1)", id).await,
        1
    );
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_3_non_admin_is_forbidden_on_user_admin_endpoints_and_audited(pool: PgPool) {
    let (admin_id, plain) = fixture(&pool).await;
    let (status, cookie, csrf) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    let users = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .expect("users");
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/users".to_owned(),
            json!({"username":"x@example.test","displayName":"X"}),
        ),
        (
            "POST",
            format!("/api/v1/users/{admin_id}/password-reset"),
            json!({}),
        ),
        (
            "PATCH",
            format!("/api/v1/users/{admin_id}"),
            json!({"active":false}),
        ),
    ] {
        let (status, _) = call(&pool, method, &path, body, (&cookie, &csrf)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    let after = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .expect("users");
    assert_eq!(users, after);
    assert!(
        count(&pool, "SELECT count(*) FROM platform_audit_events WHERE actor_id=$1 AND capability='platform.users'", plain).await
            >= 3
    );
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_5_password_reset_issues_temporary_password_and_revokes_sessions(pool: PgPool) {
    fixture(&pool).await;
    let a = admin(&pool).await;
    let (id, temp) = create_user(&pool, &a, "reset@example.test").await;
    let (status, _, _) = login(&pool, "reset@example.test", &temp).await;
    assert_eq!(status, StatusCode::CREATED);
    sqlx::query("UPDATE local_credentials SET failed_attempts=5, locked_until=now()+interval '15 minutes' WHERE user_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("lock");
    let path = format!("/api/v1/users/{id}/password-reset");
    let (status, body) = call(&pool, "POST", &path, json!({}), (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fresh = body["temporaryPassword"].as_str().expect("temp").to_owned();
    assert!(fresh.len() >= 16 && fresh != temp);
    let (version, must, failed, locked): (i32, bool, i32, bool) = sqlx::query_as(
        "SELECT credential_version, must_change_password, failed_attempts, locked_until IS NOT NULL \
         FROM local_credentials WHERE user_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("credentials");
    assert_eq!((version, must, failed, locked), (2, true, 0, false));
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM sessions WHERE user_id=$1 AND revoked_at IS NULL",
            id
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM sessions WHERE user_id=$1 AND revoked_reason='admin_revoke'",
            id
        )
        .await,
        1
    );
    assert_ne!(
        login(&pool, "reset@example.test", &temp).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        login(&pool, "reset@example.test", &fresh).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event='password_reset_issue' AND outcome='success' AND actor_id=(SELECT created_by FROM users WHERE id=$1)", id).await,
        1
    );
    let missing = format!("/api/v1/users/{}/password-reset", Uuid::new_v4());
    assert_eq!(
        call(&pool, "POST", &missing, json!({}), (&a.0, &a.1))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_7_deactivate_revokes_sessions_keeps_history_and_protects_last_admin(pool: PgPool) {
    let (admin_id, _) = fixture(&pool).await;
    let a = admin(&pool).await;
    let (id, temp) = create_user(&pool, &a, "leaver@example.test").await;
    assert_eq!(
        login(&pool, "leaver@example.test", &temp).await.0,
        StatusCode::CREATED
    );
    let path = format!("/api/v1/users/{id}");
    for body in [
        json!({"active":true}),
        json!({}),
        json!({"active":false,"version":1}),
    ] {
        assert_eq!(
            call(&pool, "PATCH", &path, body, (&a.0, &a.1)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (status, body) = call(&pool, "PATCH", &path, json!({"active":false}), (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active"], false);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM users WHERE id=$1 AND deactivated_at IS NOT NULL",
            id
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM sessions WHERE user_id=$1 AND revoked_reason='admin_revoke'",
            id
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event='user_create' AND actor_id IS NOT NULL",
            id
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event='user_deactivate' AND actor_id=(SELECT created_by FROM users WHERE id=$1)",
            id
        )
        .await,
        1
    );
    assert_ne!(
        login(&pool, "leaver@example.test", &temp).await.0,
        StatusCode::CREATED
    );
    let (status, err) = call(&pool, "PATCH", &path, json!({"active":false}), (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err.to_string().contains("user_inactive"), "{err}");
    let reset = format!("/api/v1/users/{id}/password-reset");
    let (status, err) = call(&pool, "POST", &reset, json!({}), (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err.to_string().contains("user_inactive"), "{err}");
    let own = format!("/api/v1/users/{admin_id}");
    let (status, err) = call(&pool, "PATCH", &own, json!({"active":false}), (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err.to_string().contains("last_admin"), "{err}");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM users WHERE id=$1 AND deactivated_at IS NULL",
            admin_id
        )
        .await,
        1
    );
}
