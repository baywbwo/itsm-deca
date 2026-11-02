//! O06 Laporan (D10): operational company report with a server preview over the WHOLE
//! verified workspace scope and a real, permissioned CSV export.
//!
//! - One workspace = one reporting company; the company header (name, code, reporter code)
//!   comes from the database, never from the client.
//! - Only `operational` reports exist. The official BI / regulator report stays held until a
//!   verified mapping exists (D01 freeze, PRD §4): requesting it answers
//!   `422 regulator_mapping_unavailable` and nothing is created. No regulator code, format or
//!   compliance claim is made; the CSV says "bukan format regulator BI" on every row.
//! - Period = calendar days in the workspace time zone (Asia/Jakarta today), inclusive
//!   `periodStart..=periodEnd`, evaluated half-open in SQL. Date basis:
//!   `received` = `received_at` in the period; `resolved` = at least one `resolve` event in the
//!   period (a later reopen keeps the event). `resolutionCycles` counts events (cycles),
//!   `resolvedTickets` distinct tickets; the two are never summed.
//! - Status (current), PIC, kanal, kategori and prioritas narrow the rows the same way for the
//!   totals, breakdowns, preview rows and export, because all of them read one SQL source
//!   (`rt`) in one REPEATABLE READ snapshot. Pagination is never the basis of a total.
//! - Export: `Idempotency-Key` per user + workspace, synchronous (no worker); the run records
//!   the basis, filters, row count, SHA-256 of the exact CSV bytes and an expiry. More rows than
//!   `ITSM_REPORT_MAX_ROWS` = a failed run (`row_limit`), never a truncated file.
//! - Download re-checks membership + `report.view` and the workspace on every request, checks
//!   the stored hash, refuses expired artifacts (410) and is audited.
//! - CSV cells that a spreadsheet could read as a formula (leading `=`, `+`, `-`, `@`, tab, CR)
//!   are prefixed with `'`; every cell is quoted (RFC 4180, CRLF, UTF-8 with BOM).
//! - XLSX (D13): a real Office Open XML workbook (rust_xlsxwriter), never CSV renamed. Sheet
//!   "Data" has the CSV columns and rows (same SQL, same snapshot); sheet "Ringkasan" has the
//!   header, applied filters and the preview totals. Dates and times are typed date cells,
//!   counts are numbers. User text is only ever written as a string cell (never a formula) and
//!   carries Excel's quote prefix, so editing the cell cannot turn it into one either. Same
//!   idempotency, row limit, hash, expiry, ACL and audit as CSV; the format is part of the
//!   idempotency hash and is stored on the run.

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::domain::{TicketStatus, TicketStatusView};
use crate::error::AppError;
use crate::routes::dashboard::{self, Dim, FilterOptions, Filters, NONE, UNASSIGNED};
use crate::routes::intake::idempotency_key;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

/// Rows shown in the preview table; totals and breakdowns always cover every row.
pub const PREVIEW_ROWS: i64 = 50;
/// Longest period one report may cover (days, inclusive).
pub const MAX_PERIOD_DAYS: i64 = 366;
/// How long an export artifact can be downloaded.
pub const ARTIFACT_TTL_HOURS: i64 = 24;
/// Runs listed on the reports page.
const RUN_LIST_LIMIT: i64 = 20;

const OPERATIONAL: &str = "operational";
const REGULATOR: &str = "bi_regulator";
const KIND_LABEL: &str = "Rekap operasional (bukan format regulator BI)";
const NOT_SET: &str = "Tidak diisi";
const NO_PIC: &str = "Belum ditugaskan";

macro_rules! utc {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}

// ---------- request ----------

/// Artifact format of an export run (`report_runs.format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Xlsx,
}

impl Format {
    fn parse(raw: Option<&str>) -> Option<Format> {
        match raw {
            None | Some("csv") => Some(Format::Csv),
            Some("xlsx") => Some(Format::Xlsx),
            Some(_) => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Xlsx => "xlsx",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Format::Csv => "text/csv; charset=utf-8",
            Format::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Received,
    Resolved,
}

impl Basis {
    fn parse(raw: Option<&str>) -> Option<Basis> {
        match raw {
            None | Some("received") => Some(Basis::Received),
            Some("resolved") => Some(Basis::Resolved),
            Some(_) => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Basis::Received => "received",
            Basis::Resolved => "resolved",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Basis::Received => "Tanggal diterima",
            Basis::Resolved => "Tanggal diselesaikan (peristiwa resolusi)",
        }
    }
}

/// Query of `GET /reports/preview` (all optional except the period).
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PreviewQuery {
    report_type: Option<String>,
    period_start: Option<String>,
    period_end: Option<String>,
    basis: Option<String>,
    /// Comma-separated status codes; absent = every status.
    status: Option<String>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
}

/// Body of `POST /report-runs`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExportBody {
    report_type: String,
    period_start: String,
    period_end: String,
    basis: Option<String>,
    status: Option<Vec<String>>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
    /// `csv` (default) or `xlsx`.
    format: Option<String>,
}

/// A validated report request. Serialized (canonical order) for the idempotency hash and the
/// stored `filters` of a run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    period_start: String,
    period_end: String,
    basis: Basis,
    /// Sorted, distinct; `None` = every status.
    status: Option<Vec<&'static str>>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
    #[serde(skip)]
    filters: Filters,
}

/// Strict `YYYY-MM-DD` (years 2000–2100) → days since 1970-01-01.
fn day_number(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(2000..=2100).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=dim).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn report_type(raw: Option<&str>) -> Result<(), AppError> {
    match raw {
        None | Some(OPERATIONAL) => Ok(()),
        Some(REGULATOR) => Err(AppError::RegulatorMappingUnavailable),
        Some(_) => Err(AppError::Validation("reportType")),
    }
}

#[allow(clippy::too_many_arguments)]
fn spec(
    period_start: Option<&str>,
    period_end: Option<&str>,
    basis: Option<&str>,
    status: Option<Vec<&str>>,
    owner: Option<&str>,
    channel: Option<&str>,
    category: Option<&str>,
    priority: Option<&str>,
) -> Result<Spec, AppError> {
    let start_raw = period_start.ok_or(AppError::Validation("periodStart"))?;
    let end_raw = period_end.ok_or(AppError::Validation("periodEnd"))?;
    let start = day_number(start_raw).ok_or(AppError::Validation("periodStart"))?;
    let end = day_number(end_raw).ok_or(AppError::Validation("periodEnd"))?;
    if end < start || end - start + 1 > MAX_PERIOD_DAYS {
        return Err(AppError::Validation("periodEnd"));
    }
    let basis = Basis::parse(basis).ok_or(AppError::Validation("basis"))?;
    let status = match status {
        None => None,
        Some(codes) => {
            let mut out: Vec<&'static str> = Vec::with_capacity(codes.len());
            for c in codes {
                let s = TicketStatus::from_code(c).ok_or(AppError::Validation("status"))?;
                out.push(s.code());
            }
            out.sort_unstable();
            out.dedup();
            if out.is_empty() {
                return Err(AppError::Validation("status"));
            }
            Some(out)
        }
    };
    let filters = Filters {
        owner: Dim::parse(owner, UNASSIGNED).ok_or(AppError::Validation("owner"))?,
        channel: Dim::parse(channel, NONE).ok_or(AppError::Validation("channel"))?,
        category: Dim::parse(category, NONE).ok_or(AppError::Validation("category"))?,
        priority: Dim::parse(priority, NONE).ok_or(AppError::Validation("priority"))?,
    };
    Ok(Spec {
        period_start: start_raw.to_owned(),
        period_end: end_raw.to_owned(),
        basis,
        status,
        owner: filters.owner.wire(UNASSIGNED),
        channel: filters.channel.wire(NONE),
        category: filters.category.wire(NONE),
        priority: filters.priority.wire(NONE),
        filters,
    })
}

impl PreviewQuery {
    fn spec(&self) -> Result<Spec, AppError> {
        report_type(self.report_type.as_deref())?;
        let status = self.status.as_deref().map(|s| s.split(',').collect());
        spec(
            self.period_start.as_deref(),
            self.period_end.as_deref(),
            self.basis.as_deref(),
            status,
            self.owner.as_deref(),
            self.channel.as_deref(),
            self.category.as_deref(),
            self.priority.as_deref(),
        )
    }
}

impl ExportBody {
    fn format(&self) -> Result<Format, AppError> {
        Format::parse(self.format.as_deref()).ok_or(AppError::Validation("format"))
    }

    fn spec(&self) -> Result<Spec, AppError> {
        report_type(Some(&self.report_type))?;
        spec(
            Some(&self.period_start),
            Some(&self.period_end),
            self.basis.as_deref(),
            self.status
                .as_ref()
                .map(|v| v.iter().map(String::as_str).collect()),
            self.owner.as_deref(),
            self.channel.as_deref(),
            self.category.as_deref(),
            self.priority.as_deref(),
        )
    }
}

// ---------- SQL (one source for totals, breakdowns, preview rows and export) ----------

/// Parameters: `$1` workspace, `$2/$3` first/last local day (inclusive), `$4` basis,
/// `$5` status codes (NULL = all), then per dimension a UUID and an "unset" flag:
/// `$6/$7` owner, `$8/$9` channel, `$10/$11` category, `$12/$13` priority.
/// `p` = company header + period bounds, `rt` = the report's tickets.
macro_rules! base {
    () => {
        "WITH p AS ( \
           SELECT w.timezone AS tz, w.code AS workspace_code, c.code AS company_code, \
             c.name AS company_name, c.reporter_code, \
             ($2::date::timestamp AT TIME ZONE w.timezone) AS start_at, \
             (($3::date + 1)::timestamp AT TIME ZONE w.timezone) AS end_at \
           FROM workspaces w JOIN companies c ON c.id = w.company_id WHERE w.id = $1), \
         ev AS ( \
           SELECT e.ticket_id, count(*) AS cycles, max(e.occurred_at) AS last_resolved_at \
           FROM ticket_resolution_events e, p \
           WHERE e.workspace_id = $1 AND e.action = 'resolve' \
             AND e.occurred_at >= p.start_at AND e.occurred_at < p.end_at GROUP BY 1), \
         rt AS ( \
           SELECT t.id, t.number, t.number_prefix, t.subject, t.workflow_status, t.received_at, \
             t.owner_user_id, \
             t.intake_channel_id, t.category_id, t.priority_id, \
             COALESCE(ev.cycles, 0) AS cycles, ev.last_resolved_at \
           FROM tickets t CROSS JOIN p LEFT JOIN ev ON ev.ticket_id = t.id \
           WHERE t.workspace_id = $1 \
             AND CASE $4::text WHEN 'resolved' THEN ev.ticket_id IS NOT NULL \
               ELSE t.received_at >= p.start_at AND t.received_at < p.end_at END \
             AND ($5::text[] IS NULL OR t.workflow_status = ANY($5)) \
             AND ($6::uuid IS NULL OR t.owner_user_id = $6) AND (NOT $7::bool OR t.owner_user_id IS NULL) \
             AND ($8::uuid IS NULL OR t.intake_channel_id = $8) AND (NOT $9::bool OR t.intake_channel_id IS NULL) \
             AND ($10::uuid IS NULL OR t.category_id = $10) AND (NOT $11::bool OR t.category_id IS NULL) \
             AND ($12::uuid IS NULL OR t.priority_id = $12) AND (NOT $13::bool OR t.priority_id IS NULL)) "
    };
}

macro_rules! bind_base {
    ($q:expr, $ws:expr, $s:expr) => {
        $q.bind($ws)
            .bind(&$s.period_start)
            .bind(&$s.period_end)
            .bind($s.basis.code())
            .bind(&$s.status)
            .bind($s.filters.owner.id())
            .bind($s.filters.owner.unset())
            .bind($s.filters.channel.id())
            .bind($s.filters.channel.unset())
            .bind($s.filters.category.id())
            .bind($s.filters.category.unset())
            .bind($s.filters.priority.id())
            .bind($s.filters.priority.unset())
    };
}

/// Every filter value must belong to this workspace (any membership / catalog row).
const VALID_SQL: &str = "SELECT \
       ($2::uuid IS NULL OR EXISTS (SELECT 1 FROM workspace_memberships m WHERE m.workspace_id = $1 AND m.user_id = $2)), \
       ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM intake_channels c WHERE c.workspace_id = $1 AND c.id = $3)), \
       ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM categories c WHERE c.workspace_id = $1 AND c.id = $4)), \
       ($5::uuid IS NULL OR EXISTS (SELECT 1 FROM priorities c WHERE c.workspace_id = $1 AND c.id = $5))";

const HEADER_SQL: &str = concat!(
    base!(),
    "SELECT tz, workspace_code, company_code, company_name, reporter_code, ",
    utc!("start_at"),
    " AS start_at, ",
    utc!("end_at"),
    " AS end_at, ",
    utc!("now()"),
    " AS as_of FROM p"
);

const TOTALS_SQL: &str = concat!(
    base!(),
    "SELECT count(*) AS tickets, COALESCE(sum(cycles), 0)::bigint AS resolution_cycles, \
       count(*) FILTER (WHERE cycles > 0) AS resolved_tickets, \
       count(*) FILTER (WHERE owner_user_id IS NULL) AS unassigned, \
       count(*) FILTER (WHERE intake_channel_id IS NULL) AS missing_channel, \
       count(*) FILTER (WHERE category_id IS NULL) AS missing_category, \
       count(*) FILTER (WHERE priority_id IS NULL) AS missing_priority \
     FROM rt"
);

const STATUS_SQL: &str = concat!(
    base!(),
    "SELECT workflow_status, count(*) FROM rt GROUP BY 1"
);

/// `table`/`col` are compile-time constants, never request input.
fn breakdown_sql(table: &str, col: &str) -> String {
    format!(
        concat!(
            base!(),
            "SELECT c.id, c.label, count(*) AS n FROM rt \
             LEFT JOIN {table} c ON c.workspace_id = $1 AND c.id = rt.{col} \
             GROUP BY c.id, c.label, c.sort_order \
             ORDER BY (c.id IS NULL), n DESC, c.sort_order, c.label"
        ),
        table = table,
        col = col
    )
}

const OWNER_SQL: &str = concat!(
    base!(),
    "SELECT u.id, u.display_name AS label, count(*) AS n FROM rt \
     LEFT JOIN users u ON u.id = rt.owner_user_id \
     GROUP BY u.id, u.display_name ORDER BY (u.id IS NULL), n DESC, u.display_name"
);

/// `$14` = row limit (NULL = all). Stable order: received, then number.
const ROWS_SQL: &str = concat!(
    base!(),
    "SELECT rt.id, rt.number, rt.number_prefix, rt.subject, rt.workflow_status, \
       pr.label AS priority, cat.label AS category, ic.label AS channel, u.display_name AS owner, \
       to_char(rt.received_at AT TIME ZONE p.tz, 'YYYY-MM-DD HH24:MI') AS received_local, \
       rt.cycles AS resolution_cycles, \
       to_char(rt.last_resolved_at AT TIME ZONE p.tz, 'YYYY-MM-DD HH24:MI') AS last_resolved_local \
     FROM rt CROSS JOIN p \
     LEFT JOIN priorities pr ON pr.workspace_id = $1 AND pr.id = rt.priority_id \
     LEFT JOIN categories cat ON cat.workspace_id = $1 AND cat.id = rt.category_id \
     LEFT JOIN intake_channels ic ON ic.workspace_id = $1 AND ic.id = rt.intake_channel_id \
     LEFT JOIN users u ON u.id = rt.owner_user_id \
     ORDER BY rt.received_at, rt.number \
     LIMIT $14"
);

// ---------- rows / views ----------

#[derive(sqlx::FromRow)]
struct HeaderRow {
    tz: String,
    workspace_code: String,
    company_code: String,
    company_name: String,
    reporter_code: Option<String>,
    start_at: String,
    end_at: String,
    as_of: String,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    /// Rows of the report (= rows of the export).
    tickets: i64,
    /// Resolve events in the period among those tickets (one per cycle).
    resolution_cycles: i64,
    /// Distinct tickets among them with at least one resolve event in the period.
    resolved_tickets: i64,
    unassigned: i64,
    missing_channel: i64,
    missing_category: i64,
    missing_priority: i64,
}

#[derive(sqlx::FromRow)]
struct RowDb {
    id: Uuid,
    number: i64,
    /// Prefix the ticket was issued with (kept after a workspace code change).
    number_prefix: String,
    subject: String,
    workflow_status: String,
    priority: Option<String>,
    category: Option<String>,
    channel: Option<String>,
    owner: Option<String>,
    received_local: String,
    resolution_cycles: i64,
    last_resolved_local: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRow {
    ticket_id: Uuid,
    display_number: String,
    subject: String,
    status: TicketStatusView,
    priority: Option<String>,
    category: Option<String>,
    channel: Option<String>,
    /// PIC display name; `null` = belum ditugaskan.
    owner: Option<String>,
    /// Local time of the workspace (`header.timezone`), `YYYY-MM-DD HH:MM`.
    received_local: String,
    resolution_cycles: i64,
    last_resolved_local: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Breakdown {
    /// `null` = not recorded (or belum ditugaskan for PIC).
    id: Option<Uuid>,
    label: Option<String>,
    #[sqlx(rename = "n")]
    count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusBreakdown {
    status: TicketStatusView,
    count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    company_name: String,
    company_code: String,
    /// `null` until configured in Pengaturan → Perusahaan. Never guessed.
    reporter_code: Option<String>,
    workspace_id: Uuid,
    workspace_code: String,
    timezone: String,
    period_start: String,
    period_end: String,
    /// RFC 3339 UTC, half-open `[startAt, endAt)`.
    start_at: String,
    end_at: String,
    basis: Basis,
    basis_label: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportTypeOption {
    code: &'static str,
    label: &'static str,
    available: bool,
    /// Why it is not available; `null` when available.
    reason: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationItem {
    code: &'static str,
    /// `info`, `warning` (shown, does not block the operational export) or `blocking`.
    level: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFilters {
    status: Option<Vec<&'static str>>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    report_type: &'static str,
    report_label: &'static str,
    report_types: Vec<ReportTypeOption>,
    header: Header,
    as_of: String,
    filters: AppliedFilters,
    filter_options: FilterOptions,
    totals: Totals,
    by_status: Vec<StatusBreakdown>,
    by_priority: Vec<Breakdown>,
    by_channel: Vec<Breakdown>,
    by_category: Vec<Breakdown>,
    by_owner: Vec<Breakdown>,
    validation: Vec<ValidationItem>,
    /// The first `previewLimit` rows in export order; `totals.tickets` counts all of them.
    rows: Vec<ReportRow>,
    preview_limit: i64,
    export_max_rows: i64,
    /// False when the export would be refused (too many rows).
    exportable: bool,
    notice: &'static str,
}

const NOTICE: &str = "Rekap operasional internal, bukan format atau laporan regulator BI. \
     Tidak dikirim ke BI, Lark atau pihak lain.";

fn report_types() -> Vec<ReportTypeOption> {
    vec![
        ReportTypeOption {
            code: OPERATIONAL,
            label: "Rekap operasional",
            available: true,
            reason: None,
        },
        ReportTypeOption {
            code: REGULATOR,
            label: "Laporan resmi BI",
            available: false,
            reason: Some(
                "Ditahan: format, kode dan field wajib belum diverifikasi; mapping belum valid.",
            ),
        },
    ]
}

fn row_view(r: RowDb) -> Result<ReportRow, AppError> {
    let status = TicketStatus::from_code(&r.workflow_status)
        .ok_or_else(|| AppError::internal(UnknownStatus))?;
    Ok(ReportRow {
        ticket_id: r.id,
        display_number: format!("{}-{}", r.number_prefix, r.number),
        subject: r.subject,
        status: status.into(),
        priority: r.priority,
        category: r.category,
        channel: r.channel,
        owner: r.owner,
        received_local: r.received_local,
        resolution_cycles: r.resolution_cycles,
        last_resolved_local: r.last_resolved_local,
    })
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workflow_status code in database")]
struct UnknownStatus;

/// Checks that every filter value belongs to the workspace; else 422 on that field.
async fn check_filters(conn: &mut PgConnection, ws: Uuid, s: &Spec) -> Result<(), AppError> {
    let (o, c, k, p): (bool, bool, bool, bool) = sqlx::query_as(VALID_SQL)
        .bind(ws)
        .bind(s.filters.owner.id())
        .bind(s.filters.channel.id())
        .bind(s.filters.category.id())
        .bind(s.filters.priority.id())
        .fetch_one(&mut *conn)
        .await?;
    for (ok, field) in [
        (o, "owner"),
        (c, "channel"),
        (k, "category"),
        (p, "priority"),
    ] {
        if !ok {
            return Err(AppError::Validation(field));
        }
    }
    Ok(())
}

async fn header(conn: &mut PgConnection, ws: Uuid, s: &Spec) -> Result<HeaderRow, AppError> {
    bind_base!(sqlx::query_as(HEADER_SQL), ws, s)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AppError::NotFound)
}

async fn rows(
    conn: &mut PgConnection,
    ws: Uuid,
    s: &Spec,
    limit: Option<i64>,
) -> Result<Vec<RowDb>, AppError> {
    Ok(bind_base!(sqlx::query_as(ROWS_SQL), ws, s)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?)
}

async fn snapshot(conn: &mut PgConnection, read_only: bool) -> Result<(), AppError> {
    let sql = if read_only {
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY"
    } else {
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"
    };
    sqlx::query(sql).execute(&mut *conn).await?;
    Ok(())
}

// ---------- preview ----------

pub async fn preview(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
    q: Result<Query<PreviewQuery>, QueryRejection>,
) -> Result<Json<Preview>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::ReportView)?;
    let Query(q) = q.map_err(|_| AppError::InvalidRequest)?;
    let spec = q.spec()?;
    let ws = access.workspace_id;
    let max_rows = state.config.report_max_rows;

    let mut tx = state.pool.begin().await?;
    snapshot(&mut tx, true).await?;
    check_filters(&mut tx, ws, &spec).await?;
    let h = header(&mut tx, ws, &spec).await?;
    let totals: Totals = bind_base!(sqlx::query_as(TOTALS_SQL), ws, &spec)
        .fetch_one(&mut *tx)
        .await?;
    let by_status_raw: Vec<(String, i64)> = bind_base!(sqlx::query_as(STATUS_SQL), ws, &spec)
        .fetch_all(&mut *tx)
        .await?;
    let mut dims = Vec::with_capacity(3);
    for (table, col) in [
        ("priorities", "priority_id"),
        ("intake_channels", "intake_channel_id"),
        ("categories", "category_id"),
    ] {
        let b: Vec<Breakdown> = bind_base!(sqlx::query_as(&breakdown_sql(table, col)), ws, &spec)
            .fetch_all(&mut *tx)
            .await?;
        dims.push(b);
    }
    let by_owner: Vec<Breakdown> = bind_base!(sqlx::query_as(OWNER_SQL), ws, &spec)
        .fetch_all(&mut *tx)
        .await?;
    let sample = rows(&mut tx, ws, &spec, Some(PREVIEW_ROWS)).await?;
    let filter_options = dashboard::filter_options(&mut tx, ws).await?;
    tx.commit().await?;

    if by_status_raw
        .iter()
        .any(|(k, _)| TicketStatus::from_code(k).is_none())
    {
        return Err(AppError::internal(UnknownStatus));
    }
    let by_status = TicketStatus::ALL
        .into_iter()
        .map(|st| StatusBreakdown {
            status: st.into(),
            count: by_status_raw
                .iter()
                .find(|(k, _)| k == st.code())
                .map_or(0, |(_, n)| *n),
        })
        .collect();
    let rows = sample
        .into_iter()
        .map(row_view)
        .collect::<Result<Vec<_>, _>>()?;
    let validation = validation(&h, &totals, max_rows);
    let exportable = totals.tickets <= max_rows;
    let mut dims = dims.into_iter();
    Ok(Json(Preview {
        report_type: OPERATIONAL,
        report_label: KIND_LABEL,
        report_types: report_types(),
        header: Header {
            company_name: h.company_name,
            company_code: h.company_code,
            reporter_code: h.reporter_code,
            workspace_id: ws,
            workspace_code: h.workspace_code,
            timezone: h.tz,
            period_start: spec.period_start.clone(),
            period_end: spec.period_end.clone(),
            start_at: h.start_at,
            end_at: h.end_at,
            basis: spec.basis,
            basis_label: spec.basis.label(),
        },
        as_of: h.as_of,
        filters: AppliedFilters {
            status: spec.status.clone(),
            owner: spec.owner.clone(),
            channel: spec.channel.clone(),
            category: spec.category.clone(),
            priority: spec.priority.clone(),
        },
        filter_options,
        totals,
        by_status,
        by_priority: dims.next().unwrap_or_default(),
        by_channel: dims.next().unwrap_or_default(),
        by_category: dims.next().unwrap_or_default(),
        by_owner,
        validation,
        rows,
        preview_limit: PREVIEW_ROWS,
        export_max_rows: max_rows,
        exportable,
        notice: NOTICE,
    }))
}

fn validation(h: &HeaderRow, t: &Totals, max_rows: i64) -> Vec<ValidationItem> {
    let mut v = vec![ValidationItem {
        code: "regulator_mapping_unavailable",
        level: "info",
        message: "Ekspor resmi BI ditahan: format, kode dan field wajib belum diverifikasi. \
                  Rekap ini tidak menyatakan patuh regulator."
            .to_owned(),
        count: None,
    }];
    if h.reporter_code.is_none() {
        v.push(ValidationItem {
            code: "reporter_code_missing",
            level: "warning",
            message: "Kode pelapor perusahaan belum dikonfigurasi (Pengaturan → Perusahaan). \
                      Tidak menghalangi rekap operasional."
                .to_owned(),
            count: None,
        });
    }
    for (code, n, what) in [
        ("missing_priority", t.missing_priority, "prioritas"),
        ("missing_category", t.missing_category, "kategori"),
        ("missing_channel", t.missing_channel, "kanal penerimaan"),
    ] {
        if n > 0 {
            v.push(ValidationItem {
                code,
                level: "warning",
                message: format!("{n} tiket tanpa {what}; ditulis \"{NOT_SET}\" di ekspor."),
                count: Some(n),
            });
        }
    }
    if t.unassigned > 0 {
        v.push(ValidationItem {
            code: "unassigned",
            level: "info",
            message: format!(
                "{} tiket belum memiliki PIC; ditulis \"{NO_PIC}\" di ekspor.",
                t.unassigned
            ),
            count: Some(t.unassigned),
        });
    }
    if t.tickets > max_rows {
        v.push(ValidationItem {
            code: "row_limit",
            level: "blocking",
            message: format!(
                "{} baris melebihi batas ekspor {max_rows}. Persempit periode atau filter.",
                t.tickets
            ),
            count: Some(t.tickets),
        });
    }
    v
}

// ---------- CSV ----------

const CSV_COLUMNS: [&str; 18] = [
    "jenis_laporan",
    "kode_perusahaan",
    "nama_perusahaan",
    "kode_pelapor",
    "periode_mulai",
    "periode_selesai",
    "basis_tanggal",
    "zona_waktu",
    "nomor_tiket",
    "subjek",
    "status",
    "prioritas",
    "kategori",
    "kanal",
    "pic",
    "diterima_lokal",
    "siklus_resolusi_dalam_periode",
    "resolusi_terakhir_dalam_periode_lokal",
];

/// One quoted CSV cell. A value a spreadsheet could evaluate as a formula is prefixed with
/// `'` so it stays text (OWASP CSV injection guidance).
pub fn csv_cell(raw: &str) -> String {
    let neutral = match raw.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{raw}"),
        _ => raw.to_owned(),
    };
    format!("\"{}\"", neutral.replace('"', "\"\""))
}

fn csv_line(out: &mut String, cells: &[&str]) {
    let line: Vec<String> = cells.iter().map(|c| csv_cell(c)).collect();
    out.push_str(&line.join(","));
    out.push_str("\r\n");
}

fn build_csv(h: &HeaderRow, s: &Spec, rows: &[RowDb]) -> Result<Vec<u8>, AppError> {
    let mut out = String::from("\u{feff}");
    csv_line(&mut out, &CSV_COLUMNS);
    let reporter = h.reporter_code.as_deref().unwrap_or("");
    for r in rows {
        let status = TicketStatus::from_code(&r.workflow_status)
            .ok_or_else(|| AppError::internal(UnknownStatus))?;
        let number = format!("{}-{}", r.number_prefix, r.number);
        let cycles = r.resolution_cycles.to_string();
        csv_line(
            &mut out,
            &[
                KIND_LABEL,
                &h.company_code,
                &h.company_name,
                reporter,
                &s.period_start,
                &s.period_end,
                s.basis.label(),
                &h.tz,
                &number,
                &r.subject,
                status.label(),
                r.priority.as_deref().unwrap_or(NOT_SET),
                r.category.as_deref().unwrap_or(NOT_SET),
                r.channel.as_deref().unwrap_or(NOT_SET),
                r.owner.as_deref().unwrap_or(NO_PIC),
                &r.received_local,
                &cycles,
                r.last_resolved_local.as_deref().unwrap_or(""),
            ],
        );
    }
    Ok(out.into_bytes())
}

// ---------- XLSX ----------

/// Human labels of the applied filters for the "Ringkasan" sheet ("Semua" = not filtered).
struct FilterLabels {
    status: String,
    owner: String,
    channel: String,
    category: String,
    priority: String,
}

const ALL: &str = "Semua";

async fn filter_labels(
    conn: &mut PgConnection,
    ws: Uuid,
    s: &Spec,
) -> Result<FilterLabels, AppError> {
    let (owner, channel, category, priority): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT (SELECT u.display_name FROM users u JOIN workspace_memberships m \
                   ON m.user_id = u.id AND m.workspace_id = $1 WHERE u.id = $2), \
                (SELECT label FROM intake_channels WHERE workspace_id = $1 AND id = $3), \
                (SELECT label FROM categories WHERE workspace_id = $1 AND id = $4), \
                (SELECT label FROM priorities WHERE workspace_id = $1 AND id = $5)",
    )
    .bind(ws)
    .bind(s.filters.owner.id())
    .bind(s.filters.channel.id())
    .bind(s.filters.category.id())
    .bind(s.filters.priority.id())
    .fetch_one(&mut *conn)
    .await?;
    let pick = |wire: &Option<String>, unset: &str, label: Option<String>, none: &str| match wire {
        None => ALL.to_owned(),
        Some(w) if w == unset => none.to_owned(),
        Some(_) => label.unwrap_or_default(),
    };
    let status = match &s.status {
        None => ALL.to_owned(),
        Some(codes) => codes
            .iter()
            .filter_map(|c| TicketStatus::from_code(c))
            .map(TicketStatus::label)
            .collect::<Vec<_>>()
            .join(", "),
    };
    Ok(FilterLabels {
        status,
        owner: pick(&s.owner, UNASSIGNED, owner, NO_PIC),
        channel: pick(&s.channel, NONE, channel, NOT_SET),
        category: pick(&s.category, NONE, category, NOT_SET),
        priority: pick(&s.priority, NONE, priority, NOT_SET),
    })
}

/// `YYYY-MM-DD` (already validated) → Excel date.
fn xl_date(s: &str) -> Result<rust_xlsxwriter::ExcelDateTime, AppError> {
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|p| p.parse::<u16>().ok());
    match (n(0..4), n(5..7), n(8..10)) {
        (Some(y), Some(m), Some(d)) => rust_xlsxwriter::ExcelDateTime::from_ymd(
            y,
            u8::try_from(m).map_err(AppError::internal)?,
            u8::try_from(d).map_err(AppError::internal)?,
        )
        .map_err(AppError::internal),
        _ => Err(AppError::internal(BadLocalTime)),
    }
}

/// `YYYY-MM-DD HH:MM` (from SQL `to_char`) → Excel date-time, no time zone shift.
fn xl_datetime(s: &str) -> Result<rust_xlsxwriter::ExcelDateTime, AppError> {
    if s.len() != 16 || s.as_bytes()[10] != b' ' {
        return Err(AppError::internal(BadLocalTime));
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|p| p.parse::<u16>().ok());
    let (Some(h), Some(mi)) = (n(11..13), n(14..16)) else {
        return Err(AppError::internal(BadLocalTime));
    };
    xl_date(&s[..10])?
        .and_hms(h, u8::try_from(mi).map_err(AppError::internal)?, 0)
        .map_err(AppError::internal)
}

#[derive(Debug, thiserror::Error)]
#[error("unexpected local time format from database")]
struct BadLocalTime;

fn build_xlsx(
    h: &HeaderRow,
    s: &Spec,
    rows: &[RowDb],
    t: &Totals,
    f: &FilterLabels,
) -> Result<Vec<u8>, AppError> {
    use rust_xlsxwriter::{Format as Xf, Workbook};
    let e = AppError::internal;
    let head = Xf::new().set_bold();
    // Quote prefix: the cell stays text even when a user edits it in Excel.
    let text = Xf::new().set_quote_prefix();
    let date = Xf::new().set_num_format("yyyy-mm-dd");
    let datetime = Xf::new().set_num_format("yyyy-mm-dd hh:mm");
    let int = Xf::new().set_num_format("0");

    let mut wb = Workbook::new();
    let data = wb.add_worksheet();
    data.set_name("Data").map_err(e)?;
    for (c, name) in (0u16..).zip(CSV_COLUMNS) {
        data.write_string_with_format(0, c, name, &head)
            .map_err(e)?;
    }
    let reporter = h.reporter_code.as_deref().unwrap_or("");
    let (start, end) = (xl_date(&s.period_start)?, xl_date(&s.period_end)?);
    for (r, row) in (1u32..).zip(rows) {
        let status = TicketStatus::from_code(&row.workflow_status)
            .ok_or_else(|| AppError::internal(UnknownStatus))?;
        let number = format!("{}-{}", row.number_prefix, row.number);
        let texts: [(u16, &str); 13] = [
            (0, KIND_LABEL),
            (1, &h.company_code),
            (2, &h.company_name),
            (3, reporter),
            (6, s.basis.label()),
            (7, &h.tz),
            (8, &number),
            (9, &row.subject),
            (10, status.label()),
            (11, row.priority.as_deref().unwrap_or(NOT_SET)),
            (12, row.category.as_deref().unwrap_or(NOT_SET)),
            (13, row.channel.as_deref().unwrap_or(NOT_SET)),
            (14, row.owner.as_deref().unwrap_or(NO_PIC)),
        ];
        for (c, v) in texts {
            if !v.is_empty() {
                data.write_string_with_format(r, c, v, &text).map_err(e)?;
            }
        }
        data.write_datetime_with_format(r, 4, &start, &date)
            .map_err(e)?;
        data.write_datetime_with_format(r, 5, &end, &date)
            .map_err(e)?;
        data.write_datetime_with_format(r, 15, &xl_datetime(&row.received_local)?, &datetime)
            .map_err(e)?;
        #[allow(clippy::cast_precision_loss)]
        data.write_number_with_format(r, 16, row.resolution_cycles as f64, &int)
            .map_err(e)?;
        if let Some(v) = row.last_resolved_local.as_deref() {
            data.write_datetime_with_format(r, 17, &xl_datetime(v)?, &datetime)
                .map_err(e)?;
        }
    }
    let last = u32::try_from(rows.len()).map_err(AppError::internal)?;
    data.autofilter(0, 0, last, 17).map_err(e)?;
    data.set_freeze_panes(1, 0).map_err(e)?;
    for (c, w) in [
        (0u16, 24.0),
        (2, 22.0),
        (8, 12.0),
        (9, 40.0),
        (14, 20.0),
        (15, 17.0),
        (17, 17.0),
    ] {
        data.set_column_width(c, w).map_err(e)?;
    }

    let sum = wb.add_worksheet();
    sum.set_name("Ringkasan").map_err(e)?;
    sum.set_column_width(0, 34.0).map_err(e)?;
    sum.set_column_width(1, 48.0).map_err(e)?;
    let mut r = 0u32;
    let mut kv = |k: &str, v: &str| -> Result<(), AppError> {
        sum.write_string_with_format(r, 0, k, &head).map_err(e)?;
        if !v.is_empty() {
            sum.write_string_with_format(r, 1, v, &text).map_err(e)?;
        }
        r += 1;
        Ok(())
    };
    kv("Jenis laporan", KIND_LABEL)?;
    kv("Catatan", NOTICE)?;
    kv("Perusahaan", &h.company_name)?;
    kv("Kode perusahaan", &h.company_code)?;
    kv(
        "Kode pelapor",
        h.reporter_code.as_deref().unwrap_or("Belum dikonfigurasi"),
    )?;
    kv("Workspace", &h.workspace_code)?;
    kv("Zona waktu", &h.tz)?;
    kv("Basis tanggal", s.basis.label())?;
    kv("Filter status", &f.status)?;
    kv("Filter PIC", &f.owner)?;
    kv("Filter kanal", &f.channel)?;
    kv("Filter kategori", &f.category)?;
    kv("Filter prioritas", &f.priority)?;
    kv("Data per (UTC)", &h.as_of)?;
    for (k, d) in [("Periode mulai", &start), ("Periode selesai", &end)] {
        sum.write_string_with_format(r, 0, k, &head).map_err(e)?;
        sum.write_datetime_with_format(r, 1, d, &date).map_err(e)?;
        r += 1;
    }
    r += 1;
    for (k, n) in [
        ("Total tiket", t.tickets),
        ("Siklus resolusi dalam periode", t.resolution_cycles),
        ("Tiket diselesaikan dalam periode", t.resolved_tickets),
        ("Tanpa PIC", t.unassigned),
        ("Tanpa kanal", t.missing_channel),
        ("Tanpa kategori", t.missing_category),
        ("Tanpa prioritas", t.missing_priority),
    ] {
        sum.write_string_with_format(r, 0, k, &head).map_err(e)?;
        #[allow(clippy::cast_precision_loss)]
        sum.write_number_with_format(r, 1, n as f64, &int)
            .map_err(e)?;
        r += 1;
    }
    wb.save_to_buffer().map_err(e)
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

// ---------- runs ----------

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PersonRef {
    id: Uuid,
    label: String,
}

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    report_type: String,
    period_start: String,
    period_end: String,
    basis: String,
    filters: serde_json::Value,
    state: String,
    generated_at: Option<String>,
    created_at: String,
    requested_by: Uuid,
    requested_by_name: String,
    row_count: Option<i32>,
    artifact_sha256: Option<Vec<u8>>,
    artifact_bytes: Option<i32>,
    expires_at: Option<String>,
    expired: bool,
    failure_code: Option<String>,
    workspace_code: String,
    format: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRunView {
    id: Uuid,
    report_type: String,
    period_start: String,
    period_end: String,
    basis: String,
    /// The validated filters of the request (status, owner, channel, category, priority).
    filters: serde_json::Value,
    state: String,
    generated_at: Option<String>,
    /// True when the run produced an artifact (D02 meaning, kept).
    has_artifact: bool,
    /// True while this artifact can be downloaded here (D10 artifact, not expired).
    downloadable: bool,
    created_at: String,
    requested_by: PersonRef,
    row_count: Option<i32>,
    /// Lowercase hex SHA-256 of the exact artifact bytes served by the download.
    artifact_sha256: Option<String>,
    artifact_bytes: Option<i32>,
    expires_at: Option<String>,
    expired: bool,
    failure_code: Option<String>,
    file_name: Option<String>,
    /// `csv` or `xlsx`.
    format: String,
}

const RUN_SELECT: &str = concat!(
    "SELECT r.id, r.report_type, r.period_start::text AS period_start, \
       r.period_end::text AS period_end, r.basis, r.filters, r.state, ",
    utc!("r.generated_at"),
    " AS generated_at, ",
    utc!("r.created_at"),
    " AS created_at, r.requested_by, u.display_name AS requested_by_name, r.row_count, \
       r.artifact_sha256, r.artifact_bytes, ",
    utc!("r.expires_at"),
    " AS expires_at, (r.expires_at IS NOT NULL AND r.expires_at <= now()) AS expired, \
       r.failure_code, r.workspace_code, r.format \
     FROM report_runs r JOIN users u ON u.id = r.requested_by \
     WHERE r.workspace_id = $1"
);

fn file_name(r: &RunRow) -> String {
    let short: String = r.id.to_string().chars().take(8).collect();
    format!(
        "{}-rekap-operasional-NON-REGULATOR-{}_sd_{}-{}-{short}.{}",
        r.workspace_code,
        r.period_start,
        r.period_end,
        r.basis,
        run_format(r).code()
    )
}

/// `report_runs.format` is constrained to csv/xlsx by its CHECK; 'csv' is the column default.
fn run_format(r: &RunRow) -> Format {
    Format::parse(Some(&r.format)).unwrap_or(Format::Csv)
}

fn run_view(r: RunRow) -> ReportRunView {
    let stored = r.state == "succeeded" && r.artifact_sha256.is_some();
    ReportRunView {
        file_name: stored.then(|| file_name(&r)),
        has_artifact: r.state == "succeeded",
        downloadable: stored && !r.expired,
        id: r.id,
        report_type: r.report_type,
        period_start: r.period_start,
        period_end: r.period_end,
        basis: r.basis,
        filters: r.filters,
        state: r.state,
        generated_at: r.generated_at,
        created_at: r.created_at,
        requested_by: PersonRef {
            id: r.requested_by,
            label: r.requested_by_name,
        },
        row_count: r.row_count,
        artifact_sha256: r.artifact_sha256.as_deref().map(hex),
        artifact_bytes: r.artifact_bytes,
        expires_at: r.expires_at,
        expired: r.expired,
        failure_code: r.failure_code,
        format: r.format,
    }
}

async fn load_run(conn: &mut PgConnection, ws: Uuid, id: Uuid) -> Result<RunRow, AppError> {
    sqlx::query_as(&format!("{RUN_SELECT} AND r.id = $2"))
        .bind(ws)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AppError::NotFound)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunList {
    items: Vec<ReportRunView>,
    limit: i64,
}

pub async fn list_runs(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
) -> Result<Json<RunList>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::ReportView)?;
    let rows: Vec<RunRow> = sqlx::query_as(&format!(
        "{RUN_SELECT} ORDER BY r.created_at DESC, r.id DESC LIMIT $2"
    ))
    .bind(access.workspace_id)
    .bind(RUN_LIST_LIMIT)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(RunList {
        items: rows.into_iter().map(run_view).collect(),
        limit: RUN_LIST_LIMIT,
    }))
}

pub async fn get_run(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_run)): Path<(String, String)>,
) -> Result<Json<ReportRunView>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::ReportView)?;
    let id = parse_id(&raw_run)?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(run_view(
        load_run(&mut conn, access.workspace_id, id).await?,
    )))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    run: ReportRunView,
    /// True when the same key + request answered an earlier run (nothing new created).
    replayed: bool,
}

#[allow(clippy::too_many_arguments)]
async fn audit(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    action: &str,
    run: Uuid,
    outcome: &str,
    change: serde_json::Value,
    corr: &Option<String>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO audit_events (workspace_id, actor_id, action, entity_type, entity_id, \
           safe_change, outcome, correlation_id) \
         VALUES ($1, $2, $3, 'report_run', $4, $5, $6, $7)",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .bind(action)
    .bind(run)
    .bind(change)
    .bind(outcome)
    .bind(corr)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn create_run(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw_ws): Path<String>,
    payload: Result<Json<ExportBody>, JsonRejection>,
) -> Result<(StatusCode, Json<ExportResult>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::ReportView)?;
    let key = idempotency_key(&headers)?;
    let b = body(payload)?;
    let spec = b.spec()?;
    let fmt = b.format()?;
    let filters = serde_json::json!({
        "status": spec.status,
        "owner": spec.owner,
        "channel": spec.channel,
        "category": spec.category,
        "priority": spec.priority,
    });
    let hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&(OPERATIONAL, fmt.code(), &spec)).map_err(AppError::internal)?,
    )
    .into();
    let ws = access.workspace_id;
    let corr = correlation(&headers);
    let max_rows = state.config.report_max_rows;

    let mut tx = state.pool.begin().await?;
    snapshot(&mut tx, false).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("report-run:{ws}:{}:{key}", access.user_id))
        .execute(&mut *tx)
        .await?;
    let existing: Option<(Uuid, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT id, request_hash FROM report_runs \
         WHERE workspace_id = $1 AND requested_by = $2 AND idempotency_key = $3",
    )
    .bind(ws)
    .bind(access.user_id)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, stored)) = existing {
        if stored.as_deref() != Some(hash.as_slice()) {
            tracing::info!(workspace_id = %ws, "report idempotency key reused with another request");
            return Err(AppError::IdempotencyConflict);
        }
        let run = run_view(load_run(&mut tx, ws, id).await?);
        tx.commit().await?;
        return Ok((
            StatusCode::OK,
            Json(ExportResult {
                run,
                replayed: true,
            }),
        ));
    }

    check_filters(&mut tx, ws, &spec).await?;
    let h = header(&mut tx, ws, &spec).await?;
    let data = rows(&mut tx, ws, &spec, Some(max_rows + 1)).await?;
    let id = Uuid::new_v4();
    let base_change = serde_json::json!({
        "reportType": OPERATIONAL,
        "format": fmt.code(),
        "periodStart": spec.period_start,
        "periodEnd": spec.period_end,
        "basis": spec.basis.code(),
        "filters": filters,
    });

    if i64::try_from(data.len()).unwrap_or(i64::MAX) > max_rows {
        sqlx::query(
            "INSERT INTO report_runs (id, workspace_id, requested_by, report_type, period_start, \
               period_end, basis, filters, state, idempotency_key, request_hash, failure_code, format) \
             VALUES ($1, $2, $3, 'operational', $4::date, $5::date, $6, $7, 'failed', $8, $9, 'row_limit', $10)",
        )
        .bind(id)
        .bind(ws)
        .bind(access.user_id)
        .bind(&spec.period_start)
        .bind(&spec.period_end)
        .bind(spec.basis.code())
        .bind(&filters)
        .bind(&key)
        .bind(hash.as_slice())
        .bind(fmt.code())
        .execute(&mut *tx)
        .await?;
        let mut change = base_change;
        change["failureCode"] = "row_limit".into();
        change["maxRows"] = max_rows.into();
        audit(
            &mut tx,
            &access,
            "report.export",
            id,
            "failed",
            change,
            &corr,
        )
        .await?;
    } else {
        let csv = match fmt {
            Format::Csv => build_csv(&h, &spec, &data)?,
            Format::Xlsx => {
                let totals: Totals = bind_base!(sqlx::query_as(TOTALS_SQL), ws, &spec)
                    .fetch_one(&mut *tx)
                    .await?;
                let labels = filter_labels(&mut tx, ws, &spec).await?;
                build_xlsx(&h, &spec, &data, &totals, &labels)?
            }
        };
        let sha: [u8; 32] = Sha256::digest(&csv).into();
        let n_rows = i32::try_from(data.len()).map_err(AppError::internal)?;
        let n_bytes = i32::try_from(csv.len()).map_err(AppError::internal)?;
        sqlx::query(
            "INSERT INTO report_runs (id, workspace_id, requested_by, report_type, period_start, \
               period_end, basis, filters, state, generated_at, artifact_ref, idempotency_key, \
               request_hash, row_count, artifact_sha256, artifact_bytes, expires_at, format) \
             VALUES ($1, $2, $3, 'operational', $4::date, $5::date, $6, $7, 'succeeded', now(), \
               'db:report_artifacts/' || $1::text, $8, $9, $10, $11, $12, \
               now() + make_interval(hours => $13::int), $14)",
        )
        .bind(id)
        .bind(ws)
        .bind(access.user_id)
        .bind(&spec.period_start)
        .bind(&spec.period_end)
        .bind(spec.basis.code())
        .bind(&filters)
        .bind(&key)
        .bind(hash.as_slice())
        .bind(n_rows)
        .bind(sha.as_slice())
        .bind(n_bytes)
        .bind(i32::try_from(ARTIFACT_TTL_HOURS).map_err(AppError::internal)?)
        .bind(fmt.code())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO report_artifacts (run_id, workspace_id, content, sha256) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(ws)
        .bind(&csv)
        .bind(sha.as_slice())
        .execute(&mut *tx)
        .await?;
        let mut change = base_change;
        change["rowCount"] = n_rows.into();
        change["bytes"] = n_bytes.into();
        change["sha256"] = hex(&sha).into();
        audit(
            &mut tx,
            &access,
            "report.export",
            id,
            "success",
            change,
            &corr,
        )
        .await?;
    }
    let run = run_view(load_run(&mut tx, ws, id).await?);
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, report_run_id = %id, state = %run.state, "report export");
    Ok((
        StatusCode::CREATED,
        Json(ExportResult {
            run,
            replayed: false,
        }),
    ))
}

#[derive(sqlx::FromRow)]
struct ArtifactRow {
    content: Vec<u8>,
    sha256: Vec<u8>,
}

#[derive(Debug)]
struct ArtifactMismatch;

impl std::fmt::Display for ArtifactMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stored report artifact does not match its run")
    }
}

impl std::error::Error for ArtifactMismatch {}

pub async fn download(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_run)): Path<(String, String)>,
) -> Result<Response, AppError> {
    // Membership, workspace and permission are checked again on every download.
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::ReportView)?;
    let id = parse_id(&raw_run)?;
    let ws = access.workspace_id;
    let mut tx = state.pool.begin().await?;
    let run = load_run(&mut tx, ws, id).await?;
    if run.state != "succeeded" || run.artifact_sha256.is_none() {
        return Err(AppError::NotFound);
    }
    if run.expired {
        return Err(AppError::ReportExpired);
    }
    let a: ArtifactRow = sqlx::query_as(
        "SELECT content, sha256 FROM report_artifacts WHERE workspace_id = $1 AND run_id = $2",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::internal(ArtifactMismatch))?;
    let digest: [u8; 32] = Sha256::digest(&a.content).into();
    if a.sha256 != digest || run.artifact_sha256.as_deref() != Some(digest.as_slice()) {
        return Err(AppError::internal(ArtifactMismatch));
    }
    // An XLSX run must hold a ZIP container (never CSV bytes under an .xlsx name).
    if run_format(&run) == Format::Xlsx && !a.content.starts_with(b"PK\x03\x04") {
        return Err(AppError::internal(ArtifactMismatch));
    }
    audit(
        &mut tx,
        &access,
        "report.download",
        id,
        "success",
        serde_json::json!({ "sha256": hex(&digest), "bytes": a.content.len() }),
        &correlation(&headers),
    )
    .await?;
    tx.commit().await?;

    let name = file_name(&run);
    let disp = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .map_err(AppError::internal)?;
    let mut resp = Body::from(a.content).into_response();
    let hd = resp.headers_mut();
    hd.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(run_format(&run).content_type()),
    );
    hd.insert(header::CONTENT_DISPOSITION, disp);
    hd.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    hd.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    hd.insert(
        HeaderName::from_static("x-content-sha256"),
        HeaderValue::from_str(&hex(&digest)).map_err(AppError::internal)?,
    );
    hd.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_strict() {
        assert_eq!(day_number("1970-01-01"), None); // outside 2000–2100
        assert_eq!(day_number("2000-01-01"), Some(10_957));
        assert_eq!(day_number("2026-11-01"), Some(20_758));
        assert_eq!(day_number("2024-02-29"), Some(19_782));
        for bad in [
            "2026-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-11-31",
            "2026-1-01",
            "26-11-01",
            "2026/11/01",
            "+026-11-01",
            "2026-11-0a",
            "",
        ] {
            assert_eq!(day_number(bad), None, "{bad}");
        }
    }

    #[test]
    fn period_rules() {
        let ok = |a: &str, b: &str| spec(Some(a), Some(b), None, None, None, None, None, None);
        assert!(ok("2026-11-01", "2026-11-01").is_ok());
        assert!(matches!(
            ok("2026-11-02", "2026-11-01"),
            Err(AppError::Validation("periodEnd"))
        ));
        assert!(ok("2025-11-02", "2026-11-02").is_ok()); // 366 days
        assert!(matches!(
            ok("2025-11-01", "2026-11-02"),
            Err(AppError::Validation("periodEnd"))
        ));
        assert!(matches!(
            spec(None, Some("2026-11-01"), None, None, None, None, None, None),
            Err(AppError::Validation("periodStart"))
        ));
    }

    #[test]
    fn status_and_dims_are_validated() {
        let s = spec(
            Some("2026-11-01"),
            Some("2026-11-30"),
            Some("resolved"),
            Some(vec!["open", "new", "open"]),
            Some("unassigned"),
            Some("none"),
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.status, Some(vec!["new", "open"]));
        assert_eq!(s.basis, Basis::Resolved);
        assert_eq!(s.owner.as_deref(), Some("unassigned"));
        let bad = |st: Vec<&str>| {
            spec(
                Some("2026-11-01"),
                Some("2026-11-30"),
                None,
                Some(st),
                None,
                None,
                None,
                None,
            )
        };
        assert!(matches!(
            bad(vec!["x"]),
            Err(AppError::Validation("status"))
        ));
        assert!(matches!(bad(vec![""]), Err(AppError::Validation("status"))));
        assert!(matches!(
            spec(
                Some("2026-11-01"),
                Some("2026-11-30"),
                Some("updated"),
                None,
                None,
                None,
                None,
                None
            ),
            Err(AppError::Validation("basis"))
        ));
        assert!(matches!(
            spec(
                Some("2026-11-01"),
                Some("2026-11-30"),
                None,
                None,
                Some("A1E8DD4A-A301-42F1-AC2A-58DB5C57811B"),
                None,
                None,
                None
            ),
            Err(AppError::Validation("owner"))
        ));
    }

    #[test]
    fn regulator_type_is_held() {
        assert!(matches!(
            report_type(Some("bi_regulator")),
            Err(AppError::RegulatorMappingUnavailable)
        ));
        assert!(matches!(
            report_type(Some("official")),
            Err(AppError::Validation("reportType"))
        ));
        assert!(report_type(None).is_ok());
    }

    #[test]
    fn csv_cells_are_quoted_and_neutralized() {
        assert_eq!(csv_cell("biasa"), "\"biasa\"");
        assert_eq!(csv_cell("a\"b,c"), "\"a\"\"b,c\"");
        assert_eq!(csv_cell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        for p in ["+1", "-1", "@SUM(A1)", "\tx", "\rx"] {
            assert!(csv_cell(p).starts_with("\"'"), "{p:?}");
        }
        assert_eq!(csv_cell("a=b"), "\"a=b\"");
        assert_eq!(csv_cell(""), "\"\"");
        assert_eq!(csv_cell("baris\nbaru"), "\"baris\nbaru\"");
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }
}
