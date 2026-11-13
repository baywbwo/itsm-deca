//! CP7d: rate limits shared by every API replica (fixed windows in Postgres).
//!
//! The per-process token buckets in [`crate::rate_limit`] stay the first, cheap line for all
//! traffic. With N replicas they allow N times their limit, which is acceptable for ordinary
//! reads and writes but not where a request is expensive or attackable: sign-in (password
//! guessing, Argon2 CPU), AI (provider cost), outbound integration calls (provider quotas) and
//! the Jira webhook. Those are counted here, in one table every replica writes to.
//!
//! Each hit is one `INSERT … ON CONFLICT DO UPDATE … RETURNING` on the current window; the row
//! lock serialises concurrent hits on the same key, so the count is exact across replicas.
//! A fixed window can let up to twice the limit through around a window boundary; that is the
//! documented price of one statement per hit. Identities are stored only as SHA-256 digests.
//!
//! Per-account protection for passwords and MFA codes is the lockout on the credential row
//! (5 failures, 15 minutes), which is already in Postgres and therefore shared too.

use std::net::IpAddr;

use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::AppState;
use crate::config::SharedLimits;
use crate::error::AppError;
use crate::rate_limit::ClientIp;
use crate::session::Session;

/// Expired windows are kept this long before the worker prunes them (longest window + slack).
pub const RETAIN_SECS: i64 = 2 * 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Sign-in endpoints, keyed by client address.
    AuthPerIp,
    AiPerUser,
    AiPerWorkspaceDay,
    /// Connection tests, directory sync and manual retries, keyed by user.
    OutboundPerUser,
    /// Live Jira look-ups behind pickers, keyed by user.
    LookupPerUser,
    /// Jira webhook deliveries, keyed by client address.
    WebhookPerIp,
}

impl Policy {
    pub fn scope(self) -> &'static str {
        match self {
            Policy::AuthPerIp => "auth_ip",
            Policy::AiPerUser => "ai_user",
            Policy::AiPerWorkspaceDay => "ai_workspace_day",
            Policy::OutboundPerUser => "outbound_user",
            Policy::LookupPerUser => "lookup_user",
            Policy::WebhookPerIp => "webhook_ip",
        }
    }

    pub fn window_secs(self) -> i64 {
        match self {
            Policy::AuthPerIp | Policy::OutboundPerUser => 600,
            Policy::AiPerUser | Policy::LookupPerUser | Policy::WebhookPerIp => 60,
            Policy::AiPerWorkspaceDay => 86_400,
        }
    }

    pub fn limit(self, l: &SharedLimits) -> u32 {
        match self {
            Policy::AuthPerIp => l.auth_per_ip_10min,
            Policy::AiPerUser => l.ai_per_user_min,
            Policy::AiPerWorkspaceDay => l.ai_per_workspace_day,
            Policy::OutboundPerUser => l.outbound_per_user_10min,
            Policy::LookupPerUser => l.lookup_per_user_min,
            Policy::WebhookPerIp => l.webhook_per_ip_min,
        }
    }
}

/// Who is counted. Hashed together with the scope before it reaches the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    Ip(IpAddr),
    /// No peer address (in-process callers only): one shared, never-unlimited key.
    UnknownPeer,
    User(Uuid),
    Workspace(Uuid),
}

fn digest(policy: Policy, who: Identity) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(policy.scope().as_bytes());
    h.update([0]);
    match who {
        Identity::Ip(IpAddr::V4(ip)) => {
            h.update(b"ip4");
            h.update(ip.octets());
        }
        Identity::Ip(IpAddr::V6(ip)) => {
            h.update(b"ip6");
            h.update(ip.octets());
        }
        Identity::UnknownPeer => h.update(b"unknown-peer"),
        Identity::User(id) => {
            h.update(b"user");
            h.update(id.as_bytes());
        }
        Identity::Workspace(id) => {
            h.update(b"workspace");
            h.update(id.as_bytes());
        }
    }
    h.finalize().to_vec()
}

/// Counts one request for `who` under `policy`; `RateLimited` (with the seconds left in the
/// window) once the window holds more than the limit. Runs outside any caller transaction, so
/// a request that later fails is still counted.
pub async fn hit(
    pool: &PgPool,
    limits: &SharedLimits,
    policy: Policy,
    who: Identity,
) -> Result<(), AppError> {
    let window = policy.window_secs();
    let (hits, left): (i32, i64) = sqlx::query_as(
        "INSERT INTO rate_limit_windows AS w (scope, key, window_start, hits) \
         VALUES ($1, $2, to_timestamp((floor(extract(epoch FROM clock_timestamp()) / $3) * $3)::float8), 1) \
         ON CONFLICT (scope, key, window_start) DO UPDATE SET hits = LEAST(w.hits, 2147483646) + 1 \
         RETURNING w.hits, ceil(extract(epoch FROM w.window_start - clock_timestamp()) + $3)::bigint",
    )
    .bind(policy.scope())
    .bind(digest(policy, who))
    .bind(window)
    .fetch_one(pool)
    .await?;
    if i64::from(hits) > i64::from(policy.limit(limits)) {
        let retry_after_secs = left.clamp(1, window) as u64;
        tracing::warn!(
            rate_limit.class = policy.scope(),
            retry_after_secs,
            "shared rate limit"
        );
        return Err(AppError::RateLimited { retry_after_secs });
    }
    Ok(())
}

/// Deletes windows that ended long ago (worker tick). Bounded per call.
pub async fn prune(pool: &PgPool) -> Result<u64, AppError> {
    Ok(sqlx::query(
        "DELETE FROM rate_limit_windows WHERE ctid = ANY (ARRAY(\
            SELECT ctid FROM rate_limit_windows \
            WHERE window_start < clock_timestamp() - make_interval(secs => $1) LIMIT 5000))",
    )
    .bind(RETAIN_SECS as f64)
    .execute(pool)
    .await?
    .rows_affected())
}

/// The shared policy of a route (method + template), if any. AI suggestions are counted in
/// their handler instead, after authorisation, so nobody can spend another workspace's quota.
pub fn policy_for(method: &Method, route: &str) -> Option<Policy> {
    const W: &str = "/api/v1/workspaces/{workspace_id}";
    let tail = route.strip_prefix(W);
    match (method.as_str(), route, tail) {
        ("POST", "/api/v1/login" | "/api/v1/login/mfa" | "/api/v1/uat/session", _)
        | ("GET", "/api/v1/auth/oidc/{company_code}/start" | "/api/v1/auth/oidc/callback", _) => {
            Some(Policy::AuthPerIp)
        }
        (
            "POST",
            _,
            Some(
                "/integrations/{connection_id}/test"
                | "/integrations/{connection_id}/directory/sync"
                | "/integration-outbox/{outbox_id}/retry"
                | "/ai-settings/providers/{provider}/test"
                | "/ai-settings/mcp-servers/{server_id}/test",
            ),
        ) => Some(Policy::OutboundPerUser),
        ("GET", _, Some("/integrations/{connection_id}/options" | "/intake/jira-users")) => {
            Some(Policy::LookupPerUser)
        }
        _ => None,
    }
}

/// Route layer (needs the matched template): runs after the session and per-process limiter.
pub async fn layer(
    State(state): State<AppState>,
    route: MatchedPath,
    req: Request,
    next: Next,
) -> Response {
    let Some(policy) = policy_for(req.method(), route.as_str()) else {
        return next.run(req).await;
    };
    let who = match policy {
        Policy::AuthPerIp | Policy::WebhookPerIp => peer(&req),
        _ => match req.extensions().get::<Session>() {
            Some(s) => Identity::User(s.user_id),
            // No session: the handler answers 401 without any outbound call.
            None => return next.run(req).await,
        },
    };
    match hit(&state.pool, &state.config.rate_limit.shared, policy, who).await {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// The client address resolved by the per-process limiter (trusted proxies applied).
pub fn peer(req: &Request) -> Identity {
    match req.extensions().get::<ClientIp>() {
        Some(ClientIp(ip)) => Identity::Ip(*ip),
        None => Identity::UnknownPeer,
    }
}

/// Jira webhook: no session or per-process limiter on that router, so the address is
/// resolved here (same trusted-proxy rules).
pub async fn webhook_layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let who = match crate::rate_limit::resolve_client_ip(
        &req,
        &state.config.rate_limit.trusted_proxies,
    ) {
        Some(ip) => Identity::Ip(ip),
        None => Identity::UnknownPeer,
    };
    match hit(
        &state.pool,
        &state.config.rate_limit.shared,
        Policy::WebhookPerIp,
        who,
    )
    .await
    {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_separate_scopes_and_identity_kinds() {
        let id = Uuid::from_u128(7);
        let a = digest(Policy::AiPerUser, Identity::User(id));
        assert_eq!(a.len(), 32);
        assert_ne!(a, digest(Policy::OutboundPerUser, Identity::User(id)));
        assert_ne!(a, digest(Policy::AiPerUser, Identity::Workspace(id)));
        assert_ne!(
            digest(Policy::AuthPerIp, Identity::Ip(IpAddr::from([10, 0, 0, 1]))),
            digest(Policy::AuthPerIp, Identity::Ip(IpAddr::from([10, 0, 0, 2])))
        );
    }

    #[test]
    fn routes_map_to_their_policy() {
        let p = |m: Method, r: &str| policy_for(&m, r);
        assert_eq!(p(Method::POST, "/api/v1/login"), Some(Policy::AuthPerIp));
        assert_eq!(
            p(Method::POST, "/api/v1/login/mfa"),
            Some(Policy::AuthPerIp)
        );
        assert_eq!(
            p(Method::GET, "/api/v1/auth/oidc/{company_code}/start"),
            Some(Policy::AuthPerIp)
        );
        assert_eq!(
            p(Method::GET, "/api/v1/auth/oidc/callback"),
            Some(Policy::AuthPerIp)
        );
        assert_eq!(
            p(
                Method::POST,
                "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/test"
            ),
            Some(Policy::OutboundPerUser)
        );
        assert_eq!(
            p(
                Method::GET,
                "/api/v1/workspaces/{workspace_id}/intake/jira-users"
            ),
            Some(Policy::LookupPerUser)
        );
        // Ordinary traffic stays on the per-process limiter only.
        assert_eq!(p(Method::GET, "/api/v1/login"), None);
        assert_eq!(
            p(Method::GET, "/api/v1/workspaces/{workspace_id}/tickets"),
            None
        );
        assert_eq!(
            p(
                Method::POST,
                "/api/v1/workspaces/{workspace_id}/ai-suggestions"
            ),
            None
        );
    }
}
