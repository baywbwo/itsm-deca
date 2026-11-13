//! O01 Dashboard (D05): server aggregates over the WHOLE verified workspace scope, never
//! a page of tickets. Every query is filtered by the workspace the caller was verified for
//! and runs in one read-only snapshot, so all numbers and `asOf` describe the same moment.
//!
//! Metric definitions (PRD §4, accepted by Arka for D05):
//! - active = nonterminal status (`new`, `open`, `pending_customer`, `pending_engineering`);
//! - status, PIC load, age and "perlu perhatian" are current-state counts over every active
//!   ticket in scope, never limited to tickets received in the period;
//! - received in period = `received_at` inside the period;
//! - resolved in period = `resolve` events whose `occurred_at` is inside the period, whatever
//!   the ticket's `received_at` (a later reopen keeps the event). `resolvedInPeriod` counts
//!   cycles (events), `resolvedTicketsInPeriod` counts distinct tickets;
//! - age is measured from `received_at`; no SLA label (SLA is not configured);
//! - "perlu perhatian" = active urgent/high, urgent first then oldest `received_at`, max 10:
//!   an operational rule, not an SLA.
//!
//! Shared filters (PIC, kanal, kategori, prioritas) narrow EVERY number, chart, table and
//! drilldown the same way, using the ticket's current owner/channel/category/priority.
//! Filter values must belong to the workspace (membership / catalog) or the request is 400.
//!
//! Periods are calendar days in the workspace time zone (Asia/Jakarta for every workspace
//! today), half-open `[start, end)`.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission};
use crate::domain::{TicketStatus, TicketStatusView};
use crate::error::AppError;
use crate::routes::workspaces::parse_id;
use crate::session::Session;

/// Nonterminal statuses, in display order.
pub const ACTIVE: [TicketStatus; 4] = [
    TicketStatus::New,
    TicketStatus::Open,
    TicketStatus::PendingCustomer,
    TicketStatus::PendingEngineering,
];
const ACTIVE_SQL: &str = "('new', 'open', 'pending_customer', 'pending_engineering')";
/// Priority codes that put an active ticket on the "perlu perhatian" list, in order.
const ATTENTION_PRIORITIES: [&str; 2] = ["urgent", "high"];
const ATTENTION_LIMIT: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKey {
    Today,
    // snake_case alone would give "last7_days".
    #[serde(rename = "last_7_days")]
    Last7Days,
    ThisMonth,
}

impl PeriodKey {
    pub fn parse(raw: Option<&str>) -> Option<PeriodKey> {
        match raw {
            None | Some("today") => Some(PeriodKey::Today),
            Some("last_7_days") => Some(PeriodKey::Last7Days),
            Some("this_month") => Some(PeriodKey::ThisMonth),
            Some(_) => None,
        }
    }

    fn code(self) -> &'static str {
        match self {
            PeriodKey::Today => "today",
            PeriodKey::Last7Days => "last_7_days",
            PeriodKey::ThisMonth => "this_month",
        }
    }
}

#[derive(Deserialize)]
pub struct DashboardQuery {
    period: Option<String>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
}

/// One shared filter dimension: no restriction, "not set" (`unassigned` / `none`), or one ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    Any,
    Unset,
    Id(Uuid),
}

impl Dim {
    /// Only the canonical lowercase hyphenated UUID or `unset_word` is accepted, so the
    /// echoed value equals the requested one.
    pub fn parse(raw: Option<&str>, unset_word: &str) -> Option<Dim> {
        match raw {
            None => Some(Dim::Any),
            Some(s) if s == unset_word => Some(Dim::Unset),
            Some(s) => {
                let id = Uuid::try_parse(s).ok()?;
                (id.to_string() == s).then_some(Dim::Id(id))
            }
        }
    }

    pub fn id(self) -> Option<Uuid> {
        match self {
            Dim::Id(id) => Some(id),
            _ => None,
        }
    }

    pub fn unset(self) -> bool {
        self == Dim::Unset
    }

    pub fn wire(self, unset_word: &str) -> Option<String> {
        match self {
            Dim::Any => None,
            Dim::Unset => Some(unset_word.to_owned()),
            Dim::Id(id) => Some(id.to_string()),
        }
    }
}

pub const UNASSIGNED: &str = "unassigned";
pub const NONE: &str = "none";

/// Shared dashboard filters (PRD §4 "Filter bersama" minus the period).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filters {
    pub owner: Dim,
    pub channel: Dim,
    pub category: Dim,
    pub priority: Dim,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            owner: Dim::Any,
            channel: Dim::Any,
            category: Dim::Any,
            priority: Dim::Any,
        }
    }
}

impl Filters {
    pub fn parse(
        owner: Option<&str>,
        channel: Option<&str>,
        category: Option<&str>,
        priority: Option<&str>,
    ) -> Option<Filters> {
        Some(Filters {
            owner: Dim::parse(owner, UNASSIGNED)?,
            channel: Dim::parse(channel, NONE)?,
            category: Dim::parse(category, NONE)?,
            priority: Dim::parse(priority, NONE)?,
        })
    }
}

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

/// Common prefix of every dashboard query. Parameters (bound by `bind_base!`):
/// `$1` workspace, `$2` period code, `$3` evaluation instant (NULL = transaction start, so
/// identical for every query of one request; tests pin it), then per dimension a UUID and an
/// "unset" flag: `$4/$5` owner, `$6/$7` channel, `$8/$9` category, `$10/$11` priority.
/// `p` = period bounds (+ `at`), `ft` = the workspace's tickets after the shared filters.
/// `ft` is NOT MATERIALIZED: each use is planned with its own predicates (active status,
/// received range, event join) on the indexes, instead of copying every ticket of the
/// workspace into a temporary result first (CP7c: 500k rows, 140 MB on disk, ~3 s).
macro_rules! base {
    () => {
        "WITH d AS ( \
           SELECT w.timezone AS tz, COALESCE($3::timestamptz, now()) AS at, \
             (COALESCE($3::timestamptz, now()) AT TIME ZONE w.timezone)::date AS today \
           FROM workspaces w WHERE w.id = $1), \
         b AS ( \
           SELECT tz, at, \
             CASE $2::text WHEN 'today' THEN today WHEN 'last_7_days' THEN today - 6 \
               ELSE date_trunc('month', today::timestamp)::date END AS start_date, \
             CASE $2::text WHEN 'this_month' THEN (date_trunc('month', today::timestamp) + interval '1 month')::date \
               ELSE today + 1 END AS end_date \
           FROM d), \
         p AS ( \
           SELECT tz, at, start_date, end_date, \
             (start_date::timestamp AT TIME ZONE tz) AS start_at, \
             (end_date::timestamp AT TIME ZONE tz) AS end_at \
           FROM b), \
         ft AS NOT MATERIALIZED ( \
           SELECT t.* FROM tickets t WHERE t.workspace_id = $1 \
             AND ($4::uuid IS NULL OR t.owner_user_id = $4) AND (NOT $5::bool OR t.owner_user_id IS NULL) \
             AND ($6::uuid IS NULL OR t.intake_channel_id = $6) AND (NOT $7::bool OR t.intake_channel_id IS NULL) \
             AND ($8::uuid IS NULL OR t.category_id = $8) AND (NOT $9::bool OR t.category_id IS NULL) \
             AND ($10::uuid IS NULL OR t.priority_id = $10) AND (NOT $11::bool OR t.priority_id IS NULL)) "
    };
}

/// Resolve events of the period whose ticket is in `ft` (alias `e`, needs `p`). The composite
/// foreign key (workspace_id, ticket_id) already puts every event's ticket in `$1`, so the
/// `ft` probe only matters when a filter is set; with no filter (custom plans, see `load`)
/// the planner folds it away instead of probing one ticket per event (CP7c).
macro_rules! resolved_in_scope {
    () => {
        "e.workspace_id = $1 AND e.action = 'resolve' \
         AND e.occurred_at >= p.start_at AND e.occurred_at < p.end_at \
         AND (($4::uuid IS NULL AND NOT $5::bool AND $6::uuid IS NULL AND NOT $7::bool \
               AND $8::uuid IS NULL AND NOT $9::bool AND $10::uuid IS NULL AND NOT $11::bool) \
              OR EXISTS (SELECT 1 FROM ft WHERE ft.id = e.ticket_id))"
    };
}

/// Binds `$1..$11` of `base!()` from an `Args`.
macro_rules! bind_base {
    ($q:expr, $a:expr) => {
        $q.bind($a.workspace_id)
            .bind($a.period)
            .bind($a.at)
            .bind($a.filters.owner.id())
            .bind($a.filters.owner.unset())
            .bind($a.filters.channel.id())
            .bind($a.filters.channel.unset())
            .bind($a.filters.category.id())
            .bind($a.filters.category.unset())
            .bind($a.filters.priority.id())
            .bind($a.filters.priority.unset())
    };
}

struct Args<'a> {
    workspace_id: Uuid,
    period: &'static str,
    at: Option<&'a str>,
    filters: Filters,
}

// ---------- response ----------

/// Filter a ticket list must apply to reproduce a number (drilldown). Every field is
/// optional; absent means "no restriction". Times are RFC 3339 UTC, half-open. The shared
/// dashboard filters are always carried along.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Vec<&'static str>>,
    /// A member user ID, or `unassigned`.
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    /// An intake channel ID, or `none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    /// A category ID, or `none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// Priority IDs (any of), or `["none"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_before: Option<String>,
}

impl TicketFilter {
    fn shared(f: &Filters) -> TicketFilter {
        TicketFilter {
            owner: f.owner.wire(UNASSIGNED),
            channel: f.channel.wire(NONE),
            category: f.category.wire(NONE),
            priority: f.priority.wire(NONE).map(|p| vec![p]),
            ..TicketFilter::default()
        }
    }
}

fn active_codes() -> Vec<&'static str> {
    ACTIVE.iter().map(|s| s.code()).collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    key: PeriodKey,
    timezone: String,
    /// First local day, inclusive (YYYY-MM-DD).
    start_date: String,
    /// Last local day, inclusive (YYYY-MM-DD).
    end_date: String,
    start_at: String,
    /// Exclusive.
    end_at: String,
}

/// The shared filters as applied (echo of the request; `null` = no restriction).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFilters {
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FilterOption {
    id: Uuid,
    /// `null` for people.
    code: Option<String>,
    label: String,
    /// False for an inactive membership / catalog row (still filterable for history).
    active: bool,
}

/// Values the caller may filter by, all scoped to this workspace.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterOptions {
    owners: Vec<FilterOption>,
    channels: Vec<FilterOption>,
    categories: Vec<FilterOption>,
    priorities: Vec<FilterOption>,
}

/// Date basis of every number, so the UI never mixes snapshot, received and resolution
/// counts. Values: `current_status` (snapshot at `asOf`), `received_at` (in period),
/// `resolution_event` (resolve events in period, one per cycle),
/// `resolution_event_distinct_ticket` (distinct tickets with such an event).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricBasis {
    active: &'static str,
    unassigned: &'static str,
    pending_customer: &'static str,
    received_in_period: &'static str,
    resolved_in_period: &'static str,
    resolved_tickets_in_period: &'static str,
    status_distribution: &'static str,
    pic_load: &'static str,
    age: &'static str,
    channels: &'static str,
    categories: &'static str,
    trend_received: &'static str,
    trend_resolved: &'static str,
    attention: &'static str,
}

const SNAPSHOT: &str = "current_status";
const RECEIVED: &str = "received_at";
const RESOLUTION: &str = "resolution_event";

const METRIC_BASIS: MetricBasis = MetricBasis {
    active: SNAPSHOT,
    unassigned: SNAPSHOT,
    pending_customer: SNAPSHOT,
    received_in_period: RECEIVED,
    resolved_in_period: RESOLUTION,
    resolved_tickets_in_period: "resolution_event_distinct_ticket",
    status_distribution: SNAPSHOT,
    pic_load: SNAPSHOT,
    age: SNAPSHOT,
    channels: RECEIVED,
    categories: RECEIVED,
    trend_received: RECEIVED,
    trend_resolved: RESOLUTION,
    attention: SNAPSHOT,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    tickets: i64,
    active: i64,
    unassigned: i64,
    pending_customer: i64,
    received_in_period: i64,
    /// Resolve events in the period (cycles); a ticket resolved twice counts twice.
    resolved_in_period: i64,
    /// Distinct tickets with at least one resolve event in the period.
    resolved_tickets_in_period: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Drilldowns {
    active: TicketFilter,
    /// `null` when the PIC filter is a person (the number is 0 by definition).
    unassigned: Option<TicketFilter>,
    pending_customer: TicketFilter,
    received_in_period: TicketFilter,
    resolved_in_period: TicketFilter,
    /// `null` when the priority filter excludes every urgent/high priority.
    attention: Option<TicketFilter>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCount {
    status: TicketStatusView,
    active: bool,
    count: i64,
    filter: TicketFilter,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PicLoad {
    /// `None` = belum ditugaskan.
    user_id: Option<Uuid>,
    display_name: Option<String>,
    /// False when the membership or user is no longer active but still owns tickets.
    member_active: bool,
    active_tickets: i64,
    filter: TicketFilter,
}

/// Tickets received in the period for one catalog value (`None` = not recorded).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionCount {
    id: Option<Uuid>,
    code: Option<String>,
    label: Option<String>,
    count: i64,
    filter: TicketFilter,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeBucket {
    key: &'static str,
    label: &'static str,
    count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendPoint {
    date: String,
    received: i64,
    resolved: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Named {
    id: Uuid,
    code: Option<String>,
    label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionTicket {
    id: Uuid,
    number: i64,
    display_number: String,
    subject: String,
    status: TicketStatusView,
    priority: Option<Named>,
    owner: Option<Named>,
    channel: Option<Named>,
    category: Option<Named>,
    received_at: String,
    age_seconds: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    /// Always `operational_rule`: urgent/high active tickets, not an SLA breach list.
    rule: &'static str,
    total: i64,
    limit: i64,
    items: Vec<AttentionTicket>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    workspace_id: Uuid,
    workspace_code: String,
    as_of: String,
    period: Period,
    filters: AppliedFilters,
    filter_options: FilterOptions,
    metric_basis: MetricBasis,
    sla_configured: bool,
    totals: Totals,
    drilldown: Drilldowns,
    status_distribution: Vec<StatusCount>,
    pic_load: Vec<PicLoad>,
    /// Tickets received in the period, per intake channel.
    channels: Vec<DimensionCount>,
    /// Tickets received in the period, per category.
    categories: Vec<DimensionCount>,
    /// Active tickets by age since `received_at`.
    age: Vec<AgeBucket>,
    /// One point per local day of the period; received and resolved are never summed.
    trend: Vec<TrendPoint>,
    attention: Attention,
}

// ---------- rows ----------

#[derive(sqlx::FromRow)]
struct PeriodRow {
    tz: String,
    start_date: String,
    end_date: String,
    start_at: String,
    end_at: String,
    as_of: String,
}

#[derive(sqlx::FromRow)]
struct CountRow {
    unassigned: i64,
    received_in_period: i64,
    age_lt_1d: i64,
    age_1_2d: i64,
    age_gt_2d: i64,
    resolved_in_period: i64,
    resolved_tickets_in_period: i64,
}

#[derive(sqlx::FromRow)]
struct DimensionRow {
    id: Option<Uuid>,
    code: Option<String>,
    label: Option<String>,
    n: i64,
}

#[derive(sqlx::FromRow)]
struct AttentionRow {
    id: Uuid,
    number: i64,
    number_prefix: String,
    subject: String,
    workflow_status: String,
    priority_id: Option<Uuid>,
    priority_code: Option<String>,
    priority_label: Option<String>,
    owner_user_id: Option<Uuid>,
    owner_name: Option<String>,
    channel_id: Option<Uuid>,
    channel_code: Option<String>,
    channel_label: Option<String>,
    category_id: Option<Uuid>,
    category_code: Option<String>,
    category_label: Option<String>,
    received_at: String,
    age_seconds: i64,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workflow_status code in database")]
struct UnknownStatus;

/// True when every filter value belongs to the workspace (any membership / catalog row).
const VALID_SQL: &str = concat!(
    base!(),
    "SELECT \
       ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM workspace_memberships m WHERE m.workspace_id = $1 AND m.user_id = $4)) \
       AND ($6::uuid IS NULL OR EXISTS (SELECT 1 FROM intake_channels c WHERE c.workspace_id = $1 AND c.id = $6)) \
       AND ($8::uuid IS NULL OR EXISTS (SELECT 1 FROM categories c WHERE c.workspace_id = $1 AND c.id = $8)) \
       AND ($10::uuid IS NULL OR EXISTS (SELECT 1 FROM priorities c WHERE c.workspace_id = $1 AND c.id = $10)) \
       AND EXISTS (SELECT 1 FROM p)"
);

const PERIOD_SQL: &str = concat!(
    base!(),
    "SELECT tz, start_date::text AS start_date, (end_date - 1)::text AS end_date, ",
    utc!("start_at"),
    " AS start_at, ",
    utc!("end_at"),
    " AS end_at, ",
    utc!("at"),
    " AS as_of FROM p"
);

/// Totals and active counts come from `STATUS_SQL` (same snapshot and scope); this query
/// only reads the active tickets, the period's receipts and the period's resolve events.
fn counts_sql() -> String {
    format!(
        concat!(
            base!(),
            ", a AS (SELECT \
                 count(*) FILTER (WHERE t.owner_user_id IS NULL) AS unassigned, \
                 count(*) FILTER (WHERE p.at - t.received_at < interval '1 day') AS age_lt_1d, \
                 count(*) FILTER (WHERE p.at - t.received_at >= interval '1 day' \
                                    AND p.at - t.received_at < interval '2 days') AS age_1_2d, \
                 count(*) FILTER (WHERE p.at - t.received_at >= interval '2 days') AS age_gt_2d \
               FROM ft t, p WHERE t.workflow_status IN {a}), \
             r AS (SELECT count(*) AS n FROM ft t, p \
               WHERE t.received_at >= p.start_at AND t.received_at < p.end_at), \
             rs AS (SELECT count(*) AS n, count(DISTINCT e.ticket_id) AS tickets \
               FROM ticket_resolution_events e, p WHERE ",
            resolved_in_scope!(),
            ") \
             SELECT a.unassigned, r.n AS received_in_period, a.age_lt_1d, a.age_1_2d, a.age_gt_2d, \
               rs.n AS resolved_in_period, rs.tickets AS resolved_tickets_in_period \
             FROM a, r, rs"
        ),
        a = ACTIVE_SQL
    )
}

const STATUS_SQL: &str = concat!(
    base!(),
    "SELECT workflow_status, count(*) FROM ft GROUP BY 1"
);

/// Every active ticket in scope per PIC, whatever its `received_at`.
fn pic_sql() -> String {
    format!(
        concat!(
            base!(),
            ", c AS (SELECT owner_user_id, count(*) AS n FROM ft WHERE workflow_status IN {a} GROUP BY 1) \
             SELECT m.user_id, u.display_name, (m.active AND u.active) AS member_active, \
                    COALESCE(c.n, 0) AS n \
             FROM workspace_memberships m \
             JOIN users u ON u.id = m.user_id \
             LEFT JOIN c ON c.owner_user_id = m.user_id \
             WHERE m.workspace_id = $1 AND NOT $5::bool AND ($4::uuid IS NULL OR m.user_id = $4) \
               AND (m.active OR c.n IS NOT NULL OR m.user_id = $4) \
             ORDER BY n DESC, u.display_name, m.user_id"
        ),
        a = ACTIVE_SQL
    )
}

/// Tickets received in the period per value of one catalog dimension. With a filter on
/// that dimension only the selected value (or the "not recorded" row) is listed.
/// `table`/`col`/`$id`/`$unset` come from the constants in `load`, never from input.
fn dimension_sql(table: &str, col: &str, id: u8, unset: u8) -> String {
    format!(
        concat!(
            base!(),
            ", r AS (SELECT t.{col} AS k, count(*) AS n FROM ft t, p \
               WHERE t.received_at >= p.start_at AND t.received_at < p.end_at GROUP BY 1) \
             SELECT c.id, c.code, c.label, COALESCE(r.n, 0) AS n, c.sort_order AS ord \
             FROM {table} c LEFT JOIN r ON r.k = c.id \
             WHERE c.workspace_id = $1 AND NOT ${unset}::bool \
               AND CASE WHEN ${id}::uuid IS NULL THEN (c.active OR r.n IS NOT NULL) ELSE c.id = ${id} END \
             UNION ALL \
             SELECT NULL, NULL, NULL, r.n, 2147483647 FROM r WHERE r.k IS NULL \
             ORDER BY 5, 3"
        ),
        table = table,
        col = col,
        id = id,
        unset = unset
    )
}

const TREND_SQL: &str = concat!(
    base!(),
    ", days AS ( \
       SELECT g::date AS day FROM p, \
         generate_series(p.start_date::timestamp, (p.end_date - 1)::timestamp, interval '1 day') AS g), \
     rec AS ( \
       SELECT (t.received_at AT TIME ZONE p.tz)::date AS day, count(*) AS n FROM ft t, p \
       WHERE t.received_at >= p.start_at AND t.received_at < p.end_at GROUP BY 1), \
     res AS ( \
       SELECT (e.occurred_at AT TIME ZONE p.tz)::date AS day, count(*) AS n \
       FROM ticket_resolution_events e, p WHERE ",
    resolved_in_scope!(),
    " GROUP BY 1) \
     SELECT days.day::text, COALESCE(rec.n, 0), COALESCE(res.n, 0) \
     FROM days LEFT JOIN rec USING (day) LEFT JOIN res USING (day) ORDER BY days.day"
);

/// `$12` = attention priority codes in order, `$13` = limit. Per priority, the oldest active
/// tickets are read in index order (`tickets_active_priority_idx`, 0030) up to the limit.
fn attention_sql() -> String {
    format!(
        concat!(
            base!(),
            "SELECT t.id, t.number, t.number_prefix, t.subject, t.workflow_status, \
               pr.id AS priority_id, pr.code AS priority_code, pr.label AS priority_label, \
               t.owner_user_id, u.display_name AS owner_name, \
               ic.id AS channel_id, ic.code AS channel_code, ic.label AS channel_label, \
               cat.id AS category_id, cat.code AS category_code, cat.label AS category_label, ",
            utc!("t.received_at"),
            " AS received_at, \
               GREATEST(0, floor(extract(epoch FROM p.at - t.received_at)))::bigint AS age_seconds \
             FROM unnest($12::text[]) WITH ORDINALITY AS pc (code, ord) \
             JOIN priorities pr ON pr.workspace_id = $1 AND pr.code = pc.code \
             CROSS JOIN LATERAL ( \
               SELECT t.* FROM ft t WHERE t.priority_id = pr.id AND t.workflow_status IN {a} \
               ORDER BY t.received_at, t.number LIMIT $13) t \
             CROSS JOIN p \
             LEFT JOIN users u ON u.id = t.owner_user_id \
             LEFT JOIN intake_channels ic ON ic.workspace_id = t.workspace_id AND ic.id = t.intake_channel_id \
             LEFT JOIN categories cat ON cat.workspace_id = t.workspace_id AND cat.id = t.category_id \
             ORDER BY pc.ord, t.received_at, t.number \
             LIMIT $13"
        ),
        a = ACTIVE_SQL
    )
}

fn attention_count_sql() -> String {
    format!(
        concat!(
            base!(),
            "SELECT count(*) FROM ft t \
             JOIN priorities pr ON pr.workspace_id = t.workspace_id AND pr.id = t.priority_id \
             WHERE t.workflow_status IN {a} AND pr.code = ANY($12)"
        ),
        a = ACTIVE_SQL
    )
}

const OWNER_OPTIONS_SQL: &str = "SELECT m.user_id AS id, NULL::text AS code, u.display_name AS label, \
       (m.active AND u.active) AS active \
     FROM workspace_memberships m JOIN users u ON u.id = m.user_id \
     WHERE m.workspace_id = $1 AND (m.active OR EXISTS ( \
       SELECT 1 FROM tickets t WHERE t.workspace_id = $1 AND t.owner_user_id = m.user_id)) \
     ORDER BY u.display_name, m.user_id";

/// Table names are compile-time constants, never request input.
fn catalog_options_sql(table: &str) -> String {
    format!(
        "SELECT id, code, label, active FROM {table} WHERE workspace_id = $1 \
         ORDER BY sort_order, label, id"
    )
}

/// Owners and catalog values the caller may filter by, all scoped to `wid` (shared by the
/// dashboard and the reports, D10).
pub(crate) async fn filter_options(
    conn: &mut sqlx::PgConnection,
    wid: Uuid,
) -> Result<FilterOptions, AppError> {
    let owners: Vec<FilterOption> = sqlx::query_as(OWNER_OPTIONS_SQL)
        .bind(wid)
        .fetch_all(&mut *conn)
        .await?;
    let mut catalogs = Vec::with_capacity(3);
    for table in ["intake_channels", "categories", "priorities"] {
        let rows: Vec<FilterOption> = sqlx::query_as(&catalog_options_sql(table))
            .bind(wid)
            .fetch_all(&mut *conn)
            .await?;
        catalogs.push(rows);
    }
    let mut catalogs = catalogs.into_iter();
    Ok(FilterOptions {
        owners,
        channels: catalogs.next().unwrap_or_default(),
        categories: catalogs.next().unwrap_or_default(),
        priorities: catalogs.next().unwrap_or_default(),
    })
}

// ---------- handler ----------

pub async fn get(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Dashboard>, AppError> {
    let ws = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    ws.require(Permission::TicketRead)?;
    let key = PeriodKey::parse(q.period.as_deref()).ok_or(AppError::InvalidRequest)?;
    let filters = Filters::parse(
        q.owner.as_deref(),
        q.channel.as_deref(),
        q.category.as_deref(),
        q.priority.as_deref(),
    )
    .ok_or(AppError::InvalidRequest)?;
    let d = load(
        &state.pool,
        ws.workspace_id,
        &ws.workspace_code,
        key,
        filters,
        None,
    )
    .await?;
    Ok(Json(d))
}

/// Builds the dashboard for an already authorized workspace. `at` pins the evaluation
/// instant (RFC 3339) for tests; the handler always passes `None` (= transaction start).
pub async fn load(
    pool: &PgPool,
    workspace_id: Uuid,
    workspace_code: &str,
    key: PeriodKey,
    filters: Filters,
    at: Option<&str>,
) -> Result<Dashboard, AppError> {
    let args = Args {
        workspace_id,
        period: key.code(),
        at,
        filters,
    };
    let wid = workspace_id;
    let attention_codes: Vec<String> = ATTENTION_PRIORITIES
        .iter()
        .map(|p| (*p).to_owned())
        .collect();

    // One consistent, read-only snapshot for every number below.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    // The shared filters are "$n IS NULL OR column = $n" catch-alls. A cached generic plan
    // cannot drop the unused ones and loses the index paths above, so plan each execution
    // with its actual values (cheap next to the reads; CP7c).
    sqlx::query("SET LOCAL plan_cache_mode = force_custom_plan")
        .execute(&mut *tx)
        .await?;

    let valid: Option<bool> = bind_base!(sqlx::query_scalar(VALID_SQL), args)
        .fetch_one(&mut *tx)
        .await?;
    if valid != Some(true) {
        return Err(AppError::InvalidRequest);
    }

    let p: PeriodRow = bind_base!(sqlx::query_as(PERIOD_SQL), args)
        .fetch_one(&mut *tx)
        .await?;
    let c: CountRow = bind_base!(sqlx::query_as(&counts_sql()), args)
        .fetch_one(&mut *tx)
        .await?;
    // Without filters the counts come from the per-status counters (same snapshot).
    let by_status: HashMap<String, i64> = if args.filters == Filters::default() {
        super::ticket_list::workspace_status_counts(&mut tx, workspace_id).await?
    } else {
        bind_base!(sqlx::query_as::<_, (String, i64)>(STATUS_SQL), args)
            .fetch_all(&mut *tx)
            .await?
    }
    .into_iter()
    .collect();
    let pics: Vec<(Uuid, String, bool, i64)> = bind_base!(sqlx::query_as(&pic_sql()), args)
        .fetch_all(&mut *tx)
        .await?;
    let channels: Vec<DimensionRow> = bind_base!(
        sqlx::query_as(&dimension_sql("intake_channels", "intake_channel_id", 6, 7)),
        args
    )
    .fetch_all(&mut *tx)
    .await?;
    let categories: Vec<DimensionRow> = bind_base!(
        sqlx::query_as(&dimension_sql("categories", "category_id", 8, 9)),
        args
    )
    .fetch_all(&mut *tx)
    .await?;
    let trend: Vec<(String, i64, i64)> = bind_base!(sqlx::query_as(TREND_SQL), args)
        .fetch_all(&mut *tx)
        .await?;
    let attention_total: i64 = bind_base!(sqlx::query_scalar(&attention_count_sql()), args)
        .bind(&attention_codes)
        .fetch_one(&mut *tx)
        .await?;
    let attention_rows: Vec<AttentionRow> = bind_base!(sqlx::query_as(&attention_sql()), args)
        .bind(&attention_codes)
        .bind(ATTENTION_LIMIT)
        .fetch_all(&mut *tx)
        .await?;
    let attention_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM priorities WHERE workspace_id = $1 AND code = ANY($2) \
         ORDER BY array_position($2, code), id",
    )
    .bind(wid)
    .bind(&attention_codes)
    .fetch_all(&mut *tx)
    .await?;
    let filter_options = filter_options(&mut tx, wid).await?;
    tx.commit().await?;

    // Every status code in the table must be known; an unknown one is a server fault.
    if by_status
        .keys()
        .any(|k| TicketStatus::from_code(k).is_none())
    {
        return Err(AppError::internal(UnknownStatus));
    }

    let shared = TicketFilter::shared(&filters);
    let received = TicketFilter {
        received_from: Some(p.start_at.clone()),
        received_before: Some(p.end_at.clone()),
        ..shared.clone()
    };
    let resolved = TicketFilter {
        resolved_from: Some(p.start_at.clone()),
        resolved_before: Some(p.end_at.clone()),
        ..shared.clone()
    };
    let status_filter = |s: TicketStatus| TicketFilter {
        status: Some(vec![s.code()]),
        ..shared.clone()
    };
    let owner_filter = |owner: String| TicketFilter {
        status: Some(active_codes()),
        owner: Some(owner),
        ..shared.clone()
    };
    // Urgent/high priorities, narrowed by the priority filter.
    let attention_priorities: Vec<String> = attention_ids
        .iter()
        .filter(|id| filters.priority == Dim::Any || filters.priority == Dim::Id(**id))
        .map(Uuid::to_string)
        .collect();
    let attention_filter = (!attention_priorities.is_empty()).then(|| TicketFilter {
        status: Some(active_codes()),
        priority: Some(attention_priorities),
        ..shared.clone()
    });
    let unassigned_visible = matches!(filters.owner, Dim::Any | Dim::Unset);

    let status_distribution = TicketStatus::ALL
        .into_iter()
        // The archive state only appears when historical rows exist.
        .filter(|s| *s != TicketStatus::Closed || by_status.contains_key(s.code()))
        .map(|s| StatusCount {
            status: s.into(),
            active: ACTIVE.contains(&s),
            count: by_status.get(s.code()).copied().unwrap_or(0),
            filter: status_filter(s),
        })
        .collect();

    let mut pic_load: Vec<PicLoad> = pics
        .into_iter()
        .map(|(id, name, member_active, n)| PicLoad {
            user_id: Some(id),
            display_name: Some(name),
            member_active,
            active_tickets: n,
            filter: owner_filter(id.to_string()),
        })
        .collect();
    if unassigned_visible {
        pic_load.push(PicLoad {
            user_id: None,
            display_name: None,
            member_active: true,
            active_tickets: c.unassigned,
            filter: owner_filter(UNASSIGNED.to_owned()),
        });
    }

    let dimension = |rows: Vec<DimensionRow>, set: fn(&mut TicketFilter, String)| {
        rows.into_iter()
            .map(|r| {
                let mut filter = received.clone();
                set(
                    &mut filter,
                    r.id.map_or_else(|| NONE.to_owned(), |id| id.to_string()),
                );
                DimensionCount {
                    id: r.id,
                    code: r.code,
                    label: r.label,
                    count: r.n,
                    filter,
                }
            })
            .collect::<Vec<_>>()
    };
    let channels = dimension(channels, |f, v| f.channel = Some(v));
    let categories = dimension(categories, |f, v| f.category = Some(v));

    let attention = Attention {
        rule: "operational_rule",
        total: attention_total,
        limit: ATTENTION_LIMIT,
        items: attention_rows
            .into_iter()
            .map(|r| {
                let status = TicketStatus::from_code(&r.workflow_status)
                    .ok_or_else(|| AppError::internal(UnknownStatus))?;
                Ok(AttentionTicket {
                    id: r.id,
                    number: r.number,
                    display_number: format!("{}-{}", r.number_prefix, r.number),
                    subject: r.subject,
                    status: status.into(),
                    priority: named(r.priority_id, r.priority_code, r.priority_label),
                    owner: named(r.owner_user_id, None, r.owner_name),
                    channel: named(r.channel_id, r.channel_code, r.channel_label),
                    category: named(r.category_id, r.category_code, r.category_label),
                    received_at: r.received_at,
                    age_seconds: r.age_seconds,
                })
            })
            .collect::<Result<_, AppError>>()?,
    };

    Ok(Dashboard {
        workspace_id: wid,
        workspace_code: workspace_code.to_owned(),
        as_of: p.as_of,
        filters: AppliedFilters {
            owner: filters.owner.wire(UNASSIGNED),
            channel: filters.channel.wire(NONE),
            category: filters.category.wire(NONE),
            priority: filters.priority.wire(NONE),
        },
        filter_options,
        metric_basis: METRIC_BASIS,
        sla_configured: false,
        totals: Totals {
            tickets: by_status.values().sum(),
            active: ACTIVE
                .iter()
                .map(|s| by_status.get(s.code()).copied().unwrap_or(0))
                .sum(),
            unassigned: c.unassigned,
            pending_customer: by_status.get("pending_customer").copied().unwrap_or(0),
            received_in_period: c.received_in_period,
            resolved_in_period: c.resolved_in_period,
            resolved_tickets_in_period: c.resolved_tickets_in_period,
        },
        drilldown: Drilldowns {
            active: TicketFilter {
                status: Some(active_codes()),
                ..shared.clone()
            },
            unassigned: unassigned_visible.then(|| owner_filter(UNASSIGNED.to_owned())),
            pending_customer: status_filter(TicketStatus::PendingCustomer),
            received_in_period: received,
            resolved_in_period: resolved,
            attention: attention_filter,
        },
        status_distribution,
        pic_load,
        channels,
        categories,
        age: vec![
            AgeBucket {
                key: "lt_1d",
                label: "< 1 hari",
                count: c.age_lt_1d,
            },
            AgeBucket {
                key: "d1_2",
                label: "1–2 hari",
                count: c.age_1_2d,
            },
            AgeBucket {
                key: "gt_2d",
                label: "≥ 2 hari",
                count: c.age_gt_2d,
            },
        ],
        trend: trend
            .into_iter()
            .map(|(date, received, resolved)| TrendPoint {
                date,
                received,
                resolved,
            })
            .collect(),
        attention,
        period: Period {
            key,
            timezone: p.tz,
            start_date: p.start_date,
            end_date: p.end_date,
            start_at: p.start_at,
            end_at: p.end_at,
        },
    })
}

fn named(id: Option<Uuid>, code: Option<String>, label: Option<String>) -> Option<Named> {
    Some(Named {
        id: id?,
        code,
        label: label?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_parse_defaults_to_today_and_rejects_unknown() {
        assert_eq!(PeriodKey::parse(None), Some(PeriodKey::Today));
        assert_eq!(PeriodKey::parse(Some("today")), Some(PeriodKey::Today));
        assert_eq!(
            PeriodKey::parse(Some("last_7_days")),
            Some(PeriodKey::Last7Days)
        );
        assert_eq!(
            PeriodKey::parse(Some("this_month")),
            Some(PeriodKey::ThisMonth)
        );
        for bad in ["", "TODAY", "7d", "today;", "this_month "] {
            assert_eq!(PeriodKey::parse(Some(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn period_keys_serialize_to_the_query_values() {
        for key in [PeriodKey::Today, PeriodKey::Last7Days, PeriodKey::ThisMonth] {
            let wire = serde_json::to_value(key).unwrap_or_default();
            assert_eq!(PeriodKey::parse(wire.as_str()), Some(key), "{wire}");
        }
    }

    #[test]
    fn filter_values_are_canonical_ids_or_the_unset_word() {
        let id = Uuid::from_u128(0x1234_5678_9abc_4def_8123_4567_89ab_cdef);
        let s = id.to_string();
        assert_eq!(Dim::parse(None, NONE), Some(Dim::Any));
        assert_eq!(Dim::parse(Some("none"), NONE), Some(Dim::Unset));
        assert_eq!(Dim::parse(Some("unassigned"), UNASSIGNED), Some(Dim::Unset));
        assert_eq!(Dim::parse(Some(&s), NONE), Some(Dim::Id(id)));
        // The other dimension's word, non-canonical UUID spellings and junk are rejected.
        let upper = s.to_uppercase();
        let simple = id.simple().to_string();
        let braced = format!("{{{s}}}");
        for bad in [
            "",
            "unassigned",
            "None",
            upper.as_str(),
            simple.as_str(),
            braced.as_str(),
            "x",
        ] {
            assert_eq!(Dim::parse(Some(bad), NONE), None, "{bad:?}");
        }
        assert_eq!(Dim::parse(Some("none"), UNASSIGNED), None);
        for d in [Dim::Unset, Dim::Id(id)] {
            assert_eq!(Dim::parse(d.wire(NONE).as_deref(), NONE), Some(d));
        }
        assert_eq!(Dim::Any.wire(NONE), None);
    }

    #[test]
    fn shared_filters_travel_with_every_drilldown() {
        let id = Uuid::from_u128(7);
        let f = Filters {
            owner: Dim::Unset,
            channel: Dim::Id(id),
            category: Dim::Unset,
            priority: Dim::Id(id),
        };
        let t = serde_json::to_value(TicketFilter::shared(&f)).unwrap_or_default();
        assert_eq!(
            t,
            serde_json::json!({
                "owner": "unassigned", "channel": id.to_string(), "category": "none",
                "priority": [id.to_string()]
            })
        );
        let none =
            serde_json::to_value(TicketFilter::shared(&Filters::default())).unwrap_or_default();
        assert_eq!(none, serde_json::json!({}));
    }

    #[test]
    fn active_codes_exclude_terminal_states() {
        let codes = active_codes();
        assert_eq!(
            codes,
            ["new", "open", "pending_customer", "pending_engineering"]
        );
        assert_eq!(ACTIVE_SQL.matches('\'').count(), codes.len() * 2);
        for c in &codes {
            assert!(ACTIVE_SQL.contains(&format!("'{c}'")));
        }
    }
}
