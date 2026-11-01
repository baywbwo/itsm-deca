//! Scoped reads of tickets, attachment metadata and report runs (D02). Every query is
//! filtered by the workspace the caller was verified for; an ID from another workspace
//! answers 404 exactly like an unknown ID. Mutations, lists and downloads are later tasks.

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::domain::{TicketStatus, TicketStatusView};
use crate::error::AppError;
use crate::routes::workspaces::{find_accessible, parse_id};
use crate::session::Session;

/// Timestamps are rendered by PostgreSQL as RFC 3339 UTC strings.
macro_rules! utc {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}

#[derive(sqlx::FromRow)]
struct TicketRow {
    id: Uuid,
    number: i64,
    subject: String,
    chronology: String,
    received_at: String,
    workflow_status: String,
    cycle_no: i32,
    intake_channel_id: Option<Uuid>,
    priority_id: Option<Uuid>,
    category_id: Option<Uuid>,
    product_id: Option<Uuid>,
    receiving_unit_id: Option<Uuid>,
    region_id: Option<Uuid>,
    merchant_id: Option<Uuid>,
    owner_user_id: Option<Uuid>,
    transaction_activity: Option<String>,
    cause: Option<String>,
    foreign_branch_reference: Option<String>,
    version: i32,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketView {
    id: Uuid,
    workspace_id: Uuid,
    number: i64,
    /// `KODEWORKSPACE-number`; display only, never a relation key.
    display_number: String,
    subject: String,
    chronology: String,
    received_at: String,
    status: TicketStatusView,
    cycle_no: i32,
    intake_channel_id: Option<Uuid>,
    priority_id: Option<Uuid>,
    category_id: Option<Uuid>,
    product_id: Option<Uuid>,
    receiving_unit_id: Option<Uuid>,
    region_id: Option<Uuid>,
    merchant_id: Option<Uuid>,
    owner_user_id: Option<Uuid>,
    transaction_activity: Option<String>,
    cause: Option<String>,
    foreign_branch_reference: Option<String>,
    version: i32,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workflow_status code in database")]
struct UnknownStatus;

const TICKET_SELECT: &str = concat!(
    "SELECT id, number, subject, chronology, ",
    utc!("received_at"),
    " AS received_at, workflow_status, cycle_no, intake_channel_id, priority_id, \
     category_id, product_id, receiving_unit_id, region_id, merchant_id, owner_user_id, \
     transaction_activity, cause, foreign_branch_reference, version, ",
    utc!("created_at"),
    " AS created_at, ",
    utc!("updated_at"),
    " AS updated_at FROM tickets WHERE workspace_id = $1 AND id = $2"
);

pub async fn get_one(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
) -> Result<Json<TicketView>, AppError> {
    let ws = find_accessible(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    let row = sqlx::query_as::<_, TicketRow>(TICKET_SELECT)
        .bind(ws.id)
        .bind(ticket_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let status = TicketStatus::from_code(&row.workflow_status)
        .ok_or_else(|| AppError::internal(UnknownStatus))?;
    Ok(Json(TicketView {
        id: row.id,
        workspace_id: ws.id,
        number: row.number,
        display_number: format!("{}-{}", ws.code, row.number),
        subject: row.subject,
        chronology: row.chronology,
        received_at: row.received_at,
        status: status.into(),
        cycle_no: row.cycle_no,
        intake_channel_id: row.intake_channel_id,
        priority_id: row.priority_id,
        category_id: row.category_id,
        product_id: row.product_id,
        receiving_unit_id: row.receiving_unit_id,
        region_id: row.region_id,
        merchant_id: row.merchant_id,
        owner_user_id: row.owner_user_id,
        transaction_activity: row.transaction_activity,
        cause: row.cause,
        foreign_branch_reference: row.foreign_branch_reference,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// Metadata only; the object key and the file bytes are never exposed here.
#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    id: Uuid,
    ticket_id: Uuid,
    file_name: String,
    inspected_mime: Option<String>,
    size_bytes: i64,
    scan_status: String,
    visibility: String,
    created_at: String,
}

pub async fn attachment(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket, raw_att)): Path<(String, String, String)>,
) -> Result<Json<AttachmentView>, AppError> {
    let ws = find_accessible(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    let att_id = parse_id(&raw_att)?;
    let view = sqlx::query_as::<_, AttachmentView>(concat!(
        "SELECT id, ticket_id, file_name, inspected_mime, size_bytes, scan_status, visibility, ",
        utc!("created_at"),
        " AS created_at FROM attachments \
         WHERE workspace_id = $1 AND ticket_id = $2 AND id = $3"
    ))
    .bind(ws.id)
    .bind(ticket_id)
    .bind(att_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(view))
}

/// Operational report run metadata; the artifact reference itself is not returned.
#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ReportRunView {
    id: Uuid,
    report_type: String,
    period_start: String,
    period_end: String,
    state: String,
    generated_at: Option<String>,
    has_artifact: bool,
    created_at: String,
}

pub async fn report_run(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_run)): Path<(String, String)>,
) -> Result<Json<ReportRunView>, AppError> {
    let ws = find_accessible(&state, &s, parse_id(&raw_ws)?).await?;
    let run_id = parse_id(&raw_run)?;
    let view = sqlx::query_as::<_, ReportRunView>(concat!(
        "SELECT id, report_type, period_start::text AS period_start, \
         period_end::text AS period_end, state, ",
        utc!("generated_at"),
        " AS generated_at, artifact_ref IS NOT NULL AS has_artifact, ",
        utc!("created_at"),
        " AS created_at FROM report_runs WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(ws.id)
    .bind(run_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(view))
}
