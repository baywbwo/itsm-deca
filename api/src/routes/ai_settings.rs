//! D18: setelan koneksi LLM (OpenAI / Anthropic Claude) dan MCP server — konfigurasi saja.
//!
//! Prinsip yang sama dengan integrasi D09: **tidak ada rahasia di basis data**. Kolom `key_env`
//! dan `auth_env` hanya menyimpan NAMA variabel lingkungan; API melaporkan "terpasang/belum"
//! berdasarkan keberadaan variabel saat proses berjalan dan tidak pernah mengembalikan nilainya.
//! Semua operasi butuh grant eksplisit `integration.manage`, memeriksa `version` (409) dan
//! menulis baris audit dalam transaksi yang sama.
//!
//! TM-41 / A-9: perubahan konfigurasi (penyedia: base URL, model, sumber kunci; daftar MCP) butuh
//! step-up TOTP ≤5 menit setelah cek izin (403 dulu, lalu 401 `step_up_required`). Membaca dan
//! menguji koneksi tidak mengubah konfigurasi, jadi tidak butuh step-up.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

use crate::AppState;
use crate::authz::{self, Permission, WorkspaceAccess};
use crate::error::AppError;
use crate::routes::sync::audit_entity;
use crate::routes::ticket_list::{body, correlation};
use crate::routes::workspaces::parse_id;
use crate::session::Session;

const PROVIDERS: [&str; 2] = ["openai", "anthropic"];
const NOTE_KEY_MISSING: &str =
    "Kunci belum tersedia di server: variabel lingkungan pada kolom ini belum diisi.";
const NOTE_STDIO: &str = "Transport stdio dijalankan proses agen di host, bukan oleh server ini. Uji koneksi hanya memeriksa konfigurasi.";
const NOTE_REMOTE_FAILED: &str = "Uji koneksi gagal: penyedia tidak menjawab dengan sukses.";

fn provider_code(raw: &str) -> Result<&'static str, AppError> {
    PROVIDERS
        .into_iter()
        .find(|p| *p == raw)
        .ok_or(AppError::NotFound)
}

fn env_present(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn valid_env_name(raw: &str) -> bool {
    let mut chars = raw.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && (3..=61).contains(&raw.len())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn trimmed(raw: &str, max: usize) -> Result<String, AppError> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(AppError::InvalidRequest);
    }
    Ok(t.to_owned())
}

fn https_url(raw: &str) -> Result<String, AppError> {
    let t = raw.trim().trim_end_matches('/');
    if !t.starts_with("https://") || t.len() > 200 {
        return Err(AppError::InvalidRequest);
    }
    Ok(t.to_owned())
}

#[derive(Debug, sqlx::FromRow)]
struct ProviderRow {
    provider: String,
    model: Option<String>,
    base_url: Option<String>,
    key_env: Option<String>,
    state: String,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    version: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    provider: String,
    model: Option<String>,
    base_url: Option<String>,
    key_env: Option<String>,
    /// Hanya status keberadaan variabel lingkungan; nilainya tidak pernah dikirim.
    key_present: bool,
    state: String,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    version: i32,
    /// Selalu `env_only`: aplikasi tidak menyimpan kunci di basis data.
    secret: &'static str,
}

const PROVIDER_SQL: &str = "SELECT provider, model, base_url, key_env, state, \
        to_char(tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
        last_test_note, version \
     FROM ai_providers WHERE workspace_id = $1 AND provider = $2";

fn view(row: Option<ProviderRow>, code: &'static str) -> ProviderView {
    match row {
        Some(r) => {
            let present = r.key_env.as_deref().map(env_present).unwrap_or(false);
            ProviderView {
                provider: r.provider,
                model: r.model,
                base_url: r.base_url,
                key_env: r.key_env,
                key_present: present,
                state: r.state,
                tested_at: r.tested_at,
                last_test_note: r.last_test_note,
                version: r.version,
                secret: "env_only",
            }
        }
        None => ProviderView {
            provider: code.to_owned(),
            model: None,
            base_url: None,
            key_env: None,
            key_present: false,
            state: "draft".to_owned(),
            tested_at: None,
            last_test_note: None,
            version: 0,
            secret: "env_only",
        },
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpView {
    id: Uuid,
    name: String,
    transport: String,
    endpoint: Option<String>,
    command: Option<String>,
    auth_env: Option<String>,
    auth_present: bool,
    enabled: bool,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct McpRow {
    id: Uuid,
    name: String,
    transport: String,
    endpoint: Option<String>,
    command: Option<String>,
    auth_env: Option<String>,
    enabled: bool,
    tested_at: Option<String>,
    last_test_note: Option<String>,
    version: i32,
}

impl From<McpRow> for McpView {
    fn from(r: McpRow) -> Self {
        let auth_present = r.auth_env.as_deref().map(env_present).unwrap_or(false);
        McpView {
            id: r.id,
            name: r.name,
            transport: r.transport,
            endpoint: r.endpoint,
            command: r.command,
            auth_env: r.auth_env,
            auth_present,
            enabled: r.enabled,
            tested_at: r.tested_at,
            last_test_note: r.last_test_note,
            version: r.version,
        }
    }
}

const MCP_SQL: &str = "SELECT id, name, transport, endpoint, command, auth_env, enabled, \
        to_char(tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
        last_test_note, version \
     FROM mcp_servers WHERE workspace_id = $1 ORDER BY name";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    providers: Vec<ProviderView>,
    mcp_servers: Vec<McpView>,
    /// Bagian yang belum dimiliki build ini, dinyatakan apa adanya.
    notes: Vec<&'static str>,
}

async fn require_manage(
    state: &AppState,
    s: &Session,
    raw_ws: &str,
) -> Result<WorkspaceAccess, AppError> {
    let access = authz::resolve(state, s, parse_id(raw_ws)?).await?;
    access.require(Permission::IntegrationManage)?;
    Ok(access)
}

pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw_ws): Path<String>,
) -> Result<Json<SettingsView>, AppError> {
    let access = require_manage(&state, &s, &raw_ws).await?;
    let mut providers = Vec::with_capacity(PROVIDERS.len());
    for code in PROVIDERS {
        let row: Option<ProviderRow> = sqlx::query_as(PROVIDER_SQL)
            .bind(access.workspace_id)
            .bind(code)
            .fetch_optional(&state.pool)
            .await?;
        providers.push(view(row, code));
    }
    let mcp: Vec<McpRow> = sqlx::query_as(MCP_SQL)
        .bind(access.workspace_id)
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(SettingsView {
        providers,
        mcp_servers: mcp.into_iter().map(McpView::from).collect(),
        notes: vec![
            "Kunci API tidak disimpan di aplikasi: isi variabel lingkungan di server, lalu jalankan uji koneksi.",
            NOTE_STDIO,
            "Aktivasi pemakaian model pada tiket menunggu keputusan kebijakan pemakaian AI.",
        ],
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderBody {
    model: Option<String>,
    base_url: Option<String>,
    key_env: Option<String>,
    /// Versi yang dilihat klien; `0`/null = baris belum ada.
    version: Option<i32>,
}

pub async fn save_provider(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_provider)): Path<(String, String)>,
    payload: Result<Json<ProviderBody>, JsonRejection>,
) -> Result<Json<ProviderView>, AppError> {
    let access = require_manage(&state, &s, &raw_ws).await?;
    authz::require_step_up(&state, &s)?;
    let code = provider_code(&raw_provider)?;
    let b = body(payload)?;
    let model = match b.model.as_deref() {
        Some(m) => Some(trimmed(m, 100)?),
        None => None,
    };
    let base_url = match b.base_url.as_deref() {
        Some(u) if !u.trim().is_empty() => Some(https_url(u)?),
        _ => None,
    };
    let key_env = match b.key_env.as_deref() {
        Some(k) if !k.trim().is_empty() => {
            let k = k.trim();
            if !valid_env_name(k) {
                return Err(AppError::Validation("keyEnv"));
            }
            Some(k.to_owned())
        }
        _ => None,
    };
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let current: Option<i32> = sqlx::query_scalar(
        "SELECT version FROM ai_providers WHERE workspace_id = $1 AND provider = $2 FOR UPDATE",
    )
    .bind(access.workspace_id)
    .bind(code)
    .fetch_optional(&mut *tx)
    .await?;
    match (current, b.version) {
        (None, Some(v)) if v > 0 => return Err(AppError::VersionConflict),
        (Some(_), None) => return Err(AppError::VersionConflict),
        (Some(v), Some(sent)) if v != sent => return Err(AppError::VersionConflict),
        _ => {}
    }
    sqlx::query(
        "INSERT INTO ai_providers (workspace_id, provider, model, base_url, key_env, state, version) \
         VALUES ($1, $2, $3, $4, $5, 'draft', 1) \
         ON CONFLICT (workspace_id, provider) DO UPDATE \
            SET model = EXCLUDED.model, base_url = EXCLUDED.base_url, key_env = EXCLUDED.key_env, \
                state = 'draft', tested_at = NULL, last_test_note = NULL, \
                version = ai_providers.version + 1, updated_at = now()",
    )
    .bind(access.workspace_id)
    .bind(code)
    .bind(&model)
    .bind(&base_url)
    .bind(&key_env)
    .execute(&mut *tx)
    .await?;
    audit_entity(
        &mut tx,
        access.workspace_id,
        Some(access.user_id),
        "ai.provider.save",
        "ai_provider",
        access.workspace_id,
        json!({ "provider": code }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    let row: Option<ProviderRow> = sqlx::query_as(PROVIDER_SQL)
        .bind(access.workspace_id)
        .bind(code)
        .fetch_optional(&state.pool)
        .await?;
    Ok(Json(view(row, code)))
}

fn client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| AppError::Unavailable)
}

pub async fn test_provider(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_provider)): Path<(String, String)>,
) -> Result<Json<ProviderView>, AppError> {
    let access = require_manage(&state, &s, &raw_ws).await?;
    let code = provider_code(&raw_provider)?;
    let row: Option<ProviderRow> = sqlx::query_as(PROVIDER_SQL)
        .bind(access.workspace_id)
        .bind(code)
        .fetch_optional(&state.pool)
        .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound);
    };
    let key_env = row.key_env.clone().unwrap_or_default();
    let (state_value, note) = if !env_present(&key_env) {
        ("draft", NOTE_KEY_MISSING.to_owned())
    } else {
        let key = std::env::var(&key_env).map_err(|_| AppError::Unavailable)?;
        let base = row.base_url.clone().unwrap_or_else(|| match code {
            "openai" => "https://api.openai.com".to_owned(),
            _ => "https://api.anthropic.com".to_owned(),
        });
        let url = format!("{base}/v1/models");
        let cl = client()?;
        let req = match code {
            "openai" => cl.get(&url).bearer_auth(&key),
            _ => cl
                .get(&url)
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01"),
        };
        match req.send().await {
            Ok(r) if r.status().is_success() => (
                "tested",
                format!("Uji koneksi berhasil (HTTP {}).", r.status().as_u16()),
            ),
            Ok(r) => (
                "draft",
                format!("{NOTE_REMOTE_FAILED} (HTTP {})", r.status().as_u16()),
            ),
            Err(_) => ("draft", NOTE_REMOTE_FAILED.to_owned()),
        }
    };
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE ai_providers SET state = $3, tested_at = CASE WHEN $3 = 'tested' THEN now() ELSE NULL END, \
            last_test_note = $4, version = version + 1, updated_at = now() \
         WHERE workspace_id = $1 AND provider = $2",
    )
    .bind(access.workspace_id)
    .bind(code)
    .bind(state_value)
    .bind(&note)
    .execute(&mut *tx)
    .await?;
    audit_entity(
        &mut tx,
        access.workspace_id,
        Some(access.user_id),
        "ai.provider.test",
        "ai_provider",
        access.workspace_id,
        json!({ "provider": code, "state": state_value }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    let row: Option<ProviderRow> = sqlx::query_as(PROVIDER_SQL)
        .bind(access.workspace_id)
        .bind(code)
        .fetch_optional(&state.pool)
        .await?;
    Ok(Json(view(row, code)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpBody {
    name: String,
    transport: String,
    endpoint: Option<String>,
    command: Option<String>,
    auth_env: Option<String>,
}

fn mcp_parts(b: &McpBody) -> Result<(String, String, Option<String>, Option<String>), AppError> {
    let name = trimmed(&b.name, 120)?;
    let endpoint = match b.endpoint.as_deref() {
        Some(u) if !u.trim().is_empty() => Some(https_url(u)?),
        _ => None,
    };
    let command = match b.command.as_deref() {
        Some(c) if !c.trim().is_empty() => Some(trimmed(c, 200)?),
        _ => None,
    };
    match b.transport.as_str() {
        "http" if endpoint.is_some() && command.is_none() => {}
        "stdio" if command.is_some() && endpoint.is_none() => {}
        _ => return Err(AppError::Validation("transport")),
    }
    Ok((name, b.transport.clone(), endpoint, command))
}

pub async fn create_mcp(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path(raw_ws): Path<String>,
    payload: Result<Json<McpBody>, JsonRejection>,
) -> Result<Json<McpView>, AppError> {
    let access = require_manage(&state, &s, &raw_ws).await?;
    authz::require_step_up(&state, &s)?;
    let b = body(payload)?;
    let (name, transport, endpoint, command) = mcp_parts(&b)?;
    let auth_env = match b.auth_env.as_deref() {
        Some(a) if !a.trim().is_empty() => Some(a.trim().to_owned()),
        _ => None,
    };
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let row: McpRow = sqlx::query_as(
        "INSERT INTO mcp_servers (workspace_id, name, transport, endpoint, command, auth_env) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, name, transport, endpoint, command, auth_env, enabled, \
           to_char(tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
           last_test_note, version",
    )
    .bind(access.workspace_id)
    .bind(&name)
    .bind(&transport)
    .bind(&endpoint)
    .bind(&command)
    .bind(&auth_env)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.code().as_deref() == Some("23505") => {
            AppError::Validation("name")
        }
        other => AppError::from(other),
    })?;
    audit_entity(
        &mut tx,
        access.workspace_id,
        Some(access.user_id),
        "ai.mcp.create",
        "mcp_server",
        row.id,
        json!({ "name": name, "transport": transport }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(McpView::from(row)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPatch {
    name: Option<String>,
    endpoint: Option<String>,
    command: Option<String>,
    auth_env: Option<String>,
    enabled: Option<bool>,
    version: i32,
}

pub async fn update_mcp(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
    payload: Result<Json<McpPatch>, JsonRejection>,
) -> Result<Json<McpView>, AppError> {
    let access = require_manage(&state, &s, &raw_ws).await?;
    authz::require_step_up(&state, &s)?;
    let id = parse_id(&raw_id)?;
    let b = body(payload)?;
    let name = match b.name.as_deref() {
        Some(n) => Some(trimmed(n, 120)?),
        None => None,
    };
    let endpoint = match b.endpoint.as_deref() {
        Some(u) if !u.trim().is_empty() => Some(https_url(u)?),
        _ => None,
    };
    let command = match b.command.as_deref() {
        Some(c) if !c.trim().is_empty() => Some(trimmed(c, 200)?),
        _ => None,
    };
    let auth_env = match b.auth_env.as_deref() {
        Some(a) if !a.trim().is_empty() => {
            let a = a.trim();
            if !valid_env_name(a) {
                return Err(AppError::Validation("authEnv"));
            }
            Some(a.to_owned())
        }
        _ => None,
    };
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let current: Option<i32> = sqlx::query_scalar(
        "SELECT version FROM mcp_servers WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(access.workspace_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    match current {
        None => return Err(AppError::NotFound),
        Some(v) if v != b.version => return Err(AppError::VersionConflict),
        _ => {}
    }
    let row: McpRow = sqlx::query_as(
        "UPDATE mcp_servers SET name = COALESCE($3, name), endpoint = COALESCE($4, endpoint), \
            command = COALESCE($5, command), auth_env = COALESCE($6, auth_env), \
            enabled = COALESCE($7, enabled), version = version + 1, updated_at = now() \
         WHERE workspace_id = $1 AND id = $2 \
         RETURNING id, name, transport, endpoint, command, auth_env, enabled, \
           to_char(tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
           last_test_note, version",
    )
    .bind(access.workspace_id)
    .bind(id)
    .bind(&name)
    .bind(&endpoint)
    .bind(&command)
    .bind(&auth_env)
    .bind(b.enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.code().as_deref() == Some("23514") => {
            AppError::Validation("transport")
        }
        other => AppError::from(other),
    })?;
    audit_entity(
        &mut tx,
        access.workspace_id,
        Some(access.user_id),
        "ai.mcp.update",
        "mcp_server",
        row.id,
        json!({ "enabled": row.enabled }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(McpView::from(row)))
}

pub async fn test_mcp(
    State(state): State<AppState>,
    s: Session,
    headers: HeaderMap,
    Path((raw_ws, raw_id)): Path<(String, String)>,
) -> Result<Json<McpView>, AppError> {
    let access = require_manage(&state, &s, &raw_ws).await?;
    let id = parse_id(&raw_id)?;
    let (name, endpoint, auth_env): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT name, endpoint, auth_env FROM mcp_servers WHERE workspace_id = $1 AND id = $2",
    )
    .bind(access.workspace_id)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    let note = match endpoint {
        Some(url) => {
            let cl = client()?;
            let mut req = cl.get(&url);
            if let Some(env) = auth_env.as_deref()
                && env_present(env)
            {
                let key = std::env::var(env).map_err(|_| AppError::Unavailable)?;
                req = req.bearer_auth(key);
            }
            match req.send().await {
                Ok(r) if r.status().is_success() => {
                    format!(
                        "Uji koneksi {name} berhasil (HTTP {}).",
                        r.status().as_u16()
                    )
                }
                Ok(r) => format!("{NOTE_REMOTE_FAILED} (HTTP {})", r.status().as_u16()),
                Err(_) => NOTE_REMOTE_FAILED.to_owned(),
            }
        }
        None => NOTE_STDIO.to_owned(),
    };
    let corr = correlation(&headers);
    let mut tx = state.pool.begin().await?;
    let row: McpRow = sqlx::query_as(
        "UPDATE mcp_servers SET tested_at = now(), last_test_note = $3, version = version + 1, \
            updated_at = now() WHERE workspace_id = $1 AND id = $2 \
         RETURNING id, name, transport, endpoint, command, auth_env, enabled, \
           to_char(tested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS tested_at, \
           last_test_note, version",
    )
    .bind(access.workspace_id)
    .bind(id)
    .bind(&note)
    .fetch_one(&mut *tx)
    .await?;
    audit_entity(
        &mut tx,
        access.workspace_id,
        Some(access.user_id),
        "ai.mcp.test",
        "mcp_server",
        id,
        json!({ "name": name }),
        &corr,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(McpView::from(row)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names_are_restricted() {
        assert!(valid_env_name("OPENAI_API_KEY"));
        assert!(valid_env_name("ANTHROPIC_KEY_2"));
        assert!(!valid_env_name("openai_key"));
        assert!(!valid_env_name("1OPENAI"));
        assert!(!valid_env_name("OPENAI-KEY"));
        assert!(!valid_env_name("A"));
    }

    #[test]
    fn only_https_base_urls_are_accepted() {
        assert!(https_url("https://api.openai.com/").is_ok());
        assert!(https_url("http://api.openai.com").is_err());
        assert!(https_url("ftp://example.com").is_err());
    }
}
