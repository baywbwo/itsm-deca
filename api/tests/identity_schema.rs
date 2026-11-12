//! R1 I-1 CP2a: identity lifecycle schema (0020).
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

async fn user(pool: &PgPool) -> Uuid {
    let name: String = ok(
        sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await,
        "db",
    );
    ok(seed::seed_uat(pool, &name).await, "seed");
    ok(
        sqlx::query_scalar("SELECT id FROM users ORDER BY id LIMIT 1")
            .fetch_one(pool)
            .await,
        "user",
    )
}

fn sql_state(r: Result<sqlx::postgres::PgQueryResult, sqlx::Error>, what: &str) -> String {
    match r {
        Err(sqlx::Error::Database(e)) => e.code().map(|c| c.into_owned()).unwrap_or_default(),
        other => panic!("{what}: not refused: {other:?}"),
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_6_auth_events_are_append_only(pool: PgPool) {
    let uid = user(&pool).await;
    ok(
        sqlx::query(
            "INSERT INTO auth_events (user_id, event, outcome) VALUES ($1, 'login', 'failed')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "insert",
    );
    for (what, q) in [
        ("update", "UPDATE auth_events SET outcome = 'success'"),
        ("delete", "DELETE FROM auth_events"),
        ("truncate", "TRUNCATE auth_events"),
    ] {
        let code = sql_state(sqlx::query(q).execute(&pool).await, what);
        assert_eq!(code, "42501", "{what}");
    }
    let n: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM auth_events")
            .fetch_one(&pool)
            .await,
        "count",
    );
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn us_1_4_one_active_reset_token_per_user_stored_as_digest(pool: PgPool) {
    let uid = user(&pool).await;
    let insert = |hash: Vec<u8>| {
        sqlx::query(
            "INSERT INTO password_reset_tokens (user_id, token_hash, issued_by, expires_at) \
             VALUES ($1, $2, $1, now() + interval '1 hour')",
        )
        .bind(uid)
        .bind(hash)
    };
    ok(insert(vec![1; 32]).execute(&pool).await, "first token");
    let code = sql_state(insert(vec![2; 32]).execute(&pool).await, "second active");
    assert_eq!(code, "23505");
    let code = sql_state(insert(vec![3; 16]).execute(&pool).await, "non-digest");
    assert_eq!(code, "23514");
    let code = sql_state(
        sqlx::query(
            "INSERT INTO password_reset_tokens (user_id, token_hash, issued_by, expires_at) \
             VALUES ($1, $2, $1, now() + interval '25 hours')",
        )
        .bind(uid)
        .bind(vec![4u8; 32])
        .execute(&pool)
        .await,
        "ttl over 24h",
    );
    assert_eq!(code, "23514");
    ok(
        sqlx::query("UPDATE password_reset_tokens SET used_at = now() WHERE user_id = $1")
            .bind(uid)
            .execute(&pool)
            .await,
        "use token",
    );
    ok(
        insert(vec![2; 32]).execute(&pool).await,
        "new token after use",
    );
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0020_rolls_back_and_reapplies(pool: PgPool) {
    let uid = user(&pool).await;
    ok(db::MIGRATOR.undo(&pool, 19).await, "undo to 0019");
    let left: i64 = ok(
        sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM information_schema.tables \
                      WHERE table_name IN ('auth_events','password_reset_tokens')) \
             + (SELECT count(*) FROM information_schema.columns \
                 WHERE (table_name = 'users' AND column_name IN ('email','created_by','deactivated_at')) \
                    OR (table_name = 'local_credentials' AND column_name = 'must_change_password') \
                    OR (table_name = 'sessions' AND column_name = 'idle_expires_at')) \
             + (SELECT count(*) FROM pg_proc WHERE proname = 'itsm_auth_events_append_only')",
        )
        .fetch_one(&pool)
        .await,
        "left",
    );
    assert_eq!(left, 0);
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    ok(
        sqlx::query(
            "INSERT INTO platform_grants (user_id, grant_code) VALUES ($1, 'platform_admin')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "platform_admin grant",
    );
    // Down is refused once platform_admin grants exist (data-loss guard).
    assert!(db::MIGRATOR.undo(&pool, 19).await.is_err());
    let still: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_name = 'auth_events'",
        )
        .fetch_one(&pool)
        .await,
        "still",
    );
    assert_eq!(still, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0021_rolls_back_and_reapplies(pool: PgPool) {
    let uid = user(&pool).await;
    let shape = "SELECT (SELECT count(*) FROM information_schema.columns \
                  WHERE table_name = 'auth_events' AND column_name = 'actor_id') \
               + (SELECT count(*) FROM pg_constraint \
                  WHERE conname IN ('auth_events_event_check','sessions_revoked_reason_check') \
                    AND (pg_get_constraintdef(oid) LIKE '%session_expire%' \
                         OR pg_get_constraintdef(oid) LIKE '%expired%'))";
    let up: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "up");
    assert_eq!(up, 3);
    ok(db::MIGRATOR.undo(&pool, 20).await, "undo to 0020");
    let down: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "down");
    assert_eq!(down, 0);
    // 0020 audit stays intact and still rejects the 0021-only event.
    let st = sql_state(
        sqlx::query(
            "INSERT INTO auth_events (user_id, event, outcome) VALUES ($1, 'session_expire', 'success')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "session_expire on 0020",
    );
    assert_eq!(st, "23514");
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let again: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "again");
    assert_eq!(again, 3);
    ok(
        sqlx::query(
            "INSERT INTO auth_events (user_id, actor_id, event, outcome) \
             VALUES ($1, $1, 'user_deactivate', 'success')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "actor row",
    );
    // Append-only audit cannot be rewritten, so down is refused once 0021 data exists.
    assert!(db::MIGRATOR.undo(&pool, 20).await.is_err());
    let kept: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE actor_id = $1")
            .bind(uid)
            .fetch_one(&pool)
            .await,
        "kept",
    );
    assert_eq!(kept, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0022_rolls_back_and_reapplies(pool: PgPool) {
    let uid = user(&pool).await;
    let shape = "SELECT (SELECT count(*) FROM pg_tables WHERE tablename IN \
                  ('user_mfa_totp','mfa_recovery_codes','mfa_login_challenges')) \
               + (SELECT count(*) FROM pg_proc WHERE proname = 'itsm_mfa_required') \
               + (SELECT count(*) FROM pg_constraint WHERE conname = 'auth_events_event_check' \
                    AND pg_get_constraintdef(oid) LIKE '%recovery_codes_issue%')";
    let up: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "up");
    assert_eq!(up, 5);
    ok(db::MIGRATOR.undo(&pool, 21).await, "undo to 0021");
    let down: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "down");
    assert_eq!(down, 0);
    let st = sql_state(
        sqlx::query(
            "INSERT INTO auth_events (user_id, event, outcome) \
             VALUES ($1, 'recovery_codes_issue', 'success')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "recovery_codes_issue on 0021",
    );
    assert_eq!(st, "23514");
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let again: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "again");
    assert_eq!(again, 5);
    ok(
        sqlx::query(
            "INSERT INTO mfa_recovery_codes (user_id, batch_id, code_hash) \
             VALUES ($1, gen_random_uuid(), decode(repeat('ab', 32), 'hex'))",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "recovery code row",
    );
    // A user's second factor is never dropped silently: down is refused once MFA data exists.
    assert!(db::MIGRATOR.undo(&pool, 21).await.is_err());
    let kept: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM mfa_recovery_codes WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&pool)
            .await,
        "kept",
    );
    assert_eq!(kept, 1);
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0023_rolls_back_and_reapplies(pool: PgPool) {
    let uid = user(&pool).await;
    let shape = "SELECT (SELECT count(*) FROM information_schema.columns \
                  WHERE table_name = 'auth_events' AND column_name = 'reason') \
               + (SELECT count(*) FROM pg_constraint WHERE conname = 'auth_events_event_check' \
                    AND pg_get_constraintdef(oid) LIKE '%user_unlock%' \
                    AND pg_get_constraintdef(oid) LIKE '%mfa_disable%')";
    let up: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "up");
    assert_eq!(up, 2);
    let st = sql_state(
        sqlx::query(
            "INSERT INTO auth_events (user_id, event, outcome, reason) \
             VALUES ($1, 'mfa_reset', 'success', ' padded ')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "untrimmed reason",
    );
    assert_eq!(st, "23514");
    ok(db::MIGRATOR.undo(&pool, 22).await, "undo to 0022");
    let down: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "down");
    assert_eq!(down, 0);
    let st = sql_state(
        sqlx::query(
            "INSERT INTO auth_events (user_id, event, outcome) VALUES ($1, 'user_unlock', 'success')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "user_unlock on 0022",
    );
    assert_eq!(st, "23514");
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let again: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "again");
    assert_eq!(again, 2);
    ok(
        sqlx::query(
            "INSERT INTO auth_events (user_id, event, outcome, reason) \
             VALUES ($1, 'mfa_reset', 'success', 'Uji sintetis')",
        )
        .bind(uid)
        .execute(&pool)
        .await,
        "reason row",
    );
    // An mfa-reset reason is audit evidence: down is refused rather than dropping it.
    assert!(db::MIGRATOR.undo(&pool, 22).await.is_err());
    let kept: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM auth_events WHERE reason IS NOT NULL")
            .fetch_one(&pool)
            .await,
        "kept",
    );
    assert_eq!(kept, 1);
}
