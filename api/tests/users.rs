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
        ("ITSM_MFA", "on"),
        // Synthetic test-only MFA key material (US-1.2): admins must enrol before administering.
        (
            "ITSM_KEYRING",
            "t1:AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=",
        ),
        ("ITSM_KEYRING_ACTIVE", "t1"),
        (
            "ITSM_MFA_RECOVERY_PEPPER",
            "dGVzdC1wZXBwZXItdGVzdC1wZXBwZXItdGVzdC1wZXBwZXI=",
        ),
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
    let a = (cookie, csrf);
    enrol_mfa(pool, &a).await;
    a
}
/// US-1.2: an administrator's first session is pre-MFA; enrolling verifies it. A step-up follows
/// (A-9) so the session can use the sensitive user-administration endpoints. Returns the seed.
async fn enrol_mfa(pool: &PgPool, a: &(String, String)) -> Vec<u8> {
    let (status, body) = call(
        pool,
        "POST",
        "/api/v1/me/mfa/enrollment",
        Value::Null,
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let uri = body["otpauthUri"].as_str().expect("uri");
    let secret = uri
        .split(['?', '&'])
        .find_map(|p| p.strip_prefix("secret="))
        .expect("secret");
    let seed = itsm_api::mfa::base32_decode(secret).expect("base32");
    let code = itsm_api::mfa::code_at(&seed, itsm_api::mfa::now_step());
    let id = body["enrollmentId"].as_str().expect("id");
    let (status, body) = call(
        pool,
        "POST",
        &format!("/api/v1/me/mfa/enrollment/{id}/confirm"),
        json!({"code": code}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    step_up(pool, a, &seed, 1).await;
    seed
}
/// POST /session/step-up with the TOTP code `ahead` steps from now (anti-replay needs a new step).
async fn step_up(pool: &PgPool, a: &(String, String), seed: &[u8], ahead: u64) {
    let code = itsm_api::mfa::code_at(seed, itsm_api::mfa::now_step() + ahead);
    let (status, body) = call(
        pool,
        "POST",
        "/api/v1/session/step-up",
        json!({"code": code}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
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
/// Every item of a `GET /users` page, following `nextCursor` until the end.
async fn list_all(pool: &PgPool, a: &(String, String), query: &str) -> Vec<Value> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(c) => format!("/api/v1/users?{query}&cursor={c}"),
            None => format!("/api/v1/users?{query}"),
        };
        let (status, body) = call(pool, "GET", &path, Value::Null, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::OK, "{path} {body}");
        items.extend(body["items"].as_array().expect("items").iter().cloned());
        match body["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return items,
        }
    }
}
fn usernames(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|i| i["username"].as_str())
        .collect()
}
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_3_list_users_searches_filters_pages_and_hides_secrets(pool: PgPool) {
    let (admin_id, plain) = fixture(&pool).await;
    let a = admin(&pool).await;
    let (status, body) = call(
        &pool,
        "POST",
        "/api/v1/users",
        json!({"username":"beta.list@example.test","displayName":"Beta Sintetis","email":"Beta.Mail@Example.Test"}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (gone, _) = create_user(&pool, &a, "gamma.list@example.test").await;
    let (status, _) = call(
        &pool,
        "PATCH",
        &format!("/api/v1/users/{gone}"),
        json!({"active":false}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .expect("users");

    // Whole directory, newest first; only directory fields, never credential material.
    let all = list_all(&pool, &a, "limit=100").await;
    assert_eq!(all.len() as i64, total);
    let first = all[0].as_object().expect("item");
    let mut keys: Vec<&str> = first.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "active",
            "createdAt",
            "deactivatedAt",
            "displayName",
            "email",
            "id",
            "locked",
            "mfaEnrolled",
            "mustChangePassword",
            "platformAdmin",
            "username"
        ]
    );
    assert_eq!(first["username"], "gamma.list@example.test");
    let text = Value::Array(all.clone()).to_string();
    assert!(!text.contains("$argon2") && !text.contains("credentialVersion"));
    let me = all
        .iter()
        .find(|i| i["id"] == admin_id.to_string())
        .expect("admin listed");
    assert_eq!(me["platformAdmin"], true);
    assert_eq!(me["mfaEnrolled"], true);
    let other = all
        .iter()
        .find(|i| i["id"] == plain.to_string())
        .expect("plain listed");
    assert_eq!(other["platformAdmin"], false);
    assert_eq!(other["mfaEnrolled"], false);

    // Keyset paging (limit 1) returns the same rows in the same order, without repeats.
    let paged = list_all(&pool, &a, "limit=1").await;
    let ids = |v: &[Value]| v.iter().map(|i| i["id"].clone()).collect::<Vec<_>>();
    assert_eq!(ids(&paged), ids(&all));

    // Case-insensitive search on username, display name and email; LIKE wildcards are literal.
    for (q, expect) in [
        ("BETA.LIST", vec!["beta.list@example.test"]),
        ("beta%20sintetis", vec!["beta.list@example.test"]),
        ("beta.mail@", vec!["beta.list@example.test"]),
        ("%25", vec![]),
        ("_", vec![]),
    ] {
        let found = list_all(&pool, &a, &format!("q={q}")).await;
        assert_eq!(usernames(&found), expect, "q={q}");
    }
    let found = list_all(&pool, &a, "q=.list@&active=false").await;
    assert_eq!(usernames(&found), ["gamma.list@example.test"]);
    assert!(found[0]["deactivatedAt"].is_string());
    let found = list_all(&pool, &a, "q=.list@&active=true").await;
    assert_eq!(usernames(&found), ["beta.list@example.test"]);
    assert_eq!(found[0]["mustChangePassword"], true);
    assert_eq!(found[0]["email"], "beta.mail@example.test");
    assert_eq!(list_all(&pool, &a, "q=.list@&active=all").await.len(), 2);

    // Bad filters and a cursor replayed under other filters are 400.
    let (_, page) = call(
        &pool,
        "GET",
        "/api/v1/users?limit=1",
        Value::Null,
        (&a.0, &a.1),
    )
    .await;
    let cursor = page["nextCursor"].as_str().expect("cursor").to_owned();
    for path in [
        "/api/v1/users?active=maybe".to_owned(),
        "/api/v1/users?limit=0".to_owned(),
        "/api/v1/users?limit=101".to_owned(),
        format!("/api/v1/users?q={}", "x".repeat(101)),
        format!("/api/v1/users?limit=1&active=true&cursor={cursor}"),
        "/api/v1/users?cursor=not*base64".to_owned(),
    ] {
        let (status, _) = call(&pool, "GET", &path, Value::Null, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }

    // Session shows the admin flag so the client can offer the page; plain users get 403.
    let (_, session) = call(&pool, "GET", "/api/v1/session", Value::Null, (&a.0, &a.1)).await;
    assert_eq!(session["platformAdmin"], true);
    let (status, body, cookie) = request(
        &pool,
        "password",
        "/api/v1/login",
        "POST",
        json!({"username":"plain@example.test","password":ADMIN_PASSWORD}),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["platformAdmin"], false);
    let p = (
        cookie.expect("cookie"),
        body["csrfToken"].as_str().expect("csrf").to_owned(),
    );
    let (_, session) = call(&pool, "GET", "/api/v1/session", Value::Null, (&p.0, &p.1)).await;
    assert_eq!(session["platformAdmin"], false);
    let before = count(&pool, "SELECT count(*) FROM platform_audit_events WHERE actor_id=$1 AND capability='platform.users'", plain).await;
    let (status, body) = call(&pool, "GET", "/api/v1/users", Value::Null, (&p.0, &p.1)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.get("items").is_none());
    assert_eq!(
        count(&pool, "SELECT count(*) FROM platform_audit_events WHERE actor_id=$1 AND capability='platform.users'", plain).await,
        before + 1
    );
}

/// CP3c (found end to end): the administrator created by `init-password` is the first account
/// of a fresh deployment, so it must hold `platform_admin` to create the other users.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_3_initial_admin_from_init_password_can_administer_users(pool: PgPool) {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("db");
    seed::seed_uat(&pool, &name).await.expect("seed");
    let cfg = state(&pool, "password").config;
    itsm_api::password_bootstrap::bootstrap(&pool, &cfg, " Admin.Awal ", &name, ADMIN_PASSWORD)
        .await
        .expect("bootstrap");
    let admin_id: Uuid =
        sqlx::query_scalar("SELECT user_id FROM local_credentials WHERE username='admin.awal'")
            .fetch_one(&pool)
            .await
            .expect("admin");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM platform_grants WHERE user_id=$1 AND grant_code='platform_admin'",
            admin_id
        )
        .await,
        1
    );
    let (status, cookie, csrf) = login(&pool, "admin.awal", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    let a = (cookie, csrf);
    let (_, session) = call(&pool, "GET", "/api/v1/session", Value::Null, (&a.0, &a.1)).await;
    assert_eq!(session["platformAdmin"], true);
    assert_eq!(session["mfaRequired"], true);
    let (status, _) = call(&pool, "GET", "/api/v1/users", Value::Null, (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pre-MFA until enrolled");
    enrol_mfa(&pool, &a).await;
    let (status, _) = call(&pool, "GET", "/api/v1/users", Value::Null, (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::OK);
    let (id, _) = create_user(&pool, &a, "pengguna.pertama").await;
    let actor: Option<Uuid> = sqlx::query_scalar(
        "SELECT actor_id FROM auth_events WHERE event='user_create' AND user_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("event");
    assert_eq!(actor, Some(admin_id));
    // A second bootstrap never adds another administrator.
    assert!(
        itsm_api::password_bootstrap::bootstrap(&pool, &cfg, "admin.kedua", &name, ADMIN_PASSWORD)
            .await
            .is_err()
    );
}

async fn expire_step_up(pool: &PgPool, user: Uuid) {
    sqlx::query(
        "UPDATE sessions SET step_up_at = now() - interval '5 minutes 1 second' \
         WHERE user_id = $1 AND step_up_at IS NOT NULL",
    )
    .bind(user)
    .execute(pool)
    .await
    .expect("expire step-up");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp4b_sensitive_user_admin_needs_a_fresh_step_up(pool: PgPool) {
    let (admin_id, plain) = fixture(&pool).await;
    let a = admin(&pool).await;
    let (_, temp) = create_user(&pool, &a, "fresh.step@example.test").await;
    assert!(!temp.is_empty());
    expire_step_up(&pool, admin_id).await;
    sqlx::query(
        "UPDATE local_credentials SET failed_attempts=5, locked_until=now()+interval '15 minutes' \
         WHERE user_id=$1",
    )
    .bind(plain)
    .execute(&pool)
    .await
    .expect("lock");
    // A UAT Superadmin grant changes nothing in password mode (the grant counts only in UAT mode).
    sqlx::query("INSERT INTO platform_grants(user_id,grant_code) VALUES($1,'uat_superadmin')")
        .bind(admin_id)
        .execute(&pool)
        .await
        .expect("grant");
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/users".to_owned(),
            json!({"username":"stale.step@example.test","displayName":"Synthetic"}),
        ),
        (
            "POST",
            format!("/api/v1/users/{plain}/password-reset"),
            Value::Null,
        ),
        (
            "PATCH",
            format!("/api/v1/users/{plain}"),
            json!({"active": false}),
        ),
        (
            "POST",
            format!("/api/v1/users/{plain}/mfa-reset"),
            json!({"reason": "Perangkat hilang (uji sintetis)"}),
        ),
        // Unlock needs a step-up too (04-fsd, S-12; decided after CP4b).
        ("POST", format!("/api/v1/users/{plain}/unlock"), Value::Null),
    ] {
        let (status, body) = call(&pool, method, &path, body, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(body["error"]["code"], "step_up_required", "{method} {path}");
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM local_credentials WHERE username='stale.step@example.test' AND user_id<>$1",
            admin_id
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM users WHERE id=$1 AND active",
            plain
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM local_credentials WHERE user_id=$1 AND locked_until > now()",
            plain
        )
        .await,
        1,
        "unlock refused without step-up leaves the lock"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event='user_unlock'",
            plain
        )
        .await,
        0
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp4b_mfa_reset_revokes_sessions_and_forces_reenrolment(pool: PgPool) {
    let (admin_id, plain) = fixture(&pool).await;
    sqlx::query("INSERT INTO platform_grants(user_id,grant_code) VALUES($1,'platform_admin')")
        .bind(plain)
        .execute(&pool)
        .await
        .expect("second admin");
    let (status, cookie, csrf) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    let p = (cookie, csrf);
    enrol_mfa(&pool, &p).await;
    let a = admin(&pool).await;
    let reset = format!("/api/v1/users/{plain}/mfa-reset");

    for bad in [json!({"reason": "   "}), json!({"reason": "x".repeat(501)})] {
        let (status, body) = call(&pool, "POST", &reset, bad, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["field"], "reason");
    }
    let (status, body) = call(
        &pool,
        "POST",
        &format!("/api/v1/users/{admin_id}/mfa-reset"),
        json!({"reason": "Uji sintetis"}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "self_mfa_reset");
    let (status, _) = call(
        &pool,
        "POST",
        &format!("/api/v1/users/{}/mfa-reset", Uuid::new_v4()),
        json!({"reason": "Uji sintetis"}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(
        &pool,
        "POST",
        &reset,
        json!({"reason": "  Perangkat hilang (uji sintetis)  "}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = call(&pool, "GET", "/api/v1/session", Value::Null, (&p.0, &p.1)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "sessions revoked");
    assert!(
        count(
            &pool,
            "SELECT count(*) FROM sessions WHERE user_id=$1 AND revoked_reason='mfa_reset'",
            plain
        )
        .await
            >= 1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT (SELECT count(*) FROM user_mfa_totp WHERE user_id=$1) \
                  + (SELECT count(*) FROM mfa_recovery_codes WHERE user_id=$1) \
                  + (SELECT count(*) FROM encrypted_secrets WHERE created_by=$1 AND purpose='totp_seed')",
            plain
        )
        .await,
        0
    );
    let (actor, reason): (Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT actor_id, reason FROM auth_events WHERE user_id=$1 AND event='mfa_reset' AND outcome='success'",
    )
    .bind(plain)
    .fetch_one(&pool)
    .await
    .expect("audit");
    assert_eq!(actor, Some(admin_id));
    assert_eq!(reason.as_deref(), Some("Perangkat hilang (uji sintetis)"));
    let list = list_all(&pool, &a, "q=plain@").await;
    assert_eq!(list[0]["mfaEnrolled"], false);

    // MFA is mandatory for this admin: the next login is pre-MFA until enrolled again.
    let (status, cookie, csrf) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED, "single step: no factor left");
    let p = (cookie, csrf);
    let (_, session) = call(&pool, "GET", "/api/v1/session", Value::Null, (&p.0, &p.1)).await;
    assert_eq!(session["mfaRequired"], true);
    let (status, _) = call(&pool, "GET", "/api/v1/users", Value::Null, (&p.0, &p.1)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pre-MFA");
    enrol_mfa(&pool, &p).await;
    let (status, _) = call(&pool, "GET", "/api/v1/users", Value::Null, (&p.0, &p.1)).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp4b_unlock_clears_password_and_totp_lockout(pool: PgPool) {
    let (admin_id, plain) = fixture(&pool).await;
    let (status, cookie, csrf) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    enrol_mfa(&pool, &(cookie, csrf)).await;
    let a = admin(&pool).await;
    for sql in [
        "UPDATE local_credentials SET failed_attempts=5, locked_until=now()+interval '15 minutes' WHERE user_id=$1",
        "UPDATE user_mfa_totp SET failed_attempts=5, locked_until=now()+interval '15 minutes', version=version+1 WHERE user_id=$1",
    ] {
        sqlx::query(sql)
            .bind(plain)
            .execute(&pool)
            .await
            .expect("lock");
    }
    let (status, _, _) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_ne!(status, StatusCode::CREATED, "locked");
    assert_eq!(list_all(&pool, &a, "q=plain@").await[0]["locked"], true);
    // Only the TOTP lock still counts as locked.
    sqlx::query(
        "UPDATE local_credentials SET failed_attempts=0, locked_until=NULL WHERE user_id=$1",
    )
    .bind(plain)
    .execute(&pool)
    .await
    .expect("password unlock");
    assert_eq!(list_all(&pool, &a, "q=plain@").await[0]["locked"], true);
    sqlx::query("UPDATE local_credentials SET failed_attempts=5, locked_until=now()+interval '15 minutes' WHERE user_id=$1")
        .bind(plain)
        .execute(&pool)
        .await
        .expect("relock");

    let (status, body) = call(
        &pool,
        "POST",
        &format!("/api/v1/users/{plain}/unlock"),
        Value::Null,
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        count(
            &pool,
            "SELECT (SELECT count(*) FROM local_credentials WHERE user_id=$1 \
                       AND (failed_attempts>0 OR locked_until IS NOT NULL)) \
                  + (SELECT count(*) FROM user_mfa_totp WHERE user_id=$1 \
                       AND (failed_attempts>0 OR locked_until IS NOT NULL))",
            plain
        )
        .await,
        0
    );
    assert_eq!(list_all(&pool, &a, "q=plain@").await[0]["locked"], false);
    let actor: Option<Uuid> = sqlx::query_scalar(
        "SELECT actor_id FROM auth_events WHERE user_id=$1 AND event='user_unlock' AND outcome='success'",
    )
    .bind(plain)
    .fetch_one(&pool)
    .await
    .expect("audit");
    assert_eq!(actor, Some(admin_id));
    // The password step works again (an enrolled user now gets the MFA challenge).
    let (status, _, _) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &pool,
        "POST",
        &format!("/api/v1/users/{}/unlock", Uuid::new_v4()),
        Value::Null,
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp4b_non_admin_gets_403_on_mfa_reset_and_unlock(pool: PgPool) {
    let (admin_id, plain) = fixture(&pool).await;
    let (status, cookie, csrf) = login(&pool, "plain@example.test", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    let p = (cookie, csrf);
    for (path, body) in [
        (
            format!("/api/v1/users/{admin_id}/mfa-reset"),
            json!({"reason": "Uji sintetis"}),
        ),
        (format!("/api/v1/users/{admin_id}/unlock"), Value::Null),
    ] {
        // 403 comes before the step-up check: no hint that a step-up would help.
        let (status, body) = call(&pool, "POST", &path, body, (&p.0, &p.1)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"]["code"], "forbidden");
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM auth_events WHERE actor_id=$1 AND event IN ('mfa_reset','user_unlock')",
            plain
        )
        .await,
        0
    );
}

/// TM-41 / A-9 after the main merge (CP-INT): changing an LLM provider or the MCP list needs a
/// fresh step-up; reading and testing do not. Permission is checked first (403 before 401).
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp_int_ai_settings_changes_need_a_fresh_step_up(pool: PgPool) {
    let (admin_id, _) = fixture(&pool).await;
    let a = admin(&pool).await;
    let membership: (Uuid, Uuid) = sqlx::query_as(
        "SELECT workspace_id, id FROM workspace_memberships WHERE user_id=$1 AND active LIMIT 1",
    )
    .bind(admin_id)
    .fetch_one(&pool)
    .await
    .expect("membership");
    let ws = membership.0;
    sqlx::query(
        "INSERT INTO workspace_grants(workspace_id,membership_id,grant_code,granted_by) \
         VALUES($1,$2,'integration.manage',$3)",
    )
    .bind(ws)
    .bind(membership.1)
    .bind(admin_id)
    .execute(&pool)
    .await
    .expect("grant");
    let base = format!("/api/v1/workspaces/{ws}/ai-settings");
    let provider = json!({"model":"gpt-synthetic","baseUrl":"https://llm.example.test/v1","keyEnv":"SYNTHETIC_LLM_KEY"});
    let (status, body) = call(
        &pool,
        "PUT",
        &format!("{base}/providers/openai"),
        provider.clone(),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &pool,
        "POST",
        &format!("{base}/mcp-servers"),
        json!({"name":"Synthetic MCP","transport":"http","endpoint":"https://mcp.example.test"}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let server = body["id"].as_str().expect("id").to_owned();

    expire_step_up(&pool, admin_id).await;
    let mut changed = provider;
    changed["baseUrl"] = json!("https://exfil.example.test/v1");
    changed["version"] = json!(1);
    for (method, path, body) in [
        ("PUT", format!("{base}/providers/openai"), changed),
        (
            "POST",
            format!("{base}/mcp-servers"),
            json!({"name":"Stale MCP","transport":"http","endpoint":"https://mcp2.example.test"}),
        ),
        (
            "PATCH",
            format!("{base}/mcp-servers/{server}"),
            json!({"enabled":true,"version":1}),
        ),
    ] {
        let (status, body) = call(&pool, method, &path, body, (&a.0, &a.1)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(body["error"]["code"], "step_up_required", "{method} {path}");
    }
    let (base_url, version): (Option<String>, i32) = sqlx::query_as(
        "SELECT base_url, version FROM ai_providers WHERE workspace_id=$1 AND provider='openai'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("provider");
    assert_eq!(
        (base_url.as_deref(), version),
        (Some("https://llm.example.test/v1"), 1)
    );
    let mcp: (i64, bool) =
        sqlx::query_as("SELECT count(*), bool_or(enabled) FROM mcp_servers WHERE workspace_id=$1")
            .bind(ws)
            .fetch_one(&pool)
            .await
            .expect("mcp");
    assert_eq!(mcp, (1, false));
    let (status, _) = call(&pool, "GET", &base, Value::Null, (&a.0, &a.1)).await;
    assert_eq!(status, StatusCode::OK, "reading needs no step-up");

    // Without integration.manage the answer is 403 even with a stale step-up (403 before 401).
    sqlx::query(
        "UPDATE workspace_grants SET revoked_at=now(), revoked_by=$2 \
         WHERE membership_id=$1 AND grant_code='integration.manage'",
    )
    .bind(membership.1)
    .bind(admin_id)
    .execute(&pool)
    .await
    .expect("revoke");
    let (status, body) = call(
        &pool,
        "PUT",
        &format!("{base}/providers/openai"),
        json!({"model":"x","version":1}),
        (&a.0, &a.1),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
