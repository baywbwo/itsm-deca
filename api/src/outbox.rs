//! Outbox delivery worker: live provider calls or simulation, per `ITSM_INTEGRATIONS`.
//!
//! Every delivery attempt goes through the same steps, whether it is started inline after
//! intake, by a manual retry or by the background worker:
//!
//! 1. **Claim** (own transaction): a conditional `UPDATE` takes a pending, unclaimed, due intent,
//!    bumps `attempt`, and stores a claim token with a lease. Concurrent claimers race on the row
//!    lock; exactly one wins, the others see `claim_token IS NOT NULL` and skip.
//! 2. **Attempt** (live only, no transaction): the provider call, bounded by
//!    `ITSM_INTEGRATIONS_TIMEOUT_MS`.
//! 3. **Settle** (own transaction): locks the row, checks the token is still ours, applies the
//!    outcome (provider answer, or the simulated outcome), clears the claim and audits. A token
//!    that no longer matches means the lease was lost; nothing is written.
//!
//! A claim whose lease expires without a settle (process crash, hang) is moved to `unknown` by
//! [`sweep_expired`]: the provider may or may not have received it, so it is never retried
//! blindly and must be reconciled with evidence. Only a `transient` outcome (definite
//! non-delivery, e.g. the provider refused before accepting) is retried automatically, with
//! bounded attempts and exponential backoff; when the budget runs out the intent becomes `failed`.

use std::time::Duration;

use sqlx::PgConnection;
use tokio::sync::watch;
use uuid::Uuid;

use crate::AppState;
use crate::config::IntegrationsMode;
use crate::error::AppError;
use crate::live::{self, Attempt};
use crate::providers::IssueRef;
use crate::routes::sync::{Intent, audit_entity, link_issue, lock_intent};

/// How long a claim may stay in flight before the sweeper declares the outcome unknown.
pub const LEASE: Duration = Duration::from_secs(30);
/// Automatic attempts for transient failures before the intent is marked failed.
pub const MAX_AUTO_ATTEMPTS: i32 = 5;
/// Fresh intents are left to the inline dispatch for this long before the worker takes them.
pub const INLINE_GRACE: Duration = Duration::from_secs(3);
/// Intents claimed per worker tick.
pub const BATCH: i64 = 20;

const BACKOFF_BASE_SECS: u64 = 2;
const BACKOFF_CAP_SECS: u64 = 60;

pub const LEASE_EXPIRED_ERROR: &str = "Proses pengiriman terhenti sebelum hasil diketahui. Jangan kirim ulang; rekonsiliasi dengan bukti.";

/// Delay before automatic attempt `n + 1` after `n` transient failures (`n >= 1`):
/// 2s, 4s, 8s, 16s, 32s, then capped at 60s.
pub fn backoff(auto_attempts: i32) -> Duration {
    let n = auto_attempts.max(1) as u32 - 1;
    let secs = BACKOFF_BASE_SECS
        .saturating_mul(1u64.checked_shl(n).unwrap_or(u64::MAX))
        .min(BACKOFF_CAP_SECS);
    Duration::from_secs(secs)
}

/// A held claim on one intent.
#[derive(Debug, Clone, Copy)]
pub struct Claim {
    pub workspace_id: Uuid,
    pub id: Uuid,
    pub token: Uuid,
}

/// Result of a settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    Success,
    Failed,
    Unknown,
    /// Transient failure; stays pending until `next_attempt_at`.
    Rescheduled,
    /// The claim was no longer ours (lease swept or already settled); nothing written.
    LeaseLost,
}

fn interval(d: Duration) -> String {
    format!("{} milliseconds", d.as_millis())
}

/// Claims one specific intent if it is pending, unclaimed and due.
pub async fn claim_one(
    conn: &mut PgConnection,
    ws: Uuid,
    id: Uuid,
) -> Result<Option<Claim>, AppError> {
    let token: Option<Uuid> = sqlx::query_scalar(
        "UPDATE integration_outbox \
         SET attempt = attempt + 1, auto_attempts = auto_attempts + 1, \
             claim_token = gen_random_uuid(), claimed_until = now() + $3::interval, \
             next_attempt_at = NULL \
         WHERE workspace_id = $1 AND id = $2 AND state = 'pending' AND claim_token IS NULL \
           AND (next_attempt_at IS NULL OR next_attempt_at <= now()) \
         RETURNING claim_token",
    )
    .bind(ws)
    .bind(id)
    .bind(interval(LEASE))
    .fetch_optional(&mut *conn)
    .await?;
    Ok(token.map(|token| Claim {
        workspace_id: ws,
        id,
        token,
    }))
}

/// Claims up to `limit` due intents across workspaces. Rows locked by another claimer are
/// skipped, so parallel workers never take the same intent.
pub async fn claim_due(conn: &mut PgConnection, limit: i64) -> Result<Vec<Claim>, AppError> {
    let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "WITH due AS ( \
           SELECT id FROM integration_outbox \
           WHERE state = 'pending' AND claim_token IS NULL \
             AND ((next_attempt_at IS NOT NULL AND next_attempt_at <= now()) \
               OR (next_attempt_at IS NULL AND created_at <= now() - $2::interval)) \
           ORDER BY COALESCE(next_attempt_at, created_at), id \
           LIMIT $1 FOR UPDATE SKIP LOCKED) \
         UPDATE integration_outbox o \
         SET attempt = o.attempt + 1, auto_attempts = o.auto_attempts + 1, \
             claim_token = gen_random_uuid(), claimed_until = now() + $3::interval, \
             next_attempt_at = NULL \
         FROM due WHERE o.id = due.id \
         RETURNING o.workspace_id, o.id, o.claim_token",
    )
    .bind(limit)
    .bind(interval(INLINE_GRACE))
    .bind(interval(LEASE))
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(workspace_id, id, token)| Claim {
            workspace_id,
            id,
            token,
        })
        .collect())
}

/// What one attempt achieved, before the retry budget is applied.
enum Outcome {
    /// Accepted. Jira: the issue to link (`None` when nothing new to link).
    Success(Option<IssueRef>),
    Failed(String),
    Unknown(String),
    /// Definitely not accepted; retried automatically while the budget lasts.
    Transient(String),
}

const INACTIVE_ERROR: &str =
    "Koneksi integrasi tidak aktif; tidak dikirim. Aktifkan koneksi lalu coba ulang.";
const MODE_MISMATCH_ERROR: &str = "Koneksi integrasi dikonfigurasi untuk mode server yang lain; tidak dikirim. \
     Simpan konfigurasi, uji dan aktifkan ulang koneksi lalu coba ulang.";

/// Simulation (`ITSM_INTEGRATIONS=simulation`): the outcome configured on the connection.
fn simulated(intent: &Intent, id: Uuid) -> Outcome {
    if intent.outcome == "inactive" {
        return Outcome::Failed(INACTIVE_ERROR.to_owned());
    }
    if intent.mode != "simulation" {
        return Outcome::Failed(MODE_MISMATCH_ERROR.to_owned());
    }
    match intent.outcome.as_str() {
        "failed" => Outcome::Failed(
            "Simulasi: penyedia menolak permintaan. Aman untuk dicoba ulang oleh petugas berizin."
                .to_owned(),
        ),
        "unknown" => Outcome::Unknown(
            "Simulasi: hasil tidak diketahui (batas waktu). Jangan kirim ulang; rekonsiliasi dengan bukti."
                .to_owned(),
        ),
        "transient" => Outcome::Transient(
            "Simulasi: gangguan sementara, permintaan belum diterima penyedia.".to_owned(),
        ),
        _ if intent.operation == "jira_create_issue" => Outcome::Success(Some(IssueRef {
            id: format!("sim-{id}"),
            key: format!("SIM-{}-{}", intent.ws_code, intent.number),
            status: Some("Simulasi".to_owned()),
            url: None,
        })),
        _ => Outcome::Success(None),
    }
}

fn from_live(a: Attempt) -> Outcome {
    match a {
        Attempt::Delivered(issue) => Outcome::Success(issue),
        Attempt::Inactive => Outcome::Failed(INACTIVE_ERROR.to_owned()),
        Attempt::ModeMismatch => Outcome::Failed(MODE_MISMATCH_ERROR.to_owned()),
        Attempt::Failed(e) => Outcome::Failed(e),
        Attempt::Unknown(e) => Outcome::Unknown(e),
        Attempt::Transient(e) => Outcome::Transient(e),
    }
}

/// Attempts a claimed intent and records the outcome. In live mode the provider call runs first,
/// with no database lock held; the settle transaction then checks the claim token is still ours.
/// `actor` is the requesting user for inline dispatch and manual retry, `None` for the worker.
pub async fn settle(
    state: &AppState,
    claim: Claim,
    actor: Option<Uuid>,
    corr: &Option<String>,
) -> Result<Settled, AppError> {
    let ws = claim.workspace_id;
    let id = claim.id;
    let mode = state.config.integrations.mode;
    let live = match mode {
        IntegrationsMode::Live => Some(live::attempt(state, ws, id, claim.token).await),
        IntegrationsMode::Simulation => None,
    };
    let mut tx = state.pool.begin().await?;
    let intent = lock_intent(&mut tx, ws, id).await?;
    if intent.state != "pending" || intent.claim_token != Some(claim.token) {
        tracing::warn!(workspace_id = %ws, outbox_id = %id, "outbox claim lost before settle");
        return Ok(Settled::LeaseLost);
    }
    let outcome = match live {
        Some(a) => from_live(a),
        None => simulated(&intent, id),
    };
    let (next, settled, error, retry_in) = match outcome {
        Outcome::Success(issue) => {
            if let Some(issue) = issue
                && intent.related_work_id.is_some()
            {
                link_issue(
                    &mut tx,
                    ws,
                    &intent,
                    &issue.id,
                    &issue.key,
                    issue.status.as_deref(),
                    issue.url.as_deref(),
                )
                .await?;
            }
            ("success", Settled::Success, None, None)
        }
        Outcome::Failed(e) => ("failed", Settled::Failed, Some(e), None),
        Outcome::Unknown(e) => ("unknown", Settled::Unknown, Some(e), None),
        Outcome::Transient(e) if intent.auto_attempts < MAX_AUTO_ATTEMPTS => (
            "pending",
            Settled::Rescheduled,
            Some(format!(
                "{e} Dicoba ulang otomatis ({} dari {}).",
                intent.auto_attempts, MAX_AUTO_ATTEMPTS
            )),
            Some(backoff(intent.auto_attempts)),
        ),
        Outcome::Transient(e) => (
            "failed",
            Settled::Failed,
            Some(format!(
                "{e} Batas {MAX_AUTO_ATTEMPTS} percobaan otomatis habis. Petugas berizin boleh mencoba ulang."
            )),
            None,
        ),
    };
    let error: Option<String> = error.map(|e| e.chars().take(1000).collect());
    let next_attempt_at: Option<String> = sqlx::query_scalar(
        "UPDATE integration_outbox \
         SET state = $3, safe_error = $4, claim_token = NULL, claimed_until = NULL, \
             next_attempt_at = CASE WHEN $5::interval IS NULL THEN NULL ELSE now() + $5::interval END \
         WHERE workspace_id = $1 AND id = $2 \
         RETURNING to_char(next_attempt_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .bind(ws)
    .bind(id)
    .bind(next)
    .bind(&error)
    .bind(retry_in.map(interval))
    .fetch_one(&mut *tx)
    .await?;
    audit_entity(
        &mut tx,
        ws,
        actor,
        "integration.sync",
        "integration_outbox",
        id,
        serde_json::json!({
            "ticketId": intent.ticket_id,
            "operation": intent.operation,
            "state": next,
            "attempt": intent.attempt,
            "autoAttempt": intent.auto_attempts,
            "nextAttemptAt": next_attempt_at,
            "by": if actor.is_some() { "request" } else { "worker" },
            "mode": mode.as_str(),
        }),
        corr,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(workspace_id = %ws, outbox_id = %id, state = next, mode = mode.as_str(), "delivery settled");
    Ok(settled)
}

/// Moves claims whose lease expired to `unknown` (never retried blindly) and audits each.
pub async fn sweep_expired(state: &AppState) -> Result<u64, AppError> {
    let mut tx = state.pool.begin().await?;
    let rows: Vec<(Uuid, Uuid, Uuid, i32)> = sqlx::query_as(
        "WITH expired AS ( \
           SELECT id FROM integration_outbox \
           WHERE state = 'pending' AND claim_token IS NOT NULL AND claimed_until < now() \
           FOR UPDATE SKIP LOCKED) \
         UPDATE integration_outbox o \
         SET state = 'unknown', claim_token = NULL, claimed_until = NULL, next_attempt_at = NULL, \
             safe_error = $1 \
         FROM expired WHERE o.id = expired.id \
         RETURNING o.workspace_id, o.id, o.ticket_id, o.attempt",
    )
    .bind(LEASE_EXPIRED_ERROR)
    .fetch_all(&mut *tx)
    .await?;
    for (ws, id, ticket, attempt) in &rows {
        audit_entity(
            &mut tx,
            *ws,
            None,
            "integration.lease_expired",
            "integration_outbox",
            *id,
            serde_json::json!({
                "ticketId": ticket,
                "from": "pending",
                "state": "unknown",
                "attempt": attempt,
                "by": "worker",
                "mode": state.config.integrations.mode.as_str(),
            }),
            &None,
        )
        .await?;
    }
    tx.commit().await?;
    for (ws, id, _, _) in &rows {
        tracing::warn!(workspace_id = %ws, outbox_id = %id, "outbox lease expired; marked unknown");
    }
    Ok(rows.len() as u64)
}

/// Outcome counts of one worker tick.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub expired: u64,
    pub claimed: usize,
    pub settled: usize,
}

/// One worker tick: sweep expired leases, claim due intents, settle each.
pub async fn run_once(state: &AppState) -> Result<Tick, AppError> {
    let expired = sweep_expired(state).await?;
    let claims = {
        let mut conn = state.pool.acquire().await?;
        claim_due(&mut conn, BATCH).await?
    };
    let mut tick = Tick {
        expired,
        claimed: claims.len(),
        settled: 0,
    };
    for claim in claims {
        match settle(state, claim, None, &None).await {
            Ok(Settled::LeaseLost) => {}
            Ok(_) => tick.settled += 1,
            // Left claimed; the sweeper turns it into unknown after the lease.
            Err(e) => tracing::warn!(outbox_id = %claim.id, error = ?e, "worker settle failed"),
        }
    }
    Ok(tick)
}

/// Runs the worker until `shutdown` flips to true. A tick in progress finishes first.
pub fn spawn(state: AppState, mut shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    let poll = Duration::from_millis(state.config.outbox_poll_ms);
    tokio::spawn(async move {
        tracing::info!(
            poll_ms = poll.as_millis() as u64,
            mode = state.config.integrations.mode.as_str(),
            "outbox worker started"
        );
        loop {
            match run_once(&state).await {
                Ok(t) if t.expired > 0 || t.claimed > 0 => {
                    tracing::info!(
                        expired = t.expired,
                        claimed = t.claimed,
                        settled = t.settled,
                        "outbox tick"
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = ?e, "outbox tick failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(poll) => {}
                _ = shutdown.changed() => break,
            }
            if *shutdown.borrow() {
                break;
            }
        }
        tracing::info!("outbox worker stopped");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let secs: Vec<u64> = (1..=8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, vec![2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff(0).as_secs(), 2);
        assert_eq!(backoff(i32::MAX).as_secs(), 60);
    }
}
