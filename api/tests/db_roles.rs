#![allow(clippy::expect_used)]
//! CP8: production database roles against a real PostgreSQL. The ops bootstrap SQL
//! (`deploy/production/db/*.sql`) creates the roles; every migration then runs as the
//! non-superuser owner `itsm_migrator`, and the API and worker run as `itsm_app`, which may only
//! read and write rows: no DDL, no TRUNCATE, no temp objects, no writes that an append-only
//! guard refuses, and no writes to the deployment marker or the migration bookkeeping.
use std::{collections::HashMap, str::FromStr, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use itsm_api::{AppState, app, config::Config, db, outbox, password_bootstrap, shared_limit};
use serde_json::{Value, json};
use sqlx::{
    Connection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tower::ServiceExt;
use uuid::Uuid;

const ROLES_SQL: &str = include_str!("../../deploy/production/db/roles.sql");
const DATABASE_SQL: &str = include_str!("../../deploy/production/db/database.sql");
const ADMIN: &str = "admin.dob";
const ADMIN_PASSWORD: &str = "synthetic-cp8-admin-pass-123";

/// Tables whose guard trigger refuses UPDATE (and DELETE), and tables that only refuse DELETE.
const NO_UPDATE: [&str; 6] = [
    "audit_events",
    "auth_events",
    "platform_audit_events",
    "ticket_activities",
    "ticket_resolution_events",
    "workspace_code_history",
];
const NO_DELETE_ONLY: [&str; 2] = ["attachments", "workspace_grants"];
const READ_ONLY: [&str; 2] = ["_sqlx_migrations", "itsm_deployment_marker"];

fn master() -> PgConnectOptions {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL (scripts/with-test-pg.sh)");
    PgConnectOptions::from_str(&url).expect("DATABASE_URL")
}

async fn pool_as(user: &str, db: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(master().username(user).database(db))
        .await
        .unwrap_or_else(|e| panic!("connect {user}@{db}: {e}"))
}

/// Roles (serialised: they are cluster objects shared by parallel tests) and a fresh database
/// owned by the migrator, with the database-level script applied. Returns its name.
async fn provision() -> String {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(master())
        .await
        .expect("superuser");
    let mut tx = admin.begin().await.expect("tx");
    sqlx::query("SELECT pg_advisory_xact_lock(80008)")
        .execute(&mut *tx)
        .await
        .expect("lock");
    sqlx::raw_sql(ROLES_SQL)
        .execute(&mut *tx)
        .await
        .expect("roles.sql");
    tx.commit().await.expect("commit roles");
    let name = format!("itsm_cp8_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name} OWNER itsm_migrator"))
        .execute(&admin)
        .await
        .expect("create database");
    admin.close().await;
    let opts = master();
    let su = pool_as(opts.get_username(), &name).await;
    sqlx::raw_sql(DATABASE_SQL)
        .execute(&su)
        .await
        .expect("database.sql");
    su.close().await;
    name
}

async fn drop_db(name: &str) {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(master())
        .await
        .expect("superuser");
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .expect("drop database");
}

fn sqlstate(r: Result<impl std::fmt::Debug, sqlx::Error>) -> String {
    match r {
        Err(sqlx::Error::Database(e)) => e.code().map(|c| c.into_owned()).unwrap_or_default(),
        other => format!("not a database error: {other:?}"),
    }
}

async fn privilege(pool: &PgPool, table: &str, privilege: &str) -> bool {
    sqlx::query_scalar("SELECT has_table_privilege('itsm_app', $1, $2)")
        .bind(format!("public.{table}"))
        .bind(privilege)
        .fetch_one(pool)
        .await
        .expect("has_table_privilege")
}

/// Production configuration (file-only secrets, live integrations through the egress proxy).
fn production_config() -> Arc<Config> {
    let auth_file = std::env::temp_dir().join(format!("itsm-cp8-egress-{}", Uuid::new_v4()));
    std::fs::write(&auth_file, "itsm-api:synthetic-egress-pass-01").expect("egress auth file");
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "production"),
        ("ITSM_ACCESS_MODE", "password"),
        ("ITSM_COOKIE_SECURE", "true"),
        ("DATABASE_URL", "postgres://unused"),
        ("ITSM_EGRESS_PROXY", "http://egress-proxy:3128"),
        (
            "ITSM_EGRESS_PROXY_AUTH_FILE",
            auth_file.to_str().expect("path"),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let cfg = Config::from_map(&vars);
    let _ = std::fs::remove_file(&auth_file);
    Arc::new(cfg.expect("production config"))
}

struct Client {
    st: AppState,
    cookie: String,
    csrf: String,
}

impl Client {
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> (StatusCode, Value, Option<String>) {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("x-itsm-login", "1")
            .header("sec-fetch-site", "same-origin")
            .header("x-csrf-token", &self.csrf);
        if !self.cookie.is_empty() {
            r = r.header("cookie", &self.cookie);
        }
        if let Some(k) = key {
            r = r.header("idempotency-key", k);
        }
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let res = app(self.st.clone())
            .oneshot(r.body(body).expect("request"))
            .await
            .expect("response");
        let status = res.status();
        let cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_owned);
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, json, cookie)
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>, key: Option<&str>) -> Value {
        let (status, json, _) = self.send(method, path, body, key).await;
        assert!(
            status.is_success(),
            "{method} {path} as itsm_app: {status} {json}"
        );
        json
    }
}

#[tokio::test]
async fn migrations_run_as_owner_and_runtime_role_is_least_privilege() {
    let name = provision().await;
    let mig = pool_as("itsm_migrator", &name).await;
    let is_super: bool =
        sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&mig)
            .await
            .expect("rolsuper");
    assert!(!is_super, "migrations must run without superuser");
    db::MIGRATOR
        .run(&mig)
        .await
        .expect("all migrations as itsm_migrator");

    // ---- ownership and role attributes
    let foreign: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' \
         AND tableowner <> 'itsm_migrator' ORDER BY 1",
    )
    .fetch_all(&mig)
    .await
    .expect("owners");
    assert!(
        foreign.is_empty(),
        "tables not owned by migrator: {foreign:?}"
    );
    let attrs: Vec<(String, bool, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT rolname, rolsuper, rolcreaterole, rolcreatedb, rolbypassrls, rolreplication \
         FROM pg_roles WHERE rolname IN ('itsm_migrator', 'itsm_app', 'itsm_backup') ORDER BY 1",
    )
    .fetch_all(&mig)
    .await
    .expect("roles");
    assert_eq!(
        attrs,
        vec![
            ("itsm_app".into(), false, false, false, false, false),
            ("itsm_backup".into(), false, false, false, false, true),
            ("itsm_migrator".into(), false, false, false, false, false),
        ]
    );
    let memberships: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_auth_members m JOIN pg_roles r ON r.oid = m.member \
         WHERE r.rolname IN ('itsm_migrator', 'itsm_app', 'itsm_backup')",
    )
    .fetch_one(&mig)
    .await
    .expect("memberships");
    assert_eq!(memberships, 0, "no role inherits another");

    // ---- database and schema level
    let db_privs: (bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT has_database_privilege('itsm_app', current_database(), 'CONNECT'), \
                has_database_privilege('itsm_app', current_database(), 'TEMP'), \
                has_database_privilege('itsm_app', current_database(), 'CREATE'), \
                has_database_privilege('itsm_backup', current_database(), 'CONNECT'), \
                has_schema_privilege('itsm_app', 'public', 'CREATE')",
    )
    .fetch_one(&mig)
    .await
    .expect("database privileges");
    assert_eq!(db_privs, (true, false, false, false, false));
    let public_acl: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_database d, aclexplode(d.datacl) a \
         WHERE d.datname = current_database() AND a.grantee = 0",
    )
    .fetch_one(&mig)
    .await
    .expect("public acl");
    assert_eq!(public_acl, 0, "PUBLIC has no database privilege");

    // ---- table matrix: every public table, from the catalog (a new table is covered too)
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
    )
    .fetch_all(&mig)
    .await
    .expect("tables");
    assert!(tables.len() > 40, "{tables:?}");
    for t in &tables {
        let t = t.as_str();
        assert!(privilege(&mig, t, "SELECT").await, "{t}: SELECT");
        for p in ["TRUNCATE", "REFERENCES", "TRIGGER"] {
            assert!(!privilege(&mig, t, p).await, "{t}: {p} must be refused");
        }
        let (insert, update, delete) = if READ_ONLY.contains(&t) {
            (false, false, false)
        } else if NO_UPDATE.contains(&t) {
            (true, false, false)
        } else if NO_DELETE_ONLY.contains(&t) {
            (true, true, false)
        } else {
            (true, true, true)
        };
        assert_eq!(privilege(&mig, t, "INSERT").await, insert, "{t}: INSERT");
        assert_eq!(privilege(&mig, t, "UPDATE").await, update, "{t}: UPDATE");
        assert_eq!(privilege(&mig, t, "DELETE").await, delete, "{t}: DELETE");
    }
    // The lists above are exactly what the guard triggers refuse: a new guarded table must be
    // added here deliberately (and is already revoked by itsm_grant_app()).
    let guarded: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT c.relname::text, bool_or(tg.tgtype & 16 <> 0), bool_or(tg.tgtype & 8 <> 0) \
         FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid \
         JOIN pg_class c ON c.oid = tg.tgrelid \
         WHERE NOT tg.tgisinternal AND (p.proname LIKE 'itsm\\_%append\\_only' \
                OR p.proname = 'itsm_workspace_code_history_fixed') \
         GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(&mig)
    .await
    .expect("guards");
    let mut expected: Vec<(String, bool, bool)> = NO_UPDATE
        .iter()
        .map(|t| ((*t).to_owned(), true, true))
        .chain(
            NO_DELETE_ONLY
                .iter()
                .map(|t| ((*t).to_owned(), false, true)),
        )
        .collect();
    expected.sort();
    assert_eq!(guarded, expected);

    // ---- the runtime role, connected: refused at the privilege check (42501), not by triggers
    let app_pool = pool_as("itsm_app", &name).await;
    for sql in [
        "CREATE TABLE cp8_probe (id int)",
        "CREATE TEMP TABLE cp8_probe (id int)",
        "CREATE FUNCTION cp8_probe() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        "CREATE INDEX cp8_probe ON tickets (id)",
        "ALTER TABLE tickets ADD COLUMN cp8_probe int",
        "DROP TABLE tickets",
        "TRUNCATE tickets",
        "TRUNCATE audit_events",
        "UPDATE audit_events SET action = action",
        "DELETE FROM ticket_activities",
        "DELETE FROM attachments",
        "DELETE FROM workspace_grants",
        "INSERT INTO itsm_deployment_marker (kind, marked_db) VALUES ('operational', 'x')",
        "UPDATE _sqlx_migrations SET success = true",
        "DELETE FROM _sqlx_migrations",
        "SET ROLE itsm_migrator",
        "ALTER ROLE itsm_app SUPERUSER",
    ] {
        assert_eq!(
            sqlstate(sqlx::query(sql).execute(&app_pool).await),
            "42501",
            "{sql}"
        );
    }
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&app_pool)
        .await
        .expect("app reads the migration state");
    let ups = db::MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .count();
    assert_eq!(applied, ups as i64);

    // ---- first administrator: the app role cannot mark the database operational; the migrator can
    let cfg = production_config();
    assert!(
        db::verify_runtime_guard(&cfg, &app_pool).await.is_err(),
        "unmarked database refused"
    );
    let refused =
        password_bootstrap::bootstrap(&app_pool, &cfg, ADMIN, &name, ADMIN_PASSWORD).await;
    assert!(refused.is_err(), "init-password as itsm_app must fail");
    let leftovers: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM itsm_deployment_marker), (SELECT count(*) FROM users)",
    )
    .fetch_one(&mig)
    .await
    .expect("leftovers");
    assert_eq!(leftovers, (0, 0), "refused bootstrap leaves nothing behind");
    password_bootstrap::bootstrap(&mig, &cfg, ADMIN, &name, ADMIN_PASSWORD)
        .await
        .expect("init-password as itsm_migrator");
    db::verify_runtime_guard(&cfg, &app_pool)
        .await
        .expect("operational database accepted for the app role");

    // ---- core flows through the real router, every query as itsm_app
    let st = AppState {
        pool: app_pool.clone(),
        config: cfg.clone(),
    };
    let anon = Client {
        st: st.clone(),
        cookie: String::new(),
        csrf: String::new(),
    };
    let (status, session, cookie) = anon
        .send(
            "POST",
            "/api/v1/login",
            Some(json!({"username": ADMIN, "password": ADMIN_PASSWORD})),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let c = Client {
        st: st.clone(),
        cookie: cookie.expect("session cookie"),
        csrf: session["csrfToken"].as_str().expect("csrf").to_owned(),
    };
    let list = c.ok("GET", "/api/v1/workspaces", None, None).await;
    let ws = list["items"]
        .as_array()
        .and_then(|a| a.iter().find(|w| w["code"] == "DOB"))
        .and_then(|w| w["id"].as_str())
        .expect("DOB workspace")
        .to_owned();
    let channel: Uuid = sqlx::query_scalar(
        "SELECT id FROM intake_channels WHERE workspace_id = $1::uuid ORDER BY sort_order LIMIT 1",
    )
    .bind(&ws)
    .fetch_one(&app_pool)
    .await
    .expect("intake channel");
    let w = format!("/api/v1/workspaces/{ws}");
    let created = c
        .ok(
            "POST",
            &format!("{w}/tickets"),
            Some(json!({
                "subject": "Pengaduan uji CP8 (dummy)",
                "chronology": "Kronologi uji CP8 (dummy) dengan peran itsm_app.",
                "receivedAt": "2026-11-12T09:15:00+07:00",
                "intakeChannelId": channel,
            })),
            Some("cp8-roles-ticket-1"),
        )
        .await;
    let t = created["ticket"]["id"]
        .as_str()
        .expect("ticket id")
        .to_owned();
    let tb = format!("{w}/tickets/{t}");
    c.ok(
        "POST",
        &format!("{tb}/notes"),
        Some(json!({"body": "Catatan uji CP8 (dummy)."})),
        Some("cp8-roles-note-1"),
    )
    .await;
    let version = |pool: PgPool, id: String| async move {
        sqlx::query_scalar::<_, i32>("SELECT version FROM tickets WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("version")
    };
    let v = version(app_pool.clone(), t.clone()).await;
    c.ok(
        "POST",
        &format!("{tb}/transitions"),
        Some(json!({"to": "resolved", "version": v, "summary": "Ringkasan uji CP8 (dummy)"})),
        None,
    )
    .await;
    let v = version(app_pool.clone(), t.clone()).await;
    c.ok(
        "POST",
        &format!("{tb}/transitions"),
        Some(json!({"to": "open", "version": v, "reason": "Dibuka ulang uji CP8 (dummy)"})),
        None,
    )
    .await;
    for path in [
        format!("{w}/tickets"),
        format!("{w}/tickets/board"),
        format!("{tb}/detail"),
        format!("{tb}/activity"),
        format!("{tb}/notes"),
        format!("{w}/dashboard"),
        format!("{w}/report-runs"),
        "/api/v1/users".to_owned(),
    ] {
        c.ok("GET", &path, None, None).await;
    }
    c.ok(
        "POST",
        &format!("{w}/report-runs"),
        Some(json!({"reportType": "operational", "periodStart": "2026-11-01", "periodEnd": "2026-11-30", "basis": "received"})),
        Some("cp8-roles-report-1"),
    )
    .await;
    c.ok(
        "POST",
        "/api/v1/users",
        Some(json!({"username": "cs.cp8", "displayName": "CS CP8 (dummy)"})),
        None,
    )
    .await;
    c.ok("DELETE", "/api/v1/session", None, None).await;

    // ---- worker tick (sweep, prunes, claims) as itsm_app
    outbox::run_once(&st).await.expect("worker tick");
    outbox::prune_webhook_events(&st)
        .await
        .expect("webhook prune");
    shared_limit::prune(&app_pool)
        .await
        .expect("rate limit prune");

    let audited: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM audit_events), (SELECT count(*) FROM auth_events), \
                (SELECT count(*) FROM ticket_resolution_events)",
    )
    .fetch_one(&app_pool)
    .await
    .expect("audit");
    assert!(
        audited.0 > 0 && audited.1 > 0 && audited.2 > 0,
        "{audited:?}"
    );

    app_pool.close().await;
    mig.close().await;
    drop_db(&name).await;
}

#[tokio::test]
async fn tightening_migration_rolls_back_and_reapplies_as_migrator() {
    let name = provision().await;
    let mig = pool_as("itsm_migrator", &name).await;
    let app_pool = pool_as("itsm_app", &name).await;
    let first = db::MIGRATOR.iter().next().expect("migrations").version;
    // serve/worker refuse an unmigrated schema, and the runtime role cannot migrate it.
    assert!(matches!(
        db::verify_schema(&app_pool).await,
        Err(db::SchemaError::Pending(v)) if v == first
    ));
    // On its own connection: a failed run keeps the migrator advisory lock until it closes.
    let mut conn = sqlx::PgConnection::connect_with(&master().username("itsm_app").database(&name))
        .await
        .expect("app connection");
    assert!(
        db::MIGRATOR.run(&mut conn).await.is_err(),
        "itsm_app cannot migrate"
    );
    conn.close().await.expect("close");
    db::MIGRATOR.run(&mig).await.expect("migrate");
    db::verify_schema(&app_pool).await.expect("schema current");
    for t in ["attachments", "itsm_deployment_marker", "_sqlx_migrations"] {
        assert!(!privilege(&mig, t, "DELETE").await, "{t}");
    }
    db::MIGRATOR.undo(&mig, 31).await.expect("undo 0032");
    assert!(matches!(
        db::verify_schema(&app_pool).await,
        Err(db::SchemaError::Pending(32))
    ));
    for t in ["attachments", "itsm_deployment_marker", "_sqlx_migrations"] {
        assert!(privilege(&mig, t, "DELETE").await, "{t} after down");
    }
    // 0018's revocations stay after the down migration.
    assert!(!privilege(&mig, "audit_events", "UPDATE").await);
    db::MIGRATOR.run(&mig).await.expect("re-apply 0032");
    for t in ["attachments", "itsm_deployment_marker", "_sqlx_migrations"] {
        assert!(!privilege(&mig, t, "DELETE").await, "{t} after re-apply");
    }
    db::verify_schema(&app_pool)
        .await
        .expect("schema current again");
    // A newer migration than this binary knows (next release, expand phase) is accepted; a
    // changed or failed one is not.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (99990001, 'next release', true, '\\x00', 1)",
    )
    .execute(&mig)
    .await
    .expect("newer migration row");
    db::verify_schema(&app_pool)
        .await
        .expect("newer schema accepted");
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 31")
        .execute(&mig)
        .await
        .expect("tamper checksum");
    assert!(matches!(
        db::verify_schema(&app_pool).await,
        Err(db::SchemaError::Mismatch(31))
    ));
    sqlx::query("UPDATE _sqlx_migrations SET success = false WHERE version = 99990001")
        .execute(&mig)
        .await
        .expect("dirty row");
    assert!(matches!(
        db::verify_schema(&app_pool).await,
        Err(db::SchemaError::Dirty(99990001))
    ));
    app_pool.close().await;
    mig.close().await;
    drop_db(&name).await;
}

/// Re-running the helper (e.g. after roles are created late, or after a manual grant) repairs
/// drift and covers a table guarded after 0032 without editing the explicit lists.
#[tokio::test]
async fn grant_app_repairs_drift_and_covers_new_guarded_tables() {
    let name = provision().await;
    let mig = pool_as("itsm_migrator", &name).await;
    db::MIGRATOR.run(&mig).await.expect("migrate");
    let su = pool_as(master().get_username(), &name).await;
    sqlx::raw_sql(
        "GRANT TRUNCATE ON tickets TO itsm_app; GRANT CREATE ON SCHEMA public TO itsm_app",
    )
    .execute(&su)
    .await
    .expect("inject drift");
    sqlx::raw_sql(
        "CREATE TABLE cp8_new_log (id int); \
         CREATE TRIGGER cp8_new_log_guard BEFORE UPDATE OR DELETE ON cp8_new_log \
             FOR EACH ROW EXECUTE FUNCTION itsm_append_only(); \
         CREATE TABLE cp8_new_keep (id int); \
         CREATE TRIGGER cp8_new_keep_guard BEFORE DELETE ON cp8_new_keep \
             FOR EACH STATEMENT EXECUTE FUNCTION itsm_append_only(); \
         INSERT INTO cp8_new_keep VALUES (1)",
    )
    .execute(&mig)
    .await
    .expect("new guarded tables as migrator");
    let schema_create = |pool: PgPool| async move {
        sqlx::query_scalar::<_, bool>("SELECT has_schema_privilege('itsm_app', 'public', 'CREATE')")
            .fetch_one(&pool)
            .await
            .expect("schema privilege")
    };
    // The drift is real before the repair (default privileges grant DML on new tables).
    assert!(privilege(&mig, "tickets", "TRUNCATE").await);
    assert!(schema_create(mig.clone()).await);
    assert!(privilege(&mig, "cp8_new_log", "UPDATE").await);
    assert!(privilege(&mig, "cp8_new_keep", "DELETE").await);

    sqlx::query("SELECT itsm_grant_app()")
        .execute(&mig)
        .await
        .expect("itsm_grant_app as migrator");
    assert!(!privilege(&mig, "tickets", "TRUNCATE").await, "TRUNCATE");
    assert!(!schema_create(mig.clone()).await, "schema CREATE");
    let matrix = |t: &'static str| {
        let mig = mig.clone();
        async move {
            (
                privilege(&mig, t, "INSERT").await,
                privilege(&mig, t, "UPDATE").await,
                privilege(&mig, t, "DELETE").await,
            )
        }
    };
    assert_eq!(matrix("cp8_new_log").await, (true, false, false));
    assert_eq!(matrix("cp8_new_keep").await, (true, true, false));

    let app_pool = pool_as("itsm_app", &name).await;
    for sql in [
        "TRUNCATE tickets",
        "CREATE TABLE cp8_probe (id int)",
        "UPDATE cp8_new_log SET id = id",
        "DELETE FROM cp8_new_keep",
    ] {
        assert_eq!(
            sqlstate(sqlx::query(sql).execute(&app_pool).await),
            "42501",
            "{sql}"
        );
    }
    sqlx::query("UPDATE cp8_new_keep SET id = 2")
        .execute(&app_pool)
        .await
        .expect("UPDATE stays allowed where only DELETE is guarded");
    app_pool.close().await;
    su.close().await;
    mig.close().await;
    drop_db(&name).await;
}
