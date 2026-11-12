//! CP5c slice 4: company SSO administration by a platform admin (password mode) against a local
//! stub IdP and a real PostgreSQL: create → client secret → discovery test → enable, re-test after
//! a move, turning off ends sessions, accounts linked by subject, validation, access and the
//! keyring. Every name, address and secret is synthetic.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::routes::password::hash_password;
use itsm_api::{AppState, app, seed};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

mod support;
use support::oidc_stub::*;

const PASSWORD: &str = "synthetic-password-123";

struct Res {
    status: StatusCode,
    body: Value,
    text: String,
    location: String,
    cookies: Vec<String>,
}

impl Res {
    fn cookie(&self, name: &str) -> Option<String> {
        self.cookies
            .iter()
            .find(|c| c.starts_with(&format!("{name}=")))
            .and_then(|c| c.split(';').next())
            .map(str::to_owned)
    }
}

#[derive(Clone)]
struct Auth {
    cookie: String,
    csrf: String,
}

async fn call(
    st: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
    auth: Option<&Auth>,
    cookie: Option<&str>,
) -> Res {
    loop {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(a) = auth {
            b = b
                .header(header::COOKIE, &a.cookie)
                .header("x-csrf-token", &a.csrf);
        } else if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        } else {
            b = b
                .header("x-itsm-login", "1")
                .header("sec-fetch-site", "same-origin");
        }
        let req = match &body {
            Some(v) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string())),
            None => b.body(Body::empty()),
        };
        let res = ok(app(st.clone()).oneshot(ok(req, "request")).await, "router");
        // Login hashing is bounded by a process-wide semaphore; parallel tests may see 429.
        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        let status = res.status();
        let h = res.headers();
        let location = h
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let cookies = h
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(str::to_owned)
            .collect();
        let bytes = ok(res.into_body().collect().await, "body").to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        return Res {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            text,
            location,
            cookies,
        };
    }
}

async fn api(st: &AppState, a: &Auth, method: &str, uri: &str, body: Option<Value>) -> Res {
    call(st, method, uri, body, Some(a), None).await
}

async fn id_of(pool: &PgPool, sql: &str) -> Uuid {
    ok(sqlx::query_scalar(sql).fetch_one(pool).await, sql)
}

async fn count(pool: &PgPool, sql: &str, bind: Uuid) -> i64 {
    ok(
        sqlx::query_scalar(sql).bind(bind).fetch_one(pool).await,
        sql,
    )
}

/// Gives `subject`'s seeded user a local password (and `platform_admin` when asked) and signs in.
async fn login(st: &AppState, pool: &PgPool, subject: &str, admin: bool) -> (Uuid, Auth) {
    let user: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = $1")
            .bind(subject)
            .fetch_one(pool)
            .await,
        "user",
    );
    let username = format!("{}@example.test", subject.replace(':', "-"));
    ok(
        sqlx::query(
            "INSERT INTO local_credentials (user_id, username, password_hash) VALUES ($1, $2, $3)",
        )
        .bind(user)
        .bind(&username)
        .bind(ok(hash_password(PASSWORD), "hash"))
        .execute(pool)
        .await,
        "credentials",
    );
    if admin {
        ok(
            sqlx::query(
                "INSERT INTO platform_grants (user_id, grant_code) VALUES ($1, 'platform_admin')",
            )
            .bind(user)
            .execute(pool)
            .await,
            "grant",
        );
    }
    let r = call(
        st,
        "POST",
        "/api/v1/login",
        Some(json!({"username": username, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let Some(cookie) = r.cookie("__Host-itsm_session") else {
        panic!("no session cookie: {:?}", r.cookies);
    };
    let csrf = r.body["csrfToken"].as_str().unwrap_or_default().to_owned();
    (user, Auth { cookie, csrf })
}

struct Fx {
    st: AppState,
    pool: PgPool,
    stub: Shared,
    base: String,
    admin: Uuid,
    auth: Auth,
    /// Company of workspace DUMA.
    alfa: Uuid,
    beta: Uuid,
}

impl Fx {
    fn sso(&self, company: Uuid, tail: &str) -> String {
        format!("/api/v1/platform/companies/{company}/sso{tail}")
    }

    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Res {
        api(&self.st, &self.auth, method, uri, body).await
    }
}

async fn fixture(pool: PgPool) -> Fx {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat(&pool, &name).await, "seed");
    let (stub, base) = stub().await;
    let st = state(&pool, "both", true);
    // cs-bravo has no company: a pure platform admin.
    let (admin, auth) = login(&st, &pool, "dummy:cs-bravo", true).await;
    let alfa = id_of(
        &pool,
        "SELECT company_id FROM workspaces WHERE code = 'DUMA'",
    )
    .await;
    let beta = id_of(
        &pool,
        "SELECT company_id FROM workspaces WHERE code = 'DUMB'",
    )
    .await;
    Fx {
        st,
        pool,
        stub,
        base,
        admin,
        auth,
        alfa,
        beta,
    }
}

fn settings(issuer: &str, client: &str) -> Value {
    json!({
        "displayName": "SSO Alfa",
        "issuer": issuer,
        "clientId": client,
        "emailDomains": [DOMAIN],
        "extraHosts": [],
        "groupClaim": "groups",
        "roleMapping": [{"group": "g-lead", "role": "lead_cs"}, {"group": "g-cs", "role": "cs"}],
        "syncRoles": false,
    })
}

fn with_version(mut v: Value, version: &Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("version".into(), version.clone());
    }
    v
}

/// Creates, stores the secret, tests and enables DUMA's IdP; returns its version.
async fn ready(fx: &Fx) -> i64 {
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, ""),
            Some(settings(&fx.base, CLIENT)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/secret"),
            Some(json!({"secret": SECRET, "version": 1})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.alfa, "/test"),
            Some(json!({"version": 2})),
        )
        .await;
    assert_eq!(r.body["result"]["ok"], json!(true), "{}", r.text);
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/enabled"),
            Some(json!({"enabled": true, "version": 3})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.body["item"]["version"].as_i64().unwrap_or_default()
}

/// Signs in through DUMA's IdP as `claims`; returns the session cookie (or the `?sso=` failure).
async fn sso_sign_in(fx: &Fx, code: &str, claims: Value) -> Result<String, String> {
    let r = call(
        &fx.st,
        "GET",
        "/api/v1/auth/oidc/duma/start",
        None,
        None,
        Some(""),
    )
    .await;
    if r.status != StatusCode::FOUND || !r.location.starts_with(&fx.base) {
        return Err(r.location);
    }
    let url = ok(reqwest::Url::parse(&r.location), "authorize url");
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let Some(bind) = r.cookie("__Host-itsm_oidc") else {
        panic!("binding cookie missing");
    };
    lock(&fx.stub).grants.insert(
        code.to_owned(),
        Grant {
            challenge: params.get("code_challenge").cloned().unwrap_or_default(),
            nonce: params.get("nonce").cloned().unwrap_or_default(),
            claims,
            status: None,
            rogue: false,
        },
    );
    let state = params.get("state").cloned().unwrap_or_default();
    let uri = format!("/api/v1/auth/oidc/callback?code={code}&state={state}");
    let r = call(&fx.st, "GET", &uri, None, None, Some(&bind)).await;
    r.cookie("__Host-itsm_session")
        .filter(|c| c.len() > "__Host-itsm_session=".len())
        .ok_or(r.location)
}

async fn session_status(fx: &Fx, cookie: &str) -> StatusCode {
    call(&fx.st, "GET", "/api/v1/session", None, None, Some(cookie))
        .await
        .status
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn admin_configures_tests_enables_and_turns_off_company_sso(pool: PgPool) {
    let fx = fixture(pool).await;

    let r = fx.api("GET", "/api/v1/platform/sso", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.body["server"],
        json!({"auth": "both", "redirectUri": format!("{PUBLIC_URL}/api/v1/auth/oidc/callback"),
               "keyring": true, "lab": true})
    );
    let alfa = r.body["items"]
        .as_array()
        .and_then(|i| i.iter().find(|c| c["code"] == "DUMA"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(alfa["provider"], Value::Null);
    assert_eq!(alfa["identities"], json!(0));

    // Create: off, untested, no secret.
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, ""),
            Some(settings(&fx.base, CLIENT)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let item = &r.body["item"];
    assert_eq!(
        (
            &item["enabled"],
            &item["secretSet"],
            &item["lastTestOk"],
            &item["version"]
        ),
        (&json!(false), &json!(false), &Value::Null, &json!(1))
    );
    // A second create is a stale write.
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, ""),
            Some(settings(&fx.base, CLIENT)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // Not ready: no secret, no test.
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/enabled"),
            Some(json!({"enabled": true, "version": 1})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.body["error"]["code"], "idp_not_ready", "{}", r.text);

    // Secret: write-only, replaced in place (one sealed row).
    for (version, replaced) in [(1, false), (2, true)] {
        let r = fx
            .api(
                "PUT",
                &fx.sso(fx.alfa, "/secret"),
                Some(json!({"secret": SECRET, "version": version})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.body["item"]["secretSet"], json!(true));
        assert!(!r.text.contains(SECRET), "secret echoed");
        let audited: bool = ok(
            sqlx::query_scalar(
                "SELECT (safe_change->>'replaced')::boolean FROM audit_events \
                 WHERE action = 'idp.secret.set' ORDER BY occurred_at DESC LIMIT 1",
            )
            .fetch_one(&fx.pool)
            .await,
            "secret audit",
        );
        assert_eq!(audited, replaced);
    }
    assert_eq!(
        count(
            &fx.pool,
            "SELECT count(*) FROM encrypted_secrets WHERE purpose = 'oidc_client_secret' \
             AND created_by = $1",
            fx.admin,
        )
        .await,
        1
    );

    // Test: stale version first, then a pass at the stub.
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.alfa, "/test"),
            Some(json!({"version": 2})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.alfa, "/test"),
            Some(json!({"version": 3})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.body["result"],
        json!({"ok": true, "stage": null, "code": null})
    );
    assert_eq!(r.body["item"]["lastTestOk"], json!(true));
    assert_eq!(r.body["item"]["version"], json!(4));

    // Enable; enabling again changes nothing (422).
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/enabled"),
            Some(json!({"enabled": true, "version": 4})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["item"]["enabled"], json!(true));
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/enabled"),
            Some(json!({"enabled": true, "version": 5})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);

    // The configured IdP signs people in (just-in-time provisioning).
    let rina = format!("rina@{DOMAIN}");
    let cookie = match sso_sign_in(
        &fx,
        "c1",
        json!({"sub": "sub-rina", "email": rina, "email_verified": true, "name": "Rina Alfa",
               "groups": ["g-cs"]}),
    )
    .await
    {
        Ok(c) => c,
        Err(l) => panic!("sign-in failed: {l}"),
    };
    assert_eq!(session_status(&fx, &cookie).await, StatusCode::OK);
    let r = fx.api("GET", "/api/v1/platform/sso", None).await;
    let alfa = r.body["items"]
        .as_array()
        .and_then(|i| i.iter().find(|c| c["code"] == "DUMA"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(alfa["identities"], json!(1));
    assert_eq!(alfa["provider"]["enabled"], json!(true));

    // A cosmetic change keeps it on and tested.
    let mut s = settings(&fx.base, CLIENT);
    s["displayName"] = json!("SSO Alfa Baru");
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, ""),
            Some(with_version(s, &json!(5))),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (&r.body["item"]["enabled"], &r.body["item"]["lastTestOk"]),
        (&json!(true), &json!(true))
    );

    // An IdP outage during a test is reported but does not cut sign-in.
    lock(&fx.stub).down = true;
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.alfa, "/test"),
            Some(json!({"version": 6})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.body["result"],
        json!({"ok": false, "stage": "discovery", "code": "http_503"})
    );
    assert_eq!(
        (
            &r.body["item"]["enabled"],
            &r.body["item"]["lastTestOk"],
            &r.body["item"]["version"]
        ),
        (&json!(true), &json!(true), &json!(6))
    );
    lock(&fx.stub).down = false;
    assert_eq!(session_status(&fx, &cookie).await, StatusCode::OK);

    // Moving the IdP (new client id) needs a new test: off, untested, sessions ended.
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, ""),
            Some(with_version(settings(&fx.base, "client-other"), &json!(6))),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (&r.body["item"]["enabled"], &r.body["item"]["lastTestOk"]),
        (&json!(false), &Value::Null)
    );
    assert_eq!(session_status(&fx, &cookie).await, StatusCode::UNAUTHORIZED);

    // Back, re-tested, on again; then turned off: the new session ends too.
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, ""),
            Some(with_version(settings(&fx.base, CLIENT), &json!(7))),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.alfa, "/test"),
            Some(json!({"version": 8})),
        )
        .await;
    assert_eq!(r.body["result"]["ok"], json!(true), "{}", r.text);
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/enabled"),
            Some(json!({"enabled": true, "version": 9})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let cookie = match sso_sign_in(
        &fx,
        "c2",
        json!({"sub": "sub-rina", "email": rina, "email_verified": true, "groups": ["g-cs"]}),
    )
    .await
    {
        Ok(c) => c,
        Err(l) => panic!("second sign-in failed: {l}"),
    };
    assert_eq!(session_status(&fx, &cookie).await, StatusCode::OK);
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.alfa, "/enabled"),
            Some(json!({"enabled": false, "version": 10})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(session_status(&fx, &cookie).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        sso_sign_in(&fx, "c3", json!({"sub": "sub-rina"})).await,
        Err("/?sso=not_configured".to_owned())
    );

    let actions: Vec<String> = ok(
        sqlx::query_scalar(
            "SELECT action FROM audit_events WHERE entity_type = 'identity_provider' \
             AND actor_id = $1 ORDER BY occurred_at, action",
        )
        .bind(fx.admin)
        .fetch_all(&fx.pool)
        .await,
        "audit",
    );
    for a in [
        "idp.create",
        "idp.secret.set",
        "idp.test",
        "idp.enable",
        "idp.update",
        "idp.disable",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} missing: {actions:?}");
    }
    let leaked: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE safe_change::text LIKE $1")
            .bind(format!("%{SECRET}%"))
            .fetch_one(&fx.pool)
            .await,
        "leak",
    );
    assert_eq!(leaked, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn access_validation_failed_tests_and_the_keyring(pool: PgPool) {
    let fx = fixture(pool).await;

    // A non-admin is refused and the refusal is recorded on the platform.
    let (plain, auth) = login(&fx.st, &fx.pool, "dummy:cs-alfa", false).await;
    for (method, uri, body) in [
        ("GET", "/api/v1/platform/sso".to_owned(), None),
        ("PUT", fx.sso(fx.alfa, ""), Some(settings(&fx.base, CLIENT))),
    ] {
        let r = api(&fx.st, &auth, method, &uri, body).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{method} {uri}: {}",
            r.text
        );
    }
    assert_eq!(
        count(
            &fx.pool,
            "SELECT count(*) FROM platform_audit_events WHERE actor_id = $1 \
             AND capability = 'platform.sso'",
            plain,
        )
        .await,
        2
    );

    // Meta tells the login screen whether SSO is offered.
    let r = call(&fx.st, "GET", "/api/v1/meta", None, None, Some("")).await;
    assert_eq!(r.body["sso"], json!(true), "{}", r.text);
    let password_only = state(&fx.pool, "password", true);
    let r = call(&password_only, "GET", "/api/v1/meta", None, None, Some("")).await;
    assert_eq!(r.body["sso"], json!(false), "{}", r.text);

    // Validation.
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut v = settings(&fx.base, CLIENT);
        f(&mut v);
        v
    };
    for (field, bad) in [
        (
            "issuer",
            mutate(&|v| v["issuer"] = json!("ftp://idp.example.test")),
        ),
        (
            "issuer",
            mutate(&|v| v["issuer"] = json!("https://idp.example.test/?tenant=1")),
        ),
        (
            "issuer",
            mutate(&|v| v["issuer"] = json!("https://u:p@idp.example.test")),
        ),
        ("clientId", mutate(&|v| v["clientId"] = json!("has space"))),
        ("displayName", mutate(&|v| v["displayName"] = json!(" "))),
        (
            "emailDomains",
            mutate(&|v| v["emailDomains"] = json!(["not a domain"])),
        ),
        (
            "extraHosts",
            mutate(&|v| v["extraHosts"] = json!(["-bad-.example.test"])),
        ),
        (
            "groupClaim",
            mutate(&|v| v["groupClaim"] = json!("group claim")),
        ),
        (
            "roleMapping",
            mutate(&|v| v["roleMapping"] = json!([{"group": "g", "role": "admin"}])),
        ),
        (
            "roleMapping",
            mutate(&|v| {
                v["roleMapping"] =
                    json!([{"group": "g", "role": "cs"}, {"group": "g", "role": "lead_cs"}])
            }),
        ),
    ] {
        let r = fx.api("PUT", &fx.sso(fx.alfa, ""), Some(bad)).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}: {}",
            r.text
        );
        assert_eq!(r.body["error"]["field"], json!(field), "{}", r.text);
    }
    let r = fx
        .api(
            "PUT",
            "/api/v1/platform/companies/not-a-uuid/sso",
            Some(settings(&fx.base, CLIENT)),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = fx
        .api(
            "PUT",
            &fx.sso(Uuid::new_v4(), ""),
            Some(settings(&fx.base, CLIENT)),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.alfa, "/test"),
            Some(json!({"version": 1})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "no IdP yet: {}", r.text);

    // A test that cannot reach the IdP is stored on an IdP that is off.
    let dead = "http://127.0.0.1:9";
    let r = fx
        .api("PUT", &fx.sso(fx.beta, ""), Some(settings(dead, CLIENT)))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = fx
        .api(
            "POST",
            &fx.sso(fx.beta, "/test"),
            Some(json!({"version": 1})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["result"]["ok"], json!(false));
    assert_eq!(r.body["result"]["stage"], json!("discovery"));
    assert!(r.body["result"]["code"].is_string(), "{}", r.text);
    assert_eq!(r.body["item"]["lastTestOk"], json!(false));
    assert_eq!(r.body["item"]["version"], json!(2));

    // Without the server keyring no secret can be stored.
    let no_keyring = state(&fx.pool, "both", false);
    let r = api(
        &no_keyring,
        &fx.auth,
        "PUT",
        &fx.sso(fx.beta, "/secret"),
        Some(json!({"secret": SECRET, "version": 2})),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.body["error"]["code"], "keyring_missing", "{}", r.text);
    let r = fx
        .api(
            "PUT",
            &fx.sso(fx.beta, "/secret"),
            Some(json!({"secret": "bad\u{7}secret", "version": 2})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn admin_links_and_unlinks_accounts_by_subject(pool: PgPool) {
    let fx = fixture(pool).await;
    let fx = &fx;
    ready(fx).await;
    let lead = id_of(
        &fx.pool,
        "SELECT id FROM users WHERE external_subject = 'dummy:lead-alfa'",
    )
    .await;
    let cs = id_of(
        &fx.pool,
        "SELECT id FROM users WHERE external_subject = 'dummy:cs-alfa'",
    )
    .await;
    let link = |user: Uuid, subject: &str| {
        let body = json!({"userId": user, "subject": subject});
        let uri = fx.sso(fx.alfa, "/identities");
        async move { fx.api("POST", &uri, Some(body)).await }
    };

    let r = link(lead, " entra-oid-1 ").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.body["item"]["subject"], json!("entra-oid-1"));
    assert_eq!(r.body["item"]["provisioned"], json!(false));
    let identity = r.body["item"]["id"].as_str().unwrap_or_default().to_owned();

    let r = link(cs, "entra-oid-1").await;
    assert_eq!(r.body["error"]["code"], "subject_linked", "{}", r.text);
    let r = link(lead, "entra-oid-2").await;
    assert_eq!(r.body["error"]["code"], "user_linked", "{}", r.text);
    // An account working for the other company, an inactive one and an unknown one.
    let beta_ws = id_of(&fx.pool, "SELECT id FROM workspaces WHERE code = 'DUMB'").await;
    let other: Uuid = ok(
        sqlx::query_scalar(
            "WITH u AS (INSERT INTO users (display_name) VALUES ('Beta Person') RETURNING id) \
             INSERT INTO workspace_memberships (workspace_id, user_id, role) \
             SELECT $1, id, 'cs' FROM u RETURNING user_id",
        )
        .bind(beta_ws)
        .fetch_one(&fx.pool)
        .await,
        "other",
    );
    let r = link(other, "entra-oid-3").await;
    assert_eq!(
        r.body["error"]["code"], "user_in_other_company",
        "{}",
        r.text
    );
    let gone: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO users (display_name, active) VALUES ('Gone Person', false) RETURNING id",
        )
        .fetch_one(&fx.pool)
        .await,
        "gone",
    );
    let r = link(gone, "entra-oid-4").await;
    assert_eq!(r.body["error"]["code"], "user_inactive", "{}", r.text);
    let r = link(Uuid::new_v4(), "entra-oid-5").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    let r = link(cs, "bad\nsubject").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);

    // The linked account signs in by subject alone (no verified address needed).
    let cookie = match sso_sign_in(
        fx,
        "c1",
        json!({"sub": "entra-oid-1", "groups": ["g-lead"]}),
    )
    .await
    {
        Ok(c) => c,
        Err(l) => panic!("linked sign-in failed: {l}"),
    };
    assert_eq!(session_status(fx, &cookie).await, StatusCode::OK);

    let list = |q: &str| {
        let uri = format!("{}?q={q}", fx.sso(fx.alfa, "/identities"));
        async move { fx.api("GET", &uri, None).await }
    };
    let r = list("lead").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["total"], json!(1));
    assert!(r.body["items"][0]["lastLoginAt"].is_string(), "{}", r.text);
    assert_eq!(list("zzz").await.body["total"], json!(0));
    assert_eq!(
        list("%25").await.body["total"],
        json!(0),
        "LIKE wildcards are literal"
    );
    assert_eq!(
        list(&"x".repeat(101)).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let r = fx.api("GET", &fx.sso(fx.beta, "/identities"), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no IdP: {}", r.text);

    // Unlinking ends the account's SSO session; the account stays.
    let uri = fx.sso(fx.alfa, &format!("/identities/{identity}"));
    let r = fx.api("DELETE", &uri, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(session_status(fx, &cookie).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        fx.api("DELETE", &uri, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        sso_sign_in(
            fx,
            "c2",
            json!({"sub": "entra-oid-1", "groups": ["g-lead"]})
        )
        .await,
        Err("/?sso=not_linked".to_owned())
    );
    for event in ["identity_link", "identity_unlink"] {
        let n: i64 = ok(
            sqlx::query_scalar(
                "SELECT count(*) FROM auth_events WHERE event = $1 AND actor_id = $2 \
                 AND user_id = $3",
            )
            .bind(event)
            .bind(fx.admin)
            .bind(lead)
            .fetch_one(&fx.pool)
            .await,
            "auth events",
        );
        assert_eq!(n, 1, "{event}");
    }
    let n = count(
        &fx.pool,
        "SELECT count(*) FROM audit_events WHERE actor_id = $1 \
         AND action IN ('identity.link', 'identity.unlink')",
        fx.admin,
    )
    .await;
    assert_eq!(n, 2);
}
