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

/// Dummy tickets for the D05 dashboard: (subject, status, owner, channel, priority,
/// received hours ago, resolved hours ago, reopened hours ago). Owner: 0 = none,
/// 1 = CS Alfa, 2 = Lead CS Alfa. Times are relative to the seed run.
type TicketSeed = (
    &'static str,
    &'static str,
    u8,
    Option<&'static str>,
    &'static str,
    i32,
    Option<i32>,
    Option<i32>,
);

const ALFA_TICKETS: &[TicketSeed] = &[
    (
        "Saldo tidak masuk setelah top up (dummy)",
        "new",
        0,
        Some("whatsapp"),
        "urgent",
        2,
        None,
        None,
    ),
    (
        "Transaksi QRIS gagal, saldo terpotong (dummy)",
        "new",
        0,
        Some("website"),
        "high",
        5,
        None,
        None,
    ),
    (
        "Permintaan ubah nomor rekening (dummy)",
        "new",
        0,
        Some("email"),
        "normal",
        30,
        None,
        None,
    ),
    (
        "Pengaduan tanpa kanal tercatat (dummy)",
        "new",
        0,
        None,
        "low",
        1,
        None,
        None,
    ),
    (
        "Status penarikan dana belum berubah (dummy)",
        "open",
        1,
        Some("whatsapp"),
        "high",
        50,
        None,
        None,
    ),
    (
        "Merchant tidak menerima notifikasi (dummy)",
        "open",
        1,
        Some("email"),
        "normal",
        3,
        None,
        None,
    ),
    (
        "Settlement merchant tertunda (dummy)",
        "open",
        2,
        Some("website"),
        "urgent",
        26,
        None,
        None,
    ),
    (
        "Konfirmasi dokumen pelanggan (dummy)",
        "pending_customer",
        1,
        Some("whatsapp"),
        "normal",
        72,
        None,
        None,
    ),
    (
        "Bukti transfer belum dikirim pelanggan (dummy)",
        "pending_customer",
        2,
        Some("email"),
        "low",
        8,
        None,
        None,
    ),
    (
        "Error API pembayaran, menunggu tim teknis (dummy)",
        "pending_engineering",
        2,
        Some("website"),
        "high",
        100,
        None,
        None,
    ),
    (
        "Refund ganda (dummy)",
        "resolved",
        1,
        Some("whatsapp"),
        "normal",
        20,
        Some(1),
        None,
    ),
    (
        "Pertanyaan biaya MDR (dummy)",
        "resolved",
        1,
        Some("website"),
        "normal",
        6,
        Some(2),
        None,
    ),
    (
        "Login aplikasi merchant gagal (dummy)",
        "resolved",
        2,
        Some("email"),
        "low",
        200,
        Some(150),
        None,
    ),
    (
        "Kartu ditolak di EDC (dummy)",
        "open",
        1,
        Some("whatsapp"),
        "high",
        10,
        Some(4),
        Some(1),
    ),
];

const BETA_TICKETS: &[TicketSeed] = &[
    (
        "Pengaduan workspace Beta (dummy)",
        "new",
        0,
        Some("email"),
        "high",
        4,
        None,
        None,
    ),
    (
        "Tindak lanjut Beta (dummy)",
        "open",
        0,
        Some("whatsapp"),
        "normal",
        40,
        None,
        None,
    ),
    (
        "Menunggu pelanggan Beta (dummy)",
        "pending_customer",
        0,
        Some("website"),
        "low",
        12,
        None,
        None,
    ),
];

/// Adds labelled dummy tickets to DUMA/DUMB on a marked dummy database, once: a workspace
/// that already has tickets is left untouched. Not called by `seed_uat`, so tests keep
/// their own numbering.
pub async fn seed_uat_tickets(pool: &PgPool, confirm_db: &str) -> Result<u64, SeedError> {
    let mut tx = pool.begin().await?;
    let current: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut *tx)
        .await?;
    let marked: Option<String> = sqlx::query_scalar(
        "SELECT marked_db FROM itsm_deployment_marker WHERE kind = 'test_dummy'",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if current != confirm_db || marked.as_deref() != Some(current.as_str()) {
        return Err(SeedError::ConfirmationMismatch);
    }
    let cs: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:cs-alfa'")
            .fetch_one(&mut *tx)
            .await?;
    let lead: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:lead-alfa'")
            .fetch_one(&mut *tx)
            .await?;
    let admin: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:uat-superadmin'")
            .fetch_one(&mut *tx)
            .await?;
    let mut created = 0;
    for (code, rows) in [("DUMA", ALFA_TICKETS), ("DUMB", BETA_TICKETS)] {
        let ws: Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE code = $1")
            .bind(code)
            .fetch_one(&mut *tx)
            .await?;
        let existing: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tickets WHERE workspace_id = $1")
                .bind(ws)
                .fetch_one(&mut *tx)
                .await?;
        if existing > 0 {
            continue;
        }
        for &(subject, status, owner, channel, priority, recv_h, resolved_h, reopened_h) in rows {
            let owner = match owner {
                1 => Some(cs),
                2 => Some(lead),
                _ => None,
            };
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO tickets (workspace_id, subject, chronology, received_at, created_by, \
                   owner_user_id, intake_channel_id, priority_id) \
                 VALUES ($1, $2, 'Kronologi uji (dummy).', now() - make_interval(hours => $3), $4, $5, \
                   (SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = $6), \
                   (SELECT id FROM priorities WHERE workspace_id = $1 AND code = $7)) \
                 RETURNING id",
            )
            .bind(ws)
            .bind(subject)
            .bind(recv_h)
            .bind(admin)
            .bind(owner)
            .bind(channel)
            .bind(priority)
            .fetch_one(&mut *tx)
            .await?;
            if let Some(h) = resolved_h {
                event(&mut tx, ws, id, 1, "resolve", h, owner.unwrap_or(admin)).await?;
                set_status(&mut tx, id, "resolved", 1).await?;
            }
            if let Some(h) = reopened_h {
                event(&mut tx, ws, id, 2, "reopen", h, owner.unwrap_or(admin)).await?;
                set_status(&mut tx, id, "open", 2).await?;
            }
            if status != "resolved" && status != "new" && reopened_h.is_none() {
                set_status(&mut tx, id, status, 1).await?;
            }
            created += 1;
        }
    }
    tx.commit().await?;
    Ok(created)
}

async fn event(
    tx: &mut sqlx::PgConnection,
    ws: Uuid,
    ticket: Uuid,
    cycle: i32,
    action: &str,
    hours_ago: i32,
    actor: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO ticket_resolution_events (workspace_id, ticket_id, cycle_no, action, summary, \
           actor_id, occurred_at) \
         VALUES ($1, $2, $3, $4, 'Ringkasan uji (dummy).', $5, now() - make_interval(hours => $6))",
    )
    .bind(ws)
    .bind(ticket)
    .bind(cycle)
    .bind(action)
    .bind(actor)
    .bind(hours_ago)
    .execute(tx)
    .await
    .map(|_| ())
}

async fn set_status(
    tx: &mut sqlx::PgConnection,
    ticket: Uuid,
    status: &str,
    cycle: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE tickets SET workflow_status = $2, cycle_no = $3, version = version + 1 WHERE id = $1",
    )
    .bind(ticket)
    .bind(status)
    .bind(cycle)
    .execute(tx)
    .await
    .map(|_| ())
}
