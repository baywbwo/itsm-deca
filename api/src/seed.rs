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
/// CS / Lead CS members in the first workspace only. Each workspace also gets
/// labelled dummy catalogs, a dummy merchant and simulation-only Jira/Lark directories.
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
    for ws in &workspaces {
        seed_reference_data(&mut tx, *ws).await?;
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

/// Catalog rows per workspace: (table, code, label). Priority codes are the existing
/// values; every other label is marked as test data.
const CATALOG_SEED: &[(&str, &str, &str)] = &[
    ("priorities", "low", "Rendah"),
    ("priorities", "normal", "Normal"),
    ("priorities", "high", "Tinggi"),
    ("priorities", "urgent", "Mendesak"),
    ("intake_channels", "whatsapp", "WhatsApp"),
    ("intake_channels", "email", "Email"),
    ("intake_channels", "website", "Website"),
    ("categories", "kategori_uji", "Kategori uji (dummy)"),
    ("products", "produk_uji", "Produk uji (dummy)"),
    ("receiving_units", "unit_uji", "Unit penerima uji (dummy)"),
    ("regions", "wilayah_uji", "Wilayah uji (dummy)"),
];

async fn seed_reference_data(tx: &mut sqlx::PgConnection, ws: Uuid) -> Result<(), sqlx::Error> {
    for (order, (table, code, label)) in (0_i32..).zip(CATALOG_SEED) {
        // Table names come from the constant above, never from input.
        let sql = format!(
            "INSERT INTO {table} (workspace_id, code, label, sort_order) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (workspace_id, code) DO NOTHING"
        );
        sqlx::query(&sql)
            .bind(ws)
            .bind(code)
            .bind(label)
            .bind(order)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO merchants (workspace_id, source, cif_reference, display_name) \
         VALUES ($1, 'dummy', 'DUMMY-0001', 'Merchant uji (dummy)') \
         ON CONFLICT (workspace_id, source, cif_reference) DO NOTHING",
    )
    .bind(ws)
    .execute(&mut *tx)
    .await?;

    let jira = simulation_connection(tx, ws, "jira", "Jira (simulasi)").await?;
    sqlx::query(
        "INSERT INTO jira_assignees (workspace_id, connection_id, external_account_ref, display_name) \
         VALUES ($1, $2, 'dummy-assignee-1', 'Assignee Jira uji (dummy)') \
         ON CONFLICT (connection_id, external_account_ref) DO NOTHING",
    )
    .bind(ws)
    .bind(jira)
    .execute(&mut *tx)
    .await?;
    let lark = simulation_connection(tx, ws, "lark", "Lark (simulasi)").await?;
    sqlx::query(
        "INSERT INTO lark_destinations (workspace_id, connection_id, external_chat_ref, display_name) \
         VALUES ($1, $2, 'dummy-chat-1', 'Grup Lark uji (dummy)') \
         ON CONFLICT (connection_id, external_chat_ref) DO NOTHING",
    )
    .bind(ws)
    .bind(lark)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Returns the workspace's first simulation connection for `provider`, creating it once.
async fn simulation_connection(
    tx: &mut sqlx::PgConnection,
    ws: Uuid,
    provider: &str,
    name: &str,
) -> Result<Uuid, sqlx::Error> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM integration_connections WHERE workspace_id = $1 AND provider = $2 \
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(ws)
    .bind(provider)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id, provider, mode, display_name) \
         VALUES ($1, $2, 'simulation', $3) RETURNING id",
    )
    .bind(ws)
    .bind(provider)
    .bind(name)
    .fetch_one(&mut *tx)
    .await
}
