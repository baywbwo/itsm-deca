//! CP5b live integrations: connection settings, connection tests, provider lookups and the
//! delivery attempt of one outbox intent (`ITSM_INTEGRATIONS=live`).
//!
//! Non-secret settings live in `integration_connections.config`; the Jira API token / Lark app
//! secret lives encrypted in `encrypted_secrets` and is decrypted only for the request that needs
//! it. Every provider operation runs inside one budget (`ITSM_INTEGRATIONS_TIMEOUT_MS`) and goes
//! through the outbound guard in [`crate::providers`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::AppState;
use crate::crypto::{self, Purpose, SecretBytes};
use crate::error::AppError;
use crate::providers::{
    CallError, IdName, IssueRef, Jira, JiraUser, Lark, LarkChat, NewIssue, Target,
};

/// Lark Open Platform (international). Feishu tenants set `https://open.feishu.cn`.
pub const LARK_DEFAULT_BASE: &str = "https://open.larksuite.com";

/// Ticket statuses that can be mapped to a Jira status.
pub const TICKET_STATUSES: [&str; 6] = [
    "new",
    "open",
    "pending_customer",
    "pending_engineering",
    "resolved",
    "closed",
];

/// Jira settings stored in `config` (no secret).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JiraSettings {
    pub base_url: String,
    pub email: String,
    pub project_key: String,
    pub issue_type_id: String,
    /// ITSM priority code → Jira priority id.
    pub priority_map: BTreeMap<String, String>,
    /// ITSM ticket status → Jira status id (stored and shown; no automatic transition).
    pub status_map: BTreeMap<String, String>,
}

/// Lark settings stored in `config` (no secret).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LarkSettings {
    pub base_url: String,
    pub app_id: String,
}

fn text(config: &Value, key: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn map(config: &Value, key: &str) -> BTreeMap<String, String> {
    config
        .get(key)
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

impl JiraSettings {
    pub fn from_config(config: &Value) -> Self {
        Self {
            base_url: text(config, "baseUrl"),
            email: text(config, "email"),
            project_key: text(config, "projectKey"),
            issue_type_id: text(config, "issueTypeId"),
            priority_map: map(config, "priorityMap"),
            status_map: map(config, "statusMap"),
        }
    }

    /// Enough to reach the project (issue type is chosen afterwards from the project list).
    pub fn connectable(&self) -> bool {
        !self.base_url.is_empty() && !self.email.is_empty() && !self.project_key.is_empty()
    }

    pub fn complete(&self) -> bool {
        self.connectable() && !self.issue_type_id.is_empty()
    }
}

impl LarkSettings {
    pub fn from_config(config: &Value) -> Self {
        let base = text(config, "baseUrl");
        Self {
            base_url: if base.is_empty() {
                LARK_DEFAULT_BASE.to_owned()
            } else {
                base
            },
            app_id: text(config, "appId"),
        }
    }

    pub fn complete(&self) -> bool {
        !self.app_id.is_empty()
    }
}

pub fn purpose(provider: &str) -> Purpose {
    if provider == "jira" {
        Purpose::JiraApiToken
    } else {
        Purpose::LarkAppSecret
    }
}

/// The parts of a connection a live operation needs.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LiveConn {
    pub id: Uuid,
    pub provider: String,
    pub mode: String,
    pub state: String,
    pub config: Value,
    pub credential_secret_id: Option<Uuid>,
}

pub async fn load_conn(state: &AppState, ws: Uuid, id: Uuid) -> Result<LiveConn, AppError> {
    sqlx::query_as::<_, LiveConn>(
        "SELECT id, provider, mode, state, config, credential_secret_id \
         FROM integration_connections WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Why a live operation could not start or finish. Every variant has a fixed safe note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    NotConfigured,
    NoCredential,
    NoKeyring,
    /// The stored secret cannot be decrypted with the current keyring.
    CredentialUnreadable,
    Call(CallError),
}

impl Failure {
    pub fn note(self) -> String {
        match self {
            Failure::NotConfigured => "Konfigurasi koneksi belum lengkap.".to_owned(),
            Failure::NoCredential => "Kredensial belum disimpan.".to_owned(),
            Failure::NoKeyring => {
                "Kunci enkripsi server (ITSM_KEYRING) belum dipasang; kredensial tidak dapat dibuka."
                    .to_owned()
            }
            Failure::CredentialUnreadable => {
                "Kredensial tersimpan tidak dapat dibuka dengan kunci enkripsi server saat ini. Simpan ulang kredensial."
                    .to_owned()
            }
            Failure::Call(e) => e.note(),
        }
    }

    /// Error for a synchronous lookup (options, chat list, user search).
    pub fn into_app_error(self) -> AppError {
        match self {
            Failure::NotConfigured | Failure::NoCredential => {
                AppError::Conflict("integration_not_configured")
            }
            Failure::NoKeyring => AppError::KeyringMissing,
            Failure::CredentialUnreadable => AppError::Provider(
                "Kredensial tersimpan tidak dapat dibuka dengan kunci enkripsi server saat ini.",
            ),
            Failure::Call(CallError::Refused) => AppError::Provider(
                "Alamat penyedia ditolak: hanya https ke alamat publik yang diizinkan.",
            ),
            Failure::Call(CallError::Connect) => {
                AppError::Provider("Penyedia tidak dapat dihubungi.")
            }
            Failure::Call(CallError::Timeout) => {
                AppError::Provider("Penyedia tidak menjawab dalam batas waktu.")
            }
            Failure::Call(CallError::Status(401 | 403)) => {
                AppError::Provider("Kredensial ditolak penyedia.")
            }
            Failure::Call(_) => {
                AppError::Provider("Penyedia menolak atau gagal memproses permintaan.")
            }
        }
    }
}

impl From<CallError> for Failure {
    fn from(e: CallError) -> Self {
        Failure::Call(e)
    }
}

fn budget(state: &AppState) -> Duration {
    Duration::from_millis(state.config.integrations.timeout_ms)
}

/// Runs a provider operation inside the configured budget.
async fn within<T>(
    state: &AppState,
    fut: impl std::future::Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    match tokio::time::timeout(budget(state), fut).await {
        Ok(r) => r,
        Err(_) => Err(Failure::Call(CallError::Timeout)),
    }
}

async fn secret(state: &AppState, ws: Uuid, conn: &LiveConn) -> Result<SecretBytes, Failure> {
    let id = conn.credential_secret_id.ok_or(Failure::NoCredential)?;
    let keyring = state.config.keyring.as_deref().ok_or(Failure::NoKeyring)?;
    crypto::load_secret(&state.pool, keyring, id, Some(ws), purpose(&conn.provider))
        .await
        .map_err(|e| {
            tracing::warn!(
                connection_id = %conn.id,
                error.kind = match e {
                    crypto::SecretError::Db(_) => "db",
                    _ => "decrypt",
                },
                "integration credential unreadable"
            );
            Failure::CredentialUnreadable
        })
}

async fn target(state: &AppState, base: &str) -> Result<Target, Failure> {
    Ok(Target::prepare(base, state.config.integrations.lab, budget(state)).await?)
}

async fn jira(
    state: &AppState,
    ws: Uuid,
    conn: &LiveConn,
) -> Result<(Jira, JiraSettings), Failure> {
    let s = JiraSettings::from_config(&conn.config);
    if !s.connectable() {
        return Err(Failure::NotConfigured);
    }
    let token = secret(state, ws, conn).await?;
    let t = target(state, &s.base_url).await?;
    Ok((Jira::new(t, &s.email, token.expose())?, s))
}

async fn lark(state: &AppState, ws: Uuid, conn: &LiveConn) -> Result<Lark, Failure> {
    let s = LarkSettings::from_config(&conn.config);
    if !s.complete() {
        return Err(Failure::NotConfigured);
    }
    let app_secret = secret(state, ws, conn).await?;
    let t = target(state, &s.base_url).await?;
    Ok(Lark::connect(t, &s.app_id, app_secret.expose()).await?)
}

fn short(raw: &str, max: usize) -> String {
    raw.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Result of a live connection test: `note` is safe to store and show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub ok: bool,
    pub note: String,
}

/// Real connection test. Jira: account (`/myself`), project, issue type and mapped priorities.
/// Lark: tenant token, bot info and the chat list.
pub async fn test_connection(state: &AppState, ws: Uuid, conn: &LiveConn) -> TestResult {
    let r = within(state, async {
        if conn.provider == "jira" {
            let (j, s) = jira(state, ws, conn).await?;
            j.myself().await?;
            let project = j.project(&s.project_key).await?;
            if s.issue_type_id.is_empty() {
                return Ok(Err(format!(
                    "Terhubung ke Jira dan proyek {} ditemukan. Pilih tipe isu lalu uji ulang.",
                    project.key
                )));
            }
            let Some(kind) = project.issue_types.iter().find(|t| t.id == s.issue_type_id) else {
                return Ok(Err(format!(
                    "Terhubung ke Jira, tetapi tipe isu {} tidak ada di proyek {}.",
                    s.issue_type_id, project.key
                )));
            };
            let kind = short(&kind.name, 60);
            if !s.priority_map.is_empty() {
                let known = j.priorities().await?;
                if let Some((code, _)) = s
                    .priority_map
                    .iter()
                    .find(|(_, id)| !known.iter().any(|p| &p.id == *id))
                {
                    return Ok(Err(format!(
                        "Terhubung ke Jira, tetapi prioritas Jira untuk \"{code}\" tidak ditemukan."
                    )));
                }
            }
            Ok(Ok(format!(
                "Terhubung ke Jira. Proyek {} dan tipe isu \"{kind}\" valid.",
                project.key
            )))
        } else {
            let l = lark(state, ws, conn).await?;
            let bot = short(&l.bot_name().await?, 60);
            let chats = l.chats().await?.len();
            Ok(Ok(format!(
                "Terhubung ke Lark sebagai bot \"{bot}\"; {chats} grup tersedia."
            )))
        }
    })
    .await;
    match r {
        Ok(Ok(note)) => TestResult { ok: true, note },
        Ok(Err(note)) => TestResult { ok: false, note },
        Err(f) => {
            if let Failure::Call(e) = f {
                tracing::info!(connection_id = %conn.id, error.kind = e.kind(), "integration test failed");
            }
            TestResult {
                ok: false,
                note: f.note(),
            }
        }
    }
}

/// Jira metadata for the mapping form.
pub struct JiraOptions {
    pub issue_types: Vec<IdName>,
    pub priorities: Vec<IdName>,
    pub statuses: Vec<IdName>,
}

pub async fn jira_options(
    state: &AppState,
    ws: Uuid,
    conn: &LiveConn,
) -> Result<JiraOptions, Failure> {
    within(state, async {
        let (j, s) = jira(state, ws, conn).await?;
        let project = j.project(&s.project_key).await?;
        let priorities = j.priorities().await?;
        let statuses = j.statuses(&s.project_key).await?;
        Ok(JiraOptions {
            issue_types: project.issue_types,
            priorities,
            statuses,
        })
    })
    .await
}

pub async fn jira_users(
    state: &AppState,
    ws: Uuid,
    conn: &LiveConn,
    query: &str,
) -> Result<Vec<JiraUser>, Failure> {
    within(state, async {
        let (j, s) = jira(state, ws, conn).await?;
        Ok(j.assignable(&s.project_key, query).await?)
    })
    .await
}

pub async fn lark_chats(
    state: &AppState,
    ws: Uuid,
    conn: &LiveConn,
) -> Result<Vec<LarkChat>, Failure> {
    within(state, async {
        Ok(lark(state, ws, conn).await?.chats().await?)
    })
    .await
}

// ---------- outbox delivery ----------

/// What a live delivery attempt achieved. Applied by [`crate::outbox::settle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// Accepted by the provider (Jira: with the issue to link, `None` when already linked).
    Delivered(Option<IssueRef>),
    /// The connection is not active; nothing was sent.
    Inactive,
    /// The connection was configured/tested in the other mode; nothing was sent.
    ModeMismatch,
    /// Definite failure; an officer may retry.
    Failed(String),
    /// Definitely not accepted; retried automatically with backoff.
    Transient(String),
    /// May or may not have reached the provider; reconcile with evidence.
    Unknown(String),
}

#[derive(sqlx::FromRow)]
struct Work {
    operation: String,
    requested_issue_key: Option<String>,
    number_prefix: String,
    number: i64,
    priority_code: Option<String>,
    priority_label: Option<String>,
    title: Option<String>,
    linked: bool,
    assignee_ref: Option<String>,
    assignee_source: Option<String>,
    chat_ref: Option<String>,
    dest_source: Option<String>,
    id: Uuid,
    provider: String,
    mode: String,
    state: String,
    config: Value,
    credential_secret_id: Option<Uuid>,
}

const WORK_SQL: &str = "SELECT o.operation, o.requested_issue_key, t.number_prefix, t.number, \
        p.code AS priority_code, p.label AS priority_label, r.title, \
        (r.jira_issue_id IS NOT NULL) IS TRUE AS linked, \
        a.external_account_ref AS assignee_ref, a.source AS assignee_source, \
        d.external_chat_ref AS chat_ref, d.source AS dest_source, \
        c.id, c.provider, c.mode, c.state, c.config, c.credential_secret_id \
     FROM integration_outbox o \
     JOIN integration_connections c ON c.workspace_id = o.workspace_id AND c.id = o.connection_id \
     JOIN tickets t ON t.workspace_id = o.workspace_id AND t.id = o.ticket_id \
     LEFT JOIN priorities p ON p.workspace_id = t.workspace_id AND p.id = t.priority_id \
     LEFT JOIN related_work r ON r.workspace_id = o.workspace_id AND r.id = o.related_work_id \
     LEFT JOIN jira_assignees a ON a.workspace_id = r.workspace_id AND a.id = r.jira_assignee_id \
     LEFT JOIN lark_destinations d ON d.workspace_id = o.workspace_id AND d.id = o.lark_destination_id \
     WHERE o.workspace_id = $1 AND o.id = $2 AND o.claim_token = $3";

/// Label put on every Jira issue the outbox creates; searched first so a repeat never duplicates.
pub fn idempotency_label(outbox_id: Uuid) -> String {
    format!("itsm-{outbox_id}")
}

fn classify(provider: &str, e: CallError, write_started: bool) -> Attempt {
    let note = format!(
        "{}: {}",
        if provider == "jira" { "Jira" } else { "Lark" },
        e.note()
    );
    match e {
        CallError::Refused => Attempt::Failed(note),
        CallError::Connect => Attempt::Transient(format!("{note} Dicoba ulang otomatis.")),
        e if e.is_throttle() => Attempt::Transient(format!("{note} Dicoba ulang otomatis.")),
        CallError::Timeout | CallError::Protocol | CallError::Status(500..=599)
            if write_started =>
        {
            Attempt::Unknown(format!(
                "{note} Hasil pengiriman tidak diketahui. Jangan kirim ulang; periksa di penyedia lalu rekonsiliasi dengan bukti."
            ))
        }
        CallError::Timeout | CallError::Protocol | CallError::Status(500..=599) => {
            Attempt::Transient(format!("{note} Dicoba ulang otomatis."))
        }
        CallError::Status(_) | CallError::Api(_) => Attempt::Failed(note),
    }
}

/// Performs the provider call for a claimed intent. Holds no database lock while waiting on the
/// provider; the caller settles the outcome with the claim token afterwards.
pub async fn attempt(state: &AppState, ws: Uuid, outbox_id: Uuid, token: Uuid) -> Attempt {
    let work = match sqlx::query_as::<_, Work>(WORK_SQL)
        .bind(ws)
        .bind(outbox_id)
        .bind(token)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(Some(w)) => w,
        // Claim lost: the settle step notices and writes nothing.
        Ok(None) => return Attempt::Transient("Klaim pengiriman hilang.".to_owned()),
        Err(_) => {
            return Attempt::Transient(
                "Data pengiriman tidak dapat dibaca. Dicoba ulang otomatis.".to_owned(),
            );
        }
    };
    if work.state != "active" {
        return Attempt::Inactive;
    }
    if work.mode != "live" {
        return Attempt::ModeMismatch;
    }
    let conn = LiveConn {
        id: work.id,
        provider: work.provider.clone(),
        mode: work.mode.clone(),
        state: work.state.clone(),
        config: work.config.clone(),
        credential_secret_id: work.credential_secret_id,
    };
    let writing = AtomicBool::new(false);
    let result = within(state, deliver(state, ws, outbox_id, &conn, &work, &writing)).await;
    match result {
        Ok(a) => a,
        Err(Failure::Call(e)) => {
            tracing::info!(outbox_id = %outbox_id, error.kind = e.kind(), "live delivery failed");
            classify(&work.provider, e, writing.load(Ordering::SeqCst))
        }
        Err(f) => Attempt::Failed(f.note()),
    }
}

async fn deliver(
    state: &AppState,
    ws: Uuid,
    outbox_id: Uuid,
    conn: &LiveConn,
    work: &Work,
    writing: &AtomicBool,
) -> Result<Attempt, Failure> {
    let ticket = format!("{}-{}", work.number_prefix, work.number);
    match work.operation.as_str() {
        "jira_create_issue" | "jira_link_issue" if work.linked => Ok(Attempt::Delivered(None)),
        "jira_create_issue" => {
            if work.assignee_source.as_deref() == Some("dummy") {
                return Ok(Attempt::Failed(
                    "Penerima tugas uji (dummy) tidak berlaku di mode live. Pilih pengguna Jira lalu buat permintaan baru."
                        .to_owned(),
                ));
            }
            let (j, s) = jira(state, ws, conn).await?;
            if !s.complete() {
                return Err(Failure::NotConfigured);
            }
            let label = idempotency_label(outbox_id);
            if let Some(found) = j.find_by_label(&label).await? {
                return Ok(Attempt::Delivered(Some(found)));
            }
            let summary = work.title.clone().unwrap_or_else(|| ticket.clone());
            let description = format!(
                "Dibuat dari tiket ITSM {ticket}. Rincian keluhan nasabah tidak dikirim; buka ITSM untuk konteks."
            );
            let priority = work
                .priority_code
                .as_ref()
                .and_then(|c| s.priority_map.get(c))
                .map(String::as_str);
            let labels = [label, "itsm".to_owned()];
            writing.store(true, Ordering::SeqCst);
            let issue = j
                .create_issue(&NewIssue {
                    project_key: &s.project_key,
                    issue_type_id: &s.issue_type_id,
                    summary: &summary,
                    description: &description,
                    priority_id: priority,
                    assignee_account_id: work.assignee_ref.as_deref(),
                    labels: &labels,
                })
                .await?;
            Ok(Attempt::Delivered(Some(issue)))
        }
        "jira_link_issue" => {
            let Some(key) = work.requested_issue_key.as_deref() else {
                return Ok(Attempt::Failed(
                    "Kunci isu Jira tidak tersimpan pada permintaan ini.".to_owned(),
                ));
            };
            let (j, _) = jira(state, ws, conn).await?;
            match j.issue(key).await {
                Ok(issue) => Ok(Attempt::Delivered(Some(issue))),
                Err(CallError::Status(404)) => Ok(Attempt::Failed(format!(
                    "Isu {key} tidak ditemukan di Jira atau tidak dapat diakses akun integrasi."
                ))),
                Err(e) => Err(e.into()),
            }
        }
        "lark_ticket_created" => {
            if work.dest_source.as_deref() == Some("dummy") {
                return Ok(Attempt::Failed(
                    "Tujuan Lark uji (dummy) tidak berlaku di mode live. Sinkronkan daftar grup Lark."
                        .to_owned(),
                ));
            }
            let Some(chat) = work.chat_ref.as_deref() else {
                return Ok(Attempt::Failed("Tujuan Lark tidak ada.".to_owned()));
            };
            let l = lark(state, ws, conn).await?;
            // Ticket number and priority only: no subject, chronology or customer data in chat.
            let priority = work
                .priority_label
                .as_deref()
                .map(|p| format!(" · Prioritas {}", short(p, 60)))
                .unwrap_or_default();
            let text = format!("[ITSM] Tiket baru {ticket}{priority}. Buka ITSM untuk rincian.");
            writing.store(true, Ordering::SeqCst);
            l.send_text(chat, &text, &outbox_id.to_string()).await?;
            Ok(Attempt::Delivered(None))
        }
        _ => Ok(Attempt::Failed("Operasi tidak dikenal.".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_errors_after_sending_are_unknown_never_retried() {
        assert!(matches!(
            classify("jira", CallError::Timeout, true),
            Attempt::Unknown(_)
        ));
        assert!(matches!(
            classify("jira", CallError::Timeout, false),
            Attempt::Transient(_)
        ));
        assert!(matches!(
            classify("lark", CallError::Status(503), true),
            Attempt::Transient(_)
        ));
        assert!(matches!(
            classify("lark", CallError::Status(400), true),
            Attempt::Failed(_)
        ));
        assert!(matches!(
            classify("jira", CallError::Refused, false),
            Attempt::Failed(_)
        ));
        assert!(matches!(
            classify("jira", CallError::Connect, true),
            Attempt::Transient(_)
        ));
    }

    #[test]
    fn settings_read_only_known_keys() {
        let cfg = serde_json::json!({
            "baseUrl": "https://acme.atlassian.net", "email": "bot@acme.test",
            "projectKey": "OPS", "issueTypeId": "10001",
            "priorityMap": { "high": "2", "bad": 3 }, "simulatedOutcome": "failed"
        });
        let s = JiraSettings::from_config(&cfg);
        assert!(s.complete());
        assert_eq!(s.priority_map.len(), 1);
        let l = LarkSettings::from_config(&serde_json::json!({ "appId": "cli_x" }));
        assert_eq!(l.base_url, LARK_DEFAULT_BASE);
    }
}
