//! D08 ticket detail. One scoped read returns everything the detail screen shows (header,
//! properties with labels, related work, bound attachment metadata, resolution history);
//! activity is a separate, paginated audit read. Property edits and related-work changes
//! are guarded by `version` (409 on a stale client) and refused on the closed archive.
//!
//! Jira status never completes related work or resolves a ticket: completion is only the
//! explicit `completion` call below, with a summary and evidence from a person.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::domain::TicketStatus;
use crate::error::AppError;
use crate::routes::intake::{
    MAX_CHRONOLOGY_CHARS, MAX_SUBJECT_CHARS, idempotency_key, require_active, required_text,
};
use crate::routes::ticket_list::{TicketListItem, audit, body, correlation, reload};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

macro_rules! utc {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            " AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        )
    };
}

const MAX_LONG_TEXT_CHARS: usize = 20_000;
const MAX_BRANCH_REF_CHARS: usize = 100;
const MAX_RELATED_TITLE_CHARS: usize = 300;
const MAX_RELATED_TEXT_CHARS: usize = 5000;
const ACTIVITY_DEFAULT_LIMIT: i64 = 30;
const ACTIVITY_MAX_LIMIT: i64 = 100;
const MAX_CURSOR_CHARS: usize = 100;

// ---------- response ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Labeled {
    id: Uuid,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketProperties {
    subject: String,
    chronology: String,
    received_at: String,
    intake_channel: Option<Labeled>,
    priority: Option<Labeled>,
    category: Option<Labeled>,
    product: Option<Labeled>,
    receiving_unit: Option<Labeled>,
    region: Option<Labeled>,
    /// Dummy CIF directory only; not editable here.
    merchant: Option<Labeled>,
    transaction_activity: Option<String>,
    cause: Option<String>,
    foreign_branch_reference: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraLink {
    issue_key: String,
    /// Status reported by the (simulated) provider; informational only.
    external_status: Option<String>,
    safe_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedWorkView {
    id: Uuid,
    title: String,
    safe_description: Option<String>,
    blocking: bool,
    /// `open` | `completed` — local completion, independent of Jira.
    completion_state: String,
    completion_summary: Option<String>,
    completion_evidence: Option<String>,
    requested_by: Option<Labeled>,
    completed_by: Option<Labeled>,
    completed_at: Option<String>,
    jira: Option<JiraLink>,
    version: i32,
    created_at: String,
}

/// Metadata of attachments bound to the ticket (bound implies a clean scan).
#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    id: Uuid,
    file_name: String,
    inspected_mime: Option<String>,
    size_bytes: i64,
    scan_status: String,
    visibility: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionEventView {
    id: Uuid,
    /// `resolve` | `reopen`.
    action: String,
    cycle_no: i32,
    summary: String,
    actor: Option<Labeled>,
    at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailCapabilities {
    /// Closed archive: every write is refused.
    read_only: bool,
    edit_properties: bool,
    manage_related_work: bool,
    /// Operational follow-up notes (not team chat).
    add_note: bool,
    /// Upload is offered; the server still refuses it when storage/scanner are unavailable.
    upload_attachment: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketDetail {
    ticket: TicketListItem,
    properties: TicketProperties,
    can: DetailCapabilities,
    related_work: Vec<RelatedWorkView>,
    attachments: Vec<AttachmentMeta>,
    resolutions: Vec<ResolutionEventView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailMutation {
    detail: TicketDetail,
    /// `null` when the request changed nothing.
    audit_event_id: Option<Uuid>,
}

// ---------- reads ----------

#[derive(sqlx::FromRow)]
struct PropRow {
    workflow_status: String,
    subject: String,
    chronology: String,
    received_at: String,
    transaction_activity: Option<String>,
    cause: Option<String>,
    foreign_branch_reference: Option<String>,
    ch_id: Option<Uuid>,
    ch_label: Option<String>,
    pr_id: Option<Uuid>,
    pr_label: Option<String>,
    ca_id: Option<Uuid>,
    ca_label: Option<String>,
    po_id: Option<Uuid>,
    po_label: Option<String>,
    ru_id: Option<Uuid>,
    ru_label: Option<String>,
    rg_id: Option<Uuid>,
    rg_label: Option<String>,
    me_id: Option<Uuid>,
    me_label: Option<String>,
}

fn labeled(id: Option<Uuid>, label: Option<String>) -> Option<Labeled> {
    Some(Labeled {
        id: id?,
        label: label.unwrap_or_else(|| "Tidak dikenal".to_owned()),
    })
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workflow_status code in database")]
struct UnknownStatus;

const PROPS_SQL: &str = concat!(
    "SELECT t.workflow_status, t.subject, t.chronology, ",
    utc!("t.received_at"),
    " AS received_at, t.transaction_activity, t.cause, t.foreign_branch_reference, \
     ch.id AS ch_id, ch.label AS ch_label, pr.id AS pr_id, pr.label AS pr_label, \
     ca.id AS ca_id, ca.label AS ca_label, po.id AS po_id, po.label AS po_label, \
     ru.id AS ru_id, ru.label AS ru_label, rg.id AS rg_id, rg.label AS rg_label, \
     me.id AS me_id, me.display_name AS me_label \
     FROM tickets t \
     LEFT JOIN intake_channels ch ON ch.workspace_id = t.workspace_id AND ch.id = t.intake_channel_id \
     LEFT JOIN priorities pr ON pr.workspace_id = t.workspace_id AND pr.id = t.priority_id \
     LEFT JOIN categories ca ON ca.workspace_id = t.workspace_id AND ca.id = t.category_id \
     LEFT JOIN products po ON po.workspace_id = t.workspace_id AND po.id = t.product_id \
     LEFT JOIN receiving_units ru ON ru.workspace_id = t.workspace_id AND ru.id = t.receiving_unit_id \
     LEFT JOIN regions rg ON rg.workspace_id = t.workspace_id AND rg.id = t.region_id \
     LEFT JOIN merchants me ON me.workspace_id = t.workspace_id AND me.id = t.merchant_id \
     WHERE t.workspace_id = $1 AND t.id = $2"
);

#[derive(sqlx::FromRow)]
struct RelatedRow {
    id: Uuid,
    title: String,
    safe_description: Option<String>,
    blocking: bool,
    completion_state: String,
    completion_summary: Option<String>,
    completion_evidence: Option<String>,
    rq_id: Option<Uuid>,
    rq_name: Option<String>,
    cb_id: Option<Uuid>,
    cb_name: Option<String>,
    completed_at: Option<String>,
    issue_key: Option<String>,
    external_status: Option<String>,
    safe_url: Option<String>,
    version: i32,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct ResolutionRow {
    id: Uuid,
    action: String,
    cycle_no: i32,
    summary: String,
    actor_id: Option<Uuid>,
    actor_name: Option<String>,
    at: String,
}

/// Builds the detail inside `conn` (a transaction for mutations, so the answer matches the
/// committed write). 404 when the ticket is not in this workspace.
pub(crate) async fn load(
    conn: &mut PgConnection,
    access: &WorkspaceAccess,
    ticket_id: Uuid,
) -> Result<TicketDetail, AppError> {
    let ws = access.workspace_id;
    let p: PropRow = sqlx::query_as(PROPS_SQL)
        .bind(ws)
        .bind(ticket_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AppError::NotFound)?;
    let status = TicketStatus::from_code(&p.workflow_status)
        .ok_or_else(|| AppError::internal(UnknownStatus))?;
    let ticket = reload(conn, access, ticket_id).await?;

    let related: Vec<RelatedRow> = sqlx::query_as(concat!(
        "SELECT r.id, r.title, r.safe_description, r.blocking, r.completion_state, \
         r.completion_summary, r.completion_evidence, \
         uq.id AS rq_id, uq.display_name AS rq_name, uc.id AS cb_id, uc.display_name AS cb_name, ",
        utc!("r.completed_at"),
        " AS completed_at, j.issue_key, j.external_status, j.safe_url, r.version, ",
        utc!("r.created_at"),
        " AS created_at FROM related_work r \
         LEFT JOIN users uq ON uq.id = r.requested_by \
         LEFT JOIN users uc ON uc.id = r.completed_by \
         LEFT JOIN jira_issues j ON j.workspace_id = r.workspace_id AND j.id = r.jira_issue_id \
         WHERE r.workspace_id = $1 AND r.ticket_id = $2 \
         ORDER BY r.created_at, r.id"
    ))
    .bind(ws)
    .bind(ticket_id)
    .fetch_all(&mut *conn)
    .await?;

    let can_read_attachments = access.allows(Permission::AttachmentRead);
    let attachments: Vec<AttachmentMeta> = if can_read_attachments {
        sqlx::query_as(concat!(
            "SELECT id, file_name, inspected_mime, size_bytes, scan_status, visibility, ",
            utc!("created_at"),
            " AS created_at FROM attachments WHERE workspace_id = $1 AND ticket_id = $2 \
             ORDER BY created_at, id"
        ))
        .bind(ws)
        .bind(ticket_id)
        .fetch_all(&mut *conn)
        .await?
    } else {
        Vec::new()
    };

    let resolutions: Vec<ResolutionRow> = sqlx::query_as(concat!(
        "SELECT e.id, e.action, e.cycle_no, e.summary, e.actor_id, u.display_name AS actor_name, ",
        utc!("e.occurred_at"),
        " AS at FROM ticket_resolution_events e LEFT JOIN users u ON u.id = e.actor_id \
         WHERE e.workspace_id = $1 AND e.ticket_id = $2 ORDER BY e.occurred_at DESC, e.id DESC"
    ))
    .bind(ws)
    .bind(ticket_id)
    .fetch_all(&mut *conn)
    .await?;

    let read_only = status == TicketStatus::Closed;
    Ok(TicketDetail {
        ticket,
        properties: TicketProperties {
            subject: p.subject,
            chronology: p.chronology,
            received_at: p.received_at,
            intake_channel: labeled(p.ch_id, p.ch_label),
            priority: labeled(p.pr_id, p.pr_label),
            category: labeled(p.ca_id, p.ca_label),
            product: labeled(p.po_id, p.po_label),
            receiving_unit: labeled(p.ru_id, p.ru_label),
            region: labeled(p.rg_id, p.rg_label),
            merchant: labeled(p.me_id, p.me_label),
            transaction_activity: p.transaction_activity,
            cause: p.cause,
            foreign_branch_reference: p.foreign_branch_reference,
        },
        can: DetailCapabilities {
            read_only,
            edit_properties: !read_only && access.allows(Permission::TicketUpdate),
            manage_related_work: !read_only && access.allows(Permission::RelatedWorkManage),
            add_note: !read_only && access.allows(Permission::TicketUpdate),
            upload_attachment: !read_only && access.allows(Permission::AttachmentUpload),
        },
        related_work: related
            .into_iter()
            .map(|r| RelatedWorkView {
                id: r.id,
                title: r.title,
                safe_description: r.safe_description,
                blocking: r.blocking,
                completion_state: r.completion_state,
                completion_summary: r.completion_summary,
                completion_evidence: r.completion_evidence,
                requested_by: labeled(r.rq_id, r.rq_name),
                completed_by: labeled(r.cb_id, r.cb_name),
                completed_at: r.completed_at,
                jira: r.issue_key.map(|issue_key| JiraLink {
                    issue_key,
                    external_status: r.external_status,
                    safe_url: r.safe_url,
                }),
                version: r.version,
                created_at: r.created_at,
            })
            .collect(),
        attachments,
        resolutions: resolutions
            .into_iter()
            .map(|r| ResolutionEventView {
                id: r.id,
                action: r.action,
                cycle_no: r.cycle_no,
                summary: r.summary,
                actor: labeled(r.actor_id, r.actor_name),
                at: r.at,
            })
            .collect(),
    })
}

pub async fn detail(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
) -> Result<Json<TicketDetail>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    // One snapshot so header, properties and lists agree with each other.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let d = load(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    Ok(Json(d))
}

// ---------- activity ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    id: Uuid,
    at: String,
    /// Audit action code, e.g. `ticket.update`.
    action: String,
    /// Indonesian sentence built by the server from safe audit fields only.
    label: String,
    actor: Option<Labeled>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPage {
    items: Vec<ActivityItem>,
    /// Opaque; pass back as `cursor` for older items. `null` at the end.
    next_cursor: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ActivityRow {
    id: Uuid,
    at: String,
    action: String,
    safe_change: serde_json::Value,
    actor_id: Option<Uuid>,
    actor_name: Option<String>,
}

const FIELD_LABELS: &[(&str, &str)] = &[
    ("subject", "subjek"),
    ("chronology", "kronologi"),
    ("priorityId", "prioritas"),
    ("categoryId", "kategori"),
    ("productId", "produk"),
    ("receivingUnitId", "unit penerima"),
    ("regionId", "wilayah"),
    ("transactionActivity", "aktivitas transaksi"),
    ("cause", "penyebab"),
    ("foreignBranchReference", "referensi cabang luar"),
];

fn status_label(code: Option<&str>) -> &'static str {
    code.and_then(TicketStatus::from_code)
        .map(|s| s.label())
        .unwrap_or("?")
}

/// Human sentence for one audit row. Only reads keys this API writes; never echoes free text.
pub(crate) fn activity_label(action: &str, c: &serde_json::Value) -> String {
    let s = |k: &str| c.get(k).and_then(|v| v.as_str());
    match action {
        "ticket.create" => "Membuat tiket".to_owned(),
        "ticket.transition" => format!(
            "Mengubah status {} → {}",
            status_label(s("from")),
            status_label(s("to"))
        ),
        "ticket.assign" => match (s("kind"), c.get("to").is_some_and(|v| v.is_null())) {
            (Some("take_for_self"), _) => "Mengambil tiket sebagai PIC".to_owned(),
            (_, true) => "Melepas PIC".to_owned(),
            _ => "Mengubah PIC".to_owned(),
        },
        "ticket.update" => {
            let names: Vec<&str> = c
                .get("fields")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.as_str())
                        .filter_map(|f| FIELD_LABELS.iter().find(|(k, _)| *k == f).map(|(_, l)| *l))
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                "Mengubah properti".to_owned()
            } else {
                format!("Mengubah {}", names.join(", "))
            }
        }
        "related_work.create" => {
            if c.get("blocking").and_then(|v| v.as_bool()) == Some(true) {
                "Menambah pekerjaan terkait (memblokir)".to_owned()
            } else {
                "Menambah pekerjaan terkait".to_owned()
            }
        }
        "related_work.complete" => "Menandai pekerjaan terkait selesai".to_owned(),
        "ticket.note" => "Menambah catatan tindak lanjut".to_owned(),
        "attachment.upload" => "Menambah lampiran (lolos pemindaian)".to_owned(),
        _ => "Aktivitas lain".to_owned(),
    }
}

/// Cursor = `<occurred_at RFC3339 UTC>~<audit id>` (not signed; it only narrows a read the
/// caller is already allowed to make).
fn parse_cursor(raw: &str) -> Result<(String, Uuid), AppError> {
    if raw.len() > MAX_CURSOR_CHARS {
        return Err(AppError::InvalidRequest);
    }
    let (ts, id) = raw.split_once('~').ok_or(AppError::InvalidRequest)?;
    let ts = crate::routes::ticket_list::instant(Some(ts))?.ok_or(AppError::InvalidRequest)?;
    let id = Uuid::parse_str(id).map_err(|_| AppError::InvalidRequest)?;
    Ok((ts, id))
}

pub async fn activity(
    State(state): State<AppState>,
    s: Session,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    q: Result<Query<ActivityQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<ActivityPage>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    access.require(Permission::TicketRead)?;
    let ticket_id = parse_id(&raw_ticket)?;
    let Query(q) = q.map_err(|_| AppError::InvalidRequest)?;
    let limit = match q.limit.as_deref() {
        None | Some("") => ACTIVITY_DEFAULT_LIMIT,
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=ACTIVITY_MAX_LIMIT).contains(n))
            .ok_or(AppError::InvalidRequest)?,
    };
    let cursor = q
        .cursor
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(parse_cursor)
        .transpose()?;

    let mut conn = state.pool.acquire().await?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tickets WHERE workspace_id = $1 AND id = $2")
            .bind(access.workspace_id)
            .bind(ticket_id)
            .fetch_optional(&mut *conn)
            .await?;
    if found.is_none() {
        return Err(AppError::NotFound);
    }
    let mut b = QueryBuilder::<Postgres>::new(concat!(
        "SELECT a.id, ",
        utc!("a.occurred_at"),
        " AS at, a.action, a.safe_change, a.actor_id, u.display_name AS actor_name \
         FROM audit_events a LEFT JOIN users u ON u.id = a.actor_id \
         WHERE a.outcome = 'success' AND a.entity_type = 'ticket' AND a.workspace_id = "
    ));
    b.push_bind(access.workspace_id)
        .push(" AND a.entity_id = ")
        .push_bind(ticket_id);
    if let Some((ts, id)) = &cursor {
        b.push(" AND (a.occurred_at, a.id) < (")
            .push_bind(ts.clone())
            .push("::timestamptz, ")
            .push_bind(*id)
            .push(")");
    }
    b.push(" ORDER BY a.occurred_at DESC, a.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut rows: Vec<ActivityRow> = b.build_query_as().fetch_all(&mut *conn).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = if more {
        rows.last().map(|r| format!("{}~{}", r.at, r.id))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|r| ActivityItem {
            label: activity_label(&r.action, &r.safe_change),
            id: r.id,
            at: r.at,
            action: r.action,
            actor: labeled(r.actor_id, r.actor_name),
        })
        .collect();
    Ok(Json(ActivityPage { items, next_cursor }))
}

// ---------- property edit ----------

/// `Some(None)` = explicit `null` (clear), `None` = field absent (unchanged).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertiesBody {
    version: i32,
    subject: Option<String>,
    chronology: Option<String>,
    #[serde(default, deserialize_with = "present")]
    priority_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    category_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    product_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    receiving_unit_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    region_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    transaction_activity: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    cause: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    foreign_branch_reference: Option<Option<String>>,
}

#[derive(sqlx::FromRow)]
struct CurrentProps {
    workflow_status: String,
    version: i32,
    subject: String,
    chronology: String,
    priority_id: Option<Uuid>,
    category_id: Option<Uuid>,
    product_id: Option<Uuid>,
    receiving_unit_id: Option<Uuid>,
    region_id: Option<Uuid>,
    transaction_activity: Option<String>,
    cause: Option<String>,
    foreign_branch_reference: Option<String>,
}

enum Change {
    Text(&'static str, Option<String>),
    Id(&'static str, Option<Uuid>),
}

fn nullable_text(
    v: Option<String>,
    max: usize,
    field: &'static str,
) -> Result<Option<String>, AppError> {
    v.map(|s| required_text(&s, max, field)).transpose()
}

/// Validated changes against the current row; unchanged values are dropped.
async fn changes(
    conn: &mut PgConnection,
    ws: Uuid,
    cur: &CurrentProps,
    b: PropertiesBody,
) -> Result<Vec<(&'static str, Change)>, AppError> {
    let mut out = Vec::new();
    if let Some(v) = b.subject {
        let v = required_text(&v, MAX_SUBJECT_CHARS, "subject")?;
        if v != cur.subject {
            out.push(("subject", Change::Text("subject", Some(v))));
        }
    }
    if let Some(v) = b.chronology {
        let v = required_text(&v, MAX_CHRONOLOGY_CHARS, "chronology")?;
        if v != cur.chronology {
            out.push(("chronology", Change::Text("chronology", Some(v))));
        }
    }
    let ids: [IdField; 5] = [
        (
            "priorityId",
            "priority_id",
            "priorities",
            b.priority_id,
            cur.priority_id,
        ),
        (
            "categoryId",
            "category_id",
            "categories",
            b.category_id,
            cur.category_id,
        ),
        (
            "productId",
            "product_id",
            "products",
            b.product_id,
            cur.product_id,
        ),
        (
            "receivingUnitId",
            "receiving_unit_id",
            "receiving_units",
            b.receiving_unit_id,
            cur.receiving_unit_id,
        ),
        (
            "regionId",
            "region_id",
            "regions",
            b.region_id,
            cur.region_id,
        ),
    ];
    for (field, column, table, new, old) in ids {
        if let Some(new) = new
            && new != old
        {
            // A new value must be active in this workspace; clearing is always allowed.
            require_active(conn, table, ws, new, field).await?;
            out.push((field, Change::Id(column, new)));
        }
    }
    let texts: [TextField<'_>; 3] = [
        (
            "transactionActivity",
            "transaction_activity",
            MAX_LONG_TEXT_CHARS,
            b.transaction_activity,
            &cur.transaction_activity,
        ),
        ("cause", "cause", MAX_LONG_TEXT_CHARS, b.cause, &cur.cause),
        (
            "foreignBranchReference",
            "foreign_branch_reference",
            MAX_BRANCH_REF_CHARS,
            b.foreign_branch_reference,
            &cur.foreign_branch_reference,
        ),
    ];
    for (field, column, max, new, old) in texts {
        if let Some(new) = new {
            let new = nullable_text(new, max, field)?;
            if &new != old {
                out.push((field, Change::Text(column, new)));
            }
        }
    }
    Ok(out)
}

pub async fn update_properties(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    payload: Result<Json<PropertiesBody>, JsonRejection>,
) -> Result<Json<DetailMutation>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    access.require(Permission::TicketRead)?;
    let b = body(payload)?;
    if b.version < 1 {
        return Err(AppError::InvalidRequest);
    }
    access.require(Permission::TicketUpdate)?;
    let ws = access.workspace_id;

    let mut tx = state.pool.begin().await?;
    let cur: CurrentProps = sqlx::query_as(
        "SELECT workflow_status, version, subject, chronology, priority_id, category_id, \
         product_id, receiving_unit_id, region_id, transaction_activity, cause, \
         foreign_branch_reference FROM tickets WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(ticket_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    // Same order as transitions: archive first, then a stale client gets 409.
    if cur.workflow_status == TicketStatus::Closed.code() {
        return Err(AppError::RuleViolation);
    }
    if cur.version != b.version {
        return Err(AppError::VersionConflict);
    }
    let version = b.version;
    let list = changes(&mut tx, ws, &cur, b).await?;
    if list.is_empty() {
        let detail = load(&mut tx, &access, ticket_id).await?;
        tx.commit().await?;
        return Ok(Json(DetailMutation {
            detail,
            audit_event_id: None,
        }));
    }
    let mut q = QueryBuilder::<Postgres>::new("UPDATE tickets SET version = ");
    q.push_bind(version + 1);
    for (_, c) in &list {
        match c {
            Change::Text(col, v) => {
                q.push(", ").push(*col).push(" = ").push_bind(v.clone());
            }
            Change::Id(col, v) => {
                q.push(", ").push(*col).push(" = ").push_bind(*v);
            }
        }
    }
    q.push(" WHERE workspace_id = ")
        .push_bind(ws)
        .push(" AND id = ")
        .push_bind(ticket_id);
    q.build().execute(&mut *tx).await?;
    let fields: Vec<&str> = list.iter().map(|(f, _)| *f).collect();
    let audit_id = audit(
        &mut tx,
        &access,
        "ticket.update",
        ticket_id,
        serde_json::json!({ "fields": fields, "version": version + 1 }),
        correlation(&headers),
    )
    .await?;
    let detail = load(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, fields = ?fields, "ticket properties updated");
    Ok(Json(DetailMutation {
        detail,
        audit_event_id: Some(audit_id),
    }))
}

// ---------- related work ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelatedWorkBody {
    title: String,
    safe_description: Option<String>,
    blocking: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletionBody {
    /// The related-work version the client saw.
    version: i32,
    summary: String,
    evidence: String,
}

/// Locks the ticket and refuses writes on the archive (and on resolved for new work).
pub(crate) async fn lock_open_ticket(
    conn: &mut PgConnection,
    ws: Uuid,
    ticket_id: Uuid,
    refuse_resolved: bool,
) -> Result<(), AppError> {
    let status: String = sqlx::query_scalar(
        "SELECT workflow_status FROM tickets WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ws)
    .bind(ticket_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(AppError::NotFound)?;
    let status =
        TicketStatus::from_code(&status).ok_or_else(|| AppError::internal(UnknownStatus))?;
    if status == TicketStatus::Closed || (refuse_resolved && status == TicketStatus::Resolved) {
        return Err(AppError::RuleViolation);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedWorkCreated {
    detail: TicketDetail,
    related_work_id: Uuid,
    /// `true` when the same Idempotency-Key and payload were already accepted; nothing new
    /// was written and `auditEventId` is `null`.
    replayed: bool,
    audit_event_id: Option<Uuid>,
}

/// SHA-256 over the normalized create request (never stored as text).
pub(crate) fn request_hash(v: &serde_json::Value) -> Result<Vec<u8>, AppError> {
    let bytes = serde_json::to_vec(v).map_err(AppError::internal)?;
    Ok(Sha256::digest(&bytes).to_vec())
}

pub async fn create_related_work(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket)): Path<(String, String)>,
    payload: Result<Json<RelatedWorkBody>, JsonRejection>,
) -> Result<(StatusCode, Json<RelatedWorkCreated>), AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    access.require(Permission::TicketRead)?;
    let key = idempotency_key(&headers)?;
    let b = body(payload)?;
    let title = required_text(&b.title, MAX_RELATED_TITLE_CHARS, "title")?;
    let description = nullable_text(
        b.safe_description,
        MAX_RELATED_TEXT_CHARS,
        "safeDescription",
    )?;
    access.require(Permission::RelatedWorkManage)?;
    let ws = access.workspace_id;
    let hash = request_hash(&serde_json::json!({
        "v": "d08b-related-work-v1",
        "ticketId": ticket_id,
        "title": title,
        "safeDescription": description,
        "blocking": b.blocking,
    }))?;

    let mut tx = state.pool.begin().await?;
    // Serializes concurrent submits of the same key; the unique index is the backstop.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("related-work-create:{ws}:{}:{key}", access.user_id))
        .execute(&mut *tx)
        .await?;
    let prior: Option<(Uuid, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT id, create_request_hash FROM related_work \
         WHERE workspace_id = $1 AND requested_by = $2 AND create_idempotency_key = $3",
    )
    .bind(ws)
    .bind(access.user_id)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, stored)) = prior {
        if stored.as_deref() != Some(hash.as_slice()) {
            tracing::info!(workspace_id = %ws, "related work key reused with another payload");
            return Err(AppError::IdempotencyConflict);
        }
        let detail = load(&mut tx, &access, ticket_id).await?;
        tx.commit().await?;
        tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, related_work_id = %id, "related work create replayed");
        return Ok((
            StatusCode::OK,
            Json(RelatedWorkCreated {
                detail,
                related_work_id: id,
                replayed: true,
                audit_event_id: None,
            }),
        ));
    }
    // New work on a resolved ticket would be invisible to the resolve guard: reopen first.
    lock_open_ticket(&mut tx, ws, ticket_id, true).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO related_work (workspace_id, ticket_id, title, safe_description, blocking, \
           requested_by, create_idempotency_key, create_request_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(ws)
    .bind(ticket_id)
    .bind(&title)
    .bind(&description)
    .bind(b.blocking)
    .bind(access.user_id)
    .bind(&key)
    .bind(&hash)
    .fetch_one(&mut *tx)
    .await?;
    let audit_id = audit(
        &mut tx,
        &access,
        "related_work.create",
        ticket_id,
        serde_json::json!({ "relatedWorkId": id, "blocking": b.blocking }),
        correlation(&headers),
    )
    .await?;
    let detail = load(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, related_work_id = %id, "related work created");
    Ok((
        StatusCode::CREATED,
        Json(RelatedWorkCreated {
            detail,
            related_work_id: id,
            replayed: false,
            audit_event_id: Some(audit_id),
        }),
    ))
}

pub async fn complete_related_work(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_ticket, raw_rw)): Path<(String, String, String)>,
    payload: Result<Json<CompletionBody>, JsonRejection>,
) -> Result<Json<DetailMutation>, AppError> {
    let access = authz::resolve(&state, &s, parse_id(&raw_ws)?).await?;
    let ticket_id = parse_id(&raw_ticket)?;
    let rw_id = parse_id(&raw_rw)?;
    access.require(Permission::TicketRead)?;
    let b = body(payload)?;
    if b.version < 1 {
        return Err(AppError::InvalidRequest);
    }
    let summary = required_text(&b.summary, MAX_RELATED_TEXT_CHARS, "summary")?;
    let evidence = required_text(&b.evidence, MAX_RELATED_TEXT_CHARS, "evidence")?;
    access.require(Permission::RelatedWorkManage)?;
    let ws = access.workspace_id;

    let mut tx = state.pool.begin().await?;
    lock_open_ticket(&mut tx, ws, ticket_id, false).await?;
    let (version, state_code): (i32, String) = sqlx::query_as(
        "SELECT version, completion_state FROM related_work \
         WHERE workspace_id = $1 AND ticket_id = $2 AND id = $3 FOR UPDATE",
    )
    .bind(ws)
    .bind(ticket_id)
    .bind(rw_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if version != b.version {
        return Err(AppError::VersionConflict);
    }
    if state_code != "open" {
        return Err(AppError::RuleViolation);
    }
    sqlx::query(
        "UPDATE related_work SET completion_state = 'completed', completion_summary = $4, \
         completion_evidence = $5, completed_by = $6, completed_at = now(), updated_at = now(), \
         version = $7 WHERE workspace_id = $1 AND ticket_id = $2 AND id = $3",
    )
    .bind(ws)
    .bind(ticket_id)
    .bind(rw_id)
    .bind(&summary)
    .bind(&evidence)
    .bind(access.user_id)
    .bind(version + 1)
    .execute(&mut *tx)
    .await?;
    let audit_id = audit(
        &mut tx,
        &access,
        "related_work.complete",
        ticket_id,
        serde_json::json!({ "relatedWorkId": rw_id, "version": version + 1 }),
        correlation(&headers),
    )
    .await?;
    let detail = load(&mut tx, &access, ticket_id).await?;
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, ticket_id = %ticket_id, related_work_id = %rw_id, "related work completed");
    Ok(Json(DetailMutation {
        detail,
        audit_event_id: Some(audit_id),
    }))
}

/// (json field, column, catalog table, requested, current) for a nullable catalog id.
type IdField = (
    &'static str,
    &'static str,
    &'static str,
    Option<Option<Uuid>>,
    Option<Uuid>,
);
/// (json field, column, max chars, requested, current) for a nullable free-text field.
type TextField<'a> = (
    &'static str,
    &'static str,
    usize,
    Option<Option<String>>,
    &'a Option<String>,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_labels_never_echo_free_text() {
        let c = serde_json::json!({ "fields": ["subject", "cause", "x"], "reason": "rahasia" });
        assert_eq!(
            activity_label("ticket.update", &c),
            "Mengubah subjek, penyebab"
        );
        let t = serde_json::json!({ "from": "new", "to": "open" });
        assert_eq!(
            activity_label("ticket.transition", &t),
            "Mengubah status Baru → Ditangani"
        );
        assert_eq!(activity_label("x.y", &c), "Aktivitas lain");
    }

    #[test]
    fn cursor_is_bounded_and_typed() {
        assert!(parse_cursor("nope").is_err());
        assert!(parse_cursor(&"a".repeat(200)).is_err());
        let ok = parse_cursor("2026-11-01T10:00:00.000000Z~6f1c2a8e-0d6a-4b7e-9a51-1f4c8f7b2c11");
        assert!(ok.is_ok());
    }

    #[test]
    fn absent_and_null_are_distinct() {
        let b = serde_json::from_str::<PropertiesBody>(r#"{"version":1,"cause":null}"#);
        assert!(matches!(&b, Ok(b) if matches!(b.cause, Some(None)) && b.priority_id.is_none()));
        assert!(serde_json::from_str::<PropertiesBody>(r#"{"version":1,"x":1}"#).is_err());
    }
}
