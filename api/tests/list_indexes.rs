//! R1 I-3 CP7c: indexes behind the ticket List / Board / search (0029).
//! The plans on 1M tickets are in `evidence/i3/cp7c/`; these tests keep the index
//! definitions in step with the List ORDER BY (`ticket_list.rs`: `{col} {dir}, t.id {dir}`)
//! and prove that the migration rolls back and re-applies.
use std::fmt::Display;

use itsm_api::{db, seed};
use sqlx::PgPool;
use uuid::Uuid;

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

async fn workspace(pool: &PgPool) -> Uuid {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    ok(
        sqlx::query_scalar("SELECT id FROM workspaces WHERE code = 'DUMA'")
            .fetch_one(pool)
            .await,
        "workspace",
    )
}

async fn indexdef(pool: &PgPool, name: &str) -> Option<String> {
    ok(
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE indexname = $1")
            .bind(name)
            .fetch_optional(pool)
            .await,
        name,
    )
}

async fn has_trgm(pool: &PgPool) -> bool {
    ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm')")
            .fetch_one(pool)
            .await,
        "pg_trgm",
    )
}

/// The indexed columns of an index, e.g. `workspace_id, updated_at DESC, id DESC`.
async fn columns(pool: &PgPool, name: &str) -> String {
    let def = indexdef(pool, name).await.unwrap_or_default();
    def.split_once(" USING ")
        .and_then(|(_, rest)| rest.split_once('('))
        .map(|(_, cols)| cols.trim_end_matches(')').to_owned())
        .unwrap_or_default()
}

async fn assert_cp7c(pool: &PgPool) {
    assert_eq!(
        columns(pool, "tickets_status_idx").await,
        "workspace_id, workflow_status, updated_at DESC, id DESC"
    );
    assert_eq!(
        columns(pool, "tickets_updated_idx").await,
        "workspace_id, updated_at DESC, id DESC"
    );
    assert_eq!(
        columns(pool, "tickets_received_idx").await,
        "workspace_id, received_at, id"
    );
    assert_eq!(
        columns(pool, "tickets_subject_trgm_idx").await,
        "subject gin_trgm_ops"
    );
    assert!(has_trgm(pool).await);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7c_migration_0029_rolls_back_and_reapplies(pool: PgPool) {
    let _ = workspace(&pool).await;
    assert_cp7c(&pool).await;

    ok(db::MIGRATOR.undo(&pool, 28).await, "undo to 0028");
    assert_eq!(
        columns(&pool, "tickets_status_idx").await,
        "workspace_id, workflow_status, updated_at DESC, id"
    );
    assert_eq!(
        columns(&pool, "tickets_received_idx").await,
        "workspace_id, received_at"
    );
    assert_eq!(indexdef(&pool, "tickets_updated_idx").await, None);
    assert_eq!(indexdef(&pool, "tickets_subject_trgm_idx").await, None);
    assert!(!has_trgm(&pool).await);
    // Seeded rows survive the index rebuild both ways.
    let n: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&pool)
            .await,
        "tickets",
    );

    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    assert_cp7c(&pool).await;
    let again: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&pool)
            .await,
        "tickets",
    );
    assert_eq!(n, again);
}

/// Plan of `sql` (bound to the workspace) with sequential scans priced out, so the test
/// shows which index *can* serve the query shape even on a small fixture. `hide` drops
/// indexes inside the (rolled back) transaction when a small fixture makes two indexes tie.
async fn plan(pool: &PgPool, ws: Uuid, hide: &[&str], sql: &str) -> String {
    let mut tx = ok(pool.begin().await, "begin");
    ok(
        sqlx::query("SET LOCAL enable_seqscan = off")
            .execute(&mut *tx)
            .await,
        "seqscan off",
    );
    for index in hide {
        ok(
            sqlx::query(&format!("DROP INDEX {index}"))
                .execute(&mut *tx)
                .await,
            index,
        );
    }
    let rows: Vec<String> = ok(
        sqlx::query_scalar(&format!("EXPLAIN (COSTS OFF) {sql}"))
            .bind(ws)
            .fetch_all(&mut *tx)
            .await,
        sql,
    );
    rows.join("\n")
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7c_list_shapes_are_served_by_ordered_indexes(pool: PgPool) {
    let ws = workspace(&pool).await;
    for dir in ["DESC", "ASC"] {
        // Default List / Board column: newest update first, `id` breaks ties (keyset cursor).
        let p = plan(
            &pool,
            ws,
            &[],
            &format!(
                "SELECT t.id FROM tickets t WHERE t.workspace_id = $1 \
                 ORDER BY t.updated_at {dir}, t.id {dir} LIMIT 51"
            ),
        )
        .await;
        assert!(p.contains("tickets_updated_idx"), "{dir}: {p}");
        assert!(!p.contains("Sort"), "{dir}: index order, no sort: {p}");

        // On a few rows the planner may equally walk tickets_updated_idx and filter.
        let p = plan(
            &pool,
            ws,
            &["tickets_updated_idx"],
            &format!(
                "SELECT t.id FROM tickets t WHERE t.workspace_id = $1 AND t.workflow_status = 'open' \
                 ORDER BY t.updated_at {dir}, t.id {dir} LIMIT 21"
            ),
        )
        .await;
        assert!(p.contains("tickets_status_idx"), "{dir}: {p}");
        assert!(!p.contains("Sort"), "{dir}: board column without sort: {p}");

        let p = plan(
            &pool,
            ws,
            &[],
            &format!(
                "SELECT t.id FROM tickets t WHERE t.workspace_id = $1 \
                 ORDER BY t.received_at {dir}, t.id {dir} LIMIT 51"
            ),
        )
        .await;
        assert!(p.contains("tickets_received_idx"), "{dir}: {p}");
        assert!(!p.contains("Sort"), "{dir}: {p}");
    }
    // Substring search: trigram index instead of scanning every subject. On a few rows the
    // workspace btree wins, so the workspace test is written so that no btree can serve it.
    let p = plan(
        &pool,
        ws,
        &[],
        "SELECT t.id FROM tickets t \
         WHERE t.workspace_id::text = $1::text AND t.subject ILIKE '%diskon%'",
    )
    .await;
    assert!(p.contains("tickets_subject_trgm_idx"), "{p}");
}

/// Workspace/status pairs where the 0030 counters disagree with counting the tickets.
async fn drift(pool: &PgPool) -> i64 {
    ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM \
               (SELECT workspace_id, workflow_status, count(*) AS n FROM tickets GROUP BY 1, 2) t \
             FULL JOIN \
               (SELECT workspace_id, workflow_status, sum(n)::bigint AS n FROM ticket_status_counts \
                GROUP BY 1, 2 HAVING sum(n) <> 0) c \
             USING (workspace_id, workflow_status) WHERE t.n IS DISTINCT FROM c.n",
        )
        .fetch_one(pool)
        .await,
        "drift",
    )
}

async fn counter(pool: &PgPool, ws: Uuid, status: &str) -> i64 {
    ok(
        sqlx::query_scalar(
            "SELECT coalesce(sum(n), 0)::bigint FROM ticket_status_counts \
             WHERE workspace_id = $1 AND workflow_status = $2",
        )
        .bind(ws)
        .bind(status)
        .fetch_one(pool)
        .await,
        "counter",
    )
}

/// Inserts `n` dummy tickets (status `new`) and returns their ids.
async fn insert(pool: &PgPool, ws: Uuid, n: i32) -> Vec<Uuid> {
    ok(
        sqlx::query_scalar(
            "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, \
               intake_channel_id, priority_id) \
             SELECT $1, 'Pengaduan hitung ' || g, 'Kronologi uji (dummy)', now(), \
               (SELECT id FROM users WHERE external_subject = 'dummy:uat-superadmin'), \
               (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'), \
               (SELECT id FROM priorities WHERE workspace_id = $1 AND code = 'normal') \
             FROM generate_series(1, $2) g RETURNING id",
        )
        .bind(ws)
        .bind(n)
        .fetch_all(pool)
        .await,
        "insert tickets",
    )
}

async fn move_to(pool: &PgPool, ids: &[Uuid], status: &str) {
    ok(
        sqlx::query(
            "UPDATE tickets SET workflow_status = $2, cycle_no = 1, version = version + 1 \
             WHERE id = ANY($1)",
        )
        .bind(ids)
        .bind(status)
        .execute(pool)
        .await,
        status,
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7c_status_counters_follow_every_ticket_change(pool: PgPool) {
    let ws = workspace(&pool).await;
    // Backfill matches the seeded tickets.
    assert_eq!(drift(&pool).await, 0, "after migration + seed");
    let new_before = counter(&pool, ws, "new").await;

    let ids = insert(&pool, ws, 40).await;
    assert_eq!(counter(&pool, ws, "new").await, new_before + 40);
    assert_eq!(drift(&pool).await, 0, "insert");

    // Transitions, several rows per statement and concurrently from separate connections.
    move_to(&pool, &ids[..10], "open").await;
    tokio::join!(
        move_to(&pool, &ids[10..20], "pending_customer"),
        move_to(&pool, &ids[20..30], "pending_engineering"),
        move_to(&pool, &ids[30..35], "open"),
    );
    assert_eq!(counter(&pool, ws, "new").await, new_before + 5);
    assert_eq!(drift(&pool).await, 0, "transitions");

    // A change that keeps the status leaves the counters alone.
    let open = counter(&pool, ws, "open").await;
    ok(
        sqlx::query(
            "UPDATE tickets SET subject = subject || ' (ubah)', version = version + 1 WHERE id = ANY($1)",
        )
            .bind(&ids[..])
            .execute(&pool)
            .await,
        "subject",
    );
    assert_eq!(counter(&pool, ws, "open").await, open);
    assert_eq!(drift(&pool).await, 0, "non-status update");

    // Imported / archived rows are written with triggers off (session_replication_role =
    // replica); the counters must still follow (ENABLE ALWAYS).
    let mut conn = ok(pool.acquire().await, "acquire");
    for sql in [
        "SET session_replication_role = replica",
        "UPDATE tickets SET workflow_status = 'closed', version = version + 1 \
         WHERE id = ANY($1)",
        "SET session_replication_role = origin",
    ] {
        ok(
            sqlx::query(sql).bind(&ids[35..]).execute(&mut *conn).await,
            sql,
        );
    }
    drop(conn);
    assert_eq!(counter(&pool, ws, "new").await, new_before);
    assert_eq!(drift(&pool).await, 0, "replica-mode archive");

    // Deleting a ticket (no history yet) decrements its status.
    ok(
        sqlx::query("DELETE FROM tickets WHERE id = $1")
            .bind(ids[0])
            .execute(&pool)
            .await,
        "delete",
    );
    assert_eq!(counter(&pool, ws, "open").await, open - 1);
    assert_eq!(drift(&pool).await, 0, "delete");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7c_migration_0030_rolls_back_and_backfills_on_reapply(pool: PgPool) {
    let ws = workspace(&pool).await;
    insert(&pool, ws, 7).await;
    assert_eq!(drift(&pool).await, 0);

    ok(db::MIGRATOR.undo(&pool, 29).await, "undo to 0029");
    let gone: (bool, bool, bool) = ok(
        sqlx::query_as(
            "SELECT to_regclass('ticket_status_counts') IS NULL, \
                    to_regclass('tickets_active_priority_idx') IS NULL, \
                    NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname LIKE 'tickets_status_count%')",
        )
        .fetch_one(&pool)
        .await,
        "after undo",
    );
    assert_eq!(gone, (true, true, true));
    // Changes made while 0030 is rolled back are picked up by the backfill on re-apply.
    let ids = insert(&pool, ws, 5).await;
    move_to(&pool, &ids[..2], "open").await;

    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    assert_eq!(drift(&pool).await, 0, "backfill after re-apply");
    let n: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM tickets WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(&pool)
            .await,
        "tickets",
    );
    let total: i64 = ok(
        sqlx::query_scalar(
            "SELECT sum(n)::bigint FROM ticket_status_counts WHERE workspace_id = $1",
        )
        .bind(ws)
        .fetch_one(&pool)
        .await,
        "total",
    );
    assert_eq!(n, total);
}

/// Two transactions moving tickets in opposite directions take the same counter rows. The
/// 0030 triggers lock them in key order, so neither waits on the other in a cycle (row-level
/// triggers deadlocked here within a few rounds).
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp7c_opposite_status_moves_do_not_deadlock(pool: PgPool) {
    let ws = workspace(&pool).await;
    let ids = insert(&pool, ws, 64).await;
    let (a, b) = ids.split_at(32);
    move_to(&pool, a, "open").await;
    move_to(&pool, b, "pending_customer").await;
    for round in 0..20 {
        let (to_a, to_b) = if round % 2 == 0 {
            ("pending_customer", "open")
        } else {
            ("open", "pending_customer")
        };
        tokio::join!(move_to(&pool, a, to_a), move_to(&pool, b, to_b));
    }
    assert_eq!(drift(&pool).await, 0);
}
