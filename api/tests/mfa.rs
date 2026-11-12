#![allow(clippy::expect_used)]
//! US-1.2 / TM-02: TOTP enrolment, two-step login, anti-replay, lockout and recovery codes.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use itsm_api::{AppState, app, config::Config, mfa, routes::password::hash_password, seed};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use uuid::Uuid;

// Synthetic test-only key material.
const KEYRING: &str = "t1:AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";
const PEPPER: &str = "dGVzdC1wZXBwZXItdGVzdC1wZXBwZXItdGVzdC1wZXBwZXI=";
const PASSWORD: &str = "synthetic-password-123";

fn state(pool: &PgPool, keys: bool) -> AppState {
    let mut pairs = vec![
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "password"),
        ("ITSM_COOKIE_SECURE", "true"),
        ("DATABASE_URL", "postgres://unused"),
    ];
    if keys {
        pairs.extend([
            ("ITSM_KEYRING", KEYRING),
            ("ITSM_KEYRING_ACTIVE", "t1"),
            ("ITSM_MFA_RECOVERY_PEPPER", PEPPER),
        ]);
    }
    let vars: HashMap<String, String> = pairs
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    AppState {
        pool: pool.clone(),
        config: Arc::new(Config::from_map(&vars).expect("config")),
    }
}

struct Reply {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
    cache: Option<String>,
}

/// `login` adds the login headers; `auth` is (cookie, csrf).
async fn send(
    pool: &PgPool,
    keys: bool,
    method: &str,
    path: &str,
    body: Value,
    login: bool,
    auth: Option<(&str, &str)>,
) -> Reply {
    loop {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if login {
            r = r
                .header("x-itsm-login", "1")
                .header("sec-fetch-site", "same-origin");
        }
        if let Some((cookie, csrf)) = auth {
            r = r.header("cookie", cookie).header("x-csrf-token", csrf);
        }
        let req = r.body(Body::from(body.to_string())).expect("request");
        let response = app(state(pool, keys)).oneshot(req).await.expect("response");
        // Login hashing is bounded by a process-wide semaphore; parallel tests may see 429.
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        let status = response.status();
        let header = |n: &str| {
            response
                .headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let cookie = header("set-cookie").and_then(|c| c.split(';').next().map(str::to_owned));
        let cache = header("cache-control");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("bytes")
            .to_bytes();
        return Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
            cache,
        };
    }
}

/// Seeds the dummy workspace; `admin@example.test` holds `platform_admin` (MFA mandatory),
/// `plain@example.test` holds nothing (MFA optional).
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
    let plain: Uuid = sqlx::query_scalar("SELECT u.id FROM users u WHERE u.id<>$1 AND EXISTS (SELECT 1 FROM workspace_memberships m WHERE m.user_id=u.id AND m.active) ORDER BY u.id LIMIT 1")
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
        .bind(hash_password(PASSWORD).expect("hash"))
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

async fn password_login(pool: &PgPool, username: &str) -> Reply {
    send(
        pool,
        true,
        "POST",
        "/api/v1/login",
        json!({"username": username, "password": PASSWORD}),
        true,
        None,
    )
    .await
}

fn auth(r: &Reply) -> (String, String) {
    (
        r.cookie.clone().unwrap_or_default(),
        r.body["csrfToken"].as_str().unwrap_or_default().to_owned(),
    )
}

async fn get(pool: &PgPool, path: &str, a: &(String, String)) -> Reply {
    send(
        pool,
        true,
        "GET",
        path,
        Value::Null,
        false,
        Some((&a.0, &a.1)),
    )
    .await
}

async fn post(pool: &PgPool, path: &str, body: Value, a: &(String, String)) -> Reply {
    send(pool, true, "POST", path, body, false, Some((&a.0, &a.1))).await
}

fn seed_of(uri: &str) -> Vec<u8> {
    let b32 = uri
        .split(['?', '&'])
        .find_map(|p| p.strip_prefix("secret="))
        .expect("secret param");
    mfa::base32_decode(b32).expect("base32")
}

/// A six-digit value that matches none of the steps the server could accept right now.
fn wrong_code(seed: &[u8]) -> String {
    let now = mfa::now_step();
    let live: Vec<String> = (now - 2..=now + 2).map(|s| mfa::code_at(seed, s)).collect();
    (0..10u32)
        .map(|d| d.to_string().repeat(6))
        .find(|c| !live.contains(c))
        .expect("free code")
}

struct Enrolled {
    session: (String, String),
    seed: Vec<u8>,
    uri: String,
    codes: Vec<String>,
}

/// Password login (pre-MFA for the admin), enrolment and confirmation with the current code.
async fn enrol(pool: &PgPool, username: &str) -> Enrolled {
    let login = password_login(pool, username).await;
    assert_eq!(login.status, StatusCode::CREATED, "{}", login.body);
    let session = auth(&login);
    let started = post(pool, "/api/v1/me/mfa/enrollment", Value::Null, &session).await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    let uri = started.body["otpauthUri"].as_str().expect("uri").to_owned();
    let id = started.body["enrollmentId"]
        .as_str()
        .expect("id")
        .to_owned();
    let seed = seed_of(&uri);
    let code = mfa::code_at(&seed, mfa::now_step());
    let confirmed = post(
        pool,
        &format!("/api/v1/me/mfa/enrollment/{id}/confirm"),
        json!({"code": code}),
        &session,
    )
    .await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    let codes = confirmed.body["recoveryCodes"]
        .as_array()
        .expect("codes")
        .iter()
        .map(|c| c.as_str().expect("code").to_owned())
        .collect();
    Enrolled {
        session,
        seed,
        uri,
        codes,
    }
}

/// Step 1 for an enrolled user: 401 mfa_required with a challenge and no session cookie.
async fn challenge(pool: &PgPool, username: &str) -> String {
    let r = password_login(pool, username).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["error"]["code"], "mfa_required");
    assert!(r.cookie.is_none(), "no session before the second factor");
    r.body["error"]["challengeId"]
        .as_str()
        .expect("challengeId")
        .to_owned()
}

/// POST /session/step-up with the TOTP code `ahead` steps from now (anti-replay needs a new step).
async fn step_up(pool: &PgPool, a: &(String, String), seed: &[u8], ahead: u64) -> Reply {
    let code = mfa::code_at(seed, mfa::now_step() + ahead);
    post(pool, "/api/v1/session/step-up", json!({"code": code}), a).await
}

async fn second_step(pool: &PgPool, body: Value) -> Reply {
    send(pool, true, "POST", "/api/v1/login/mfa", body, true, None).await
}

async fn events(pool: &PgPool, user: Uuid, event: &str, outcome: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event=$2 AND outcome=$3",
    )
    .bind(user)
    .bind(event)
    .bind(outcome)
    .fetch_one(pool)
    .await
    .expect("events")
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_enrol_confirm_and_two_step_login(pool: PgPool) {
    let (admin, _) = fixture(&pool).await;
    let login = password_login(&pool, "admin@example.test").await;
    assert_eq!(login.status, StatusCode::CREATED);
    assert_eq!(login.body["mfaRequired"], true);
    assert_eq!(login.body["mfaEnrolled"], false);
    let pre = auth(&login);
    let status = get(&pool, "/api/v1/me/mfa", &pre).await;
    assert_eq!(
        status.body,
        json!({"enrolled": false, "required": true, "recoveryCodesLeft": 0})
    );

    let started = post(&pool, "/api/v1/me/mfa/enrollment", Value::Null, &pre).await;
    assert_eq!(started.status, StatusCode::CREATED);
    assert_eq!(started.cache.as_deref(), Some("no-store"));
    let uri = started.body["otpauthUri"].as_str().expect("uri");
    assert!(uri.starts_with("otpauth://totp/ITSM%20DECA:admin%40example.test?secret="));
    assert!(
        uri.contains("algorithm=SHA1") && uri.contains("digits=6") && uri.contains("period=30")
    );
    let id = started.body["enrollmentId"].as_str().expect("id");
    let seed = seed_of(uri);
    assert_eq!(seed.len(), mfa::SEED_LEN);
    let confirm_path = format!("/api/v1/me/mfa/enrollment/{id}/confirm");

    // A wrong code keeps the session (422, not 401) and nothing is activated.
    let wrong = post(
        &pool,
        &confirm_path,
        json!({"code": wrong_code(&seed)}),
        &pre,
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(wrong.body["error"]["field"], "code");
    assert_eq!(events(&pool, admin, "mfa_enroll", "failed").await, 1);
    // An unknown enrolment id is 404.
    let other = post(
        &pool,
        &format!("/api/v1/me/mfa/enrollment/{}/confirm", Uuid::new_v4()),
        json!({"code": "123456"}),
        &pre,
    )
    .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);

    let ok = post(
        &pool,
        &confirm_path,
        json!({"code": mfa::code_at(&seed, mfa::now_step())}),
        &pre,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.cache.as_deref(), Some("no-store"));
    let codes: Vec<&str> = ok.body["recoveryCodes"]
        .as_array()
        .expect("codes")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(codes.len(), 10);
    let unique: std::collections::HashSet<_> = codes.iter().collect();
    assert_eq!(unique.len(), 10);
    assert!(
        codes
            .iter()
            .all(|c| c.len() == 19 && c.matches('-').count() == 3)
    );
    assert_eq!(events(&pool, admin, "mfa_enroll", "success").await, 1);
    // The confirming session is now MFA-verified: admin routes open, a second enrolment is 409.
    assert_eq!(
        get(&pool, "/api/v1/users", &pre).await.status,
        StatusCode::OK
    );
    let again = post(&pool, "/api/v1/me/mfa/enrollment", Value::Null, &pre).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.body["error"]["code"], "mfa_already_enrolled");
    let session = get(&pool, "/api/v1/session", &pre).await;
    assert_eq!(session.body["mfaEnrolled"], true);
    assert_eq!(session.body["mfaRequired"], true);

    // Two-step login: a challenge, no cookie; the second step needs the login headers.
    let challenge_id = challenge(&pool, "admin@example.test").await;
    let next = mfa::code_at(&seed, mfa::now_step() + 1);
    let headerless = send(
        &pool,
        true,
        "POST",
        "/api/v1/login/mfa",
        json!({"challengeId": challenge_id, "code": next}),
        false,
        None,
    )
    .await;
    assert_eq!(headerless.status, StatusCode::UNAUTHORIZED);
    let both = second_step(
        &pool,
        json!({"challengeId": challenge_id, "code": next, "recoveryCode": codes[0]}),
    )
    .await;
    assert_eq!(both.status, StatusCode::UNPROCESSABLE_ENTITY);
    let done = second_step(&pool, json!({"challengeId": challenge_id, "code": next})).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    assert_eq!(done.body["mfaEnrolled"], true);
    assert!(done.cookie.is_some());
    let full = auth(&done);
    assert_eq!(
        get(&pool, "/api/v1/users", &full).await.status,
        StatusCode::OK
    );
    assert_eq!(events(&pool, admin, "mfa_verify", "success").await, 1);
    // The challenge is single-use.
    let reused = second_step(&pool, json!({"challengeId": challenge_id, "code": next})).await;
    assert_eq!(reused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reused.body["error"]["code"], "mfa_invalid");

    // An expired challenge is 410.
    let stale = challenge(&pool, "admin@example.test").await;
    sqlx::query(
        "UPDATE mfa_login_challenges SET created_at=now()-interval '20 minutes', \
         expires_at=now()-interval '15 minutes' WHERE id=$1::uuid",
    )
    .bind(&stale)
    .execute(&pool)
    .await
    .expect("age challenge");
    let late = second_step(&pool, json!({"challengeId": stale, "code": next})).await;
    assert_eq!(late.status, StatusCode::GONE);
    assert_eq!(late.body["error"]["code"], "mfa_challenge_expired");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_same_totp_code_is_refused_the_second_time(pool: PgPool) {
    let (admin, _) = fixture(&pool).await;
    let e = enrol(&pool, "admin@example.test").await;
    let code = mfa::code_at(&e.seed, mfa::now_step() + 1);
    let first = challenge(&pool, "admin@example.test").await;
    let ok = second_step(&pool, json!({"challengeId": first, "code": code})).await;
    assert_eq!(ok.status, StatusCode::OK);
    let second = challenge(&pool, "admin@example.test").await;
    let replay = second_step(&pool, json!({"challengeId": second, "code": code})).await;
    assert_eq!(replay.status, StatusCode::UNAUTHORIZED);
    assert_eq!(replay.body["error"]["code"], "mfa_invalid");
    let failure: Option<String> = sqlx::query_scalar(
        "SELECT failure_code FROM auth_events WHERE user_id=$1 AND event='mfa_verify' \
         AND outcome='failed'",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .expect("replay event");
    assert_eq!(failure.as_deref(), Some("replay"));
    // The code used at confirmation is spent as well.
    let confirm_code = mfa::code_at(&e.seed, mfa::now_step());
    let third = challenge(&pool, "admin@example.test").await;
    let older = second_step(&pool, json!({"challengeId": third, "code": confirm_code})).await;
    assert_eq!(older.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_recovery_code_is_single_use_and_regeneration_spends_the_old_batch(pool: PgPool) {
    let (admin, _) = fixture(&pool).await;
    let e = enrol(&pool, "admin@example.test").await;
    // Case and separators do not matter.
    let typed = e.codes[0].to_lowercase().replace('-', " ");
    let c1 = challenge(&pool, "admin@example.test").await;
    let ok = second_step(&pool, json!({"challengeId": c1, "recoveryCode": typed})).await;
    assert_eq!(ok.status, StatusCode::OK);
    let c2 = challenge(&pool, "admin@example.test").await;
    let twice = second_step(
        &pool,
        json!({"challengeId": c2, "recoveryCode": e.codes[0]}),
    )
    .await;
    assert_eq!(twice.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        events(&pool, admin, "recovery_code_used", "success").await,
        1
    );
    assert_eq!(
        events(&pool, admin, "recovery_code_used", "failed").await,
        1
    );
    let session = auth(&ok);
    let left = get(&pool, "/api/v1/me/mfa", &session).await;
    assert_eq!(left.body["recoveryCodesLeft"], 9);
    assert_eq!(
        step_up(&pool, &session, &e.seed, 1).await.status,
        StatusCode::NO_CONTENT
    );

    let fresh = post(
        &pool,
        "/api/v1/me/mfa/recovery-codes",
        Value::Null,
        &session,
    )
    .await;
    assert_eq!(fresh.status, StatusCode::CREATED);
    assert_eq!(fresh.cache.as_deref(), Some("no-store"));
    let new_codes: Vec<String> = fresh.body["recoveryCodes"]
        .as_array()
        .expect("codes")
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    assert_eq!(new_codes.len(), 10);
    assert!(new_codes.iter().all(|c| !e.codes.contains(c)));
    assert_eq!(
        events(&pool, admin, "recovery_codes_issue", "success").await,
        1
    );
    let c3 = challenge(&pool, "admin@example.test").await;
    let old = second_step(
        &pool,
        json!({"challengeId": c3, "recoveryCode": e.codes[1]}),
    )
    .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new = second_step(
        &pool,
        json!({"challengeId": c3, "recoveryCode": new_codes[0]}),
    )
    .await;
    assert_eq!(new.status, StatusCode::OK);
    // Without an enrolment there is nothing to regenerate.
    let plain = password_login(&pool, "plain@example.test").await;
    let none = post(
        &pool,
        "/api/v1/me/mfa/recovery-codes",
        Value::Null,
        &auth(&plain),
    )
    .await;
    assert_eq!(none.status, StatusCode::CONFLICT);
    assert_eq!(none.body["error"]["code"], "mfa_not_enrolled");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_five_wrong_codes_lock_totp_but_not_recovery_codes(pool: PgPool) {
    let (admin, _) = fixture(&pool).await;
    let e = enrol(&pool, "admin@example.test").await;
    let c = challenge(&pool, "admin@example.test").await;
    let bad = wrong_code(&e.seed);
    for _ in 0..5 {
        let r = second_step(&pool, json!({"challengeId": c, "code": bad})).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.body["error"]["code"], "mfa_invalid");
    }
    let locked: bool = sqlx::query_scalar(
        "SELECT locked_until BETWEEN now()+interval '14 minutes' AND now()+interval '16 minutes' \
         FROM user_mfa_totp WHERE user_id=$1",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .expect("lock");
    assert!(locked);
    let lockouts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth_events WHERE user_id=$1 AND event='lockout' AND failure_code='mfa'",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .expect("lockout");
    assert_eq!(lockouts, 1);
    // While locked even the right code is refused.
    let right = mfa::code_at(&e.seed, mfa::now_step() + 1);
    let r = second_step(&pool, json!({"challengeId": c, "code": right})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let denied: Option<String> = sqlx::query_scalar(
        "SELECT failure_code FROM auth_events WHERE user_id=$1 AND event='mfa_verify' \
         AND outcome='denied'",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .expect("denied");
    assert_eq!(denied.as_deref(), Some("locked"));
    // TM-10: a recovery code still works and clears the lock.
    let r = second_step(&pool, json!({"challengeId": c, "recoveryCode": e.codes[3]})).await;
    assert_eq!(r.status, StatusCode::OK);
    let cleared: bool = sqlx::query_scalar(
        "SELECT locked_until IS NULL AND failed_attempts=0 FROM user_mfa_totp WHERE user_id=$1",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .expect("cleared");
    assert!(cleared);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_admin_without_enrolment_gets_only_a_pre_mfa_session(pool: PgPool) {
    let (admin, plain) = fixture(&pool).await;
    let login = password_login(&pool, "admin@example.test").await;
    assert_eq!(login.status, StatusCode::CREATED);
    let pre = auth(&login);
    for path in ["/api/v1/users", "/api/v1/workspaces"] {
        assert_eq!(
            get(&pool, path, &pre).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let change = post(
        &pool,
        "/api/v1/account/password",
        json!({"currentPassword": PASSWORD, "newPassword": "another-synthetic-1"}),
        &pre,
    )
    .await;
    assert_eq!(change.status, StatusCode::FORBIDDEN);
    let regen = post(&pool, "/api/v1/me/mfa/recovery-codes", Value::Null, &pre).await;
    assert_eq!(regen.status, StatusCode::FORBIDDEN);
    for path in ["/api/v1/session", "/api/v1/meta", "/api/v1/me/mfa"] {
        assert_eq!(
            get(&pool, path, &pre).await.status,
            StatusCode::OK,
            "{path}"
        );
    }
    let session = get(&pool, "/api/v1/session", &pre).await;
    assert_eq!(session.body["mfaRequired"], true);
    assert_eq!(session.body["mfaEnrolled"], false);

    // An enrolment left unconfirmed past its 10 minutes is 410; a new one replaces it.
    let started = post(&pool, "/api/v1/me/mfa/enrollment", Value::Null, &pre).await;
    let id = started.body["enrollmentId"]
        .as_str()
        .expect("id")
        .to_owned();
    sqlx::query(
        "UPDATE user_mfa_totp SET enrolled_at=now()-interval '11 minutes' WHERE user_id=$1",
    )
    .bind(admin)
    .execute(&pool)
    .await
    .expect("age");
    let seed = seed_of(started.body["otpauthUri"].as_str().expect("uri"));
    let late = post(
        &pool,
        &format!("/api/v1/me/mfa/enrollment/{id}/confirm"),
        json!({"code": mfa::code_at(&seed, mfa::now_step())}),
        &pre,
    )
    .await;
    assert_eq!(late.status, StatusCode::GONE);
    assert_eq!(late.body["error"]["code"], "mfa_enrollment_expired");
    let restarted = post(&pool, "/api/v1/me/mfa/enrollment", Value::Null, &pre).await;
    assert_eq!(restarted.status, StatusCode::CREATED);
    let seeds: i64 =
        sqlx::query_scalar("SELECT count(*) FROM encrypted_secrets WHERE purpose='totp_seed'")
            .fetch_one(&pool)
            .await
            .expect("seeds");
    assert_eq!(seeds, 1, "the abandoned seed is deleted");

    // Logout stays possible from a pre-MFA session.
    let out = send(
        &pool,
        true,
        "DELETE",
        "/api/v1/session",
        Value::Null,
        false,
        Some((&pre.0, &pre.1)),
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);

    // A user without admin grants is not forced; a live admin.* or integration.* grant forces MFA.
    let p = password_login(&pool, "plain@example.test").await;
    assert_eq!(p.body["mfaRequired"], false);
    assert_eq!(
        get(&pool, "/api/v1/workspaces", &auth(&p)).await.status,
        StatusCode::OK
    );
    let required = |user: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT itsm_mfa_required($1)")
                .bind(user)
                .fetch_one(&pool)
                .await
                .expect("required")
        }
    };
    assert!(!required(plain).await);
    let membership: (Uuid, Uuid) = sqlx::query_as(
        "SELECT workspace_id, id FROM workspace_memberships WHERE user_id=$1 AND active LIMIT 1",
    )
    .bind(plain)
    .fetch_one(&pool)
    .await
    .expect("membership");
    let grant: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace_grants(workspace_id,membership_id,grant_code,granted_by) \
         VALUES($1,$2,'integration.manage',$3) RETURNING id",
    )
    .bind(membership.0)
    .bind(membership.1)
    .bind(admin)
    .fetch_one(&pool)
    .await
    .expect("grant");
    assert!(required(plain).await);
    // The plain user's existing, unverified session is now pre-MFA.
    assert_eq!(
        get(&pool, "/api/v1/workspaces", &auth(&p)).await.status,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE workspace_grants SET revoked_at=now(), revoked_by=$2 WHERE id=$1")
        .bind(grant)
        .bind(admin)
        .execute(&pool)
        .await
        .expect("revoke");
    assert!(!required(plain).await);
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One process-wide subscriber for this test binary: a scoped one misses events whose callsite
/// interest was cached by tests running in parallel without one. Every test logs into it.
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

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_no_seed_or_codes_in_responses_audit_or_logs(pool: PgPool) {
    let captured = capture();

    let (admin, _) = fixture(&pool).await;
    let e = enrol(&pool, "admin@example.test").await;
    let b32 = mfa::base32_encode(&e.seed);
    let c = challenge(&pool, "admin@example.test").await;
    let done = second_step(&pool, json!({"challengeId": c, "recoveryCode": e.codes[0]})).await;
    let wrong = second_step(&pool, json!({"challengeId": c, "recoveryCode": e.codes[0]})).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let s = auth(&done);
    let mut later = vec![
        get(&pool, "/api/v1/me/mfa", &s).await.body,
        get(&pool, "/api/v1/session", &s).await.body,
        post(&pool, "/api/v1/me/mfa/enrollment", Value::Null, &e.session)
            .await
            .body,
        done.body.clone(),
        wrong.body.clone(),
    ];
    assert_eq!(
        step_up(&pool, &s, &e.seed, 1).await.status,
        StatusCode::NO_CONTENT
    );
    let regen = post(&pool, "/api/v1/me/mfa/recovery-codes", Value::Null, &s).await;
    let regen_codes: Vec<String> = regen.body["recoveryCodes"]
        .as_array()
        .expect("codes")
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    later.push(get(&pool, "/api/v1/me/mfa", &s).await.body);

    let mut secrets: Vec<String> = vec![b32.clone(), e.uri.clone()];
    for code in e.codes.iter().chain(regen_codes.iter()) {
        secrets.push(code.clone());
        secrets.push(code.replace('-', ""));
    }
    for body in &later {
        let text = body.to_string();
        for secret in &secrets {
            assert!(
                !text.contains(secret.as_str()),
                "response leaked a secret: {text}"
            );
        }
    }
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT row_to_json(e)::text FROM auth_events e WHERE user_id=$1")
            .bind(admin)
            .fetch_all(&pool)
            .await
            .expect("audit");
    assert!(!audit.is_empty());
    for row in &audit {
        for secret in &secrets {
            assert!(!row.contains(secret.as_str()), "audit leaked a secret");
        }
    }
    // The seed is stored only as ciphertext; recovery codes only as 32-byte HMACs.
    let ct: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM encrypted_secrets WHERE purpose='totp_seed'")
            .fetch_one(&pool)
            .await
            .expect("ciphertext");
    assert!(!ct.windows(e.seed.len()).any(|w| w == e.seed.as_slice()));
    let hashes: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT code_hash FROM mfa_recovery_codes WHERE user_id=$1")
            .bind(admin)
            .fetch_all(&pool)
            .await
            .expect("hashes");
    assert_eq!(hashes.len(), 20);
    assert!(hashes.iter().all(|h| h.len() == 32));

    let logs = String::from_utf8(captured.0.lock().expect("lock").clone()).expect("utf8");
    assert!(
        logs.contains("/api/v1/login/mfa"),
        "the capture saw the requests"
    );
    for secret in &secrets {
        assert!(!logs.contains(secret.as_str()), "log leaked a secret");
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_2_without_keys_mfa_is_unavailable_in_uat(pool: PgPool) {
    fixture(&pool).await;
    let login = send(
        &pool,
        false,
        "POST",
        "/api/v1/login",
        json!({"username": "admin@example.test", "password": PASSWORD}),
        true,
        None,
    )
    .await;
    assert_eq!(login.status, StatusCode::CREATED);
    let a = auth(&login);
    let r = send(
        &pool,
        false,
        "POST",
        "/api/v1/me/mfa/enrollment",
        Value::Null,
        false,
        Some((&a.0, &a.1)),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

async fn delete(pool: &PgPool, path: &str, a: &(String, String)) -> Reply {
    send(
        pool,
        true,
        "DELETE",
        path,
        Value::Null,
        false,
        Some((&a.0, &a.1)),
    )
    .await
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
async fn cp4b_step_up_opens_sensitive_actions_for_five_minutes(pool: PgPool) {
    let (admin, _) = fixture(&pool).await;
    let e = enrol(&pool, "admin@example.test").await;
    let s = &e.session;
    let regen = post(&pool, "/api/v1/me/mfa/recovery-codes", Value::Null, s).await;
    assert_eq!(regen.status, StatusCode::UNAUTHORIZED);
    assert_eq!(regen.body["error"]["code"], "step_up_required");

    let wrong = post(
        &pool,
        "/api/v1/session/step-up",
        json!({"code": wrong_code(&e.seed)}),
        s,
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body["error"]["code"], "mfa_invalid");
    assert_eq!(events(&pool, admin, "step_up", "failed").await, 1);
    let regen = post(&pool, "/api/v1/me/mfa/recovery-codes", Value::Null, s).await;
    assert_eq!(
        regen.body["error"]["code"], "step_up_required",
        "a failure opens nothing"
    );

    let code = mfa::code_at(&e.seed, mfa::now_step() + 1);
    let ok = post(&pool, "/api/v1/session/step-up", json!({"code": code}), s).await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT, "{}", ok.body);
    assert_eq!(events(&pool, admin, "step_up", "success").await, 1);
    let regen = post(&pool, "/api/v1/me/mfa/recovery-codes", Value::Null, s).await;
    assert_eq!(regen.status, StatusCode::CREATED, "{}", regen.body);
    let codes: Vec<String> = regen.body["recoveryCodes"]
        .as_array()
        .expect("codes")
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    let replay = post(&pool, "/api/v1/session/step-up", json!({"code": code}), s).await;
    assert_eq!(
        replay.body["error"]["code"], "mfa_invalid",
        "same code twice"
    );

    // Older than five minutes: closed again.
    expire_step_up(&pool, admin).await;
    let regen = post(&pool, "/api/v1/me/mfa/recovery-codes", Value::Null, s).await;
    assert_eq!(regen.status, StatusCode::UNAUTHORIZED);
    assert_eq!(regen.body["error"]["code"], "step_up_required");
    // The regenerated batch replaced the enrolment codes: an old code does not step up.
    let old = post(
        &pool,
        "/api/v1/session/step-up",
        json!({"code": e.codes[1]}),
        s,
    )
    .await;
    assert_eq!(old.body["error"]["code"], "mfa_invalid");
    // A current recovery code steps up (and is spent).
    let r = post(
        &pool,
        "/api/v1/session/step-up",
        json!({"code": codes[0]}),
        s,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    assert_eq!(
        get(&pool, "/api/v1/me/mfa", s).await.body["recoveryCodesLeft"],
        9
    );
    let r = delete(&pool, "/api/v1/me/mfa", s).await;
    assert_eq!(
        r.body["error"]["code"], "mfa_required_for_role",
        "step-up passed, role refuses"
    );

    // Step-up belongs to one session: a new session of the same user starts closed.
    let c = challenge(&pool, "admin@example.test").await;
    let other = second_step(&pool, json!({"challengeId": c, "recoveryCode": codes[1]})).await;
    assert_eq!(other.status, StatusCode::OK, "{}", other.body);
    let regen = post(
        &pool,
        "/api/v1/me/mfa/recovery-codes",
        Value::Null,
        &auth(&other),
    )
    .await;
    assert_eq!(regen.body["error"]["code"], "step_up_required");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp4b_step_up_failures_lock_like_the_login_and_need_an_enrolled_factor(pool: PgPool) {
    let (admin, plain) = fixture(&pool).await;
    let e = enrol(&pool, "admin@example.test").await;
    let bad = wrong_code(&e.seed);
    for _ in 0..5 {
        let r = post(
            &pool,
            "/api/v1/session/step-up",
            json!({"code": bad}),
            &e.session,
        )
        .await;
        assert_eq!(r.body["error"]["code"], "mfa_invalid");
    }
    assert_eq!(events(&pool, admin, "lockout", "success").await, 1);
    let good = mfa::code_at(&e.seed, mfa::now_step() + 1);
    let r = post(
        &pool,
        "/api/v1/session/step-up",
        json!({"code": good}),
        &e.session,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "TOTP locked");
    assert_eq!(events(&pool, admin, "step_up", "denied").await, 1);
    // The same lock holds at login (shared counter).
    let c = challenge(&pool, "admin@example.test").await;
    let r = second_step(&pool, json!({"challengeId": c, "code": good})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // A recovery code still works while TOTP is locked and clears the lock (TM-10).
    let r = post(
        &pool,
        "/api/v1/session/step-up",
        json!({"code": e.codes[0]}),
        &e.session,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);

    // A user without a factor cannot step up.
    let login = password_login(&pool, "plain@example.test").await;
    assert_eq!(login.status, StatusCode::CREATED);
    let r = post(
        &pool,
        "/api/v1/session/step-up",
        json!({"code": "123456"}),
        &auth(&login),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"]["code"], "mfa_not_enrolled");
    assert_eq!(events(&pool, plain, "step_up", "failed").await, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp4b_delete_own_mfa_is_refused_for_admins_and_needs_step_up(pool: PgPool) {
    let (admin, plain) = fixture(&pool).await;
    let a = enrol(&pool, "admin@example.test").await;
    assert_eq!(
        step_up(&pool, &a.session, &a.seed, 1).await.status,
        StatusCode::NO_CONTENT
    );
    let r = delete(&pool, "/api/v1/me/mfa", &a.session).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"]["code"], "mfa_required_for_role");
    assert_eq!(
        get(&pool, "/api/v1/me/mfa", &a.session).await.body["enrolled"],
        true
    );
    assert_eq!(events(&pool, admin, "mfa_disable", "success").await, 0);

    // A plain user enrolled voluntarily may remove the factor, after a step-up.
    let p = enrol(&pool, "plain@example.test").await;
    let r = delete(&pool, "/api/v1/me/mfa", &p.session).await;
    assert_eq!(r.body["error"]["code"], "step_up_required");
    assert_eq!(
        step_up(&pool, &p.session, &p.seed, 1).await.status,
        StatusCode::NO_CONTENT
    );
    let r = delete(&pool, "/api/v1/me/mfa", &p.session).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM user_mfa_totp WHERE user_id = $1) \
              + (SELECT count(*) FROM mfa_recovery_codes WHERE user_id = $1) \
              + (SELECT count(*) FROM encrypted_secrets WHERE created_by = $1 AND purpose = 'totp_seed')",
    )
    .bind(plain)
    .fetch_one(&pool)
    .await
    .expect("left");
    assert_eq!(left, 0);
    assert_eq!(events(&pool, plain, "mfa_disable", "success").await, 1);
    let r = delete(&pool, "/api/v1/me/mfa", &p.session).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "idempotent");
    assert_eq!(events(&pool, plain, "mfa_disable", "success").await, 1);
    // The next login is a single step again.
    assert_eq!(
        password_login(&pool, "plain@example.test").await.status,
        StatusCode::CREATED
    );
}
