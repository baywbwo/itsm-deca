//! Explicit synthetic seed for a dummy UAT database. Marks the database as a test
//! database only after the operator confirms the exact database name.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("nama database konfirmasi tidak cocok dengan database tujuan")]
    ConfirmationMismatch,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

pub const UAT_SUPERADMIN_NAME: &str = "Superadmin Uji (dummy)";

/// Idempotent. Creates two dummy companies/workspaces, a UAT Superadmin, and
/// CS / Lead CS members in the first workspace only.
pub async fn seed_uat(pool: &PgPool, confirm_db: &str) -> Result<(), SeedError> {
    let mut tx = pool.begin().await?;
    let current: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut *tx)
        .await?;
    if current != confirm_db {
        return Err(SeedError::ConfirmationMismatch);
    }
    sqlx::query(
        "INSERT INTO itsm_deployment_marker (kind, marked_db) VALUES ('test_dummy', $1) \
         ON CONFLICT (singleton) DO NOTHING",
    )
    .bind(&current)
    .execute(&mut *tx)
    .await?;

    let mut workspaces = Vec::new();
    for (code, name) in [
        ("DUMA", "PT Dummy Alfa (uji)"),
        ("DUMB", "PT Dummy Beta (uji)"),
    ] {
        let company: Uuid = sqlx::query_scalar(
            "INSERT INTO companies (code, name) VALUES ($1, $2) \
             ON CONFLICT (code) DO UPDATE SET code = EXCLUDED.code RETURNING id",
        )
        .bind(code)
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
        let ws: Uuid = sqlx::query_scalar(
            "INSERT INTO workspaces (company_id, code, name) VALUES ($1, $2, $3) \
             ON CONFLICT (code) DO UPDATE SET code = EXCLUDED.code RETURNING id",
        )
        .bind(company)
        .bind(code)
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
        workspaces.push(ws);
    }

    let superadmin = upsert_user(&mut tx, "dummy:uat-superadmin", UAT_SUPERADMIN_NAME).await?;
    sqlx::query(
        "INSERT INTO platform_grants (user_id, grant_code) VALUES ($1, 'uat_superadmin') \
         ON CONFLICT DO NOTHING",
    )
    .bind(superadmin)
    .execute(&mut *tx)
    .await?;

    let cs = upsert_user(&mut tx, "dummy:cs-alfa", "CS Alfa (dummy)").await?;
    let lead = upsert_user(&mut tx, "dummy:lead-alfa", "Lead CS Alfa (dummy)").await?;
    for (user, role) in [(cs, "cs"), (lead, "lead_cs")] {
        sqlx::query(
            "INSERT INTO workspace_memberships (workspace_id, user_id, role) VALUES ($1, $2, $3) \
             ON CONFLICT (workspace_id, user_id) DO NOTHING",
        )
        .bind(workspaces[0])
        .bind(user)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn upsert_user(
    tx: &mut sqlx::PgConnection,
    subject: &str,
    name: &str,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO users (external_subject, display_name) VALUES ($1, $2) \
         ON CONFLICT (external_subject) DO UPDATE SET display_name = EXCLUDED.display_name \
         RETURNING id",
    )
    .bind(subject)
    .bind(name)
    .fetch_one(tx)
    .await
}
