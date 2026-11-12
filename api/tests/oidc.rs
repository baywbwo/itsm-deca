//! CP5c company SSO (OIDC) against a real PostgreSQL: schema guards of migration 0026 and its
//! rollback. All data is synthetic; no real identity provider is used.

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

struct Fx {
    ws: Uuid,
    admin: Uuid,
    lead: Uuid,
}

async fn uuid_of(pool: &PgPool, sql: &str, key: &str) -> Uuid {
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
    let user = "SELECT id FROM users WHERE external_subject = $1";
    Fx {
        ws: uuid_of(pool, "SELECT id FROM workspaces WHERE code = $1", "DUMA").await,
        admin: uuid_of(pool, user, "dummy:uat-superadmin").await,
        lead: uuid_of(pool, user, "dummy:lead-alfa").await,
    }
}

async fn insert_idp(pool: &PgPool, fx: &Fx) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO identity_providers (workspace_id, display_name, issuer, client_id, \
         email_domains, created_by, updated_by) \
         VALUES ($1, 'SSO Alfa', 'https://login.example.test/tenant-a/v2.0', 'client-a', \
         '{alfa.example.test}', $2, $2) RETURNING id",
    )
    .bind(fx.ws)
    .bind(fx.admin)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0026_guards_sso_rows(pool: PgPool) {
    let fx = fixture(&pool).await;
    let idp = ok(insert_idp(&pool, &fx).await, "insert idp");

    // One IdP per company workspace.
    assert!(insert_idp(&pool, &fx).await.is_err());
    // Enabling needs a stored client secret and a passing discovery test.
    let enable =
        "UPDATE identity_providers SET enabled = true, version = version + 1 WHERE id = $1";
    assert!(sqlx::query(enable).bind(idp).execute(&pool).await.is_err());
    // Every update bumps the version by exactly one; the workspace never changes.
    assert!(
        sqlx::query("UPDATE identity_providers SET display_name = 'x' WHERE id = $1")
            .bind(idp)
            .execute(&pool)
            .await
            .is_err()
    );
    // Issuers are absolute http(s) URLs without user info, query or fragment.
    for bad in [
        "login.example.test",
        "https://u@login.example.test",
        "https://login.example.test/?x=1",
        "javascript://x",
    ] {
        let r = sqlx::query(
            "UPDATE identity_providers SET issuer = $2, version = version + 1 WHERE id = $1",
        )
        .bind(idp)
        .bind(bad)
        .execute(&pool)
        .await;
        assert!(r.is_err(), "{bad}");
    }

    // A subject links to one user per IdP, and a user has one subject per IdP.
    let link = "INSERT INTO external_identities (user_id, idp_id, subject) VALUES ($1, $2, $3)";
    ok(
        sqlx::query(link)
            .bind(fx.lead)
            .bind(idp)
            .bind("sub-1")
            .execute(&pool)
            .await,
        "link",
    );
    for (user, sub) in [(fx.admin, "sub-1"), (fx.lead, "sub-2")] {
        assert!(
            sqlx::query(link)
                .bind(user)
                .bind(idp)
                .bind(sub)
                .execute(&pool)
                .await
                .is_err()
        );
    }

    // An OIDC session always names its IdP; a password session never does.
    let sess = "INSERT INTO sessions (token_hash, csrf_hash, user_id, access_mode, expires_at, \
                auth_method, idp_id) VALUES ($1, $1, $2, 'password', now() + interval '1 hour', $3, $4)";
    for (n, method, with_idp) in [
        (1u8, "oidc", false),
        (2, "password", true),
        (3, "magic", false),
    ] {
        let r = sqlx::query(sess)
            .bind(vec![n; 32])
            .bind(fx.lead)
            .bind(method)
            .bind(with_idp.then_some(idp))
            .execute(&pool)
            .await;
        assert!(r.is_err(), "{method} {with_idp}");
    }
    ok(
        sqlx::query(sess)
            .bind(vec![9u8; 32])
            .bind(fx.lead)
            .bind("oidc")
            .bind(Some(idp))
            .execute(&pool)
            .await,
        "oidc session",
    );

    // A login attempt lives at most 15 minutes.
    let attempt = "INSERT INTO oidc_login_attempts (state_hash, idp_id, nonce_hash, verifier_key, \
                   verifier_nonce, verifier_ct, expires_at) VALUES ($1, $2, $1, 'k1', $3, $4, now() + $5::interval)";
    for (n, ttl, good) in [(1u8, "10 minutes", true), (2, "16 minutes", false)] {
        let r = sqlx::query(attempt)
            .bind(vec![n; 32])
            .bind(idp)
            .bind(vec![0u8; 12])
            .bind(vec![0u8; 48])
            .bind(ttl)
            .execute(&pool)
            .await;
        assert_eq!(r.is_ok(), good, "{ttl}");
    }
}

#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn migration_0026_rolls_back_and_reapplies(pool: PgPool) {
    let shape = "SELECT count(*) FROM information_schema.tables WHERE table_name IN \
                 ('identity_providers', 'external_identities', 'oidc_login_attempts')";
    let up: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "up");
    assert_eq!(up, 3);
    ok(db::MIGRATOR.undo(&pool, 25).await, "undo to 0025");
    let down: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "down");
    assert_eq!(down, 0);
    let col: i64 = ok(
        sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_name = 'sessions' AND column_name IN ('auth_method', 'idp_id')",
        )
        .fetch_one(&pool)
        .await,
        "session columns",
    );
    assert_eq!(col, 0);
    ok(db::MIGRATOR.run(&pool).await, "re-apply");
    let again: i64 = ok(sqlx::query_scalar(shape).fetch_one(&pool).await, "again");
    assert_eq!(again, 3);

    // Once an IdP is configured, down is refused instead of dropping it.
    let fx = fixture(&pool).await;
    ok(insert_idp(&pool, &fx).await, "insert idp");
    assert!(db::MIGRATOR.undo(&pool, 25).await.is_err());
    let kept: i64 = ok(
        sqlx::query_scalar("SELECT count(*) FROM identity_providers")
            .fetch_one(&pool)
            .await,
        "kept",
    );
    assert_eq!(kept, 1);
}
