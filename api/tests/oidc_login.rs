//! CP5c company SSO end to end against a local stub IdP (axum on 127.0.0.1, ITSM_OIDC_LAB=on) and
//! a real PostgreSQL: start → IdP → callback, PKCE, single-use attempts bound to the browser,
//! ID token checks, just-in-time provisioning, role mapping and session lifetime. The signing key
//! is generated per test run; every name, address and secret is synthetic.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use itsm_api::crypto::{Purpose, store_secret};
use itsm_api::{AppState, app, oidc, seed};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

mod support;
use support::oidc_stub::*;

struct Fx {
    st: AppState,
    idp: Uuid,
    ws: Uuid,
    stub: Shared,
}

/// Seeds UAT data and an enabled IdP for workspace DUMA pointing at a fresh stub.
async fn fixture(pool: &PgPool) -> Fx {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    let ws: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM workspaces WHERE code = 'DUMA'")
            .fetch_one(pool)
            .await,
        "ws",
    );
    let admin: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:uat-superadmin'")
            .fetch_one(pool)
            .await,
        "admin",
    );
    let (stub, base) = stub().await;
    let st = state(pool, "both", true);
    let Some(keyring) = st.config.keyring.clone() else {
        panic!("keyring");
    };
    let mut conn = ok(pool.acquire().await, "conn");
    let secret = ok(
        store_secret(
            &mut conn,
            &keyring,
            Some(ws),
            Purpose::OidcClientSecret,
            SECRET.as_bytes(),
            Some(admin),
        )
        .await,
        "secret",
    );
    let idp: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO identity_providers (workspace_id, display_name, issuer, client_id, \
             client_secret_id, client_secret_updated_at, email_domains, role_mapping, \
             last_test_ok, last_tested_at, enabled, created_by, updated_by) \
             VALUES ($1, 'SSO Alfa', $2, $3, $4, now(), $5, $6, true, now(), true, $7, $7) \
             RETURNING id",
        )
        .bind(ws)
        .bind(&base)
        .bind(CLIENT)
        .bind(secret)
        .bind(vec![DOMAIN.to_owned()])
        .bind(json!([{"group": "g-lead", "role": "lead_cs"}, {"group": "g-cs", "role": "cs"}]))
        .bind(admin)
        .fetch_one(&mut *conn)
        .await,
        "idp",
    );
    Fx { st, idp, ws, stub }
}

struct Resp {
    status: StatusCode,
    location: String,
    cookies: Vec<String>,
}

impl Resp {
    /// `name=value` of a Set-Cookie header (empty when cleared or absent).
    fn cookie(&self, name: &str) -> Option<String> {
        self.cookies
            .iter()
            .find(|c| c.starts_with(&format!("{name}=")))
            .and_then(|c| c.split(';').next())
            .map(str::to_owned)
    }
}

async fn get_req(st: &AppState, uri: &str, cookie: Option<&str>) -> Resp {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let req = ok(b.body(Body::empty()), "request");
    let res = ok(app(st.clone()).oneshot(req).await, "router");
    let h = res.headers();
    Resp {
        status: res.status(),
        location: h
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        cookies: h
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(str::to_owned)
            .collect(),
    }
}

/// A started sign-in as the browser sees it after the IdP redirect.
struct Started {
    state: String,
    bind: String,
    params: HashMap<String, String>,
}

async fn start(fx: &Fx) -> Started {
    let r = get_req(&fx.st, "/api/v1/auth/oidc/duma/start", None).await;
    assert_eq!(r.status, StatusCode::FOUND, "{}", r.location);
    let url = ok(reqwest::Url::parse(&r.location), "authorize url");
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let Some(bind) = r.cookie("__Host-itsm_oidc") else {
        panic!("binding cookie missing: {:?}", r.cookies);
    };
    Started {
        state: params.get("state").cloned().unwrap_or_default(),
        bind,
        params,
    }
}

/// The IdP authenticates `claims` and redirects back with `code`.
fn approve(fx: &Fx, s: &Started, code: &str, claims: Value) {
    approve_with(fx, s, code, claims, None, false);
}

fn approve_with(fx: &Fx, s: &Started, code: &str, claims: Value, status: Option<u16>, rogue: bool) {
    let g = Grant {
        challenge: s.params.get("code_challenge").cloned().unwrap_or_default(),
        nonce: s.params.get("nonce").cloned().unwrap_or_default(),
        claims,
        status,
        rogue,
    };
    lock(&fx.stub).grants.insert(code.to_owned(), g);
}

async fn callback(fx: &Fx, s: &Started, code: &str) -> Resp {
    let uri = format!("/api/v1/auth/oidc/callback?code={code}&state={}", s.state);
    get_req(&fx.st, &uri, Some(&s.bind)).await
}

/// Full sign-in for `claims`; returns the callback response.
async fn sign_in(fx: &Fx, code: &str, claims: Value) -> Resp {
    let s = start(fx).await;
    approve(fx, &s, code, claims);
    callback(fx, &s, code).await
}

fn person(sub: &str, email: &str, groups: &[&str]) -> Value {
    json!({"sub": sub, "email": email, "email_verified": true, "name": "Rina Alfa",
           "groups": groups})
}

async fn session_status(st: &AppState, r: &Resp) -> StatusCode {
    let Some(cookie) = r.cookie("__Host-itsm_session") else {
        panic!("no session cookie: {:?}", r.cookies);
    };
    get_req(st, "/api/v1/session", Some(&cookie)).await.status
}

async fn count(pool: &PgPool, sql: &str, bind: Uuid) -> i64 {
    ok(
        sqlx::query_scalar(sql).bind(bind).fetch_one(pool).await,
        sql,
    )
}

async fn audit_codes(pool: &PgPool) -> Vec<String> {
    ok(
        sqlx::query_scalar(
            "SELECT outcome || ':' || coalesce(failure_code, '-') FROM auth_events \
             WHERE event = 'oidc_login' ORDER BY occurred_at",
        )
        .fetch_all(pool)
        .await,
        "audit",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn sign_in_provisions_maps_the_role_and_opens_a_session(pool: PgPool) {
    let fx = fixture(&pool).await;
    let s = start(&fx).await;
    // PKCE S256, state and nonce from the CSPRNG; the authorization endpoint's own query is kept.
    for (k, v) in [
        ("response_type", "code"),
        ("client_id", CLIENT),
        ("scope", "openid email profile"),
        ("code_challenge_method", "S256"),
        ("prompt", "login"),
        (
            "redirect_uri",
            "https://itsm.example.test/api/v1/auth/oidc/callback",
        ),
    ] {
        assert_eq!(s.params.get(k).map(String::as_str), Some(v), "{k}");
    }
    assert_eq!(s.state.len(), 43);
    assert_eq!(s.params.get("nonce").map(String::len), Some(43));
    // Only digests of state and nonce are stored; the verifier is sealed.
    let stored: (Vec<u8>, Vec<u8>) = ok(
        sqlx::query_as("SELECT state_hash, verifier_ct FROM oidc_login_attempts")
            .fetch_one(&pool)
            .await,
        "attempt",
    );
    assert_eq!(stored.0, oidc::sha256(s.state.as_bytes()).to_vec());
    assert!(!String::from_utf8_lossy(&stored.1).contains(&s.state));

    approve(
        &fx,
        &s,
        "code-1",
        person("sub-rina", "Rina@Alfa.example.test", &["g-cs", "g-lead"]),
    );
    let r = callback(&fx, &s, "code-1").await;
    assert_eq!((r.status, r.location.as_str()), (StatusCode::FOUND, "/"));
    assert_eq!(
        r.cookie("__Host-itsm_oidc").as_deref(),
        Some("__Host-itsm_oidc=")
    );
    assert_eq!(session_status(&fx.st, &r).await, StatusCode::OK);

    let (user, email, name): (Uuid, String, String) = ok(
        sqlx::query_as(
            "SELECT u.id, u.email, u.display_name FROM users u \
             JOIN external_identities e ON e.user_id = u.id \
             WHERE e.idp_id = $1 AND e.subject = 'sub-rina' AND e.linked_by IS NULL",
        )
        .bind(fx.idp)
        .fetch_one(&pool)
        .await,
        "provisioned user",
    );
    assert_eq!(
        (email.as_str(), name.as_str()),
        ("rina@alfa.example.test", "Rina Alfa")
    );
    let role: String = ok(
        sqlx::query_scalar(
            "SELECT role FROM workspace_memberships WHERE workspace_id = $1 AND user_id = $2 AND active",
        )
        .bind(fx.ws)
        .bind(user)
        .fetch_one(&pool)
        .await,
        "membership",
    );
    assert_eq!(role, "lead_cs");
    let sql = "SELECT count(*) FROM sessions WHERE user_id = $1 AND auth_method = 'oidc' \
               AND idp_id IS NOT NULL AND credential_version IS NULL";
    assert_eq!(count(&pool, sql, user).await, 1);
    let sql = "SELECT count(*) FROM auth_events WHERE user_id = $1 AND event = 'oidc_provision'";
    assert_eq!(count(&pool, sql, user).await, 1);
    let sql = "SELECT count(*) FROM audit_events WHERE workspace_id = $1 \
               AND action = 'membership.add' AND actor_id IS NULL \
               AND safe_change->>'source' = 'oidc'";
    assert_eq!(count(&pool, sql, fx.ws).await, 1);
    assert_eq!(audit_codes(&pool).await, ["success:-"]);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM oidc_login_attempts WHERE idp_id = $1",
            fx.idp
        )
        .await,
        0
    );

    // Next sign-in: same user, name follows the IdP; without sync_roles the role stays.
    let mut again = person("sub-rina", "rina@alfa.example.test", &["g-cs"]);
    again["name"] = json!("Rina A. Alfa");
    let r = sign_in(&fx, "code-2", again.clone()).await;
    assert_eq!(r.location, "/");
    let sql = "SELECT count(*) FROM users WHERE email = 'rina@alfa.example.test' \
               AND display_name = 'Rina A. Alfa' AND id = $1";
    assert_eq!(count(&pool, sql, user).await, 1);
    let sql = "SELECT count(*) FROM workspace_memberships WHERE user_id = $1 AND role = 'lead_cs'";
    assert_eq!(count(&pool, sql, user).await, 1);

    // With sync_roles the mapped role wins, audited as a membership update.
    ok(
        sqlx::query(
            "UPDATE identity_providers SET sync_roles = true, version = version + 1 WHERE id = $1",
        )
        .bind(fx.idp)
        .execute(&pool)
        .await,
        "sync on",
    );
    let r = sign_in(&fx, "code-3", again).await;
    assert_eq!(r.location, "/");
    let sql = "SELECT count(*) FROM workspace_memberships WHERE user_id = $1 AND role = 'cs'";
    assert_eq!(count(&pool, sql, user).await, 1);
    let sql = "SELECT count(*) FROM audit_events WHERE workspace_id = $1 \
               AND action = 'membership.update' AND safe_change->>'source' = 'oidc'";
    assert_eq!(count(&pool, sql, fx.ws).await, 1);
    // The cached discovery and keys served all three sign-ins.
    assert_eq!(lock(&fx.stub).jwks_calls, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn attempts_are_single_use_bound_to_the_browser_and_expire(pool: PgPool) {
    let fx = fixture(&pool).await;
    let claims = person("sub-1", "satu@alfa.example.test", &["g-cs"]);

    // Replay of a finished callback.
    let s = start(&fx).await;
    approve(&fx, &s, "c-1", claims.clone());
    assert_eq!(callback(&fx, &s, "c-1").await.location, "/");
    approve(&fx, &s, "c-1", claims.clone());
    assert_eq!(callback(&fx, &s, "c-1").await.location, "/?sso=expired");

    // Callback in another browser (no or different binding cookie): refused, attempt kept.
    let s = start(&fx).await;
    approve(&fx, &s, "c-2", claims.clone());
    let uri = format!("/api/v1/auth/oidc/callback?code=c-2&state={}", s.state);
    let r = get_req(&fx.st, &uri, None).await;
    assert_eq!(r.location, "/?sso=expired");
    let r = get_req(&fx.st, &uri, Some("__Host-itsm_oidc=forged")).await;
    assert_eq!(r.location, "/?sso=expired");
    assert_eq!(callback(&fx, &s, "c-2").await.location, "/");

    // Expired attempt.
    let s = start(&fx).await;
    approve(&fx, &s, "c-3", claims.clone());
    ok(
        sqlx::query(
            "UPDATE oidc_login_attempts SET created_at = now() - interval '20 minutes', \
             expires_at = now() - interval '10 minutes'",
        )
        .execute(&pool)
        .await,
        "age attempt",
    );
    assert_eq!(callback(&fx, &s, "c-3").await.location, "/?sso=expired");

    // The IdP refused (user cancelled): the attempt is spent and the IdP text never shown.
    let s = start(&fx).await;
    let uri = format!(
        "/api/v1/auth/oidc/callback?error=access_denied&error_description=%3Cb%3Ex&state={}",
        s.state
    );
    let r = get_req(&fx.st, &uri, Some(&s.bind)).await;
    assert_eq!(r.location, "/?sso=idp_error");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM oidc_login_attempts WHERE idp_id = $1",
            fx.idp
        )
        .await,
        0
    );

    assert_eq!(
        audit_codes(&pool).await,
        [
            "success:-",
            "denied:unknown_state",
            "denied:browser_mismatch",
            "denied:browser_mismatch",
            "success:-",
            "denied:attempt_expired",
            "failed:idp_error",
        ]
    );
    // Refused callbacks never reach the token endpoint: only the two sign-ins exchanged a code.
    assert_eq!(lock(&fx.stub).token_calls, 2);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn bad_tokens_and_token_endpoint_errors_are_denied(pool: PgPool) {
    let fx = fixture(&pool).await;
    let claims = person("sub-2", "dua@alfa.example.test", &["g-cs"]);

    // Nonce of another attempt.
    let s = start(&fx).await;
    approve(&fx, &s, "n-1", claims.clone());
    if let Some(g) = lock(&fx.stub).grants.get_mut("n-1") {
        g.nonce = "other-nonce".into();
    }
    assert_eq!(callback(&fx, &s, "n-1").await.location, "/?sso=denied");

    // Wrong audience, expired token, foreign issuer.
    for (code, extra) in [
        ("n-2", json!({"aud": "client-other"})),
        ("n-3", json!({"exp": now() - 3600})),
        ("n-4", json!({"iss": "https://evil.example.test"})),
    ] {
        let mut c = claims.clone();
        if let (Some(o), Some(e)) = (c.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        let s = start(&fx).await;
        approve(&fx, &s, code, c);
        assert_eq!(
            callback(&fx, &s, code).await.location,
            "/?sso=denied",
            "{code}"
        );
    }

    // Signed by a key the IdP never published (refresh refused right after the first fetch).
    let s = start(&fx).await;
    approve_with(&fx, &s, "n-5", claims.clone(), None, true);
    assert_eq!(callback(&fx, &s, "n-5").await.location, "/?sso=denied");

    // Token endpoint: 4xx = denied, 5xx = IdP unavailable.
    let s = start(&fx).await;
    approve_with(&fx, &s, "n-6", claims.clone(), Some(400), false);
    assert_eq!(callback(&fx, &s, "n-6").await.location, "/?sso=denied");
    let s = start(&fx).await;
    approve_with(&fx, &s, "n-7", claims.clone(), Some(503), false);
    assert_eq!(
        callback(&fx, &s, "n-7").await.location,
        "/?sso=idp_unavailable"
    );

    assert_eq!(
        audit_codes(&pool).await,
        [
            "denied:token_nonce",
            "denied:token_audience",
            "denied:token_expired",
            "denied:token_issuer",
            "denied:token_unknown_key",
            "failed:token_rejected",
            "failed:token_endpoint",
        ]
    );
    let sql = "SELECT count(*) FROM external_identities WHERE idp_id = $1";
    assert_eq!(count(&pool, sql, fx.idp).await, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn provisioning_needs_a_verified_allowed_unused_address(pool: PgPool) {
    let fx = fixture(&pool).await;
    let lead: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:lead-alfa'")
            .fetch_one(&pool)
            .await,
        "lead",
    );
    ok(
        sqlx::query("UPDATE users SET email = 'lead@alfa.example.test' WHERE id = $1")
            .bind(lead)
            .execute(&pool)
            .await,
        "email",
    );

    let mut unverified = person("sub-a", "a@alfa.example.test", &["g-cs"]);
    unverified["email_verified"] = json!(false);
    for (code, claims, expected) in [
        (
            "p-1",
            person("sub-b", "b@other.example.test", &["g-cs"]),
            "not_linked",
        ),
        ("p-2", unverified, "not_linked"),
        (
            "p-3",
            json!({"sub": "sub-c", "groups": ["g-cs"]}),
            "not_linked",
        ),
        // Same address as a local account: never taken over by address.
        (
            "p-4",
            person("sub-d", "LEAD@alfa.example.test", &["g-lead"]),
            "email_in_use",
        ),
    ] {
        let r = sign_in(&fx, code, claims).await;
        assert_eq!(r.location, format!("/?sso={expected}"), "{code}");
        assert!(r.cookie("__Host-itsm_session").is_none(), "{code}");
    }
    let sql = "SELECT count(*) FROM external_identities WHERE idp_id = $1";
    assert_eq!(count(&pool, sql, fx.idp).await, 0);

    // An admin-linked local account signs in through its subject, whatever the address.
    ok(
        sqlx::query(
            "INSERT INTO external_identities (user_id, idp_id, subject, linked_by) \
             VALUES ($1, $2, 'sub-lead', $1)",
        )
        .bind(lead)
        .bind(fx.idp)
        .execute(&pool)
        .await,
        "link",
    );
    let r = sign_in(
        &fx,
        "p-5",
        person("sub-lead", "x@elsewhere.example.test", &[]),
    )
    .await;
    assert_eq!(r.location, "/");
    assert_eq!(session_status(&fx.st, &r).await, StatusCode::OK);

    // No mapped group: signed in, but no membership (the web shows "menunggu akses").
    let r = sign_in(
        &fx,
        "p-6",
        person("sub-e", "e@alfa.example.test", &["g-unknown"]),
    )
    .await;
    assert_eq!(r.location, "/");
    let sql = "SELECT count(*) FROM workspace_memberships m JOIN external_identities e \
               ON e.user_id = m.user_id WHERE e.idp_id = $1 AND e.subject = 'sub-e'";
    assert_eq!(count(&pool, sql, fx.idp).await, 0);

    // A deactivated user is refused.
    ok(
        sqlx::query("UPDATE users SET active = false, deactivated_at = now() WHERE id = $1")
            .bind(lead)
            .execute(&pool)
            .await,
        "deactivate",
    );
    let r = sign_in(
        &fx,
        "p-7",
        person("sub-lead", "x@elsewhere.example.test", &[]),
    )
    .await;
    assert_eq!(r.location, "/?sso=inactive");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn disabled_idp_or_password_only_ends_sso_sessions(pool: PgPool) {
    let fx = fixture(&pool).await;
    let r = sign_in(
        &fx,
        "d-1",
        person("sub-z", "z@alfa.example.test", &["g-cs"]),
    )
    .await;
    assert_eq!(session_status(&fx.st, &r).await, StatusCode::OK);

    // ITSM_AUTH=password: routes are gone and live OIDC sessions stop working.
    let pw = state(&pool, "password", true);
    assert_eq!(session_status(&pw, &r).await, StatusCode::UNAUTHORIZED);
    for uri in [
        "/api/v1/auth/oidc/duma/start",
        "/api/v1/auth/oidc/callback?state=x",
    ] {
        assert_eq!(
            get_req(&pw, uri, None).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    assert_eq!(session_status(&fx.st, &r).await, StatusCode::OK);

    // Disabling the IdP ends its sessions and its sign-in.
    ok(
        sqlx::query(
            "UPDATE identity_providers SET enabled = false, version = version + 1 WHERE id = $1",
        )
        .bind(fx.idp)
        .execute(&pool)
        .await,
        "disable",
    );
    assert_eq!(session_status(&fx.st, &r).await, StatusCode::UNAUTHORIZED);
    let r = get_req(&fx.st, "/api/v1/auth/oidc/DUMA/start", None).await;
    assert_eq!(r.location, "/?sso=not_configured");

    // Unknown company and a server without keyring never reach an IdP.
    for code in ["ZZZZ", "x", "bad-code!"] {
        let r = get_req(&fx.st, &format!("/api/v1/auth/oidc/{code}/start"), None).await;
        assert_eq!(r.location, "/?sso=not_configured", "{code}");
    }
    ok(
        sqlx::query(
            "UPDATE identity_providers SET enabled = true, version = version + 1 WHERE id = $1",
        )
        .bind(fx.idp)
        .execute(&pool)
        .await,
        "enable",
    );
    let no_keyring = state(&pool, "both", false);
    let r = get_req(&no_keyring, "/api/v1/auth/oidc/DUMA/start", None).await;
    assert_eq!(r.location, "/?sso=unavailable");
}
