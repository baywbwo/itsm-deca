//! O02/O03 Tiket List + Board (D06) and status transitions.
//!
//! List and Board read the same tickets with the same filters, search, sort and permissions.
//! Every query is scoped to the workspace the caller was verified for and runs in one
//! read-only snapshot, so `total`, `countsByStatus`, the page and `asOf` agree.
//!
//! - Filters are the dashboard drilldown contract (D05): `status`, `owner` (`unassigned`),
//!   `channel` / `category` (`none`), `priority` (ID list or `none`), `receivedFrom/Before`
//!   and `resolvedFrom/Before`, plus `q` (subject or ticket number). Dimension filters use the
//!   ticket's CURRENT attributes (operational scope, not a historical report).
//! - `resolvedFrom/Before` keeps tickets with at least one `resolve` event in the range, each
//!   ticket once (multi-cycle dedupe); `resolutionEvents` counts the events separately.
//! - Sort is server-side with the ticket ID as tie-break; the cursor is keyset, opaque and
//!   bound to the workspace, filters and sort (a cursor from another scope is 400). The
//!   scope is re-applied on every page, so a forged cursor cannot widen access.
//! - `total` counts the whole filtered scope (not the page); `countsByStatus` counts the same
//!   scope without the status filter, so Board columns and List tabs share one basis.
//! - Transitions follow the D01 lifecycle: moves between the four active statuses, resolve
//!   with a summary (blocking related work refuses it), reopen of a resolved ticket with a
//!   reason (next cycle). `closed` is an archive: never a target, never changed. The client
//!   version must match (409), and event + status + audit commit in one transaction.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, AssignmentKind, Permission, WorkspaceAccess};
use crate::domain::{TicketStatus, TicketStatusView};
use crate::error::AppError;
use crate::routes::dashboard::{Dim, NONE, UNASSIGNED};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 100;
pub const BOARD_DEFAULT_LIMIT: i64 = 20;
pub const BOARD_MAX_LIMIT: i64 = 50;
const MAX_QUERY_CHARS: usize = 100;
const MAX_CURSOR_CHARS: usize = 512;
const MAX_NOTE_CHARS: usize = 5000;
const MAX_ASSIGN_REASON_CHARS: usize = 500;

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

// ---------- query ----------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    pub(crate) q: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) channel: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) priority: Option<String>,
    pub(crate) received_from: Option<String>,
    pub(crate) received_before: Option<String>,
    pub(crate) resolved_from: Option<String>,
    pub(crate) resolved_before: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) order: Option<String>,
    pub(crate) limit: Option<String>,
    pub(crate) cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    UpdatedAt,
    ReceivedAt,
    Number,
}

impl SortKey {
    fn parse(raw: Option<&str>) -> Option<SortKey> {
        match raw {
            None | Some("updated_at") => Some(SortKey::UpdatedAt),
            Some("received_at") => Some(SortKey::ReceivedAt),
            Some("number") => Some(SortKey::Number),
            Some(_) => None,
        }
    }

    fn code(self) -> &'static str {
        match self {
            SortKey::UpdatedAt => "updated_at",
            SortKey::ReceivedAt => "received_at",
            SortKey::Number => "number",
        }
    }

    /// Column compared by the keyset, and how the cursor's text key is cast back.
    fn column(self) -> &'static str {
        match self {
            SortKey::UpdatedAt => "t.updated_at",
            SortKey::ReceivedAt => "t.received_at",
            SortKey::Number => "t.number",
        }
    }

    fn key_text(self) -> &'static str {
        match self {
            SortKey::UpdatedAt => utc!("t.updated_at"),
            SortKey::ReceivedAt => utc!("t.received_at"),
            SortKey::Number => "t.number::text",
        }
    }

    fn key_cast(self) -> &'static str {
        match self {
            SortKey::Number => "::bigint",
            _ => "::timestamptz",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Asc,
    Desc,
}

impl Order {
    fn parse(raw: Option<&str>) -> Option<Order> {
        match raw {
            None | Some("desc") => Some(Order::Desc),
            Some("asc") => Some(Order::Asc),
            Some(_) => None,
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Order::Asc => "ASC",
            Order::Desc => "DESC",
        }
    }

    fn cmp(self) -> &'static str {
        match self {
            Order::Asc => ">",
            Order::Desc => "<",
        }
    }
}

/// Parsed, validated filters (everything except paging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketQuery {
    /// Search text as entered (trimmed); matched against subject or the ticket number.
    pub q: Option<String>,
    /// `None` = every status (including the closed archive).
    pub status: Option<Vec<TicketStatus>>,
    pub owner: Dim,
    pub channel: Dim,
    pub category: Dim,
    /// `None` = any, `Some(vec![])` is never produced; `priority_unset` = `none`.
    pub priority: Option<Vec<Uuid>>,
    pub priority_unset: bool,
    pub received_from: Option<String>,
    pub received_before: Option<String>,
    pub resolved_from: Option<String>,
    pub resolved_before: Option<String>,
    pub sort: SortKey,
    pub order: Order,
}

pub(crate) fn non_empty(raw: Option<&str>) -> Option<&str> {
    raw.filter(|s| !s.is_empty())
}

/// RFC 3339 shape check before PostgreSQL validates the calendar value.
pub(crate) fn instant(raw: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(s) = non_empty(raw) else {
        return Ok(None);
    };
    let b = s.as_bytes();
    let shaped = s.len() <= 40
        && b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && (s.ends_with('Z') || s[19..].contains('+') || s[19..].contains('-'))
        && s.bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'-' | b':' | b'.' | b'T' | b'Z' | b'+'));
    if shaped {
        Ok(Some(s.to_owned()))
    } else {
        Err(AppError::InvalidRequest)
    }
}

impl TicketQuery {
    pub fn parse(q: &ListQuery) -> Result<TicketQuery, AppError> {
        let bad = || AppError::InvalidRequest;
        let search = match q.q.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s) if s.chars().count() > MAX_QUERY_CHARS => return Err(bad()),
            Some(s) => Some(s.to_owned()),
        };
        let status = match non_empty(q.status.as_deref()) {
            None => None,
            Some(raw) => {
                let mut v = Vec::new();
                for code in raw.split(',') {
                    let s = TicketStatus::from_code(code).ok_or_else(bad)?;
                    if !v.contains(&s) {
                        v.push(s);
                    }
                }
                Some(v)
            }
        };
        let (priority, priority_unset) = match non_empty(q.priority.as_deref()) {
            None => (None, false),
            Some(p) if p == NONE => (None, true),
            Some(raw) => {
                let mut ids = Vec::new();
                for part in raw.split(',') {
                    match Dim::parse(Some(part), NONE) {
                        Some(Dim::Id(id)) => {
                            if !ids.contains(&id) {
                                ids.push(id);
                            }
                        }
                        _ => return Err(bad()),
                    }
                }
                (Some(ids), false)
            }
        };
        Ok(TicketQuery {
            q: search,
            status,
            owner: Dim::parse(non_empty(q.owner.as_deref()), UNASSIGNED).ok_or_else(bad)?,
            channel: Dim::parse(non_empty(q.channel.as_deref()), NONE).ok_or_else(bad)?,
            category: Dim::parse(non_empty(q.category.as_deref()), NONE).ok_or_else(bad)?,
            priority,
            priority_unset,
            received_from: instant(q.received_from.as_deref())?,
            received_before: instant(q.received_before.as_deref())?,
            resolved_from: instant(q.resolved_from.as_deref())?,
            resolved_before: instant(q.resolved_before.as_deref())?,
            sort: SortKey::parse(non_empty(q.sort.as_deref())).ok_or_else(bad)?,
            order: Order::parse(non_empty(q.order.as_deref())).ok_or_else(bad)?,
        })
    }

    fn resolved_filter(&self) -> bool {
        self.resolved_from.is_some() || self.resolved_before.is_some()
    }

    /// Ticket number searched for: `12`, `#12` or `DUMA-12` (any workspace code prefix is
    /// accepted; the scope is still this workspace).
    fn number(&self) -> Option<i64> {
        let q = self.q.as_deref()?;
        let digits = q
            .rsplit_once('-')
            .map(|(_, n)| n)
            .unwrap_or(q)
            .trim_start_matches('#');
        digits.parse::<i64>().ok().filter(|n| *n > 0)
    }

    /// Prefix of a searched ticket number ("duma-12" → DUMA). A ticket keeps the prefix it was
    /// issued with, so after a code change "DUMA-12" finds only the old ticket 12 and the new
    /// code's "DECA-12" does not match it. Bare numbers ("12", "#12") match any prefix.
    fn number_prefix(&self) -> Option<String> {
        let (prefix, _) = self.q.as_deref()?.rsplit_once('-')?;
        let p = prefix.trim().to_ascii_uppercase();
        let code = (2..=10).contains(&p.len())
            && p.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        code.then_some(p)
    }

    fn like(&self) -> Option<String> {
        let q = self.q.as_deref()?;
        let mut out = String::with_capacity(q.len() + 2);
        out.push('%');
        for c in q.chars() {
            if matches!(c, '%' | '_' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('%');
        Some(out)
    }

    /// Canonical form the cursor is bound to, together with the workspace.
    fn fingerprint(&self, workspace_id: Uuid) -> String {
        let mut status: Vec<&str> = self.status.iter().flatten().map(|s| s.code()).collect();
        status.sort_unstable();
        let mut priority: Vec<String> = self
            .priority
            .iter()
            .flatten()
            .map(Uuid::to_string)
            .collect();
        priority.sort_unstable();
        let canonical = serde_json::json!([
            workspace_id,
            self.q,
            self.status.as_ref().map(|_| status),
            self.owner.wire(UNASSIGNED),
            self.channel.wire(NONE),
            self.category.wire(NONE),
            self.priority.as_ref().map(|_| priority),
            self.priority_unset,
            self.received_from,
            self.received_before,
            self.resolved_from,
            self.resolved_before,
            self.sort.code(),
            self.order,
        ]);
        let digest = Sha256::digest(canonical.to_string().as_bytes());
        URL_SAFE_NO_PAD.encode(&digest[..16])
    }

    fn with_status(&self, s: TicketStatus) -> TicketQuery {
        TicketQuery {
            status: Some(vec![s]),
            ..self.clone()
        }
    }

    /// Canonical query-string form stored by a saved view (C32): filters, sort and order in
    /// the List/Board wire format. Search text is never part of it (free text may hold PII).
    pub(crate) fn saved_params(&self) -> BTreeMap<&'static str, String> {
        let mut out = BTreeMap::new();
        if let Some(v) = &self.status {
            let codes: Vec<&str> = v.iter().map(|s| s.code()).collect();
            out.insert("status", codes.join(","));
        }
        for (key, value) in [
            ("owner", self.owner.wire(UNASSIGNED)),
            ("channel", self.channel.wire(NONE)),
            ("category", self.category.wire(NONE)),
        ] {
            if let Some(v) = value {
                out.insert(key, v);
            }
        }
        if self.priority_unset {
            out.insert("priority", NONE.to_owned());
        } else if let Some(ids) = &self.priority {
            let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
            out.insert("priority", ids.join(","));
        }
        for (key, value) in [
            ("receivedFrom", &self.received_from),
            ("receivedBefore", &self.received_before),
            ("resolvedFrom", &self.resolved_from),
            ("resolvedBefore", &self.resolved_before),
        ] {
            if let Some(v) = value {
                out.insert(key, v.clone());
            }
        }
        out.insert("sort", self.sort.code().to_owned());
        out.insert("order", self.order.sql().to_ascii_lowercase());
        out
    }

    pub fn applied(&self) -> AppliedTicketFilters {
        AppliedTicketFilters {
            q: self.q.clone(),
            status: self
                .status
                .as_ref()
                .map(|v| v.iter().map(|s| s.code()).collect()),
            owner: self.owner.wire(UNASSIGNED),
            channel: self.channel.wire(NONE),
            category: self.category.wire(NONE),
            priority: if self.priority_unset {
                Some(vec![NONE.to_owned()])
            } else {
                self.priority
                    .as_ref()
                    .map(|v| v.iter().map(Uuid::to_string).collect())
            },
            received_from: self.received_from.clone(),
            received_before: self.received_before.clone(),
            resolved_from: self.resolved_from.clone(),
            resolved_before: self.resolved_before.clone(),
            sort: self.sort,
            order: self.order,
        }
    }
}

/// Keyset position after the last row of a page.
#[derive(Debug, Serialize, Deserialize)]
struct CursorBody {
    v: u8,
    /// Fingerprint of workspace + filters + sort.
    f: String,
    /// Sort key of the last row, as text.
    k: String,
    i: Uuid,
}

pub(crate) fn encode_cursor(fp: &str, key: String, id: Uuid) -> Result<String, AppError> {
    let body = CursorBody {
        v: 1,
        f: fp.to_owned(),
        k: key,
        i: id,
    };
    let json = serde_json::to_vec(&body).map_err(AppError::internal)?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

pub(crate) fn decode_cursor(raw: &str, fp: &str) -> Result<(String, Uuid), AppError> {
    if raw.len() > MAX_CURSOR_CHARS {
        return Err(AppError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| AppError::InvalidRequest)?;
    let body: CursorBody = serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    if body.v != 1 || body.f != fp || body.k.len() > 64 {
        tracing::info!(error.kind = "cursor_scope_mismatch", "cursor refused");
        return Err(AppError::InvalidRequest);
    }
    Ok((body.k, body.i))
}

pub(crate) fn parse_limit(raw: Option<&str>, default: i64, max: i64) -> Result<i64, AppError> {
    match non_empty(raw) {
        None => Ok(default),
        Some(s) => s
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=max).contains(n))
            .ok_or(AppError::InvalidRequest),
    }
}

// ---------- SQL ----------

/// Appends the shared WHERE clause (workspace scope + filters). `with_status` false drops
/// the status filter (for `countsByStatus`).
fn push_scope(b: &mut QueryBuilder<'_, Postgres>, ws: Uuid, f: &TicketQuery, with_status: bool) {
    b.push(" WHERE t.workspace_id = ").push_bind(ws);
    if with_status && let Some(status) = &f.status {
        let codes: Vec<String> = status.iter().map(|s| s.code().to_owned()).collect();
        b.push(" AND t.workflow_status = ANY(")
            .push_bind(codes)
            .push(")");
    }
    let dims = [
        ("t.owner_user_id", f.owner),
        ("t.intake_channel_id", f.channel),
        ("t.category_id", f.category),
    ];
    for (col, dim) in dims {
        match dim {
            Dim::Any => {}
            Dim::Unset => {
                b.push(format_args!(" AND {col} IS NULL"));
            }
            Dim::Id(id) => {
                b.push(format_args!(" AND {col} = ")).push_bind(id);
            }
        }
    }
    if f.priority_unset {
        b.push(" AND t.priority_id IS NULL");
    } else if let Some(ids) = &f.priority {
        b.push(" AND t.priority_id = ANY(")
            .push_bind(ids.clone())
            .push(")");
    }
    if let Some(v) = &f.received_from {
        b.push(" AND t.received_at >= ")
            .push_bind(v.clone())
            .push("::timestamptz");
    }
    if let Some(v) = &f.received_before {
        b.push(" AND t.received_at < ")
            .push_bind(v.clone())
            .push("::timestamptz");
    }
    if f.resolved_filter() {
        b.push(
            " AND EXISTS (SELECT 1 FROM ticket_resolution_events e \
               WHERE e.workspace_id = t.workspace_id AND e.ticket_id = t.id AND e.action = 'resolve'",
        );
        push_event_range(b, f);
        b.push(")");
    }
    if let Some(like) = f.like() {
        b.push(" AND (t.subject ILIKE ")
            .push_bind(like)
            .push(" ESCAPE '\\'");
        if let Some(n) = f.number() {
            b.push(" OR (t.number = ").push_bind(n);
            if let Some(p) = f.number_prefix() {
                b.push(" AND t.number_prefix = ").push_bind(p);
            }
            b.push(")");
        }
        b.push(")");
    }
}

fn push_event_range(b: &mut QueryBuilder<'_, Postgres>, f: &TicketQuery) {
    if let Some(v) = &f.resolved_from {
        b.push(" AND e.occurred_at >= ")
            .push_bind(v.clone())
            .push("::timestamptz");
    }
    if let Some(v) = &f.resolved_before {
        b.push(" AND e.occurred_at < ")
            .push_bind(v.clone())
            .push("::timestamptz");
    }
}

const ITEM_COLUMNS: &str = concat!(
    "SELECT t.id, t.number, t.number_prefix, t.subject, t.workflow_status, t.cycle_no, t.version, ",
    utc!("t.received_at"),
    " AS received_at, ",
    utc!("t.updated_at"),
    " AS updated_at, \
     floor(extract(epoch FROM now() - t.received_at))::bigint AS age_seconds, \
     p.id AS priority_id, p.code AS priority_code, p.label AS priority_label, \
     u.id AS owner_id, u.display_name AS owner_name, \
     c.id AS channel_id, c.label AS channel_label, \
     k.id AS category_id, k.label AS category_label, \
     (SELECT count(*) FROM related_work r WHERE r.workspace_id = t.workspace_id \
        AND r.ticket_id = t.id AND r.completion_state = 'open') AS related_open, \
     (SELECT count(*) FROM related_work r WHERE r.workspace_id = t.workspace_id \
        AND r.ticket_id = t.id AND r.completion_state = 'open' AND r.blocking) AS related_blocking_open"
);

const ITEM_FROM: &str = " FROM tickets t \
     LEFT JOIN priorities p ON p.workspace_id = t.workspace_id AND p.id = t.priority_id \
     LEFT JOIN users u ON u.id = t.owner_user_id \
     LEFT JOIN intake_channels c ON c.workspace_id = t.workspace_id AND c.id = t.intake_channel_id \
     LEFT JOIN categories k ON k.workspace_id = t.workspace_id AND k.id = t.category_id";

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: Uuid,
    number: i64,
    number_prefix: String,
    subject: String,
    workflow_status: String,
    cycle_no: i32,
    version: i32,
    received_at: String,
    updated_at: String,
    age_seconds: i64,
    priority_id: Option<Uuid>,
    priority_code: Option<String>,
    priority_label: Option<String>,
    owner_id: Option<Uuid>,
    owner_name: Option<String>,
    channel_id: Option<Uuid>,
    channel_label: Option<String>,
    category_id: Option<Uuid>,
    category_label: Option<String>,
    related_open: i64,
    related_blocking_open: i64,
    sort_key: String,
}

// ---------- response ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ref {
    id: Uuid,
    /// `null` for people.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Move,
    Resolve,
    Reopen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Requires {
    None,
    /// Resolution summary (`summary`).
    Summary,
    /// Reopen reason (`reason`).
    Reason,
}

/// A status change the caller may request now. The server re-checks everything on submit.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowedAction {
    to: TicketStatusView,
    kind: ActionKind,
    requires: Requires,
    /// Why the action is currently refused (`blocking_related_work`), or `null`.
    blocked_by: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedWorkSummary {
    open: i64,
    blocking_open: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketListItem {
    id: Uuid,
    number: i64,
    display_number: String,
    subject: String,
    status: TicketStatusView,
    cycle_no: i32,
    version: i32,
    priority: Option<Ref>,
    owner: Option<Ref>,
    channel: Option<Ref>,
    category: Option<Ref>,
    received_at: String,
    updated_at: String,
    /// Seconds since `receivedAt` at `asOf`; no SLA meaning.
    age_seconds: i64,
    related_work: RelatedWorkSummary,
    allowed_actions: Vec<AllowedAction>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedTicketFilters {
    q: Option<String>,
    status: Option<Vec<&'static str>>,
    owner: Option<String>,
    channel: Option<String>,
    category: Option<String>,
    priority: Option<Vec<String>>,
    received_from: Option<String>,
    received_before: Option<String>,
    resolved_from: Option<String>,
    resolved_before: Option<String>,
    sort: SortKey,
    order: Order,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCountView {
    status: TicketStatusView,
    count: i64,
}

/// How the numbers are counted, for UI labels and help text.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListBasis {
    /// `ticket` — `total` and `countsByStatus` count tickets, each once.
    unit: &'static str,
    /// `current_ticket_attributes` — PIC/kanal/kategori/prioritas filters use today's values.
    dimensions: &'static str,
    /// `resolve_event_in_range` when a resolution range is applied (ticket kept once even
    /// with several resolve events), otherwise `null`.
    resolved: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketList {
    items: Vec<TicketListItem>,
    /// Tickets in the whole filtered scope (all pages).
    total: i64,
    /// Same scope without the status filter, every status (closed only when present).
    counts_by_status: Vec<StatusCountView>,
    /// Resolve events in the range for tickets in scope; only with a resolution range.
    resolution_events: Option<i64>,
    next_cursor: Option<String>,
    filters: AppliedTicketFilters,
    basis: ListBasis,
    as_of: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardColumn {
    status: TicketStatusView,
    /// Server count of the whole column scope (before paging).
    total: i64,
    items: Vec<TicketListItem>,
    /// Continue with `GET .../tickets?status=<code>&...&cursor=`.
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketBoard {
    columns: Vec<BoardColumn>,
    total: i64,
    resolution_events: Option<i64>,
    filters: AppliedTicketFilters,
    basis: ListBasis,
    as_of: String,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workflow_status code in database")]
struct UnknownStatus;

fn basis(f: &TicketQuery) -> ListBasis {
    ListBasis {
        unit: "ticket",
        dimensions: "current_ticket_attributes",
        resolved: f.resolved_filter().then_some("resolve_event_in_range"),
    }
}

/// Status changes `access` may request for a ticket in `status` (D01 lifecycle).
pub fn allowed_actions(
    access: &WorkspaceAccess,
    status: TicketStatus,
    blocking_open: i64,
) -> Vec<AllowedAction> {
    if !access.allows(Permission::TicketTransition) {
        return Vec::new();
    }
    let action = |to: TicketStatus, kind, requires, blocked_by| AllowedAction {
        to: to.into(),
        kind,
        requires,
        blocked_by,
    };
    match status {
        TicketStatus::Closed => Vec::new(),
        TicketStatus::Resolved => vec![action(
            TicketStatus::Open,
            ActionKind::Reopen,
            Requires::Reason,
            None,
        )],
        _ => {
            let mut out: Vec<AllowedAction> = crate::routes::dashboard::ACTIVE
                .into_iter()
                .filter(|s| *s != status)
                .map(|s| action(s, ActionKind::Move, Requires::None, None))
                .collect();
            if access.allows(Permission::TicketResolve) {
                out.push(action(
                    TicketStatus::Resolved,
                    ActionKind::Resolve,
                    Requires::Summary,
                    (blocking_open > 0).then_some("blocking_related_work"),
                ));
            }
            out
        }
    }
}

fn item(access: &WorkspaceAccess, r: ItemRow) -> Result<(TicketListItem, String), AppError> {
    let status = TicketStatus::from_code(&r.workflow_status)
        .ok_or_else(|| AppError::internal(UnknownStatus))?;
    let reference = |id: Option<Uuid>, code: Option<String>, label: Option<String>| {
        id.zip(label).map(|(id, label)| Ref { id, code, label })
    };
    let item = TicketListItem {
        id: r.id,
        number: r.number,
        display_number: format!("{}-{}", r.number_prefix, r.number),
        subject: r.subject,
        status: status.into(),
        cycle_no: r.cycle_no,
        version: r.version,
        priority: reference(r.priority_id, r.priority_code, r.priority_label),
        owner: reference(r.owner_id, None, r.owner_name),
        channel: reference(r.channel_id, None, r.channel_label),
        category: reference(r.category_id, None, r.category_label),
        received_at: r.received_at,
        updated_at: r.updated_at,
        age_seconds: r.age_seconds,
        related_work: RelatedWorkSummary {
            open: r.related_open,
            blocking_open: r.related_blocking_open,
        },
        allowed_actions: allowed_actions(access, status, r.related_blocking_open),
    };
    Ok((item, r.sort_key))
}

/// Starts the read-only snapshot shared by every query of one List/Board response.
async fn snapshot(state: &AppState) -> Result<sqlx::Transaction<'static, Postgres>, AppError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// 400 unless every filter value belongs to the workspace and every instant is a real
/// timestamp. Inactive members / catalog rows stay filterable (history).
pub(crate) async fn validate(
    conn: &mut PgConnection,
    ws: Uuid,
    f: &TicketQuery,
) -> Result<(), AppError> {
    let mut b = QueryBuilder::<Postgres>::new("SELECT true");
    if let Some(id) = f.owner.id() {
        b.push(" AND EXISTS (SELECT 1 FROM workspace_memberships m WHERE m.workspace_id = ")
            .push_bind(ws)
            .push(" AND m.user_id = ")
            .push_bind(id)
            .push(")");
    }
    for (table, dim) in [("intake_channels", f.channel), ("categories", f.category)] {
        if let Some(id) = dim.id() {
            b.push(format_args!(
                " AND EXISTS (SELECT 1 FROM {table} x WHERE x.workspace_id = "
            ))
            .push_bind(ws)
            .push(" AND x.id = ")
            .push_bind(id)
            .push(")");
        }
    }
    if let Some(ids) = &f.priority {
        b.push(" AND (SELECT count(*) FROM priorities x WHERE x.workspace_id = ")
            .push_bind(ws)
            .push(" AND x.id = ANY(")
            .push_bind(ids.clone())
            .push(format_args!(")) = {}", ids.len()));
    }
    for v in [
        &f.received_from,
        &f.received_before,
        &f.resolved_from,
        &f.resolved_before,
    ]
    .into_iter()
    .flatten()
    {
        b.push(" AND ")
            .push_bind(v.clone())
            .push("::timestamptz IS NOT NULL");
    }
    let ok: Option<bool> = match b.build_query_scalar().fetch_optional(&mut *conn).await {
        Ok(v) => v,
        // 22007/22008: not a valid timestamp. Anything else is a real error.
        Err(sqlx::Error::Database(db)) if db.code().is_some_and(|c| c.starts_with("22")) => {
            return Err(AppError::InvalidRequest);
        }
        Err(e) => return Err(e.into()),
    };
    if ok == Some(true) {
        Ok(())
    } else {
        Err(AppError::InvalidRequest)
    }
}

async fn count(conn: &mut PgConnection, ws: Uuid, f: &TicketQuery) -> Result<i64, AppError> {
    let mut b = QueryBuilder::<Postgres>::new("SELECT count(*) FROM tickets t");
    push_scope(&mut b, ws, f, true);
    Ok(b.build_query_scalar().fetch_one(&mut *conn).await?)
}

async fn counts_by_status(
    conn: &mut PgConnection,
    ws: Uuid,
    f: &TicketQuery,
) -> Result<Vec<StatusCountView>, AppError> {
    let mut b = QueryBuilder::<Postgres>::new("SELECT t.workflow_status, count(*) FROM tickets t");
    push_scope(&mut b, ws, f, false);
    b.push(" GROUP BY t.workflow_status");
    let rows: Vec<(String, i64)> = b.build_query_as().fetch_all(&mut *conn).await?;
    let by: BTreeMap<String, i64> = rows.into_iter().collect();
    Ok(TicketStatus::ALL
        .into_iter()
        .filter(|s| *s != TicketStatus::Closed || by.contains_key(s.code()))
        .map(|s| StatusCountView {
            status: s.into(),
            count: by.get(s.code()).copied().unwrap_or(0),
        })
        .collect())
}

async fn resolution_events(
    conn: &mut PgConnection,
    ws: Uuid,
    f: &TicketQuery,
) -> Result<Option<i64>, AppError> {
    if !f.resolved_filter() {
        return Ok(None);
    }
    let mut b = QueryBuilder::<Postgres>::new(
        "SELECT count(*) FROM ticket_resolution_events e JOIN tickets t \
           ON t.workspace_id = e.workspace_id AND t.id = e.ticket_id",
    );
    push_scope(&mut b, ws, f, true);
    b.push(" AND e.action = 'resolve'");
    push_event_range(&mut b, f);
    Ok(Some(b.build_query_scalar().fetch_one(&mut *conn).await?))
}

/// One page in sort order, keyset after `after`; returns the items and the next cursor.
async fn page(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    f: &TicketQuery,
    after: Option<(String, Uuid)>,
    limit: i64,
) -> Result<(Vec<TicketListItem>, Option<String>), AppError> {
    let ws = access.workspace_id;
    let mut b = QueryBuilder::<Postgres>::new(ITEM_COLUMNS);
    b.push(format_args!(", {} AS sort_key", f.sort.key_text()));
    b.push(ITEM_FROM);
    push_scope(&mut b, ws, f, true);
    if let Some((key, id)) = after {
        b.push(format_args!(
            " AND ({}, t.id) {} (",
            f.sort.column(),
            f.order.cmp()
        ))
        .push_bind(key)
        .push(format_args!("{}, ", f.sort.key_cast()))
        .push_bind(id)
        .push(")");
    }
    b.push(format_args!(
        " ORDER BY {col} {dir}, t.id {dir} LIMIT ",
        col = f.sort.column(),
        dir = f.order.sql()
    ))
    .push_bind(limit + 1);
    let mut rows: Vec<ItemRow> = match b.build_query_as().fetch_all(&mut *conn).await {
        Ok(r) => r,
        // A tampered cursor key that is not a timestamp / number.
        Err(sqlx::Error::Database(db)) if db.code().is_some_and(|c| c.starts_with("22")) => {
            return Err(AppError::InvalidRequest);
        }
        Err(e) => return Err(e.into()),
    };
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let mut items = Vec::with_capacity(rows.len());
    let mut last = None;
    for r in rows {
        let (it, key) = item(access, r)?;
        last = Some((key, it.id));
        items.push(it);
    }
    let next = match (more, last) {
        (true, Some((key, id))) => Some(encode_cursor(&f.fingerprint(ws), key, id)?),
        _ => None,
    };
    Ok((items, next))
}

async fn as_of(conn: &mut PgConnection) -> Result<String, AppError> {
    Ok(sqlx::query_scalar(concat!("SELECT ", utc!("now()")))
        .fetch_one(&mut *conn)
        .await?)
}

// ---------- handlers ----------

pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<TicketList>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let f = TicketQuery::parse(&q)?;
    let limit = parse_limit(q.limit.as_deref(), DEFAULT_LIMIT, MAX_LIMIT)?;
    let ws = access.workspace_id;
    let after = match non_empty(q.cursor.as_deref()) {
        Some(c) => Some(decode_cursor(c, &f.fingerprint(ws))?),
        None => None,
    };

    let mut tx = snapshot(&state).await?;
    validate(&mut tx, ws, &f).await?;
    let total = count(&mut tx, ws, &f).await?;
    let counts = counts_by_status(&mut tx, ws, &f).await?;
    let events = resolution_events(&mut tx, ws, &f).await?;
    let (items, next_cursor) = page(&mut tx, &access, &f, after, limit).await?;
    let as_of = as_of(&mut tx).await?;
    tx.commit().await?;

    Ok(Json(TicketList {
        items,
        total,
        counts_by_status: counts,
        resolution_events: events,
        next_cursor,
        basis: basis(&f),
        filters: f.applied(),
        as_of,
    }))
}

pub async fn board(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<TicketBoard>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    if q.cursor.is_some() {
        // Columns continue through the List endpoint with their own cursor.
        return Err(AppError::InvalidRequest);
    }
    let f = TicketQuery::parse(&q)?;
    let limit = parse_limit(q.limit.as_deref(), BOARD_DEFAULT_LIMIT, BOARD_MAX_LIMIT)?;
    let ws = access.workspace_id;

    let mut tx = snapshot(&state).await?;
    validate(&mut tx, ws, &f).await?;
    let counts = counts_by_status(&mut tx, ws, &f).await?;
    let selected: Vec<TicketStatus> = match &f.status {
        Some(v) => TicketStatus::ALL
            .into_iter()
            .filter(|s| v.contains(s))
            .collect(),
        None => counts
            .iter()
            .filter_map(|c| TicketStatus::from_code(c.status.code))
            .collect(),
    };
    let mut columns = Vec::with_capacity(selected.len());
    let mut total = 0;
    for s in selected {
        let cf = f.with_status(s);
        let n = count(&mut tx, ws, &cf).await?;
        let (items, next_cursor) = page(&mut tx, &access, &cf, None, limit).await?;
        total += n;
        columns.push(BoardColumn {
            status: s.into(),
            total: n,
            items,
            next_cursor,
        });
    }
    let events = resolution_events(&mut tx, ws, &f).await?;
    let as_of = as_of(&mut tx).await?;
    tx.commit().await?;

    Ok(Json(TicketBoard {
        columns,
        total,
        resolution_events: events,
        basis: basis(&f),
        filters: f.applied(),
        as_of,
    }))
}

// ---------- mutations ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransitionBody {
    to: String,
    /// The ticket version the client saw.
    version: i32,
    /// Required to resolve; refused otherwise.
    summary: Option<String>,
    /// Required to reopen; refused otherwise.
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentBody {
    /// New PIC (active member of this workspace), or `null` to remove the PIC.
    owner_user_id: Option<Uuid>,
    version: i32,
    /// Required when replacing or removing another PIC (Lead CS).
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationResult {
    ticket: TicketListItem,
    audit_event_id: Uuid,
}

pub(crate) fn note(raw: Option<String>, max: usize) -> Result<Option<String>, AppError> {
    match raw {
        None => Ok(None),
        Some(s) => {
            let t = s.trim();
            if t.is_empty() || t.chars().count() > max {
                Err(AppError::InvalidRequest)
            } else {
                Ok(Some(t.to_owned()))
            }
        }
    }
}

pub(crate) fn body<T>(b: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    b.map(|Json(v)| v).map_err(|e| {
        tracing::info!(error.kind = "invalid_body", status = %e.status(), "request body refused");
        AppError::InvalidRequest
    })
}

pub(crate) fn correlation(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 100)
        .map(str::to_owned)
}

#[derive(sqlx::FromRow)]
struct LockedTicket {
    workflow_status: String,
    cycle_no: i32,
    version: i32,
    owner_user_id: Option<Uuid>,
}

async fn lock(
    conn: &mut PgConnection,
    ws: Uuid,
    id: Uuid,
) -> Result<(LockedTicket, TicketStatus), AppError> {
    let t: LockedTicket = sqlx::query_as(
        "SELECT workflow_status, cycle_no, version, owner_user_id FROM tickets \
         WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::NotFound)?;
    let status = TicketStatus::from_code(&t.workflow_status)
        .ok_or_else(|| AppError::internal(UnknownStatus))?;
    Ok((t, status))
}

pub(crate) async fn audit(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    action: &str,
    ticket: Uuid,
    change: serde_json::Value,
    correlation_id: Option<String>,
) -> Result<Uuid, AppError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_events (workspace_id, actor_id, action, entity_type, entity_id, \
           safe_change, outcome, correlation_id) \
         VALUES ($1, $2, $3, 'ticket', $4, $5, 'success', $6) RETURNING id",
    )
    .bind(access.workspace_id)
    .bind(access.user_id)
    .bind(action)
    .bind(ticket)
    .bind(change)
    .bind(correlation_id)
    .fetch_one(&mut *conn)
    .await?)
}

pub(crate) async fn reload(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    id: Uuid,
) -> Result<TicketListItem, AppError> {
    let mut b = QueryBuilder::<Postgres>::new(ITEM_COLUMNS);
    b.push(", ''::text AS sort_key");
    b.push(ITEM_FROM);
    b.push(" WHERE t.workspace_id = ")
        .push_bind(access.workspace_id)
        .push(" AND t.id = ")
        .push_bind(id);
    let row: ItemRow = b.build_query_as().fetch_one(&mut *conn).await?;
    Ok(item(access, row)?.0)
}

pub async fn transition(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    payload: Result<Json<TransitionBody>, JsonRejection>,
) -> Result<Json<MutationResult>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    access.require(Permission::TicketRead)?;
    let b = body(payload)?;
    let to = TicketStatus::from_code(&b.to).ok_or(AppError::InvalidRequest)?;
    if b.version < 1 {
        return Err(AppError::InvalidRequest);
    }
    let summary = note(b.summary, MAX_NOTE_CHARS)?;
    let reason = note(b.reason, MAX_NOTE_CHARS)?;
    access.require(Permission::TicketTransition)?;

    let mut tx = state.pool.begin().await?;
    let (t, from) = lock(&mut tx, access.workspace_id, ticket_id).await?;
    // The archive refuses first; then a stale client gets 409 (reload) before any rule
    // about the requested move, so a replayed request never reads as a rule error.
    if from == TicketStatus::Closed {
        return Err(AppError::RuleViolation);
    }
    if t.version != b.version {
        return Err(AppError::VersionConflict);
    }
    let kind = match (from, to) {
        // Closed is an archive and never a target (close final is held).
        (TicketStatus::Closed, _) | (_, TicketStatus::Closed) => {
            return Err(AppError::RuleViolation);
        }
        (a, b) if a == b => return Err(AppError::RuleViolation),
        (TicketStatus::Resolved, TicketStatus::Open) => ActionKind::Reopen,
        (TicketStatus::Resolved, _) => return Err(AppError::RuleViolation),
        (_, TicketStatus::Resolved) => ActionKind::Resolve,
        _ => ActionKind::Move,
    };
    if kind == ActionKind::Resolve {
        access.require(Permission::TicketResolve)?;
    }
    let (cycle, event) = match kind {
        ActionKind::Move => {
            if summary.is_some() || reason.is_some() {
                return Err(AppError::InvalidRequest);
            }
            (t.cycle_no, None)
        }
        ActionKind::Resolve => match (summary, reason) {
            (Some(text), None) => (t.cycle_no, Some(("resolve", text))),
            _ => return Err(AppError::InvalidRequest),
        },
        ActionKind::Reopen => match (reason, summary) {
            (Some(text), None) => (t.cycle_no + 1, Some(("reopen", text))),
            _ => return Err(AppError::InvalidRequest),
        },
    };
    // The event must exist before the status update (DB lifecycle guard checks it).
    let event_id: Option<Uuid> = match &event {
        Some((action, text)) => Some(
            sqlx::query_scalar(
                "INSERT INTO ticket_resolution_events \
                   (workspace_id, ticket_id, cycle_no, action, summary, actor_id) \
                 VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(access.workspace_id)
            .bind(ticket_id)
            .bind(cycle)
            .bind(*action)
            .bind(text)
            .bind(access.user_id)
            .fetch_one(&mut *tx)
            .await?,
        ),
        None => None,
    };
    sqlx::query(
        "UPDATE tickets SET workflow_status = $3, cycle_no = $4, version = $5 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(access.workspace_id)
    .bind(ticket_id)
    .bind(to.code())
    .bind(cycle)
    .bind(b.version + 1)
    .execute(&mut *tx)
    .await?;
    let audit_id = audit(
        &mut tx,
        &access,
        "ticket.transition",
        ticket_id,
        serde_json::json!({
            "kind": kind,
            "from": from.code(),
            "to": to.code(),
            "cycleNo": cycle,
            "version": b.version + 1,
            "resolutionEventId": event_id,
        }),
        correlation(&headers),
    )
    .await?;
    let ticket = reload(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    tracing::info!(
        workspace_id = %access.workspace_id,
        ticket_id = %ticket_id,
        from = from.code(),
        to = to.code(),
        "ticket transition"
    );
    Ok(Json(MutationResult {
        ticket,
        audit_event_id: audit_id,
    }))
}

pub async fn assign(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    payload: Result<Json<AssignmentBody>, JsonRejection>,
) -> Result<Json<MutationResult>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    access.require(Permission::TicketRead)?;
    let b = body(payload)?;
    if b.version < 1 {
        return Err(AppError::InvalidRequest);
    }
    let reason = note(b.reason, MAX_ASSIGN_REASON_CHARS)?;

    let mut tx = state.pool.begin().await?;
    let (t, status) = lock(&mut tx, access.workspace_id, ticket_id).await?;
    let kind = authz::check_assignment(&access, t.owner_user_id, b.owner_user_id)?;
    if matches!(status, TicketStatus::Closed) {
        return Err(AppError::RuleViolation);
    }
    if t.version != b.version {
        return Err(AppError::VersionConflict);
    }
    match kind {
        AssignmentKind::Unchanged => return Err(AppError::RuleViolation),
        AssignmentKind::TakeForSelf if reason.is_some() => return Err(AppError::InvalidRequest),
        AssignmentKind::Reassign if reason.is_none() => return Err(AppError::InvalidRequest),
        _ => {}
    }
    sqlx::query(
        "UPDATE tickets SET owner_user_id = $3, version = $4 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(access.workspace_id)
    .bind(ticket_id)
    .bind(b.owner_user_id)
    .bind(b.version + 1)
    .execute(&mut *tx)
    .await?;
    let audit_id = audit(
        &mut tx,
        &access,
        "ticket.assign",
        ticket_id,
        serde_json::json!({
            "kind": match kind {
                AssignmentKind::TakeForSelf => "take_for_self",
                _ => "reassign",
            },
            "from": t.owner_user_id,
            "to": b.owner_user_id,
            "version": b.version + 1,
            "reason": reason,
        }),
        correlation(&headers),
    )
    .await?;
    let ticket = reload(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    Ok(Json(MutationResult {
        ticket,
        audit_event_id: audit_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(pairs: &[(&str, &str)]) -> Result<TicketQuery, AppError> {
        let qs: String = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let q: ListQuery = match serde_urlencoded_from(&qs) {
            Some(q) => q,
            None => return Err(AppError::InvalidRequest),
        };
        TicketQuery::parse(&q)
    }

    fn serde_urlencoded_from(qs: &str) -> Option<ListQuery> {
        let uri: axum::http::Uri = format!("/x?{qs}").parse().ok()?;
        Query::<ListQuery>::try_from_uri(&uri).ok().map(|q| q.0)
    }

    #[test]
    fn defaults_and_drilldown_params_parse() {
        let f = parse(&[]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            (f.sort, f.order, f.status.clone()),
            (SortKey::UpdatedAt, Order::Desc, None)
        );
        let id = "9d2699f3-0806-469c-966b-de8db28f108f";
        let f = parse(&[
            ("status", "new,open,new"),
            ("owner", "unassigned"),
            ("priority", &format!("{id},{id}")),
            ("receivedFrom", "2026-11-01T00:00:00Z"),
        ])
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(f.status.map(|v| v.len()), Some(2));
        assert_eq!(f.owner, Dim::Unset);
        assert_eq!(f.priority.map(|v| v.len()), Some(1));
    }

    #[test]
    fn invalid_values_are_refused() {
        for bad in [
            ("status", "done"),
            ("owner", "NOT-A-UUID"),
            ("priority", "none,9d2699f3-0806-469c-966b-de8db28f108f"),
            ("receivedFrom", "yesterday"),
            ("sort", "subject"),
            ("order", "up"),
        ] {
            assert!(parse(&[bad]).is_err(), "{bad:?}");
        }
        let long = "x".repeat(MAX_QUERY_CHARS + 1);
        assert!(parse(&[("q", &long)]).is_err());
    }

    #[test]
    fn search_number_and_like_escape() {
        let f = parse(&[("q", "DUMA-12")]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(f.number(), Some(12));
        assert_eq!(f.number_prefix().as_deref(), Some("DUMA"));
        let f = parse(&[("q", "duma-12")]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(f.number_prefix().as_deref(), Some("DUMA"));
        let f = parse(&[("q", "%2312")]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!((f.number(), f.number_prefix()), (Some(12), None));
        let f = parse(&[("q", "50%25_off")]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(f.like().as_deref(), Some("%50\\%\\_off%"));
        assert_eq!(f.number(), None);
    }

    #[test]
    fn cursor_is_bound_to_scope_filters_and_sort() {
        let ws = Uuid::from_u128(1);
        let f = parse(&[("owner", "unassigned")]).unwrap_or_else(|e| panic!("{e:?}"));
        let c = encode_cursor(&f.fingerprint(ws), "2026-11-01T00:00:00.000000Z".into(), ws)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(decode_cursor(&c, &f.fingerprint(ws)).is_ok());
        assert!(decode_cursor(&c, &f.fingerprint(Uuid::from_u128(2))).is_err());
        let other =
            parse(&[("owner", "unassigned"), ("order", "asc")]).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(decode_cursor(&c, &other.fingerprint(ws)).is_err());
        assert!(decode_cursor("not*base64", &f.fingerprint(ws)).is_err());
    }
}
