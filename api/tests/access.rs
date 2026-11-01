//! D03 permission foundation against a real PostgreSQL.
//! Role fixtures are created directly in the test database (memberships, grants) and sessions
//! are issued with `session::issue`: there is no HTTP impersonation route. Only operations that
//! exist at this SHA are exercised; mutations of D06–D08 are not built for these tests.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use itsm_api::config::{Config, Environment};
use itsm_api::error::AppError;
use itsm_api::{AppState, app, db, seed, session};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

// ---------- helpers ----------

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

fn sqlstate<T>(r: &Result<T, sqlx::Error>) -> Option<String> {
    match r {
        Ok(_) => None,
        Err(sqlx::Error::Database(db)) => db.code().map(|c| c.into_owned()),
        Err(e) => Some(format!("non-db error: {e}")),
    }
}

const FK: &str = "23503";
const UNIQUE: &str = "23505";
const CHECK: &str = "23514";
const RULE: &str = "IT422";

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
    ok(Config::from_map(&vars), "test config")
}

fn state_with(pool: &PgPool, config: Config) -> AppState {
    AppState {
        config: Arc::new(config),
        pool: pool.clone(),
    }
}

async fn seeded(pool: &PgPool) {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "current_database",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
}

struct Resp {
    status: StatusCode,
    json: Value,
    raw: String,
}

async fn call_state(state: AppState, req: Request<Body>) -> Resp {
    let res = match app(state).oneshot(req).await {
        Ok(r) => r,
        Err(e) => match e {},
    };
    let status = res.status();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    Resp {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        raw: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get(pool: &PgPool, uri: &str, token: Option<&str>) -> Resp {
    get_in(state_with(pool, uat_config()), uri, token).await
}

async fn get_in(state: AppState, uri: &str, token: Option<&str>) -> Resp {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        b = b.header(
            header::COOKIE,
            format!("{}={t}", session::cookie_name(false)),
        );
    }
    call_state(state, ok(b.body(Body::empty()), "request")).await
}

async fn id_of(pool: &PgPool, sql: &str, key: &str) -> Uuid {
    ok(
        sqlx::query_scalar(sql).bind(key).fetch_one(pool).await,
        &format!("lookup {key}"),
    )
}

async fn ws(pool: &PgPool, code: &str) -> Uuid {
    id_of(pool, "SELECT id FROM workspaces WHERE code = $1", code).await
}

async fn user(pool: &PgPool, subject: &str) -> Uuid {
    id_of(
        pool,
        "SELECT id FROM users WHERE external_subject = $1",
        subject,
    )
    .await
}

async fn new_user(pool: &PgPool, subject: &str, name: &str) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO users (external_subject, display_name) VALUES ($1, $2) RETURNING id",
        )
        .bind(subject)
        .bind(name)
        .fetch_one(pool)
        .await,
        "insert user",
    )
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

async fn add_member(pool: &PgPool, ws: Uuid, user: Uuid, role: &str) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO workspace_memberships (workspace_id, user_id, role) VALUES ($1, $2, $3) \
             RETURNING id",
        )
        .bind(ws)
        .bind(user)
        .bind(role)
        .fetch_one(pool)
        .await,
        "add member",
    )
}

async fn try_grant(
    pool: &PgPool,
    ws: Uuid,
    membership: Uuid,
    code: &str,
    by: Uuid,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(ws)
    .bind(membership)
    .bind(code)
    .bind(by)
    .fetch_one(pool)
    .await
}

async fn grant(pool: &PgPool, ws: Uuid, membership: Uuid, code: &str, by: Uuid) -> Uuid {
    ok(try_grant(pool, ws, membership, code, by).await, "grant")
}

async fn revoke_grant(pool: &PgPool, id: Uuid, by: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE workspace_grants SET revoked_at = now(), revoked_by = $2 WHERE id = $1")
        .bind(id)
        .bind(by)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Test-harness session for a fixture account (no HTTP route involved).
async fn login(pool: &PgPool, user: Uuid) -> String {
    ok(session::issue(pool, user, "uat_superadmin").await, "issue").token
}

async fn ticket(pool: &PgPool, ws: Uuid, creator: Uuid) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by) \
             VALUES ($1, 'Pengaduan uji', 'Kronologi uji (dummy)', now(), $2) RETURNING id",
        )
        .bind(ws)
        .bind(creator)
        .fetch_one(pool)
        .await,
        "ticket",
    )
}

fn strings(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

const GRANT_CODES: [&str; 5] = [
    "admin.members",
    "admin.settings",
    "integration.manage",
    "integration.credentials",
    "integration.recovery",
];

struct Fx {
    a: Uuid,
    b: Uuid,
    admin: Uuid,
    cs: Uuid,
    lead: Uuid,
}

async fn fixture(pool: &PgPool) -> Fx {
    seeded(pool).await;
    Fx {
        a: ws(pool, "DUMA").await,
        b: ws(pool, "DUMB").await,
        admin: user(pool, "dummy:uat-superadmin").await,
        cs: user(pool, "dummy:cs-alfa").await,
        lead: user(pool, "dummy:lead-alfa").await,
    }
}

async fn perms(pool: &PgPool, ws: Uuid, token: &str) -> Resp {
    get(
        pool,
        &format!("/api/v1/workspaces/{ws}/permissions"),
        Some(token),
    )
    .await
}

// ---------- metadata ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn meta_exposes_persistent_uat_notice(pool: PgPool) {
    let r = get(&pool, "/api/v1/meta", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["banner"], "Mode uji — Superadmin");
    assert_eq!(r.json["uat"]["banner"], "Mode uji — Superadmin");
    assert_eq!(r.json["uat"]["persistent"], true);
    assert_eq!(r.json["uat"]["dataScope"], "dummy");
    assert_eq!(r.json["uat"]["superadminProvesRbac"], false);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn permission_catalog_needs_session_and_keeps_grants_off_roles(pool: PgPool) {
    let fx = fixture(&pool).await;
    let anon = get(&pool, "/api/v1/permissions", None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);

    let t = login(&pool, fx.cs).await;
    let r = get(&pool, "/api/v1/permissions", Some(&t)).await;
    assert_eq!(r.status, StatusCode::OK);
    let grant_only: BTreeSet<String> = r.json["permissions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| p["source"] == "grant")
                .filter_map(|p| p["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        grant_only,
        GRANT_CODES.iter().map(|c| (*c).to_owned()).collect()
    );
    let roles = r.json["roles"].as_array().cloned().unwrap_or_default();
    let codes: Vec<_> = roles.iter().map(|x| x["code"].clone()).collect();
    assert_eq!(codes, vec![Value::from("cs"), Value::from("lead_cs")]);
    for role in &roles {
        let p = strings(&role["permissions"]);
        assert!(
            p.is_disjoint(&grant_only),
            "{} carries a grant",
            role["code"]
        );
    }
}

// ---------- effective permissions per role ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cs_and_lead_cs_get_role_permissions_and_no_admin_rights(pool: PgPool) {
    let fx = fixture(&pool).await;
    let cs = perms(&pool, fx.a, &login(&pool, fx.cs).await).await;
    assert_eq!(cs.status, StatusCode::OK);
    assert_eq!(cs.json["role"], "cs");
    assert_eq!(cs.json["uatSuperadmin"], false);
    let p = strings(&cs.json["permissions"]);
    assert!(p.contains("ticket.assign_self") && p.contains("ticket.read"));
    assert!(!p.contains("ticket.assign_any"));

    let lead = perms(&pool, fx.a, &login(&pool, fx.lead).await).await;
    assert_eq!(lead.json["role"], "lead_cs");
    let lp = strings(&lead.json["permissions"]);
    assert!(lp.contains("ticket.assign_any"));
    for code in GRANT_CODES {
        assert!(!p.contains(code) && !lp.contains(code), "{code}");
    }
    assert_eq!(strings(&lead.json["grants"]), BTreeSet::new());

    let sa = perms(&pool, fx.a, &login(&pool, fx.admin).await).await;
    assert_eq!(sa.json["uatSuperadmin"], true);
    assert_eq!(sa.json["role"], Value::Null);
    assert!(
        GRANT_CODES
            .iter()
            .all(|c| strings(&sa.json["permissions"]).contains(*c))
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn grant_applies_only_in_its_own_workspace_and_roles_differ_per_workspace(pool: PgPool) {
    let fx = fixture(&pool).await;
    // Multi-workspace dummy account: Lead CS in DUMA, CS in DUMB.
    let multi = new_user(&pool, "dummy:multi", "Multi workspace (dummy)").await;
    let ma = add_member(&pool, fx.a, multi, "lead_cs").await;
    add_member(&pool, fx.b, multi, "cs").await;
    grant(&pool, fx.a, ma, "integration.recovery", fx.admin).await;
    let t = login(&pool, multi).await;

    let a = perms(&pool, fx.a, &t).await;
    assert_eq!(a.json["role"], "lead_cs");
    assert!(strings(&a.json["permissions"]).contains("integration.recovery"));
    assert_eq!(
        strings(&a.json["grants"]),
        BTreeSet::from(["integration.recovery".to_owned()])
    );
    // Only the granted code: no credentials, no member admin.
    assert!(!strings(&a.json["permissions"]).contains("integration.credentials"));
    assert!(!strings(&a.json["permissions"]).contains("admin.members"));

    let b = perms(&pool, fx.b, &t).await;
    assert_eq!(b.json["role"], "cs");
    let bp = strings(&b.json["permissions"]);
    assert!(!bp.contains("integration.recovery"));
    assert!(!bp.contains("ticket.assign_any"));
    assert_eq!(strings(&b.json["grants"]), BTreeSet::new());
}

// ---------- deny by default on a grant-gated operation ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn grants_list_is_default_deny_and_isolated(pool: PgPool) {
    let fx = fixture(&pool).await;
    let uri_a = format!("/api/v1/workspaces/{}/grants", fx.a);
    let uri_b = format!("/api/v1/workspaces/{}/grants", fx.b);
    let lead_m = membership(&pool, fx.a, fx.lead).await;
    // A grant that exists, so a leak would be visible in a denied body.
    grant(&pool, fx.a, lead_m, "integration.manage", fx.admin).await;

    assert_eq!(
        get(&pool, &uri_a, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    for who in [fx.cs, fx.lead] {
        let r = get(&pool, &uri_a, Some(&login(&pool, who).await)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.json["error"]["code"], "forbidden");
        assert!(r.json.get("items").is_none());
        assert!(!r.raw.contains("integration.manage") && !r.raw.contains("Lead CS Alfa"));
    }

    // Lead CS with the explicit admin.members grant may read; the job title alone may not.
    grant(&pool, fx.a, lead_m, "admin.members", fx.admin).await;
    let lead = login(&pool, fx.lead).await;
    let r = get(&pool, &uri_a, Some(&lead)).await;
    assert_eq!(r.status, StatusCode::OK);
    let codes: BTreeSet<String> = r.json["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|g| g["grantCode"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        codes,
        BTreeSet::from(["admin.members".to_owned(), "integration.manage".to_owned()])
    );
    // The grant does not open another workspace: 404, not 403.
    for uri in [
        uri_b.clone(),
        format!("/api/v1/workspaces/{}/grants", Uuid::new_v4()),
        "/api/v1/workspaces/nope/grants".to_owned(),
    ] {
        let r = get(&pool, &uri, Some(&lead)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(r.json["error"]["code"], "not_found");
    }

    let sa = login(&pool, fx.admin).await;
    assert_eq!(get(&pool, &uri_a, Some(&sa)).await.status, StatusCode::OK);
    let rb = get(&pool, &uri_b, Some(&sa)).await;
    assert_eq!(rb.status, StatusCode::OK);
    assert_eq!(rb.json["items"].as_array().map(Vec::len), Some(0));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn revoke_and_deactivation_take_effect_on_the_next_request(pool: PgPool) {
    let fx = fixture(&pool).await;
    let uri = format!("/api/v1/workspaces/{}/grants", fx.a);
    let lead_m = membership(&pool, fx.a, fx.lead).await;
    let g = grant(&pool, fx.a, lead_m, "admin.members", fx.admin).await;
    let t = login(&pool, fx.lead).await;
    assert_eq!(get(&pool, &uri, Some(&t)).await.status, StatusCode::OK);

    ok(revoke_grant(&pool, g, fx.admin).await, "revoke");
    assert_eq!(
        get(&pool, &uri, Some(&t)).await.status,
        StatusCode::FORBIDDEN
    );

    // A new live grant after a revoke is allowed (history kept), then the membership ends.
    grant(&pool, fx.a, lead_m, "admin.members", fx.admin).await;
    assert_eq!(get(&pool, &uri, Some(&t)).await.status, StatusCode::OK);
    ok(
        sqlx::query(
            "UPDATE workspace_memberships SET active = false, version = version + 1 WHERE id = $1",
        )
        .bind(lead_m)
        .execute(&pool)
        .await,
        "deactivate",
    );
    let tk = ticket(&pool, fx.a, fx.cs).await;
    for uri in [
        uri.clone(),
        format!("/api/v1/workspaces/{}/permissions", fx.a),
        format!("/api/v1/workspaces/{}/members", fx.a),
        format!("/api/v1/workspaces/{}/catalogs", fx.a),
        format!("/api/v1/workspaces/{}/tickets/{tk}", fx.a),
    ] {
        let r = get(&pool, &uri, Some(&t)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
}

// ---------- isolation on the reads that exist ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn every_role_reads_own_workspace_and_gets_404_elsewhere(pool: PgPool) {
    let fx = fixture(&pool).await;
    let ta = ticket(&pool, fx.a, fx.cs).await;
    let tb = ticket(&pool, fx.b, fx.admin).await;
    let multi = new_user(&pool, "dummy:multi-b", "Hanya Beta (dummy)").await;
    add_member(&pool, fx.b, multi, "lead_cs").await;

    let reads = |w: Uuid, t: Uuid| {
        vec![
            format!("/api/v1/workspaces/{w}"),
            format!("/api/v1/workspaces/{w}/members"),
            format!("/api/v1/workspaces/{w}/catalogs"),
            format!("/api/v1/workspaces/{w}/permissions"),
            format!("/api/v1/workspaces/{w}/tickets/{t}"),
        ]
    };
    for (who, home, home_ticket, away, away_ticket) in [
        (fx.cs, fx.a, ta, fx.b, tb),
        (fx.lead, fx.a, ta, fx.b, tb),
        (multi, fx.b, tb, fx.a, ta),
    ] {
        let t = login(&pool, who).await;
        for uri in reads(home, home_ticket) {
            assert_eq!(
                get(&pool, &uri, Some(&t)).await.status,
                StatusCode::OK,
                "{uri}"
            );
        }
        // Foreign workspace, and a foreign ticket under the home workspace: both 404.
        let mut denied = reads(away, away_ticket);
        denied.push(format!("/api/v1/workspaces/{home}/tickets/{away_ticket}"));
        for uri in denied {
            let r = get(&pool, &uri, Some(&t)).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
            assert!(!r.raw.contains("Pengaduan uji"));
        }
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn superadmin_grant_is_ignored_outside_uat_mode(pool: PgPool) {
    // Config refuses production + UAT Superadmin at load time; this proves the request path
    // is fail-closed too if such a config were ever constructed.
    let fx = fixture(&pool).await;
    let t = login(&pool, fx.admin).await;
    let mut cfg = uat_config();
    cfg.environment = Environment::Production;
    let prod = state_with(&pool, cfg);
    for uri in [
        format!("/api/v1/workspaces/{}/permissions", fx.a),
        format!("/api/v1/workspaces/{}/grants", fx.a),
        format!("/api/v1/workspaces/{}", fx.a),
    ] {
        let r = get_in(prod.clone(), &uri, Some(&t)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let list = get_in(prod, "/api/v1/workspaces", Some(&t)).await;
    assert_eq!(list.json["items"].as_array().map(Vec::len), Some(0));
}

// ---------- database guards for grants ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn grant_rows_cannot_cross_workspaces_or_be_rewritten(pool: PgPool) {
    let fx = fixture(&pool).await;
    let lead_m = membership(&pool, fx.a, fx.lead).await;

    // Membership of DUMA under workspace DUMB: composite FK refuses.
    assert_eq!(
        sqlstate(&try_grant(&pool, fx.b, lead_m, "admin.members", fx.admin).await),
        Some(FK.into())
    );
    // A business role name is not a grant.
    for code in ["workspace_admin", "lead_cs", "ticket.assign_any"] {
        assert_eq!(
            sqlstate(&try_grant(&pool, fx.a, lead_m, code, fx.admin).await),
            Some(CHECK.into()),
            "{code}"
        );
    }
    let g = grant(&pool, fx.a, lead_m, "admin.settings", fx.admin).await;
    assert_eq!(
        sqlstate(&try_grant(&pool, fx.a, lead_m, "admin.settings", fx.admin).await),
        Some(UNIQUE.into())
    );
    let born_revoked = sqlx::query(
        "INSERT INTO workspace_grants (workspace_id, membership_id, grant_code, granted_by, \
         revoked_at, revoked_by) VALUES ($1, $2, 'admin.members', $3, now(), $3)",
    )
    .bind(fx.a)
    .bind(lead_m)
    .bind(fx.admin)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&born_revoked), Some(RULE.into()));

    let rewrite = sqlx::query(
        "UPDATE workspace_grants SET grant_code = 'integration.credentials' WHERE id = $1",
    )
    .bind(g)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&rewrite), Some(RULE.into()));
    let delete = sqlx::query("DELETE FROM workspace_grants WHERE id = $1")
        .bind(g)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&delete), Some(RULE.into()));

    ok(revoke_grant(&pool, g, fx.admin).await, "revoke once");
    assert_eq!(
        sqlstate(&revoke_grant(&pool, g, fx.admin).await),
        Some(RULE.into())
    );

    // Membership identity is fixed, so grants cannot follow a moved membership.
    for sql in [
        "UPDATE workspace_memberships SET user_id = $2 WHERE id = $1",
        "UPDATE workspace_memberships SET workspace_id = (SELECT id FROM workspaces WHERE code = 'DUMB') WHERE id = $1 AND $2 = $2",
    ] {
        let r = sqlx::query(sql)
            .bind(lead_m)
            .bind(fx.cs)
            .execute(&pool)
            .await;
        assert_eq!(sqlstate(&r), Some(RULE.into()), "{sql}");
    }
}

// ---------- safe error mapping ----------

async fn response_of(err: sqlx::Error) -> (StatusCode, String) {
    let res = AppError::from(err).into_response();
    let status = res.status();
    let bytes = ok(res.into_body().collect().await, "body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn domain_guard_errors_map_to_generic_422_without_sql_details(pool: PgPool) {
    let fx = fixture(&pool).await;
    let t = ticket(&pool, fx.a, fx.cs).await;
    let lead_m = membership(&pool, fx.a, fx.lead).await;
    let g = grant(&pool, fx.a, lead_m, "admin.members", fx.admin).await;

    let failures = [
        sqlx::query("UPDATE tickets SET number = 99, version = version + 1 WHERE id = $1")
            .bind(t)
            .execute(&pool)
            .await,
        sqlx::query(
            "UPDATE tickets SET workflow_status = 'resolved', version = version + 1 WHERE id = $1",
        )
        .bind(t)
        .execute(&pool)
        .await,
        sqlx::query("DELETE FROM workspace_grants WHERE id = $1")
            .bind(g)
            .execute(&pool)
            .await,
    ];
    for r in failures {
        assert_eq!(sqlstate(&r), Some(RULE.into()));
        let Err(e) = r else {
            panic!("expected failure")
        };
        let (status, body) = response_of(e).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let json: Value = ok(serde_json::from_str(&body), "json");
        assert_eq!(json["error"]["code"], "rule_violation");
        for leak in [
            "IT422",
            "tickets",
            "workspace_grants",
            "nomor",
            "ringkasan",
            "append-only",
            &t.to_string(),
        ] {
            assert!(!body.contains(leak), "leaked {leak}: {body}");
        }
    }

    // IT409 keeps its own meaning.
    let stale = sqlx::query("UPDATE tickets SET subject = 'x' WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await;
    let Err(e) = stale else {
        panic!("expected version conflict")
    };
    let (status, body) = response_of(e).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("version_conflict"));
}

// ---------- rollback ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0003_rolls_back_and_reapplies_on_dummy_data(pool: PgPool) {
    let fx = fixture(&pool).await;
    let lead_m = membership(&pool, fx.a, fx.lead).await;
    grant(&pool, fx.a, lead_m, "admin.members", fx.admin).await;
    ticket(&pool, fx.a, fx.cs).await;

    ok(db::MIGRATOR.undo(&pool, 2).await, "undo to 2");
    let exists: bool = ok(
        sqlx::query_scalar("SELECT to_regclass('workspace_grants') IS NOT NULL")
            .fetch_one(&pool)
            .await,
        "regclass",
    );
    assert!(!exists);
    let tickets: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&pool)
            .await,
        "tickets",
    );
    assert_eq!(tickets, 1);
    // Membership identity guard is gone with 0003.
    let moved = sqlx::query("UPDATE workspace_memberships SET user_id = user_id WHERE id = $1")
        .bind(lead_m)
        .execute(&pool)
        .await;
    assert!(moved.is_ok());

    ok(db::MIGRATOR.run(&pool).await, "reapply");
    let live: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspace_grants")
            .fetch_one(&pool)
            .await,
        "grants",
    );
    assert_eq!(live, 0);
    grant(&pool, fx.a, lead_m, "admin.members", fx.admin).await;
}
