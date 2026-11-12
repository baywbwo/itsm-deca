//! R1 I-1 CP1: DB hardening (0018) and the encrypted secret store (0019).
use std::fmt::Display;

use itsm_api::crypto::{self, Keyring, Purpose, SecretError};
use itsm_api::{db, seed};
use sqlx::PgPool;
use uuid::Uuid;

const KEYS: &str = "old:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=,new:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

fn keyring(active: &str) -> Keyring {
    ok(Keyring::parse(Some(KEYS), Some(active)), "keyring")
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
        sqlx::query_scalar("SELECT id FROM workspaces ORDER BY code LIMIT 1")
            .fetch_one(pool)
            .await,
        "workspace",
    )
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn append_only_tables_refuse_truncate(pool: PgPool) {
    for t in [
        "audit_events",
        "ticket_activities",
        "ticket_resolution_events",
        "workspace_code_history",
        "platform_audit_events",
    ] {
        let r = sqlx::query(&format!("TRUNCATE {t} CASCADE"))
            .execute(&pool)
            .await;
        match r {
            Err(sqlx::Error::Database(e)) => assert_eq!(e.code().as_deref(), Some("IT422"), "{t}"),
            other => panic!("{t}: TRUNCATE not refused: {other:?}"),
        }
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn secret_round_trip_is_bound_to_scope_and_never_plaintext(pool: PgPool) {
    let ws = workspace(&pool).await;
    let kr = keyring("new");
    let mut conn = ok(pool.acquire().await, "conn");
    let id = ok(
        crypto::store_secret(
            &mut conn,
            &kr,
            Some(ws),
            Purpose::LlmApiKey,
            b"dummy-llm-key",
            None,
        )
        .await,
        "store",
    );
    drop(conn);
    let got = ok(
        crypto::load_secret(&pool, &kr, id, Some(ws), Purpose::LlmApiKey).await,
        "load",
    );
    assert_eq!(got.expose(), b"dummy-llm-key");
    let raw: Vec<u8> = ok(
        sqlx::query_scalar("SELECT ciphertext FROM encrypted_secrets WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await,
        "raw",
    );
    assert!(!raw.windows(13).any(|w| w == b"dummy-llm-key"));
    // Wrong purpose or tenant does not resolve.
    assert!(matches!(
        crypto::load_secret(&pool, &kr, id, Some(ws), Purpose::SmtpPassword).await,
        Err(SecretError::NotFound)
    ));
    assert!(matches!(
        crypto::load_secret(&pool, &kr, id, None, Purpose::LlmApiKey).await,
        Err(SecretError::NotFound)
    ));
    // A row moved to another purpose fails authentication (AAD binds id/tenant/purpose).
    ok(
        sqlx::query("UPDATE encrypted_secrets SET purpose = 'smtp_password' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await,
        "tamper",
    );
    assert!(matches!(
        crypto::load_secret(&pool, &kr, id, Some(ws), Purpose::SmtpPassword).await,
        Err(SecretError::Decrypt)
    ));
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn secret_scope_check_and_rotation_reseal(pool: PgPool) {
    let ws = workspace(&pool).await;
    // totp_seed is platform-level; tenant secrets need a workspace.
    let bad = sqlx::query(
        "INSERT INTO encrypted_secrets (workspace_id, purpose, key_id, algorithm, nonce, ciphertext) \
         VALUES ($1, 'totp_seed', 'k', 'aes-256-gcm', '\\x000000000000000000000000', '\\x0000000000000000000000000000000000')",
    )
    .bind(ws)
    .execute(&pool)
    .await;
    assert!(bad.is_err(), "totp_seed with workspace accepted");

    let old = keyring("old");
    let mut conn = ok(pool.acquire().await, "conn");
    let id = ok(
        crypto::store_secret(
            &mut conn,
            &old,
            None,
            Purpose::TotpSeed,
            b"dummy-seed",
            None,
        )
        .await,
        "store",
    );
    drop(conn);
    let new = keyring("new");
    assert_eq!(
        ok(crypto::reseal(&pool, &new, false).await, "reseal").resealed,
        1
    );
    assert_eq!(
        ok(crypto::reseal(&pool, &new, false).await, "reseal again").resealed,
        0
    );
    let (key_id, version): (String, i32) = ok(
        sqlx::query_as("SELECT key_id, version FROM encrypted_secrets WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await,
        "row",
    );
    assert_eq!((key_id.as_str(), version), ("new", 2));
    let got = ok(
        crypto::load_secret(&pool, &new, id, None, Purpose::TotpSeed).await,
        "load",
    );
    assert_eq!(got.expose(), b"dummy-seed");
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migrations_0018_0019_roll_back_and_reapply(pool: PgPool) {
    let _ = workspace(&pool).await;
    ok(db::MIGRATOR.undo(&pool, 17).await, "undo to 0017");
    let left: i64 = ok(
        sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM information_schema.tables WHERE table_name = 'encrypted_secrets') \
             + (SELECT count(*) FROM pg_trigger WHERE tgname LIKE '%\\_no\\_truncate' AND tgname <> 'platform_audit_events_no_truncate') \
             + (SELECT count(*) FROM pg_proc WHERE proname = 'itsm_grant_app')",
        )
        .fetch_one(&pool)
        .await,
        "left",
    );
    assert_eq!(left, 0);
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let triggers: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM pg_trigger WHERE tgname LIKE '%\\_no\\_truncate' \
             AND tgname <> 'auth_events_no_truncate'",
        )
        .fetch_one(&pool)
        .await,
        "triggers",
    );
    assert_eq!(triggers, 5);
}

const NEW_ONLY: &str = "new:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const GONE: &str = "gone:AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

async fn sealed_with(pool: &PgPool) -> Vec<(Uuid, String, i32)> {
    ok(
        sqlx::query_as("SELECT id, key_id, version FROM encrypted_secrets ORDER BY id")
            .fetch_all(pool)
            .await,
        "rows",
    )
}

/// CP5B-03: rotation runbook end to end — dry run writes nothing, a row the keyring cannot open
/// is skipped (not fatal) and keeps the verdict failing, and after the pass the old key can be
/// dropped from the keyring with every secret still readable.
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn cp5b03_two_key_rotation_retires_the_old_key(pool: PgPool) {
    let ws = workspace(&pool).await;
    let old = keyring("old");
    let gone = ok(Keyring::parse(Some(GONE), Some("gone")), "gone keyring");
    let mut conn = ok(pool.acquire().await, "conn");
    let mut store = async |kr: &Keyring, ws: Option<Uuid>, purpose: Purpose, plain: &[u8]| {
        ok(
            crypto::store_secret(&mut conn, kr, ws, purpose, plain, None).await,
            "store",
        )
    };
    let seed = store(&old, None, Purpose::TotpSeed, b"dummy-seed").await;
    let token = store(&old, Some(ws), Purpose::JiraApiToken, b"dummy-token").await;
    let orphan = store(&gone, Some(ws), Purpose::LarkAppSecret, b"dummy-orphan").await;
    drop(conn);

    let new = keyring("new");
    let before = sealed_with(&pool).await;
    let plan = ok(crypto::reseal(&pool, &new, true).await, "dry run");
    assert_eq!(
        (plan.resealed, plan.failed, plan.remaining),
        (2, 1, 3),
        "dry run counts"
    );
    assert_eq!(sealed_with(&pool).await, before, "dry run wrote");
    assert!(plan.verdict(true).is_err(), "unopenable row must block");

    let pass = ok(crypto::reseal(&pool, &new, false).await, "reseal");
    assert_eq!((pass.resealed, pass.failed, pass.remaining), (2, 1, 1));
    assert!(pass.verdict(false).is_err(), "old-key row left");
    let left: Vec<_> = sealed_with(&pool)
        .await
        .into_iter()
        .filter(|(_, k, _)| k != "new")
        .map(|(id, k, _)| (id, k))
        .collect();
    assert_eq!(left, vec![(orphan, "gone".to_owned())]);

    // Runbook: the unreadable credential is re-entered by its owner; here it is removed.
    ok(
        sqlx::query("DELETE FROM encrypted_secrets WHERE id = $1")
            .bind(orphan)
            .execute(&pool)
            .await,
        "delete orphan",
    );
    let done = ok(crypto::reseal(&pool, &new, false).await, "reseal again");
    assert_eq!(
        done.verdict(false),
        Ok("reseal: resealed=0 failed=0 remaining=0".to_owned())
    );

    let only_new = ok(Keyring::parse(Some(NEW_ONLY), Some("new")), "new only");
    let got = ok(
        crypto::load_secret(&pool, &only_new, seed, None, Purpose::TotpSeed).await,
        "seed",
    );
    assert_eq!(got.expose(), b"dummy-seed");
    let got = ok(
        crypto::load_secret(&pool, &only_new, token, Some(ws), Purpose::JiraApiToken).await,
        "token",
    );
    assert_eq!(got.expose(), b"dummy-token");
}
