//! D02 domain schema against a real PostgreSQL: per-workspace numbering, cross-workspace
//! rejection by composite FKs, ticket lifecycle guards, related work, outbox transitions,
//! attachment binding, scoped HTTP reads (incl. attachment/report metadata) and the
//! 0002 rollback on dummy data. Fixtures are synthetic; no external system is contacted.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use itsm_api::config::Config;
use itsm_api::error::AppError;
use itsm_api::{AppState, app, db, seed, session};
use serde_json::Value;
use sqlx::{PgExecutor, PgPool};
use tower::ServiceExt;
use uuid::Uuid;

// ---------- helpers ----------

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

/// SQLSTATE of a failed statement, or None when it succeeded.
fn sqlstate<T>(r: &Result<T, sqlx::Error>) -> Option<String> {
    match r {
        Ok(_) => None,
        Err(sqlx::Error::Database(db)) => db.code().map(|c| c.into_owned()),
        Err(e) => Some(format!("non-db error: {e}")),
    }
}

const FK: &str = "23503";
const CHECK: &str = "23514";
const RULE: &str = "IT422";
const VERSION: &str = "IT409";

fn state(pool: &PgPool) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    AppState {
        config: Arc::new(ok(Config::from_map(&vars), "test config")),
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

async fn catalog_id(pool: &PgPool, table: &str, ws: Uuid, code: &str) -> Uuid {
    let sql = format!("SELECT id FROM {table} WHERE workspace_id = $1 AND code = $2");
    ok(
        sqlx::query_scalar(&sql)
            .bind(ws)
            .bind(code)
            .fetch_one(pool)
            .await,
        &format!("{table}.{code}"),
    )
}

async fn connection(pool: &PgPool, ws: Uuid, provider: &str) -> Uuid {
    ok(
        sqlx::query_scalar(
            "SELECT id FROM integration_connections WHERE workspace_id = $1 AND provider = $2",
        )
        .bind(ws)
        .bind(provider)
        .fetch_one(pool)
        .await,
        "connection",
    )
}

async fn lark_destination(pool: &PgPool, ws: Uuid) -> Uuid {
    ok(
        sqlx::query_scalar("SELECT id FROM lark_destinations WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(pool)
            .await,
        "lark destination",
    )
}

async fn try_ticket<'e>(
    exec: impl PgExecutor<'e>,
    ws: Uuid,
    creator: Uuid,
    owner: Option<Uuid>,
) -> Result<(Uuid, i64), sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, owner_user_id) \
         VALUES ($1, 'Pengaduan uji', 'Kronologi uji (dummy)', now(), $2, $3) RETURNING id, number",
    )
    .bind(ws)
    .bind(creator)
    .bind(owner)
    .fetch_one(exec)
    .await
}

async fn ticket(pool: &PgPool, ws: Uuid, creator: Uuid) -> (Uuid, i64) {
    ok(try_ticket(pool, ws, creator, None).await, "insert ticket")
}

async fn set_status(pool: &PgPool, t: Uuid, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE tickets SET workflow_status = $2, version = version + 1 WHERE id = $1")
        .bind(t)
        .bind(status)
        .execute(pool)
        .await
        .map(|_| ())
}

async fn status_of(pool: &PgPool, t: Uuid) -> (String, i32, i32) {
    ok(
        sqlx::query_as("SELECT workflow_status, cycle_no, version FROM tickets WHERE id = $1")
            .bind(t)
            .fetch_one(pool)
            .await,
        "status",
    )
}

async fn event(
    pool: &PgPool,
    ws: Uuid,
    t: Uuid,
    cycle: i32,
    action: &str,
    actor: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO ticket_resolution_events (workspace_id, ticket_id, cycle_no, action, summary, actor_id) \
         VALUES ($1, $2, $3, $4, 'Ringkasan uji (dummy)', $5)",
    )
    .bind(ws)
    .bind(t)
    .bind(cycle)
    .bind(action)
    .bind(actor)
    .execute(pool)
    .await
    .map(|_| ())
}

struct Fx {
    a: Uuid,
    b: Uuid,
    admin: Uuid,
    cs: Uuid,
}

async fn fixture(pool: &PgPool) -> Fx {
    seeded(pool).await;
    Fx {
        a: ws(pool, "DUMA").await,
        b: ws(pool, "DUMB").await,
        admin: user(pool, "dummy:uat-superadmin").await,
        cs: user(pool, "dummy:cs-alfa").await,
    }
}

// ---------- numbering ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn ticket_numbers_are_sequential_per_workspace(pool: PgPool) {
    let f = fixture(&pool).await;
    let counters: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspace_ticket_counters")
            .fetch_one(&pool)
            .await,
        "counters",
    );
    assert_eq!(counters, 2, "every workspace gets a counter on insert");

    assert_eq!(ticket(&pool, f.a, f.admin).await.1, 1);
    assert_eq!(ticket(&pool, f.a, f.admin).await.1, 2);
    assert_eq!(ticket(&pool, f.b, f.admin).await.1, 1);

    // A client-supplied number is ignored; the counter decides.
    let forced: i64 = ok(
        sqlx::query_scalar(
            "INSERT INTO tickets (workspace_id, number, subject, chronology, received_at, created_by) \
             VALUES ($1, 999, 's', 'c', now(), $2) RETURNING number",
        )
        .bind(f.a)
        .bind(f.admin)
        .fetch_one(&pool)
        .await,
        "forced number",
    );
    assert_eq!(forced, 3);

    // A rolled-back insert does not consume a number.
    let mut tx = ok(pool.begin().await, "begin");
    ok(
        try_ticket(&mut *tx, f.a, f.admin, None).await,
        "insert in tx",
    );
    ok(tx.rollback().await, "rollback");
    assert_eq!(ticket(&pool, f.a, f.admin).await.1, 4);

    // Number cannot be rewritten afterwards.
    let (t, _) = ticket(&pool, f.b, f.admin).await;
    let r = sqlx::query("UPDATE tickets SET number = 50, version = version + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn concurrent_inserts_get_unique_numbers(pool: PgPool) {
    let f = fixture(&pool).await;
    let mut handles = Vec::new();
    for _ in 0..12 {
        let p = pool.clone();
        let (a, admin) = (f.a, f.admin);
        handles.push(tokio::spawn(async move {
            try_ticket(&p, a, admin, None).await.map(|(_, n)| n)
        }));
    }
    let mut numbers = BTreeSet::new();
    for h in handles {
        let n = ok(ok(h.await, "join"), "concurrent insert");
        assert!(numbers.insert(n), "duplicate number {n}");
    }
    assert_eq!(numbers, (1..=12).collect::<BTreeSet<i64>>());
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn workspace_code_rename_keeps_old_numbers_and_reserves_codes(pool: PgPool) {
    let f = fixture(&pool).await;
    let prefix = |id: Uuid| {
        let pool = pool.clone();
        async move {
            ok(
                sqlx::query_scalar::<_, String>("SELECT number_prefix FROM tickets WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await,
                "prefix",
            )
        }
    };
    let rename = |ws: Uuid, code: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query("UPDATE workspaces SET code = $2 WHERE id = $1")
                .bind(ws)
                .bind(code)
                .execute(&pool)
                .await
                .map(|_| ())
        }
    };

    let (old, n1) = ticket(&pool, f.a, f.admin).await;
    assert_eq!((prefix(old).await.as_str(), n1), ("DUMA", 1));

    // D17: the business code may change after numbers exist; old tickets keep theirs.
    assert!(rename(f.a, "DUMY").await.is_ok());
    assert_eq!(prefix(old).await, "DUMA", "history is never renumbered");
    let (new, n2) = ticket(&pool, f.a, f.admin).await;
    assert_eq!(
        (prefix(new).await.as_str(), n2),
        ("DUMY", 2),
        "counter continues"
    );

    // Neither the current nor a historical code can move to another workspace.
    for code in ["DUMA", "DUMY"] {
        let r = rename(f.b, code).await;
        assert_eq!(sqlstate(&r).as_deref(), Some("23505"), "{code} reserved");
    }
    // The owner may return to its own earlier code; numbering still continues.
    assert!(rename(f.a, "DUMA").await.is_ok());
    let (back, n3) = ticket(&pool, f.a, f.admin).await;
    assert_eq!((prefix(back).await.as_str(), n3), ("DUMA", 3));
    let codes: Vec<String> = ok(
        sqlx::query_scalar(
            "SELECT code FROM workspace_code_history WHERE workspace_id = $1 ORDER BY code",
        )
        .bind(f.a)
        .fetch_all(&pool)
        .await,
        "history",
    );
    assert_eq!(codes, ["DUMA", "DUMY"]);

    // Format is still enforced by the database.
    assert!(rename(f.a, "du-ma").await.is_err());

    // The stamped prefix and the reservation history are immutable.
    let r = sqlx::query(
        "UPDATE tickets SET number_prefix = 'DUMY', version = version + 1 WHERE id = $1",
    )
    .bind(old)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE));
    for sql in [
        "DELETE FROM workspace_code_history WHERE code = 'DUMY'",
        "UPDATE workspace_code_history SET workspace_id = workspace_id WHERE code = 'DUMY'",
    ] {
        let r = sqlx::query(sql).execute(&pool).await;
        assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "{sql}");
    }

    // Company and workspace names stay editable.
    let rename =
        sqlx::query("UPDATE workspaces SET name = 'PT Dummy Alfa Baru (uji)' WHERE id = $1")
            .bind(f.a)
            .execute(&pool)
            .await;
    assert!(rename.is_ok());
    let company = sqlx::query(
        "UPDATE companies SET name = 'PT Dummy Alfa Baru (uji)' \
         WHERE id = (SELECT company_id FROM workspaces WHERE id = $1)",
    )
    .bind(f.a)
    .execute(&pool)
    .await;
    assert!(company.is_ok());
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn concurrent_code_changes_serialise(pool: PgPool) {
    let f = fixture(&pool).await;

    // Two workspaces racing for the same new code: exactly one wins.
    let mut ta = ok(pool.begin().await, "begin a");
    ok(
        sqlx::query("UPDATE workspaces SET code = 'RACE' WHERE id = $1")
            .bind(f.a)
            .execute(&mut *ta)
            .await,
        "a takes RACE",
    );
    let p = pool.clone();
    let b = f.b;
    let other = tokio::spawn(async move {
        sqlx::query("UPDATE workspaces SET code = 'RACE' WHERE id = $1")
            .bind(b)
            .execute(&p)
            .await
            .map(|_| ())
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!other.is_finished(), "second rename waits for the first");
    ok(ta.commit().await, "commit a");
    let r = ok(other.await, "join");
    assert_eq!(sqlstate(&r).as_deref(), Some("23505"));

    // A ticket created while a rename is in flight waits and gets the committed code;
    // the counter neither resets nor duplicates.
    let (_, n1) = ticket(&pool, f.a, f.admin).await;
    let mut tr = ok(pool.begin().await, "begin rename");
    ok(
        sqlx::query("UPDATE workspaces SET code = 'RACF' WHERE id = $1")
            .bind(f.a)
            .execute(&mut *tr)
            .await,
        "rename in tx",
    );
    let p = pool.clone();
    let (a, admin) = (f.a, f.admin);
    let insert = tokio::spawn(async move {
        let (id, n) = try_ticket(&p, a, admin, None).await?;
        let pre: String = sqlx::query_scalar("SELECT number_prefix FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(&p)
            .await?;
        Ok::<_, sqlx::Error>((pre, n))
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!insert.is_finished(), "insert waits for the rename");
    ok(tr.commit().await, "commit rename");
    let (pre, n2) = ok(ok(insert.await, "join"), "insert");
    assert_eq!(pre, "RACF");
    assert_eq!(n2, n1 + 1);
    let dupes: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM (SELECT number FROM tickets WHERE workspace_id = $1 \
             GROUP BY number HAVING count(*) > 1) d",
        )
        .bind(f.a)
        .fetch_one(&pool)
        .await,
        "dupes",
    );
    assert_eq!(dupes, 0);
}

// ---------- isolation ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cross_workspace_references_are_rejected_by_the_database(pool: PgPool) {
    let f = fixture(&pool).await;
    let (ta, _) = ticket(&pool, f.a, f.admin).await;
    let (tb, _) = ticket(&pool, f.b, f.admin).await;

    // Ticket in B pointing at A's catalogs / merchant.
    for (col, table, code) in [
        ("priority_id", "priorities", "normal"),
        ("category_id", "categories", "kategori_uji"),
        ("product_id", "products", "produk_uji"),
        ("receiving_unit_id", "receiving_units", "unit_uji"),
        ("region_id", "regions", "wilayah_uji"),
        ("intake_channel_id", "intake_channels", "whatsapp"),
    ] {
        let foreign = catalog_id(&pool, table, f.a, code).await;
        let sql = format!(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, {col}) \
             VALUES ($1, 's', 'c', now(), $2, $3)"
        );
        let r = sqlx::query(&sql)
            .bind(f.b)
            .bind(f.admin)
            .bind(foreign)
            .execute(&pool)
            .await;
        assert_eq!(sqlstate(&r).as_deref(), Some(FK), "{col}");
        let update = format!("UPDATE tickets SET {col} = $2, version = version + 1 WHERE id = $1");
        let r = sqlx::query(&update)
            .bind(tb)
            .bind(foreign)
            .execute(&pool)
            .await;
        assert_eq!(sqlstate(&r).as_deref(), Some(FK), "update {col}");
    }
    let merchant_a: Uuid = ok(
        sqlx::query_scalar("SELECT id FROM merchants WHERE workspace_id = $1")
            .bind(f.a)
            .fetch_one(&pool)
            .await,
        "merchant",
    );
    let r = sqlx::query("UPDATE tickets SET merchant_id = $2, version = version + 1 WHERE id = $1")
        .bind(tb)
        .bind(merchant_a)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "merchant");

    // PIC from A (cs-alfa is a member of A only) on a B ticket.
    let r = try_ticket(&pool, f.b, f.admin, Some(f.cs)).await;
    // The active-member trigger fires before the composite FK is checked.
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "owner");

    // Children of A's ticket created under B.
    let r = sqlx::query(
        "INSERT INTO related_work (workspace_id, ticket_id, title, requested_by) VALUES ($1, $2, 't', $3)",
    )
    .bind(f.b)
    .bind(ta)
    .bind(f.admin)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "related_work");
    let r = event(&pool, f.b, ta, 1, "resolve", f.admin).await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "resolution event");
    let r = sqlx::query(
        "INSERT INTO attachments (workspace_id, ticket_id, uploaded_by, object_key, file_name, size_bytes, scan_status, bound_at) \
         VALUES ($1, $2, $3, 'quarantine/x1', 'bukti.pdf', 10, 'clean', now())",
    )
    .bind(f.b)
    .bind(ta)
    .bind(f.admin)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "attachment");

    // Integration relations: B rows pointing at A's connection / destination.
    let jira_a = connection(&pool, f.a, "jira").await;
    let r = sqlx::query(
        "INSERT INTO jira_issues (workspace_id, connection_id, external_issue_id, issue_key) \
         VALUES ($1, $2, '10001', 'DUM-1')",
    )
    .bind(f.b)
    .bind(jira_a)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "jira issue");
    let lark_b = connection(&pool, f.b, "lark").await;
    let dest_a = lark_destination(&pool, f.a).await;
    let r = sqlx::query(
        "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, lark_destination_id, idempotency_key) \
         VALUES ($1, $2, $3, 'lark', 'lark_ticket_created', $4, 'idem-cross-1')",
    )
    .bind(f.b)
    .bind(tb)
    .bind(lark_b)
    .bind(dest_a)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "lark destination");
    // A Jira connection cannot be used as a Lark connection (provider pinned by FK).
    let jira_b = connection(&pool, f.b, "jira").await;
    let r = sqlx::query(
        "INSERT INTO lark_destinations (workspace_id, connection_id, external_chat_ref, display_name) \
         VALUES ($1, $2, 'chat-x', 'x')",
    )
    .bind(f.b)
    .bind(jira_b)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(FK), "provider pinned");

    // Moving a row to another workspace is refused.
    let r =
        sqlx::query("UPDATE tickets SET workspace_id = $2, version = version + 1 WHERE id = $1")
            .bind(ta)
            .bind(f.b)
            .execute(&pool)
            .await;
    assert!(sqlstate(&r).is_some(), "ticket move must fail");
    let run = report_run(&pool, f.a, f.admin).await;
    let r = sqlx::query("UPDATE report_runs SET workspace_id = $2 WHERE id = $1")
        .bind(run)
        .bind(f.b)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "report run move");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn owner_must_be_an_active_member(pool: PgPool) {
    let f = fixture(&pool).await;
    assert!(try_ticket(&pool, f.a, f.admin, Some(f.cs)).await.is_ok());

    ok(
        sqlx::query("UPDATE workspace_memberships SET active = false WHERE user_id = $1")
            .bind(f.cs)
            .execute(&pool)
            .await,
        "deactivate membership",
    );
    let r = try_ticket(&pool, f.a, f.admin, Some(f.cs)).await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "inactive membership");

    let lead = user(&pool, "dummy:lead-alfa").await;
    ok(
        sqlx::query("UPDATE users SET active = false WHERE id = $1")
            .bind(lead)
            .execute(&pool)
            .await,
        "deactivate user",
    );
    let (t, _) = ticket(&pool, f.a, f.admin).await;
    let r =
        sqlx::query("UPDATE tickets SET owner_user_id = $2, version = version + 1 WHERE id = $1")
            .bind(t)
            .bind(lead)
            .execute(&pool)
            .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "inactive user");
}

// ---------- lifecycle ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn resolve_requires_summary_and_no_open_blocking_work(pool: PgPool) {
    let f = fixture(&pool).await;
    for bad in ["resolved", "closed"] {
        let r = sqlx::query(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, workflow_status) \
             VALUES ($1, 's', 'c', now(), $2, $3)",
        )
        .bind(f.a)
        .bind(f.admin)
        .bind(bad)
        .execute(&pool)
        .await;
        assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "insert {bad}");
    }
    let (t, _) = ticket(&pool, f.a, f.admin).await;
    for s in ["open", "pending_customer", "pending_engineering", "open"] {
        assert!(set_status(&pool, t, s).await.is_ok(), "to {s}");
    }
    let r = set_status(&pool, t, "resolved").await;
    assert_eq!(
        sqlstate(&r).as_deref(),
        Some(RULE),
        "resolve without summary"
    );

    let rw: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO related_work (workspace_id, ticket_id, title, blocking, requested_by) \
             VALUES ($1, $2, 'Perbaikan uji', true, $3) RETURNING id",
        )
        .bind(f.a)
        .bind(t)
        .bind(f.admin)
        .fetch_one(&pool)
        .await,
        "related work",
    );
    assert!(event(&pool, f.a, t, 1, "resolve", f.admin).await.is_ok());
    let r = set_status(&pool, t, "resolved").await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "open blocking work");

    // Jira "Done" on the linked issue changes neither the work nor the ticket.
    let jira = connection(&pool, f.a, "jira").await;
    let issue: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO jira_issues (workspace_id, connection_id, external_issue_id, issue_key, external_status) \
             VALUES ($1, $2, 'sim-1', 'SIM-1', 'To Do') RETURNING id",
        )
        .bind(f.a)
        .bind(jira)
        .fetch_one(&pool)
        .await,
        "jira issue",
    );
    assert!(
        sqlx::query(
            "UPDATE related_work SET jira_issue_id = $2, version = version + 1 WHERE id = $1"
        )
        .bind(rw)
        .bind(issue)
        .execute(&pool)
        .await
        .is_ok()
    );
    assert!(
        sqlx::query("UPDATE jira_issues SET external_status = 'Done' WHERE id = $1")
            .bind(issue)
            .execute(&pool)
            .await
            .is_ok()
    );
    let state: String = ok(
        sqlx::query_scalar("SELECT completion_state FROM related_work WHERE id = $1")
            .bind(rw)
            .fetch_one(&pool)
            .await,
        "completion",
    );
    assert_eq!(state, "open");
    assert_eq!(status_of(&pool, t).await.0, "open");
    let r = set_status(&pool, t, "resolved").await;
    assert_eq!(
        sqlstate(&r).as_deref(),
        Some(RULE),
        "Jira Done is not completion"
    );

    // Local completion needs summary + evidence + actor + time.
    let r = sqlx::query(
        "UPDATE related_work SET completion_state = 'completed', version = version + 1 WHERE id = $1",
    )
    .bind(rw)
    .execute(&pool)
    .await;
    assert_eq!(
        sqlstate(&r).as_deref(),
        Some(CHECK),
        "completion without evidence"
    );
    assert!(
        sqlx::query(
            "UPDATE related_work SET completion_state = 'completed', completion_summary = 'Sudah diperbaiki (uji)', \
             completion_evidence = 'Bukti uji (dummy)', completed_by = $2, completed_at = now(), \
             version = version + 1 WHERE id = $1",
        )
        .bind(rw)
        .bind(f.cs)
        .execute(&pool)
        .await
        .is_ok()
    );
    assert!(set_status(&pool, t, "resolved").await.is_ok());
    assert_eq!(status_of(&pool, t).await.0, "resolved");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn reopen_starts_next_cycle_and_close_is_held(pool: PgPool) {
    let f = fixture(&pool).await;
    let (t, _) = ticket(&pool, f.a, f.admin).await;
    assert!(event(&pool, f.a, t, 1, "resolve", f.admin).await.is_ok());
    assert!(set_status(&pool, t, "resolved").await.is_ok());

    let r = set_status(&pool, t, "closed").await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "close final held");

    // Leaving resolved: only to open, next cycle, with a recorded reopen reason.
    let r = sqlx::query(
        "UPDATE tickets SET workflow_status = 'open', cycle_no = 2, version = version + 1 WHERE id = $1",
    )
    .bind(t)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "reopen without reason");
    let r = event(&pool, f.a, t, 3, "reopen", f.admin).await;
    assert_eq!(
        sqlstate(&r).as_deref(),
        Some(RULE),
        "reopen skipping a cycle"
    );
    assert!(event(&pool, f.a, t, 2, "reopen", f.admin).await.is_ok());
    let r = sqlx::query(
        "UPDATE tickets SET workflow_status = 'pending_customer', cycle_no = 2, version = version + 1 WHERE id = $1",
    )
    .bind(t)
    .execute(&pool)
    .await;
    assert_eq!(
        sqlstate(&r).as_deref(),
        Some(RULE),
        "reopen must go to open"
    );
    assert!(
        sqlx::query(
            "UPDATE tickets SET workflow_status = 'open', cycle_no = 2, version = version + 1 WHERE id = $1",
        )
        .bind(t)
        .execute(&pool)
        .await
        .is_ok()
    );
    assert_eq!(status_of(&pool, t).await, ("open".to_owned(), 2, 3));

    // Cycle never changes outside a reopen; history is append-only.
    let r = sqlx::query("UPDATE tickets SET cycle_no = 5, version = version + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE));
    let r =
        sqlx::query("UPDATE ticket_resolution_events SET summary = 'ubah' WHERE ticket_id = $1")
            .bind(t)
            .execute(&pool)
            .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE));
    let r = sqlx::query("DELETE FROM ticket_resolution_events WHERE ticket_id = $1")
        .bind(t)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE));

    // A closed row (historical archive) is read-only; it cannot be produced by updates.
    let r = set_status(&pool, t, "closed").await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn stale_version_maps_to_version_conflict(pool: PgPool) {
    let f = fixture(&pool).await;
    let (t, _) = ticket(&pool, f.a, f.admin).await;
    let r = sqlx::query("UPDATE tickets SET subject = 'baru' WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(VERSION));
    let Err(e) = r else {
        panic!("update without version bump succeeded");
    };
    assert!(matches!(AppError::from(e), AppError::VersionConflict));

    // Optimistic update with the expected version touches exactly one row once.
    let first = ok(
        sqlx::query(
            "UPDATE tickets SET subject = 'baru', version = version + 1 WHERE id = $1 AND version = 1",
        )
        .bind(t)
        .execute(&pool)
        .await,
        "first update",
    );
    let second = ok(
        sqlx::query(
            "UPDATE tickets SET subject = 'lagi', version = version + 1 WHERE id = $1 AND version = 1",
        )
        .bind(t)
        .execute(&pool)
        .await,
        "stale update",
    );
    assert_eq!((first.rows_affected(), second.rows_affected()), (1, 0));
}

// ---------- outbox, attachments, catalogs ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn outbox_states_are_per_provider_and_never_touch_ticket_status(pool: PgPool) {
    let f = fixture(&pool).await;
    let lark = connection(&pool, f.a, "lark").await;
    let jira = connection(&pool, f.a, "jira").await;
    let dest = lark_destination(&pool, f.a).await;

    // Ticket + intents in one transaction: a bad intent rolls the ticket back too.
    let mut tx = ok(pool.begin().await, "begin");
    let (_, n) = ok(try_ticket(&mut *tx, f.a, f.admin, None).await, "ticket");
    assert_eq!(n, 1);
    let bad = sqlx::query(
        "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, lark_destination_id, idempotency_key) \
         SELECT $1, id, $2, 'lark', 'lark_ticket_created', $3, 'idem-atomic' FROM tickets WHERE workspace_id = $1",
    )
    .bind(f.a)
    .bind(jira)
    .bind(dest)
    .execute(&mut *tx)
    .await;
    assert_eq!(sqlstate(&bad).as_deref(), Some(FK));
    ok(tx.rollback().await, "rollback");
    let count: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&pool)
            .await,
        "count",
    );
    assert_eq!(count, 0);

    let (t, n) = ticket(&pool, f.a, f.admin).await;
    assert_eq!(n, 1, "rolled-back intake did not consume a number");
    let rw: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO related_work (workspace_id, ticket_id, title, requested_by) \
             VALUES ($1, $2, 'Issue uji', $3) RETURNING id",
        )
        .bind(f.a)
        .bind(t)
        .bind(f.admin)
        .fetch_one(&pool)
        .await,
        "related work",
    );
    let lark_row: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, lark_destination_id, idempotency_key) \
             VALUES ($1, $2, $3, 'lark', 'lark_ticket_created', $4, 'idem-lark-1') RETURNING id",
        )
        .bind(f.a)
        .bind(t)
        .bind(lark)
        .bind(dest)
        .fetch_one(&pool)
        .await,
        "lark intent",
    );
    let jira_row: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, related_work_id, idempotency_key) \
             VALUES ($1, $2, $3, 'jira', 'jira_create_issue', $4, 'idem-jira-1') RETURNING id",
        )
        .bind(f.a)
        .bind(t)
        .bind(jira)
        .bind(rw)
        .fetch_one(&pool)
        .await,
        "jira intent",
    );
    // Same idempotency key twice in one workspace is refused.
    let dup = sqlx::query(
        "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, lark_destination_id, idempotency_key) \
         VALUES ($1, $2, $3, 'lark', 'lark_ticket_created', $4, 'idem-lark-1')",
    )
    .bind(f.a)
    .bind(t)
    .bind(lark)
    .bind(dest)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&dup).as_deref(), Some("23505"));
    // Operation must match provider.
    let mismatch = sqlx::query(
        "INSERT INTO integration_outbox (workspace_id, ticket_id, connection_id, provider, operation, related_work_id, idempotency_key) \
         VALUES ($1, $2, $3, 'jira', 'lark_ticket_created', $4, 'idem-mismatch')",
    )
    .bind(f.a)
    .bind(t)
    .bind(jira)
    .bind(rw)
    .execute(&pool)
    .await;
    assert_eq!(sqlstate(&mismatch).as_deref(), Some(CHECK));

    let to = |id: Uuid, state: &'static str, evidence: Option<&'static str>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "UPDATE integration_outbox SET state = $2, attempt = attempt + 1, reconcile_evidence = $3 WHERE id = $1",
            )
            .bind(id)
            .bind(state)
            .bind(evidence)
            .execute(&pool)
            .await
            .map(|_| ())
        }
    };
    assert!(to(lark_row, "failed", None).await.is_ok());
    assert!(to(jira_row, "unknown", None).await.is_ok());
    // Unknown must be reconciled before any retry.
    assert_eq!(
        sqlstate(&to(jira_row, "pending", None).await).as_deref(),
        Some(RULE)
    );
    assert_eq!(
        sqlstate(&to(jira_row, "success", None).await).as_deref(),
        Some(RULE)
    );
    assert!(
        to(jira_row, "success", Some("Dicek manual di simulasi (uji)"))
            .await
            .is_ok()
    );
    assert_eq!(
        sqlstate(&to(jira_row, "failed", None).await).as_deref(),
        Some(RULE),
        "success is final"
    );
    // Failed may be retried in a controlled way.
    assert!(to(lark_row, "pending", None).await.is_ok());

    let states: Vec<(String, String)> = ok(
        sqlx::query_as(
            "SELECT provider, state FROM integration_outbox WHERE ticket_id = $1 ORDER BY provider",
        )
        .bind(t)
        .fetch_all(&pool)
        .await,
        "states",
    );
    assert_eq!(
        states,
        [
            ("jira".to_owned(), "success".to_owned()),
            ("lark".to_owned(), "pending".to_owned())
        ]
    );
    assert_eq!(status_of(&pool, t).await, ("new".to_owned(), 1, 1));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn attachment_binds_only_when_clean_and_values_are_constrained(pool: PgPool) {
    let f = fixture(&pool).await;
    let (t, _) = ticket(&pool, f.a, f.admin).await;
    let att: Uuid = ok(
        sqlx::query_scalar(
            "INSERT INTO attachments (workspace_id, uploaded_by, object_key, file_name, size_bytes) \
             VALUES ($1, $2, 'quarantine/a1', 'bukti.pdf', 1200) RETURNING id",
        )
        .bind(f.a)
        .bind(f.admin)
        .fetch_one(&pool)
        .await,
        "quarantined attachment",
    );
    for status in ["pending", "infected", "scanner_unavailable", "rejected"] {
        let r = sqlx::query(
            "UPDATE attachments SET scan_status = $2, ticket_id = $3, bound_at = now() WHERE id = $1",
        )
        .bind(att)
        .bind(status)
        .bind(t)
        .execute(&pool)
        .await;
        assert_eq!(sqlstate(&r).as_deref(), Some(CHECK), "bind while {status}");
    }
    assert!(
        sqlx::query(
            "UPDATE attachments SET scan_status = 'clean', ticket_id = $2, bound_at = now() WHERE id = $1",
        )
        .bind(att)
        .bind(t)
        .execute(&pool)
        .await
        .is_ok()
    );

    let checks = [
        (
            "INSERT INTO priorities (workspace_id, code, label) VALUES ($1, 'critical', 'Kritis')",
            "new priority scale",
        ),
        (
            "INSERT INTO merchants (workspace_id, source, display_name) VALUES ($1, 'cif', 'x')",
            "real CIF source",
        ),
        (
            "INSERT INTO integration_connections (workspace_id, provider, mode, display_name) VALUES ($1, 'jira', 'live', 'x')",
            "live provider mode",
        ),
        (
            "INSERT INTO report_runs (workspace_id, requested_by, report_type, period_start, period_end) \
          SELECT $1, id, 'bi_official', current_date, current_date FROM users LIMIT 1",
            "BI export held",
        ),
    ];
    for (sql, what) in checks {
        let r = sqlx::query(sql).bind(f.a).execute(&pool).await;
        assert_eq!(sqlstate(&r).as_deref(), Some(CHECK), "{what}");
    }
    let r = sqlx::query(
        "INSERT INTO audit_events (workspace_id, action, entity_type, outcome) VALUES ($1, 'ticket.read', 'ticket', 'success')",
    )
    .bind(f.a)
    .execute(&pool)
    .await;
    assert!(r.is_ok());
    let r = sqlx::query("UPDATE audit_events SET outcome = 'failed'")
        .execute(&pool)
        .await;
    assert_eq!(sqlstate(&r).as_deref(), Some(RULE), "audit append-only");
    let r = sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(f.b)
        .execute(&pool)
        .await;
    // ON DELETE RESTRICT answers restrict_violation, not foreign_key_violation.
    assert_eq!(sqlstate(&r).as_deref(), Some("23001"), "no cascade delete");
}

// ---------- scoped HTTP reads ----------

struct Resp {
    status: StatusCode,
    json: Value,
}

async fn get(pool: &PgPool, uri: &str, token: Option<&str>) -> Resp {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        b = b.header(
            header::COOKIE,
            format!("{}={t}", session::cookie_name(false)),
        );
    }
    let request = ok(b.body(Body::empty()), "request");
    let res = match app(state(pool)).oneshot(request).await {
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

async fn token_for(pool: &PgPool, uid: Uuid) -> String {
    ok(session::issue(pool, uid, "uat_superadmin").await, "issue").token
}

async fn bound_attachment(pool: &PgPool, ws: Uuid, t: Uuid, by: Uuid, key: &str) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO attachments (workspace_id, ticket_id, uploaded_by, object_key, file_name, size_bytes, scan_status, visibility, bound_at) \
             VALUES ($1, $2, $3, $4, 'bukti.pdf', 1200, 'clean', 'internal', now()) RETURNING id",
        )
        .bind(ws)
        .bind(t)
        .bind(by)
        .bind(key)
        .fetch_one(pool)
        .await,
        "attachment",
    )
}

async fn report_run(pool: &PgPool, ws: Uuid, by: Uuid) -> Uuid {
    ok(
        sqlx::query_scalar(
            "INSERT INTO report_runs (workspace_id, requested_by, report_type, period_start, period_end, state, generated_at, artifact_ref) \
             VALUES ($1, $2, 'operational', '2026-10-01', '2026-10-31', 'succeeded', now(), 'reports/dummy.csv') RETURNING id",
        )
        .bind(ws)
        .bind(by)
        .fetch_one(pool)
        .await,
        "report run",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn member_reads_are_scoped_and_foreign_ids_answer_404(pool: PgPool) {
    let f = fixture(&pool).await;
    let (ta, _) = ticket(&pool, f.a, f.admin).await;
    let (ta2, _) = ticket(&pool, f.a, f.admin).await;
    let (tb, _) = ticket(&pool, f.b, f.admin).await;
    let att_a = bound_attachment(&pool, f.a, ta, f.admin, "quarantine/ha").await;
    let att_b = bound_attachment(&pool, f.b, tb, f.admin, "quarantine/hb").await;
    let run_a = report_run(&pool, f.a, f.admin).await;
    let run_b = report_run(&pool, f.b, f.admin).await;
    let token = token_for(&pool, f.cs).await;
    let tk = Some(token.as_str());
    let (a, b) = (f.a, f.b);

    let t = get(&pool, &format!("/api/v1/workspaces/{a}/tickets/{ta2}"), tk).await;
    assert_eq!(t.status, StatusCode::OK);
    assert_eq!(t.json["displayNumber"], "DUMA-2");
    assert_eq!(t.json["status"]["code"], "new");
    assert_eq!(t.json["status"]["label"], "Baru");
    assert_eq!(t.json["version"], 1);
    assert!(
        t.json["receivedAt"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z'))
    );

    let att = get(
        &pool,
        &format!("/api/v1/workspaces/{a}/tickets/{ta}/attachments/{att_a}"),
        tk,
    )
    .await;
    assert_eq!(att.status, StatusCode::OK);
    assert_eq!(att.json["scanStatus"], "clean");
    assert!(
        att.json.get("objectKey").is_none(),
        "object key not exposed"
    );
    let run = get(
        &pool,
        &format!("/api/v1/workspaces/{a}/report-runs/{run_a}"),
        tk,
    )
    .await;
    assert_eq!(run.status, StatusCode::OK);
    assert_eq!(run.json["hasArtifact"], true);
    assert!(
        run.json.get("artifactRef").is_none(),
        "artifact ref not exposed"
    );

    let cat = get(&pool, &format!("/api/v1/workspaces/{a}/catalogs"), tk).await;
    assert_eq!(cat.status, StatusCode::OK);
    let codes: Vec<&str> = cat.json["priorities"]
        .as_array()
        .map(|v| v.iter().filter_map(|p| p["code"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(codes, ["low", "normal", "high", "urgent"]);

    // Foreign workspace, foreign object through own workspace, wrong parent, garbage IDs.
    let unknown = Uuid::new_v4();
    for uri in [
        format!("/api/v1/workspaces/{b}/catalogs"),
        format!("/api/v1/workspaces/{b}/tickets/{tb}"),
        format!("/api/v1/workspaces/{a}/tickets/{tb}"),
        format!("/api/v1/workspaces/{a}/tickets/{unknown}"),
        format!("/api/v1/workspaces/{a}/tickets/not-a-uuid"),
        format!("/api/v1/workspaces/{b}/tickets/{tb}/attachments/{att_b}"),
        format!("/api/v1/workspaces/{a}/tickets/{tb}/attachments/{att_b}"),
        format!("/api/v1/workspaces/{a}/tickets/{ta}/attachments/{att_b}"),
        format!("/api/v1/workspaces/{a}/tickets/{ta2}/attachments/{att_a}"),
        format!("/api/v1/workspaces/{b}/report-runs/{run_b}"),
        format!("/api/v1/workspaces/{a}/report-runs/{run_b}"),
    ] {
        let r = get(&pool, &uri, tk).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(r.json["error"]["code"], "not_found", "{uri}");
    }
    // No session: nothing is readable.
    let r = get(&pool, &format!("/api/v1/workspaces/{a}/tickets/{ta}"), None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn uat_superadmin_reads_any_workspace_and_status_catalog(pool: PgPool) {
    let f = fixture(&pool).await;
    let (tb, _) = ticket(&pool, f.b, f.admin).await;
    let token = token_for(&pool, f.admin).await;
    let tk = Some(token.as_str());
    let b = f.b;
    let r = get(&pool, &format!("/api/v1/workspaces/{b}/tickets/{tb}"), tk).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["displayNumber"], "DUMB-1");

    assert_eq!(
        get(&pool, "/api/v1/ticket-statuses", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let s = get(&pool, "/api/v1/ticket-statuses", tk).await;
    assert_eq!(s.status, StatusCode::OK);
    let items = s.json["items"].as_array().cloned().unwrap_or_default();
    let summary: Vec<(String, bool)> = items
        .iter()
        .map(|i| {
            (
                i["code"].as_str().unwrap_or_default().to_owned(),
                i["selectable"].as_bool().unwrap_or(true),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("new".to_owned(), true),
            ("open".to_owned(), true),
            ("pending_customer".to_owned(), true),
            ("pending_engineering".to_owned(), true),
            ("resolved".to_owned(), true),
            ("closed".to_owned(), false),
        ]
    );
}

// ---------- seed and rollback ----------

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn seed_reference_data_is_idempotent_and_labelled(pool: PgPool) {
    seeded(&pool).await;
    seeded(&pool).await;
    for (table, expected) in [
        ("priorities", 8_i64),
        ("intake_channels", 6),
        ("categories", 4),
        ("merchants", 2),
        ("integration_connections", 4),
        ("jira_assignees", 4),
        ("jira_issues", 4),
        ("lark_destinations", 4),
    ] {
        let n: i64 = ok(
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&pool)
                .await,
            table,
        );
        assert_eq!(n, expected, "{table}");
    }
    let non_sim: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM integration_connections WHERE mode <> 'simulation'",
        )
        .fetch_one(&pool)
        .await,
        "modes",
    );
    assert_eq!(non_sim, 0);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0002_rolls_back_and_reapplies_on_dummy_data(pool: PgPool) {
    let f = fixture(&pool).await;
    ticket(&pool, f.a, f.admin).await;

    ok(db::MIGRATOR.undo(&pool, 1).await, "undo to 0001");
    let left: Vec<String> = ok(
        sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name IN ('tickets', 'workspace_ticket_counters', 'audit_events')",
        )
        .fetch_all(&pool)
        .await,
        "tables after undo",
    );
    assert!(left.is_empty(), "D02 tables remain: {left:?}");
    let companies: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM companies")
            .fetch_one(&pool)
            .await,
        "companies",
    );
    assert_eq!(companies, 2, "0001 data untouched");
    let funcs: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM pg_proc WHERE proname LIKE 'itsm\\_%'")
            .fetch_one(&pool)
            .await,
        "functions",
    );
    assert_eq!(funcs, 0);

    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let counters: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM workspace_ticket_counters WHERE last_number = 0")
            .fetch_one(&pool)
            .await,
        "counters",
    );
    assert_eq!(counters, 2, "backfilled for existing workspaces");
    seeded(&pool).await;
    assert_eq!(ticket(&pool, f.a, f.admin).await.1, 1);
}
