//! O01 Dashboard (D05): server aggregates over the WHOLE verified workspace scope, never
//! a page of tickets. Every query is filtered by the workspace the caller was verified for
//! and runs in one read-only snapshot, so all numbers and `asOf` describe the same moment.
//!
//! Metric definitions (PRD §4):
//! - active = nonterminal status (`new`, `open`, `pending_customer`, `pending_engineering`);
//! - status, PIC load, age and "perlu perhatian" are current-state counts, not period counts;
//! - received in period = `received_at` inside the period (basis: diterima);
//! - resolved in period = `resolve` events inside the period (a later reopen keeps it);
//! - age is measured from `received_at`; no SLA label (SLA is not configured).
//!
//! Periods are calendar days in the workspace time zone (Asia/Jakarta for every workspace
//! today), half-open `[start, end)`.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
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
/// Priority codes that put an active ticket on the "perlu perhatian" list.
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

/// Period bounds in the workspace time zone. `$1` = workspace, `$2` = period code.
/// `now()` is the transaction start, identical for every query of one request.
macro_rules! period_cte {
    () => {
        "WITH d AS ( \
           SELECT w.timezone AS tz, (now() AT TIME ZONE w.timezone)::date AS today \
           FROM workspaces w WHERE w.id = $1), \
         b AS ( \
           SELECT tz, \
             CASE $2::text WHEN 'today' THEN today WHEN 'last_7_days' THEN today - 6 \
               ELSE date_trunc('month', today::timestamp)::date END AS start_date, \
             CASE $2::text WHEN 'this_month' THEN (date_trunc('month', today::timestamp) + interval '1 month')::date \
               ELSE today + 1 END AS end_date \
           FROM d), \
         p AS ( \
           SELECT tz, start_date, end_date, \
             (start_date::timestamp AT TIME ZONE tz) AS start_at, \
             (end_date::timestamp AT TIME ZONE tz) AS end_at \
           FROM b) "
    };
}

// ---------- response ----------

/// Filter a ticket list must apply to reproduce a number (drilldown). Every field is
/// optional; absent means "no restriction". Times are RFC 3339 UTC, half-open.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Vec<&'static str>>,
    /// A member user ID, or `unassigned`.
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_before: Option<String>,
}

fn active_codes() -> Vec<&'static str> {
    ACTIVE.iter().map(|s| s.code()).collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    key: PeriodKey,
    basis: &'static str,
    timezone: String,
    /// First local day, inclusive (YYYY-MM-DD).
    start_date: String,
    /// Last local day, inclusive (YYYY-MM-DD).
    end_date: String,
    start_at: String,
    /// Exclusive.
    end_at: String,
}

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
    unassigned: TicketFilter,
    pending_customer: TicketFilter,
    received_in_period: TicketFilter,
    resolved_in_period: TicketFilter,
    attention: TicketFilter,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelCount {
    /// `None` = no intake channel recorded.
    channel_id: Option<Uuid>,
    code: Option<String>,
    label: Option<String>,
    count: i64,
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
    received_at: String,
    age_seconds: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
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
    sla_configured: bool,
    totals: Totals,
    drilldown: Drilldowns,
    status_distribution: Vec<StatusCount>,
    pic_load: Vec<PicLoad>,
    /// Tickets received in the period, per intake channel.
    channels: Vec<ChannelCount>,
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
    tickets: i64,
    active: i64,
    unassigned: i64,
    received_in_period: i64,
    age_lt_1d: i64,
    age_1_2d: i64,
    age_gt_2d: i64,
    resolved_in_period: i64,
    resolved_tickets_in_period: i64,
}

#[derive(sqlx::FromRow)]
struct AttentionRow {
    id: Uuid,
    number: i64,
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
    received_at: String,
    age_seconds: i64,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workflow_status code in database")]
struct UnknownStatus;

const PERIOD_SQL: &str = concat!(
    period_cte!(),
    "SELECT tz, start_date::text AS start_date, (end_date - 1)::text AS end_date, ",
    utc!("start_at"),
    " AS start_at, ",
    utc!("end_at"),
    " AS end_at, ",
    utc!("now()"),
    " AS as_of FROM p"
);

fn counts_sql() -> String {
    format!(
        concat!(
            period_cte!(),
            "SELECT \
               count(t.id) AS tickets, \
               count(*) FILTER (WHERE t.workflow_status IN {a}) AS active, \
               count(*) FILTER (WHERE t.workflow_status IN {a} AND t.owner_user_id IS NULL) AS unassigned, \
               count(*) FILTER (WHERE t.received_at >= p.start_at AND t.received_at < p.end_at) AS received_in_period, \
               count(*) FILTER (WHERE t.workflow_status IN {a} AND now() - t.received_at < interval '1 day') AS age_lt_1d, \
               count(*) FILTER (WHERE t.workflow_status IN {a} AND now() - t.received_at >= interval '1 day' \
                                 AND now() - t.received_at < interval '2 days') AS age_1_2d, \
               count(*) FILTER (WHERE t.workflow_status IN {a} AND now() - t.received_at >= interval '2 days') AS age_gt_2d, \
               (SELECT count(*) FROM ticket_resolution_events e WHERE e.workspace_id = $1 AND e.action = 'resolve' \
                  AND e.occurred_at >= p.start_at AND e.occurred_at < p.end_at) AS resolved_in_period, \
               (SELECT count(DISTINCT e.ticket_id) FROM ticket_resolution_events e WHERE e.workspace_id = $1 \
                  AND e.action = 'resolve' AND e.occurred_at >= p.start_at AND e.occurred_at < p.end_at) \
                  AS resolved_tickets_in_period \
             FROM p LEFT JOIN tickets t ON t.workspace_id = $1 \
             GROUP BY p.start_at, p.end_at"
        ),
        a = ACTIVE_SQL
    )
}

const STATUS_SQL: &str =
    "SELECT workflow_status, count(*) FROM tickets WHERE workspace_id = $1 GROUP BY 1";

fn pic_sql() -> String {
    format!(
        "WITH c AS ( \
           SELECT owner_user_id, count(*) AS n FROM tickets \
           WHERE workspace_id = $1 AND workflow_status IN {ACTIVE_SQL} GROUP BY 1) \
         SELECT m.user_id, u.display_name, (m.active AND u.active) AS member_active, \
                COALESCE(c.n, 0) AS n \
         FROM workspace_memberships m \
         JOIN users u ON u.id = m.user_id \
         LEFT JOIN c ON c.owner_user_id = m.user_id \
         WHERE m.workspace_id = $1 AND (m.active OR c.n IS NOT NULL) \
         ORDER BY n DESC, u.display_name, m.user_id"
    )
}

const CHANNEL_SQL: &str = concat!(
    period_cte!(),
    ", r AS (SELECT t.intake_channel_id, count(*) AS n FROM tickets t, p \
       WHERE t.workspace_id = $1 AND t.received_at >= p.start_at AND t.received_at < p.end_at \
       GROUP BY 1) \
     SELECT ic.id, ic.code, ic.label, COALESCE(r.n, 0) AS n, ic.sort_order \
     FROM intake_channels ic LEFT JOIN r ON r.intake_channel_id = ic.id \
     WHERE ic.workspace_id = $1 AND (ic.active OR r.n IS NOT NULL) \
     UNION ALL \
     SELECT NULL, NULL, NULL, r.n, 2147483647 FROM r WHERE r.intake_channel_id IS NULL \
     ORDER BY 5, 3"
);

const TREND_SQL: &str = concat!(
    period_cte!(),
    ", days AS ( \
       SELECT g::date AS day FROM p, \
         generate_series(p.start_date::timestamp, (p.end_date - 1)::timestamp, interval '1 day') AS g), \
     rec AS ( \
       SELECT (t.received_at AT TIME ZONE p.tz)::date AS day, count(*) AS n FROM tickets t, p \
       WHERE t.workspace_id = $1 AND t.received_at >= p.start_at AND t.received_at < p.end_at GROUP BY 1), \
     res AS ( \
       SELECT (e.occurred_at AT TIME ZONE p.tz)::date AS day, count(*) AS n \
       FROM ticket_resolution_events e, p \
       WHERE e.workspace_id = $1 AND e.action = 'resolve' \
         AND e.occurred_at >= p.start_at AND e.occurred_at < p.end_at GROUP BY 1) \
     SELECT days.day::text, COALESCE(rec.n, 0), COALESCE(res.n, 0) \
     FROM days LEFT JOIN rec USING (day) LEFT JOIN res USING (day) ORDER BY days.day"
);

fn attention_sql() -> String {
    format!(
        concat!(
            "SELECT t.id, t.number, t.subject, t.workflow_status, \
               pr.id AS priority_id, pr.code AS priority_code, pr.label AS priority_label, \
               t.owner_user_id, u.display_name AS owner_name, \
               ic.id AS channel_id, ic.code AS channel_code, ic.label AS channel_label, ",
            utc!("t.received_at"),
            " AS received_at, \
               GREATEST(0, floor(extract(epoch FROM now() - t.received_at)))::bigint AS age_seconds \
             FROM tickets t \
             JOIN priorities pr ON pr.workspace_id = t.workspace_id AND pr.id = t.priority_id \
             LEFT JOIN users u ON u.id = t.owner_user_id \
             LEFT JOIN intake_channels ic ON ic.workspace_id = t.workspace_id AND ic.id = t.intake_channel_id \
             WHERE t.workspace_id = $1 AND t.workflow_status IN {a} AND pr.code = ANY($2) \
             ORDER BY array_position($2, pr.code), t.received_at, t.number \
             LIMIT $3"
        ),
        a = ACTIVE_SQL
    )
}

fn attention_count_sql() -> String {
    format!(
        "SELECT count(*) FROM tickets t \
         JOIN priorities pr ON pr.workspace_id = t.workspace_id AND pr.id = t.priority_id \
         WHERE t.workspace_id = $1 AND t.workflow_status IN {ACTIVE_SQL} AND pr.code = ANY($2)"
    )
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
    let wid = ws.workspace_id;
    let code = key.code();
    let attention_priorities: Vec<String> = ATTENTION_PRIORITIES
        .iter()
        .map(|p| (*p).to_owned())
        .collect();

    // One consistent, read-only snapshot for every number below.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;

    let p: PeriodRow = sqlx::query_as(PERIOD_SQL)
        .bind(wid)
        .bind(code)
        .fetch_one(&mut *tx)
        .await?;
    let c: CountRow = sqlx::query_as(&counts_sql())
        .bind(wid)
        .bind(code)
        .fetch_one(&mut *tx)
        .await?;
    let by_status: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(STATUS_SQL)
        .bind(wid)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
    let pics: Vec<(Uuid, String, bool, i64)> = sqlx::query_as(&pic_sql())
        .bind(wid)
        .fetch_all(&mut *tx)
        .await?;
    #[allow(clippy::type_complexity)]
    let channels: Vec<(Option<Uuid>, Option<String>, Option<String>, i64, i32)> =
        sqlx::query_as(CHANNEL_SQL)
            .bind(wid)
            .bind(code)
            .fetch_all(&mut *tx)
            .await?;
    let trend: Vec<(String, i64, i64)> = sqlx::query_as(TREND_SQL)
        .bind(wid)
        .bind(code)
        .fetch_all(&mut *tx)
        .await?;
    let attention_total: i64 = sqlx::query_scalar(&attention_count_sql())
        .bind(wid)
        .bind(&attention_priorities)
        .fetch_one(&mut *tx)
        .await?;
    let attention_rows: Vec<AttentionRow> = sqlx::query_as(&attention_sql())
        .bind(wid)
        .bind(&attention_priorities)
        .bind(ATTENTION_LIMIT)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    // Every status code in the table must be known; an unknown one is a server fault.
    if by_status
        .keys()
        .any(|k| TicketStatus::from_code(k).is_none())
    {
        return Err(AppError::internal(UnknownStatus));
    }

    let received = TicketFilter {
        received_from: Some(p.start_at.clone()),
        received_before: Some(p.end_at.clone()),
        ..TicketFilter::default()
    };
    let resolved = TicketFilter {
        resolved_from: Some(p.start_at.clone()),
        resolved_before: Some(p.end_at.clone()),
        ..TicketFilter::default()
    };
    let status_filter = |s: TicketStatus| TicketFilter {
        status: Some(vec![s.code()]),
        ..TicketFilter::default()
    };
    let attention_filter = TicketFilter {
        status: Some(active_codes()),
        priority: Some(ATTENTION_PRIORITIES.to_vec()),
        ..TicketFilter::default()
    };

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

    let owner_filter = |owner: String| TicketFilter {
        status: Some(active_codes()),
        owner: Some(owner),
        ..TicketFilter::default()
    };
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
    pic_load.push(PicLoad {
        user_id: None,
        display_name: None,
        member_active: true,
        active_tickets: c.unassigned,
        filter: owner_filter("unassigned".to_owned()),
    });

    let attention = Attention {
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
                    display_number: format!("{}-{}", ws.workspace_code, r.number),
                    subject: r.subject,
                    status: status.into(),
                    priority: named(r.priority_id, r.priority_code, r.priority_label),
                    owner: named(r.owner_user_id, None, r.owner_name),
                    channel: named(r.channel_id, r.channel_code, r.channel_label),
                    received_at: r.received_at,
                    age_seconds: r.age_seconds,
                })
            })
            .collect::<Result<_, AppError>>()?,
    };

    Ok(Json(Dashboard {
        workspace_id: wid,
        workspace_code: ws.workspace_code.clone(),
        as_of: p.as_of,
        sla_configured: false,
        totals: Totals {
            tickets: c.tickets,
            active: c.active,
            unassigned: c.unassigned,
            pending_customer: by_status.get("pending_customer").copied().unwrap_or(0),
            received_in_period: c.received_in_period,
            resolved_in_period: c.resolved_in_period,
            resolved_tickets_in_period: c.resolved_tickets_in_period,
        },
        drilldown: Drilldowns {
            active: TicketFilter {
                status: Some(active_codes()),
                ..TicketFilter::default()
            },
            unassigned: owner_filter("unassigned".to_owned()),
            pending_customer: status_filter(TicketStatus::PendingCustomer),
            received_in_period: received,
            resolved_in_period: resolved,
            attention: attention_filter,
        },
        status_distribution,
        pic_load,
        channels: channels
            .into_iter()
            .map(|(id, code, label, n, _)| ChannelCount {
                channel_id: id,
                code,
                label,
                count: n,
            })
            .collect(),
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
            basis: "received",
            timezone: p.tz,
            start_date: p.start_date,
            end_date: p.end_date,
            start_at: p.start_at,
            end_at: p.end_at,
        },
    }))
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
