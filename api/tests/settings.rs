//! D09 settings administration against a real PostgreSQL: default deny for CS and Lead CS
//! without explicit grants (403) and for foreign workspaces (404), optimistic versions (409)
//! with the audit row written in the same transaction, member administration bounded by the
//! grantor's own rights (no self-administration, deactivation revokes grants and ends access),
//! catalog rules (no priority create, unique codes, no hard delete), superadmin-only company
//! creation without implicit memberships, and the simulated connection lifecycle
//! save → test → activate / disable. Fixtures are synthetic and labelled dummy.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
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

fn state(pool: &PgPool, mode: &str) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", mode),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        // Password mode refuses insecure cookies.
        (
            "ITSM_COOKIE_SECURE",
            if mode == "password" { "true" } else { "false" },
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    AppState {
        config: Arc::new(ok(Config::from_map(&vars), "test config")),
        pool: pool.clone(),
    }
}

struct Fx {
    a: Uuid,
    b: Uuid,
    admin: Uuid,
    cs: Uuid,
    lead: Uuid,
    bravo: Uuid,
}

async fn scalar_uuid(pool: &PgPool, sql: &str, key: &str) -> Uuid {
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
        a: scalar_uuid(pool, ws, "DUMA").await,
        b: scalar_uuid(pool, ws, "DUMB").await,
        admin: scalar_uuid(pool, user, "dummy:uat-superadmin").await,
        cs: scalar_uuid(pool, user, "dummy:cs-alfa").await,
        lead: scalar_uuid(pool, user, "dummy:lead-alfa").await,
        bravo: scalar_uuid(pool, user, "dummy:cs-bravo").await,
    }
}

#[derive(Debug)]
struct Resp {
    status: StatusCode,
    json: Value,
}

struct Tok {
    token: String,
    csrf: String,
    /// Access mode of the session and of the app it is sent to.
    mode: &'static str,
}

async fn tok(pool: &PgPool, uid: Uuid) -> Tok {
    tok_in(pool, uid, "uat_superadmin").await
}

async fn tok_in(pool: &PgPool, uid: Uuid, mode: &'static str) -> Tok {
    let s = ok(session::issue(pool, uid, mode).await, "issue");
    Tok {
        token: s.token,
        csrf: s.csrf_token,
        mode,
    }
}

async fn send(
    pool: &PgPool,
    method: &str,
    uri: &str,
    t: &Tok,
    csrf: bool,
    body: Option<&Value>,
) -> Resp {
    let mut r = Request::builder().method(method).uri(uri).header(
        header::COOKIE,
        format!("{}={}", session::cookie_name(t.mode == "password"), t.token),
    );
    if csrf {
        r = r.header(session::CSRF_HEADER, &t.csrf);
    }
    let req = match body {
        Some(b) => ok(
            r.header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            "request",
        ),
        None => ok(r.body(Body::empty()), "request"),
    };
    let res = match app(state(pool, t.mode)).oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn ws_uri(ws: Uuid, rest: &str) -> String {
    format!("/api/v1/workspaces/{ws}/{rest}")
}

async fn get(pool: &PgPool, uri: &str, t: &Tok) -> Resp {
    send(pool, "GET", uri, t, false, None).await
}

async fn write(pool: &PgPool, method: &str, uri: &str, t: &Tok, body: Value) -> Resp {
    send(pool, method, uri, t, true, Some(&body)).await
}

fn s(v: &Value) -> String {
    match v.as_str() {
        Some(x) => x.to_owned(),
        None => panic!("not a string: {v}"),
    }
}

async fn membership(pool: &PgPool, ws: Uuid, user: Uuid) -> Uuid {
    ok(
        sqlx::query_scalar(
            "SELECT id FROM workspace_memberships WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(ws)
        .bind(user)
        .fetch_one(pool)
        .await,
        "membership",
    )
}

async fn grant(pool: &PgPool, ws: Uuid, user: Uuid, code: &str, by: Uuid) {
    let m = membership(pool, ws, user).await;
    ok(
        sqlx::query(
            "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(ws)
        .bind(m)
        .bind(code)
        .bind(by)
        .execute(pool)
        .await,
        "grant",
    );
}

async fn audit_count(pool: &PgPool, ws: Uuid, action: &str) -> i64 {
    ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE workspace_id = $1 AND action = $2",
        )
        .bind(ws)
        .bind(action)
        .fetch_one(pool)
        .await,
        "audit count",
    )
}

async fn audit_exists(pool: &PgPool, id: &Value, action: &str, actor: Uuid) {
    let id = ok(Uuid::parse_str(&s(id)), "audit id");
    let n: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE id = $1 AND action = $2 AND actor_id = $3",
        )
        .bind(id)
        .bind(action)
        .bind(actor)
        .fetch_one(pool)
        .await,
        "audit row",
    );
    assert_eq!(n, 1, "audit {action} committed with the change");
}

fn member(list: &Value, user: Uuid) -> &Value {
    match list["items"]
        .as_array()
        .and_then(|a| a.iter().find(|m| m["userId"] == user.to_string()))
    {
        Some(m) => m,
        None => panic!("no member {user} in {list}"),
    }
}

fn connection<'a>(list: &'a Value, provider: &str) -> &'a Value {
    match list["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["provider"] == provider))
    {
        Some(c) => c,
        None => panic!("no {provider} in {list}"),
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cs_and_lead_without_grants_are_denied_everywhere(pool: PgPool) {
    let f = fixture(&pool).await;
    let lead_m = membership(&pool, f.a, f.lead).await;
    let cs_m = membership(&pool, f.a, f.cs).await;
    let category = ok(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM categories WHERE workspace_id = $1 ORDER BY code LIMIT 1",
        )
        .bind(f.a)
        .fetch_one(&pool)
        .await,
        "category",
    );
    let jira = ok(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM integration_connections WHERE workspace_id = $1 AND provider = 'jira'",
        )
        .bind(f.a)
        .fetch_one(&pool)
        .await,
        "jira",
    );
    for (who, other_m) in [(f.cs, lead_m), (f.lead, cs_m)] {
        let t = tok(&pool, who).await;
        // Reads that every member has: read-only shape, no grants exposed.
        let r = get(&pool, &ws_uri(f.a, "settings"), &t).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(r.json["canEdit"], false);
        let r = get(&pool, &ws_uri(f.a, "members"), &t).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(r.json["canManage"], false);
        assert_eq!(r.json["grantable"], json!([]));
        assert!(
            r.json["items"]
                .as_array()
                .is_some_and(|a| a.iter().all(|m| m["grants"] == json!([]))),
            "grants hidden without admin.members: {}",
            r.json
        );
        let r = get(&pool, &ws_uri(f.a, "catalogs/categories"), &t).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(r.json["canEdit"], false);

        // Every administration call: 403.
        let denied = [
            ("GET", ws_uri(f.a, "integrations"), Value::Null),
            (
                "PATCH",
                ws_uri(f.a, "settings"),
                json!({"workspaceName": "X", "companyName": "Y", "timezone": "Asia/Jakarta",
                       "reporterCode": null, "version": 1}),
            ),
            (
                "POST",
                ws_uri(f.a, "catalogs/categories"),
                json!({"code": "baru", "label": "Baru (dummy)", "sortOrder": 1}),
            ),
            (
                "PATCH",
                ws_uri(f.a, &format!("catalogs/categories/{category}")),
                json!({"label": "Ubah", "active": false, "sortOrder": 1, "version": 1}),
            ),
            (
                "POST",
                ws_uri(f.a, "members"),
                json!({"subject": "dummy:cs-bravo", "role": "cs"}),
            ),
            (
                "PATCH",
                ws_uri(f.a, &format!("members/{other_m}")),
                json!({"role": "cs", "active": false, "version": 1}),
            ),
            (
                "POST",
                ws_uri(f.a, &format!("members/{other_m}/grants")),
                json!({"grantCode": "admin.members", "version": 1}),
            ),
            (
                "POST",
                ws_uri(f.a, &format!("members/{other_m}/grants/revoke")),
                json!({"grantCode": "admin.members", "version": 1}),
            ),
            (
                "POST",
                ws_uri(f.a, "integrations"),
                json!({"provider": "jira", "displayName": "Jira"}),
            ),
            (
                "POST",
                ws_uri(f.a, &format!("integrations/{jira}/disable")),
                json!({"version": 1}),
            ),
            ("GET", "/api/v1/companies".to_owned(), Value::Null),
            (
                "POST",
                "/api/v1/companies".to_owned(),
                json!({"code": "ZZ1", "name": "PT Dummy", "timezone": "Asia/Jakarta"}),
            ),
            (
                "PATCH",
                format!("/api/v1/companies/{}", f.a),
                json!({"active": false, "version": 1}),
            ),
        ];
        for (method, uri, body) in &denied {
            let r = if method == &"GET" {
                get(&pool, uri, &t).await
            } else {
                write(&pool, method, uri, &t, body.clone()).await
            };
            assert_eq!(
                r.status,
                StatusCode::FORBIDDEN,
                "{method} {uri}: {}",
                r.json
            );
        }
        // Foreign workspace: not even visible.
        let r = get(&pool, &ws_uri(f.b, "settings"), &t).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
        let r = get(&pool, &ws_uri(f.b, "members"), &t).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    }
    let n: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action LIKE ANY ($1)")
            .bind(vec![
                "settings.%",
                "catalog.%",
                "membership.%",
                "grant.%",
                "integration.%",
                "company.%",
            ])
            .fetch_one(&pool)
            .await,
        "audits",
    );
    assert_eq!(n, 0, "denied calls change nothing");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn settings_update_is_versioned_and_audited_atomically(pool: PgPool) {
    let f = fixture(&pool).await;
    grant(&pool, f.a, f.lead, "admin.settings", f.admin).await;
    let t = tok(&pool, f.lead).await;
    let r = get(&pool, &ws_uri(f.a, "settings"), &t).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["canEdit"], true);
    let v = r.json["settings"]["version"].clone();
    let body = json!({"workspaceName": "  Ruang Uji Alfa (dummy) ", "companyName": "PT Uji Alfa (dummy)",
                      "timezone": "Asia/Makassar", "reporterCode": "LPR-UJI", "version": v});
    let r = write(&pool, "PATCH", &ws_uri(f.a, "settings"), &t, body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        r.json["settings"]["workspaceName"],
        "Ruang Uji Alfa (dummy)"
    );
    assert_eq!(r.json["settings"]["timezone"], "Asia/Makassar");
    audit_exists(&pool, &r.json["auditEventId"], "settings.update", f.lead).await;

    // Stale version: 409, nothing written.
    let before = audit_count(&pool, f.a, "settings.update").await;
    let mut stale = body.clone();
    stale["workspaceName"] = json!("Tidak tersimpan");
    let r = write(&pool, "PATCH", &ws_uri(f.a, "settings"), &t, stale).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(audit_count(&pool, f.a, "settings.update").await, before);

    // Reload in a new session: persisted.
    let t2 = tok(&pool, f.lead).await;
    let r = get(&pool, &ws_uri(f.a, "settings"), &t2).await;
    assert_eq!(
        r.json["settings"]["workspaceName"],
        "Ruang Uji Alfa (dummy)"
    );
    assert_eq!(r.json["settings"]["reporterCode"], "LPR-UJI");
    let v2 = r.json["settings"]["version"].clone();

    // No-op: no version bump, no audit.
    let mut same = body.clone();
    same["version"] = v2.clone();
    let r = write(&pool, "PATCH", &ws_uri(f.a, "settings"), &t2, same).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["auditEventId"], Value::Null);
    assert_eq!(r.json["settings"]["version"], v2);

    // Unsupported timezone / empty name: 422 with field.
    let mut bad = body.clone();
    bad["version"] = v2.clone();
    bad["timezone"] = json!("Europe/Berlin");
    let r = write(&pool, "PATCH", &ws_uri(f.a, "settings"), &t2, bad).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(r.json["error"]["field"], "timezone");

    // Grant is per workspace: still 404 elsewhere.
    let r = get(&pool, &ws_uri(f.b, "settings"), &t2).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn member_admin_is_bounded_by_own_rights_and_deactivation_ends_access(pool: PgPool) {
    let f = fixture(&pool).await;
    grant(&pool, f.a, f.lead, "admin.members", f.admin).await;
    let lead = tok(&pool, f.lead).await;
    let lead_m = membership(&pool, f.a, f.lead).await;

    let r = get(&pool, &ws_uri(f.a, "members"), &lead).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["canManage"], true);
    assert_eq!(r.json["grantable"], json!(["admin.members"]));
    assert_eq!(r.json["myMembershipId"], lead_m.to_string());
    assert_eq!(member(&r.json, f.lead)["grants"], json!(["admin.members"]));

    // Add by exact subject; unknown subject and duplicates are a generic 422.
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, "members"),
        &lead,
        json!({"subject": "dummy:cs-bravo", "role": "cs"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "membership.add", f.lead).await;
    let bravo_m = ok(Uuid::parse_str(&s(&r.json["item"]["membershipId"])), "id");
    for subject in ["dummy:cs-bravo", "dummy:tidak-ada"] {
        let r = write(
            &pool,
            "POST",
            &ws_uri(f.a, "members"),
            &lead,
            json!({"subject": subject, "role": "cs"}),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
        assert_eq!(r.json["error"]["field"], "subject");
    }
    let bravo = tok(&pool, f.bravo).await;
    let r = get(&pool, &ws_uri(f.a, "settings"), &bravo).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "new member has access: {}",
        r.json
    );

    // Self-administration and grants beyond own rights: 403.
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.a, &format!("members/{lead_m}")),
        &lead,
        json!({"role": "cs", "active": true, "version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    let list = get(&pool, &ws_uri(f.a, "members"), &lead).await.json;
    let v = member(&list, f.bravo)["version"].clone();
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, &format!("members/{bravo_m}/grants")),
        &lead,
        json!({"grantCode": "admin.settings", "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, &format!("members/{bravo_m}/grants")),
        &lead,
        json!({"grantCode": "ticket.read", "version": v}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "role permission is not a grant: {}",
        r.json
    );

    // Grant within own rights, then stale version → 409.
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, &format!("members/{bravo_m}/grants")),
        &lead,
        json!({"grantCode": "admin.members", "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "grant.add", f.lead).await;
    assert_eq!(r.json["item"]["grants"], json!(["admin.members"]));
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, &format!("members/{bravo_m}/grants/revoke")),
        &lead,
        json!({"grantCode": "admin.members", "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let r2 = get(&pool, &ws_uri(f.a, "members"), &bravo).await;
    assert_eq!(r2.json["canManage"], true, "grant effective immediately");

    // Deactivation revokes live grants in the same transaction and ends access.
    let v = member(
        &get(&pool, &ws_uri(f.a, "members"), &lead).await.json,
        f.bravo,
    )["version"]
        .clone();
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.a, &format!("members/{bravo_m}")),
        &lead,
        json!({"role": "cs", "active": false, "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    audit_exists(
        &pool,
        &r.json["auditEventId"],
        "membership.deactivate",
        f.lead,
    )
    .await;
    assert_eq!(r.json["item"]["active"], false);
    assert_eq!(r.json["item"]["grants"], json!([]));
    let live: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM workspace_grants WHERE membership_id = $1 AND revoked_at IS NULL",
        )
        .bind(bravo_m)
        .fetch_one(&pool)
        .await,
        "live grants",
    );
    assert_eq!(live, 0);
    let r = get(&pool, &ws_uri(f.a, "settings"), &bravo).await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "deactivated member loses access"
    );
    // Granting to an inactive member is refused.
    let v = member(
        &get(&pool, &ws_uri(f.a, "members"), &lead).await.json,
        f.bravo,
    )["version"]
        .clone();
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, &format!("members/{bravo_m}/grants")),
        &lead,
        json!({"grantCode": "admin.members", "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);

    // Foreign membership id: 404.
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.b, &format!("members/{bravo_m}")),
        &lead,
        json!({"role": "cs", "active": true, "version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn catalog_editor_rules(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = tok(&pool, f.admin).await;
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, "catalogs/categories"),
        &t,
        json!({"code": "uji_baru", "label": "Kategori uji (dummy)", "sortOrder": 50}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "catalog.create", f.admin).await;
    let id = s(&r.json["item"]["id"]);
    let v = r.json["item"]["version"].clone();

    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, "catalogs/categories"),
        &t,
        json!({"code": "uji_baru", "label": "Duplikat", "sortOrder": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(r.json["error"]["field"], "code");
    // Same code in another workspace is fine (codes are per workspace).
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.b, "catalogs/categories"),
        &t,
        json!({"code": "uji_baru", "label": "Kategori uji B (dummy)", "sortOrder": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    for bad in ["Uji Baru", "", "a-b"] {
        let r = write(
            &pool,
            "POST",
            &ws_uri(f.a, "catalogs/products"),
            &t,
            json!({"code": bad, "label": "X", "sortOrder": 1}),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.json
        );
    }
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, "catalogs/priorities"),
        &t,
        json!({"code": "kritis", "label": "Kritis", "sortOrder": 9}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "priorities are fixed: {}",
        r.json
    );
    let r = get(&pool, &ws_uri(f.a, "catalogs/unknown"), &t).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Deactivate (no hard delete), stale → 409, foreign workspace → 404.
    let uri = ws_uri(f.a, &format!("catalogs/categories/{id}"));
    let r = write(
        &pool,
        "PATCH",
        &uri,
        &t,
        json!({"label": "Kategori uji (dummy)", "active": false, "sortOrder": 50, "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "catalog.update", f.admin).await;
    assert_eq!(r.json["item"]["active"], false);
    let r = write(
        &pool,
        "PATCH",
        &uri,
        &t,
        json!({"label": "Lagi", "active": true, "sortOrder": 50, "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.b, &format!("catalogs/categories/{id}")),
        &t,
        json!({"label": "Lagi", "active": true, "sortOrder": 50, "version": 2}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);

    // Editor lists inactive entries; the intake catalog hides them.
    let cs = tok(&pool, f.cs).await;
    let r = get(&pool, &ws_uri(f.a, "catalogs/categories"), &cs).await;
    assert!(
        r.json["items"].as_array().is_some_and(|a| a
            .iter()
            .any(|e| e["id"] == id.as_str() && e["active"] == false)),
        "{}",
        r.json
    );
    // Intake refuses the inactive entry (server-side, not only hidden in the UI).
    let ch: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM intake_channels WHERE workspace_id = $1 AND active ORDER BY code LIMIT 1")
            .bind(f.a)
            .fetch_one(&pool)
            .await,
        "channel",
    );
    let mut req = Request::builder()
        .method("POST")
        .uri(ws_uri(f.a, "tickets"))
        .header(
            header::COOKIE,
            format!("{}={}", session::cookie_name(false), cs.token),
        )
        .header(session::CSRF_HEADER, &cs.csrf)
        .header("Idempotency-Key", "d09-inactive-category")
        .header(header::CONTENT_TYPE, "application/json");
    req = req.header("X-Correlation-Id", "d09-test");
    let body = json!({"subject": "Uji kategori nonaktif (dummy)", "chronology": "Kronologi uji (dummy)",
        "receivedAt": "2026-11-01T09:00:00+07:00", "intakeChannelId": ch, "categoryId": id});
    let res = match app(state(&pool, "uat_superadmin"))
        .oneshot(ok(req.body(Body::from(body.to_string())), "req"))
        .await
    {
        Ok(r) => r,
        Err(e) => match e {},
    };
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    let err: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(err["error"]["field"], "categoryId", "{err}");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn companies_are_superadmin_only_and_start_without_members(pool: PgPool) {
    let f = fixture(&pool).await;
    grant(&pool, f.a, f.lead, "admin.settings", f.admin).await;
    grant(&pool, f.a, f.lead, "admin.members", f.admin).await;
    let lead = tok(&pool, f.lead).await;
    let body = json!({"code": "UJI3", "name": "PT Uji Tiga (dummy)", "timezone": "Asia/Jayapura", "reporterCode": null});
    let r = write(&pool, "POST", "/api/v1/companies", &lead, body.clone()).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "workspace admin grants are not platform rights"
    );

    let t = tok(&pool, f.admin).await;
    // CP4b: UAT Superadmin has no factor, so the step-up guard is skipped (no step_up_at here)
    // and the step-up endpoint itself does not exist outside password mode.
    let r = write(&pool, "POST", "/api/v1/companies", &t, body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "company.create", f.admin).await;
    let ws = ok(Uuid::parse_str(&s(&r.json["item"]["workspaceId"])), "ws");
    let company = s(&r.json["item"]["companyId"]);
    assert_eq!(r.json["item"]["memberCount"], 0);
    let n: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM priorities WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(&pool)
            .await,
        "priorities",
    );
    assert_eq!(n, 4, "fixed priorities seeded");
    let r = write(&pool, "POST", "/api/v1/companies", &t, body.clone()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(r.json["error"]["field"], "code");
    let r = get(&pool, &ws_uri(ws, "settings"), &lead).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no implicit membership");
    let r = write(
        &pool,
        "POST",
        "/api/v1/session/step-up",
        &t,
        json!({"code": "000000"}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "no step-up in UAT mode: {}",
        r.json
    );

    let list = get(&pool, "/api/v1/companies", &t).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    let v = list.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["companyId"] == company.as_str()))
        .map(|c| c["version"].clone())
        .unwrap_or_default();
    let uri = format!("/api/v1/companies/{company}");
    let r = write(
        &pool,
        "PATCH",
        &uri,
        &t,
        json!({"active": false, "version": 99}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let r = write(
        &pool,
        "PATCH",
        &uri,
        &t,
        json!({"active": false, "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    audit_exists(
        &pool,
        &r.json["auditEventId"],
        "company.deactivate",
        f.admin,
    )
    .await;
    let r = get(&pool, "/api/v1/workspaces", &t).await;
    assert!(
        !r.json.to_string().contains(&ws.to_string()),
        "inactive workspace hidden"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn company_profile_edit_is_superadmin_versioned_and_keeps_tickets(pool: PgPool) {
    let f = fixture(&pool).await;
    let db: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await,
        "db name",
    );
    ok(seed::seed_uat_tickets(&pool, &db).await, "seed tickets");
    // A workspace admin grant is not platform administration.
    grant(&pool, f.a, f.lead, "admin.settings", f.admin).await;
    let tickets = |pool: PgPool, ws: Uuid| async move {
        let rows: Vec<(Uuid, i64)> = ok(
            sqlx::query_as("SELECT id, number FROM tickets WHERE workspace_id = $1 ORDER BY id")
                .bind(ws)
                .fetch_all(&pool)
                .await,
            "tickets",
        );
        rows
    };
    let before = tickets(pool.clone(), f.a).await;
    assert!(!before.is_empty(), "seed has DUMA tickets");
    let t = tok(&pool, f.admin).await;
    let list = get(&pool, "/api/v1/companies", &t).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    let duma = list.json["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["workspaceCode"] == "DUMA"))
        .cloned()
        .unwrap_or_default();
    let company = s(&duma["companyId"]);
    let uri = format!("/api/v1/companies/{company}/profile");
    let edit = |v: &Value| {
        json!({"code": duma["workspaceCode"],
               "workspaceName": "Ruang Alfa Baru (dummy)", "companyName": "PT Alfa Baru (dummy)",
               "timezone": duma["timezone"], "reporterCode": duma["reporterCode"],
               "brandKey": duma["brandKey"], "brandName": duma["brandName"], "version": v})
    };

    for (who, uid) in [
        ("cs", f.cs),
        ("lead admin.settings", f.lead),
        ("bravo", f.bravo),
    ] {
        let other = tok(&pool, uid).await;
        let r = write(&pool, "PATCH", &uri, &other, edit(&duma["version"])).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who}: {}", r.json);
    }
    // D17: the code is part of the body and required, so an older client that omits it
    // cannot clear or change it by accident.
    let mut without_code = edit(&duma["version"]);
    if let Some(o) = without_code.as_object_mut() {
        o.remove("code");
    }
    let r = write(&pool, "PATCH", &uri, &t, without_code).await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "code is required: {}",
        r.json
    );
    let r = write(&pool, "PATCH", &uri, &t, edit(&json!(999))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let unknown = format!("/api/v1/companies/{}/profile", Uuid::new_v4());
    let r = write(&pool, "PATCH", &unknown, &t, edit(&duma["version"])).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    let mut blank = edit(&duma["version"]);
    blank["companyName"] = json!("  ");
    let r = write(&pool, "PATCH", &uri, &t, blank).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(
        audit_count(&pool, f.a, "company.update").await,
        0,
        "nothing written yet"
    );

    let r = write(&pool, "PATCH", &uri, &t, edit(&duma["version"])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "company.update", f.admin).await;
    assert_eq!(r.json["item"]["workspaceName"], "Ruang Alfa Baru (dummy)");
    assert_eq!(r.json["item"]["companyName"], "PT Alfa Baru (dummy)");
    assert_eq!(r.json["item"]["workspaceCode"], "DUMA", "code unchanged");
    assert_eq!(r.json["item"]["companyCode"], duma["companyCode"]);
    let v2 = r.json["item"]["version"].clone();
    assert_eq!(v2.as_i64(), duma["version"].as_i64().map(|v| v + 1));
    let changed: Value = ok(
        sqlx::query_scalar(
            "SELECT safe_change->'changed' FROM audit_events WHERE action = 'company.update'",
        )
        .fetch_one(&pool)
        .await,
        "audit payload",
    );
    assert_eq!(changed, json!(["workspaceName", "companyName"]));

    // The old version is now stale; an unchanged save writes nothing.
    let r = write(&pool, "PATCH", &uri, &t, edit(&duma["version"])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let r = write(&pool, "PATCH", &uri, &t, edit(&v2)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["auditEventId"], Value::Null);
    assert_eq!(r.json["item"]["version"], v2);
    assert_eq!(audit_count(&pool, f.a, "company.update").await, 1);

    // Reload: the switcher list and the members' own settings show the new name.
    let ws = get(&pool, "/api/v1/workspaces", &t).await;
    assert!(
        ws.json.to_string().contains("Ruang Alfa Baru (dummy)"),
        "{}",
        ws.json
    );
    let cs = tok(&pool, f.cs).await;
    let r = get(&pool, &ws_uri(f.a, "settings"), &cs).await;
    assert_eq!(r.json["settings"]["companyName"], "PT Alfa Baru (dummy)");
    assert_eq!(
        tickets(pool.clone(), f.a).await,
        before,
        "ticket ids and numbers intact"
    );
    let r = get(&pool, &ws_uri(f.a, "tickets"), &cs).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(
        r.json.to_string().contains("\"DUMA-"),
        "display numbers keep the code"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn connection_lifecycle_is_save_test_activate(pool: PgPool) {
    let f = fixture(&pool).await;
    grant(&pool, f.a, f.lead, "integration.manage", f.admin).await;
    let lead = tok(&pool, f.lead).await;
    let list = get(&pool, &ws_uri(f.a, "integrations"), &lead).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    assert_eq!(list.json["creatable"], json!([]));
    assert_eq!(list.json["canManageCredentials"], false);
    let jira = connection(&list.json, "jira").clone();
    assert_eq!(jira["state"], "active", "seeded dummy connection is active");
    assert_eq!(jira["mode"], "simulation");
    assert_eq!(
        jira["credential"],
        json!({"state": "missing", "updatedAt": null})
    );
    assert!(jira.get("mapping").is_none());
    assert!(jira["settings"].is_object());
    assert_eq!(list.json["integrationsMode"], "simulation");
    let id = s(&jira["id"]);
    let step = |a: &str| ws_uri(f.a, &format!("integrations/{id}/{a}"));

    let r = write(
        &pool,
        "POST",
        &step("disable"),
        &lead,
        json!({"version": jira["version"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    audit_exists(
        &pool,
        &r.json["auditEventId"],
        "integration.disable",
        f.lead,
    )
    .await;
    assert_eq!(r.json["item"]["state"], "disabled");
    let v = r.json["item"]["version"].clone();
    // Intake no longer offers the disabled connection.
    let d = get(&pool, &ws_uri(f.a, "intake-directory"), &lead).await;
    assert_eq!(d.json["jira"]["options"], json!([]), "{}", d.json);

    let r = write(
        &pool,
        "POST",
        &step("activate"),
        &lead,
        json!({"version": v}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "activate needs a test first: {}",
        r.json
    );
    let r = write(&pool, "POST", &step("test"), &lead, json!({"version": 1})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let r = write(&pool, "POST", &step("test"), &lead, json!({"version": v})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "tested");
    assert!(r.json["item"]["testedAt"].is_string());
    assert!(s(&r.json["item"]["lastTestNote"]).contains("tidak dihubungi"));
    let r = write(
        &pool,
        "POST",
        &step("activate"),
        &lead,
        json!({"version": r.json["item"]["version"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["state"], "active");
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.a, &format!("integrations/{id}")),
        &lead,
        json!({"displayName": "Jira simulasi (dummy)", "version": r.json["item"]["version"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let actions: Vec<String> = r.json["item"]["recent"]
        .as_array()
        .map(|a| a.iter().map(s_of).collect())
        .unwrap_or_default();
    assert_eq!(
        actions,
        [
            "integration.update",
            "integration.activate",
            "integration.test",
            "integration.disable"
        ]
    );

    // One connection per provider; unknown provider refused; no config smuggling.
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, "integrations"),
        &lead,
        json!({"provider": "jira", "displayName": "Lagi"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    assert_eq!(r.json["error"]["field"], "provider");
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.a, "integrations"),
        &lead,
        json!({"provider": "whatsapp", "displayName": "WA"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.a, &format!("integrations/{id}")),
        &lead,
        json!({"displayName": "X", "version": 9, "apiToken": "x"}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "unknown fields refused: {}",
        r.json
    );

    // Grant is per workspace; foreign connection id is 404.
    let r = get(&pool, &ws_uri(f.b, "integrations"), &lead).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let admin = tok(&pool, f.admin).await;
    let r = write(
        &pool,
        "POST",
        &ws_uri(f.b, &format!("integrations/{id}/disable")),
        &admin,
        json!({"version": 1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}

fn s_of(v: &Value) -> String {
    v["action"].as_str().unwrap_or_default().to_owned()
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0009_rolls_back_and_promotes_only_dummy_connections(pool: PgPool) {
    let f = fixture(&pool).await;
    ok(db::MIGRATOR.undo(&pool, 8).await, "undo to 0008");
    let cols: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns WHERE \
             (table_name = 'companies' AND column_name = 'reporter_code') \
             OR (table_name = 'integration_connections' AND column_name = 'last_test_note')",
        )
        .fetch_one(&pool)
        .await,
        "columns",
    );
    assert_eq!(cols, 0);
    let draft = "UPDATE integration_connections SET state = 'draft', tested_at = NULL";
    let states = "SELECT array_agg(DISTINCT state) FROM integration_connections";
    ok(sqlx::query(draft).execute(&pool).await, "draft");
    // Without the dummy marker the data step leaves connections alone.
    let marker: String = ok(
        sqlx::query_scalar(
            "DELETE FROM itsm_deployment_marker WHERE kind = 'test_dummy' RETURNING marked_db",
        )
        .fetch_one(&pool)
        .await,
        "marker",
    );
    ok(db::MIGRATOR.run(&pool).await, "reapply");
    let s1: Vec<String> = ok(sqlx::query_scalar(states).fetch_one(&pool).await, "states");
    assert_eq!(s1, ["draft"], "no promotion without the dummy marker");

    ok(db::MIGRATOR.undo(&pool, 8).await, "undo again");
    ok(
        sqlx::query(
            "INSERT INTO itsm_deployment_marker (kind, marked_db) VALUES ('test_dummy', $1)",
        )
        .bind(&marker)
        .execute(&pool)
        .await,
        "marker back",
    );
    ok(db::MIGRATOR.run(&pool).await, "reapply with marker");
    let s2: Vec<String> = ok(sqlx::query_scalar(states).fetch_one(&pool).await, "states");
    assert_eq!(s2, ["active"]);
    let t = tok(&pool, f.admin).await;
    let r = get(&pool, &ws_uri(f.a, "integrations"), &t).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

/// D15: per-company branding is a bundled preset (or none) edited with the company profile.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn company_branding_is_a_preset_scoped_per_company_and_versioned(pool: PgPool) {
    let f = fixture(&pool).await;
    grant(&pool, f.a, f.lead, "admin.settings", f.admin).await;
    let t = tok(&pool, f.admin).await;
    let list = get(&pool, "/api/v1/companies", &t).await;
    let find = |code: &str| {
        list.json["items"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["workspaceCode"] == code))
            .cloned()
            .unwrap_or_default()
    };
    let (duma, dumb) = (find("DUMA"), find("DUMB"));
    assert_eq!(
        duma["brandKey"],
        Value::Null,
        "unconfigured until an admin picks a preset"
    );
    assert_eq!(duma["brandName"], Value::Null);
    let body = |c: &Value, key: Value, name: Value, v: &Value| {
        json!({"code": c["workspaceCode"],
               "workspaceName": c["workspaceName"], "companyName": c["companyName"],
               "timezone": c["timezone"], "reporterCode": c["reporterCode"],
               "brandKey": key, "brandName": name, "version": v})
    };
    let uri = |c: &Value| format!("/api/v1/companies/{}/profile", s(&c["companyId"]));
    let none = Value::Null;

    // Only bundled presets: never a URL, a path or an unknown key.
    for bad in [
        "https://example.com/logo.png",
        "../logo",
        "DECA",
        "qash",
        "",
    ] {
        let b = body(&dumb, json!(bad), none.clone(), &dumb["version"]);
        let r = write(&pool, "PATCH", &uri(&dumb), &t, b).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["field"], "brandKey", "{bad}");
    }
    let long = json!("x".repeat(61));
    let r = write(
        &pool,
        "PATCH",
        &uri(&dumb),
        &t,
        body(&dumb, json!("ajr"), long, &dumb["version"]),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    // Omitting a branding field is rejected, never read as "clear".
    let mut partial = body(&dumb, json!("ajr"), none.clone(), &dumb["version"]);
    if let Some(o) = partial.as_object_mut() {
        o.remove("brandKey");
    }
    let r = write(&pool, "PATCH", &uri(&dumb), &t, partial).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    // Not editable through workspace settings (even with admin.settings), nor by CS, Lead or Bravo.
    let lead = tok(&pool, f.lead).await;
    let mut ws_body = body(&duma, json!("ajr"), none.clone(), &duma["version"]);
    if let Some(o) = ws_body.as_object_mut() {
        o.remove("brandName");
    }
    let r = write(&pool, "PATCH", &ws_uri(f.a, "settings"), &lead, ws_body).await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "brandKey is no settings field: {}",
        r.json
    );
    for uid in [f.cs, f.lead, f.bravo] {
        let other = tok(&pool, uid).await;
        let b = body(&duma, json!("ajr"), none.clone(), &duma["version"]);
        let r = write(&pool, "PATCH", &uri(&duma), &other, b).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    }
    let r = write(
        &pool,
        "PATCH",
        &uri(&duma),
        &t,
        body(&duma, json!("deca"), none.clone(), &json!(999)),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(audit_count(&pool, f.a, "company.update").await, 0);
    assert_eq!(audit_count(&pool, f.b, "company.update").await, 0);

    // Two companies, two presets.
    let r = write(
        &pool,
        "PATCH",
        &uri(&duma),
        &t,
        body(&duma, json!("deca"), none.clone(), &duma["version"]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    audit_exists(&pool, &r.json["auditEventId"], "company.update", f.admin).await;
    assert_eq!(r.json["item"]["brandKey"], "deca");
    let b = body(
        &dumb,
        json!("ajr"),
        json!("  Bravo uji  "),
        &dumb["version"],
    );
    let r = write(&pool, "PATCH", &uri(&dumb), &t, b).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["brandName"], "Bravo uji", "trimmed");
    let v = r.json["item"]["version"].clone();
    let changed: Value = ok(
        sqlx::query_scalar(
            "SELECT safe_change->'changed' FROM audit_events \
             WHERE action = 'company.update' AND workspace_id = $1",
        )
        .bind(f.b)
        .fetch_one(&pool)
        .await,
        "audit change",
    );
    assert_eq!(
        changed,
        json!(["brandKey", "brandName"]),
        "names only, no values"
    );
    let r = write(
        &pool,
        "PATCH",
        &uri(&dumb),
        &t,
        body(&dumb, json!("deca"), none.clone(), &dumb["version"]),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "a stale version keeps AJR: {}",
        r.json
    );

    // The switcher list carries each workspace's own branding; members see only their own.
    let all = get(&pool, "/api/v1/workspaces", &t).await;
    let by_code = |code: &str| {
        all.json["items"]
            .as_array()
            .and_then(|a| a.iter().find(|w| w["code"] == code))
            .map(|w| (w["brandKey"].clone(), w["brandName"].clone()))
            .unwrap_or_default()
    };
    assert_eq!(
        by_code("DUMA"),
        (json!("deca"), duma["companyName"].clone()),
        "name falls back to the company"
    );
    assert_eq!(by_code("DUMB"), (json!("ajr"), json!("Bravo uji")));
    let mine = get(&pool, "/api/v1/workspaces", &tok(&pool, f.cs).await).await;
    let items = mine.json["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 1, "{}", mine.json);
    assert_eq!(
        (items[0]["code"].clone(), items[0]["brandKey"].clone()),
        (json!("DUMA"), json!("deca"))
    );
    let none_of_mine = get(&pool, "/api/v1/workspaces", &tok(&pool, f.bravo).await).await;
    assert_eq!(
        none_of_mine.json["items"],
        json!([]),
        "no membership, no branding of anyone"
    );

    // A workspace settings save keeps the branding; clearing it is an explicit null.
    let cur = get(&pool, &ws_uri(f.b, "settings"), &t).await.json["settings"].clone();
    let b = json!({"workspaceName": "Ruang Bravo (dummy)", "companyName": cur["companyName"],
                   "timezone": cur["timezone"], "reporterCode": cur["reporterCode"], "version": v});
    let r = write(&pool, "PATCH", &ws_uri(f.b, "settings"), &t, b).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let saved = r.json["settings"].clone();
    assert_eq!(
        (saved["brandKey"].clone(), saved["brandName"].clone()),
        (json!("ajr"), json!("Bravo uji"))
    );
    let r = write(
        &pool,
        "PATCH",
        &uri(&dumb),
        &t,
        body(&saved, none.clone(), none.clone(), &saved["version"]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["item"]["brandKey"], Value::Null);
    let w = get(&pool, &format!("/api/v1/workspaces/{}", f.b), &t).await;
    assert_eq!(
        (w.json["brandKey"].clone(), w.json["brandName"].clone()),
        (none, dumb["companyName"].clone())
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0013_rolls_back_and_reapplies(pool: PgPool) {
    let f = fixture(&pool).await;
    ok(
        sqlx::query("UPDATE companies SET brand_key = 'ajr', brand_name = 'X'")
            .execute(&pool)
            .await,
        "brand",
    );
    let count = "SELECT count(*) FROM companies";
    let before: i64 = ok(sqlx::query_scalar(count).fetch_one(&pool).await, "before");
    ok(db::MIGRATOR.undo(&pool, 12).await, "undo to 0012");
    let cols: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_name = 'companies' AND column_name IN ('brand_key', 'brand_name')",
        )
        .fetch_one(&pool)
        .await,
        "columns",
    );
    assert_eq!(cols, 0);
    let after: i64 = ok(sqlx::query_scalar(count).fetch_one(&pool).await, "after");
    assert_eq!(after, before, "company rows kept");
    ok(db::MIGRATOR.run(&pool).await, "reapply");
    let t = tok(&pool, f.admin).await;
    let w = get(&pool, "/api/v1/workspaces", &t).await;
    assert_eq!(w.status, StatusCode::OK, "{}", w.json);
    let items = w.json["items"].as_array().cloned().unwrap_or_default();
    assert!(
        !items.is_empty() && items.iter().all(|x| x["brandKey"].is_null()),
        "{}",
        w.json
    );
    let bad = sqlx::query("UPDATE companies SET brand_key = 'https://x'")
        .execute(&pool)
        .await;
    assert!(
        bad.is_err(),
        "the CHECK keeps the preset allowlist in the database too"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn bmt_branding_is_scoped_and_survives_reads(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = tok(&pool, f.admin).await;
    let a = get(&pool, &ws_uri(f.a, "settings"), &t).await.json["settings"].clone();
    let b_before = get(&pool, &ws_uri(f.b, "settings"), &t).await.json["settings"].clone();
    let uri = format!("/api/v1/companies/{}/profile", s(&a["companyId"]));
    let body = json!({"code": a["workspaceCode"], "workspaceName": a["workspaceName"],
        "companyName": a["companyName"], "timezone": a["timezone"],
        "reporterCode": a["reporterCode"], "brandKey": "bmt", "brandName": "BMT Uji", "version": a["version"]});
    let denied = write(&pool, "PATCH", &uri, &tok(&pool, f.cs).await, body.clone()).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let saved = write(&pool, "PATCH", &uri, &t, body).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json);
    let current = get(&pool, &format!("/api/v1/workspaces/{}", f.a), &t).await;
    assert_eq!(current.json["brandKey"], "bmt");
    assert_eq!(current.json["brandName"], "BMT Uji");
    let b_after = get(&pool, &ws_uri(f.b, "settings"), &t).await.json["settings"].clone();
    assert_eq!(
        b_before, b_after,
        "branding must not change another company"
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn bmt_migration_downgrade_refuses_to_discard_branding(pool: PgPool) {
    let _f = fixture(&pool).await;
    ok(
        sqlx::query("UPDATE companies SET brand_key = 'bmt'")
            .execute(&pool)
            .await,
        "set BMT",
    );
    assert!(db::MIGRATOR.undo(&pool, 14).await.is_err());
    let count: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM companies WHERE brand_key = 'bmt'")
            .fetch_one(&pool)
            .await,
        "preserved BMT",
    );
    assert!(count > 0);
    ok(
        sqlx::query("UPDATE companies SET brand_key = NULL")
            .execute(&pool)
            .await,
        "clear preset explicitly",
    );
    ok(db::MIGRATOR.undo(&pool, 14).await, "downgrade");
    assert!(
        sqlx::query("UPDATE companies SET brand_key = 'bmt'")
            .execute(&pool)
            .await
            .is_err()
    );
    ok(db::MIGRATOR.run(&pool).await, "upgrade");
    ok(
        sqlx::query("UPDATE companies SET brand_key = 'bmt'")
            .execute(&pool)
            .await,
        "BMT accepted again",
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn ai_suggestions_enforce_scope_csrf_and_input_before_provider(pool: PgPool) {
    let f = fixture(&pool).await;
    let t = tok(&pool, f.cs).await;
    let uri = ws_uri(f.a, "ai-suggestions");
    let invalid = json!({"mode":"unsupported","text":"Synthetic text"});
    assert_eq!(
        send(&pool, "POST", &uri, &t, false, Some(&invalid))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        write(
            &pool,
            "POST",
            &ws_uri(f.b, "ai-suggestions"),
            &t,
            invalid.clone()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        write(&pool, "POST", &uri, &t, invalid).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        write(
            &pool,
            "POST",
            &uri,
            &t,
            json!({"mode":"reply","text":"x".repeat(6001)})
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        write(
            &pool,
            "POST",
            &uri,
            &t,
            json!({"mode":"reply","text":"   "})
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5a_company_config_writes_are_404_outside_uat_and_reads_still_work(pool: PgPool) {
    // With SSO per company the workspace configuration is not editable in production modes:
    // reads stay, writes answer 404 (no hint that the route exists), nothing is written.
    let f = fixture(&pool).await;
    grant(&pool, f.a, f.lead, "admin.settings", f.admin).await;
    let t = tok_in(&pool, f.lead, "password").await;
    let r = get(&pool, &ws_uri(f.a, "settings"), &t).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let v = r.json["settings"]["version"].clone();
    let before = audit_count(&pool, f.a, "settings.update").await;
    let body = json!({"workspaceName": "Tidak tersimpan", "companyName": "PT Tidak (dummy)",
                      "timezone": "Asia/Jakarta", "reporterCode": "LPR-X", "version": v});
    let r = write(&pool, "PATCH", &ws_uri(f.a, "settings"), &t, body).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    assert_eq!(audit_count(&pool, f.a, "settings.update").await, before);
    let r = get(&pool, &ws_uri(f.a, "settings"), &t).await;
    assert_ne!(r.json["settings"]["workspaceName"], "Tidak tersimpan");

    // Company writes: 404 in password mode even for the (ignored) UAT Superadmin account.
    let company: Uuid = ok(
        sqlx::query_scalar("SELECT company_id FROM workspaces WHERE id = $1")
            .bind(f.a)
            .fetch_one(&pool)
            .await,
        "company",
    );
    let admin = tok_in(&pool, f.admin, "password").await;
    for (method, uri, body) in [
        (
            "POST",
            "/api/v1/companies".to_owned(),
            json!({"code": "DUMX", "name": "PT Baru (dummy)", "timezone": "Asia/Jakarta", "reporterCode": null}),
        ),
        (
            "PATCH",
            format!("/api/v1/companies/{company}"),
            json!({"active": false, "version": 1}),
        ),
        (
            "PATCH",
            format!("/api/v1/companies/{company}/profile"),
            json!({"code": "DUMA", "workspaceName": "X", "companyName": "X", "timezone": "Asia/Jakarta",
                    "reporterCode": null, "brandKey": null, "brandName": null, "version": 1}),
        ),
    ] {
        for who in [&t, &admin] {
            let r = write(&pool, method, &uri, who, body.clone()).await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "{method} {uri}: {}",
                r.json
            );
        }
    }
    let n: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM companies WHERE name = 'PT Baru (dummy)'")
            .fetch_one(&pool)
            .await,
        "count",
    );
    assert_eq!(n, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5a_a_user_belongs_to_one_company(pool: PgPool) {
    let f = fixture(&pool).await;
    let admin = tok(&pool, f.admin).await;
    let add = |ws: Uuid| {
        let admin = &admin;
        let pool = &pool;
        async move {
            write(
                pool,
                "POST",
                &ws_uri(ws, "members"),
                admin,
                json!({"subject": "dummy:cs-bravo", "role": "cs"}),
            )
            .await
        }
    };
    let r = add(f.a).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let bravo_a = s(&r.json["item"]["membershipId"]);

    // Second company while the first membership is active: 409, nothing added.
    let r = add(f.b).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "user_in_other_company");
    let rows: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspace_memberships WHERE user_id = $1")
            .bind(f.bravo)
            .fetch_one(&pool)
            .await,
        "count",
    );
    assert_eq!(rows, 1);

    // Moving companies = deactivate the old membership first; reactivating it is then refused.
    let list = get(&pool, &ws_uri(f.a, "members"), &admin).await.json;
    let v = member(&list, f.bravo)["version"].clone();
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.a, &format!("members/{bravo_a}")),
        &admin,
        json!({"role": "cs", "active": false, "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = add(f.b).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let list = get(&pool, &ws_uri(f.a, "members"), &admin).await.json;
    let v = member(&list, f.bravo)["version"].clone();
    let r = write(
        &pool,
        "PATCH",
        &ws_uri(f.a, &format!("members/{bravo_a}")),
        &admin,
        json!({"role": "cs", "active": true, "version": v}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "user_in_other_company");

    // The user now opens only the new company.
    let bravo = tok(&pool, f.bravo).await;
    assert_eq!(
        get(&pool, &ws_uri(f.b, "settings"), &bravo).await.status,
        StatusCode::OK
    );
    assert_eq!(
        get(&pool, &ws_uri(f.a, "settings"), &bravo).await.status,
        StatusCode::NOT_FOUND
    );
}
