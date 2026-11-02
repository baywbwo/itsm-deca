//! Per-process request rate limiting (token bucket, bounded memory).
//!
//! Identity: an authenticated request is keyed by its user (all of a user's sessions share one
//! bucket, so opening more sessions does not raise the limit), reads and writes separately.
//! A request without a session is keyed by the peer address of the TCP connection;
//! `X-Forwarded-For` is honoured only when that peer is a configured trusted proxy.
//!
//! Limits live in this process only. With N replicas a client can reach N times the limit;
//! a shared store is needed before scaling out (recorded in the gap register).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::config::RateLimitConfig;
use crate::error::AppError;
use crate::session::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Read(Uuid),
    Write(Uuid),
    /// No session: the client address (after trusted-proxy resolution).
    Anon(IpAddr),
    /// No session and no peer address known (only in-process callers such as tests).
    /// Shared by every such caller: fail-safe, never unlimited.
    AnonUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { retry_after_secs: u64 },
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<Key, Bucket>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        RateLimiter {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn per_min(&self, key: &Key) -> f64 {
        f64::from(match key {
            Key::Read(_) => self.config.read_per_min,
            Key::Write(_) => self.config.write_per_min,
            Key::Anon(_) | Key::AnonUnknown => self.config.anon_per_min,
        })
    }

    /// Number of buckets currently held (never above `max_keys`).
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Bucket>> {
        // The map holds plain numbers; a panic elsewhere cannot leave it inconsistent.
        self.buckets.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes one token for `key` at `now`. Never blocks on I/O; the lock is held only for
    /// arithmetic (and, when the map is full, one pass to drop refilled or oldest buckets).
    pub fn check(&self, key: Key, now: Instant) -> Decision {
        let capacity = self.per_min(&key);
        let rate = capacity / 60.0;
        let mut buckets = self.lock();
        if !buckets.contains_key(&key) && buckets.len() >= self.config.max_keys {
            // A fully refilled bucket is the same as a new one: dropping it loses nothing.
            buckets.retain(|k, b| {
                let cap = self.per_min(k);
                b.tokens + now.saturating_duration_since(b.last).as_secs_f64() * cap / 60.0 < cap
            });
            if buckets.len() >= self.config.max_keys
                && let Some(oldest) = buckets.iter().min_by_key(|(_, b)| b.last).map(|(k, _)| *k)
            {
                buckets.remove(&oldest);
                tracing::warn!(rate_limit.evicted = 1, "rate limit key table full");
            }
        }
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: capacity,
            last: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Decision::Allow
        } else {
            let wait = ((1.0 - bucket.tokens) / rate).ceil();
            Decision::Deny {
                retry_after_secs: (wait as u64).max(1),
            }
        }
    }
}

/// The client address: the TCP peer, or, only when the peer is a trusted proxy, the
/// right-most `X-Forwarded-For` entry that is not itself a trusted proxy. A malformed entry
/// stops the walk and the peer is used, so a forged header can never pick a new identity
/// through an untrusted hop.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpAddr]) -> IpAddr {
    if !trusted.contains(&peer) {
        return peer;
    }
    let mut hops: Vec<&str> = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        let Ok(text) = value.to_str() else {
            return peer;
        };
        hops.extend(text.split(',').map(str::trim));
    }
    for hop in hops.iter().rev() {
        match hop.parse::<IpAddr>() {
            Ok(ip) if trusted.contains(&ip) => continue,
            Ok(ip) => return ip,
            Err(_) => return peer,
        }
    }
    peer
}

fn key_for(req: &Request, trusted: &[IpAddr]) -> Key {
    if let Some(session) = req.extensions().get::<Session>() {
        return if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
            Key::Read(session.user_id)
        } else {
            Key::Write(session.user_id)
        };
    }
    match req.extensions().get::<ConnectInfo<SocketAddr>>() {
        Some(ConnectInfo(addr)) => Key::Anon(client_ip(addr.ip(), req.headers(), trusted)),
        None => Key::AnonUnknown,
    }
}

/// Runs after the session layer (so the user is known) and before any handler work.
pub async fn layer(State(limiter): State<Arc<RateLimiter>>, req: Request, next: Next) -> Response {
    let key = key_for(&req, &limiter.config.trusted_proxies);
    match limiter.check(key, Instant::now()) {
        Decision::Allow => next.run(req).await,
        Decision::Deny { retry_after_secs } => {
            let class = match key {
                Key::Read(_) => "read",
                Key::Write(_) => "write",
                Key::Anon(_) | Key::AnonUnknown => "anon",
            };
            tracing::warn!(rate_limit.class = class, retry_after_secs, "rate limited");
            AppError::RateLimited { retry_after_secs }.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(per_min: u32, max_keys: usize) -> RateLimitConfig {
        RateLimitConfig {
            read_per_min: per_min,
            write_per_min: per_min,
            anon_per_min: per_min,
            max_keys,
            trusted_proxies: Vec::new(),
        }
    }

    fn ip(s: &str) -> IpAddr {
        match s.parse() {
            Ok(ip) => ip,
            Err(e) => panic!("{s}: {e}"),
        }
    }

    #[test]
    fn bucket_allows_capacity_then_denies_with_retry_after_then_recovers() {
        let l = RateLimiter::new(cfg(6, 100)); // 6/min = one token per 10 s
        let t0 = Instant::now();
        let k = Key::Write(Uuid::nil());
        for _ in 0..6 {
            assert_eq!(l.check(k, t0), Decision::Allow);
        }
        assert_eq!(
            l.check(k, t0),
            Decision::Deny {
                retry_after_secs: 10
            }
        );
        assert_eq!(
            l.check(k, t0 + Duration::from_secs(4)),
            Decision::Deny {
                retry_after_secs: 6
            }
        );
        assert_eq!(l.check(k, t0 + Duration::from_secs(10)), Decision::Allow);
        assert!(matches!(
            l.check(k, t0 + Duration::from_secs(10)),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn keys_are_isolated_by_user_class_and_peer() {
        let l = RateLimiter::new(cfg(1, 100));
        let t = Instant::now();
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert_eq!(l.check(Key::Write(a), t), Decision::Allow);
        assert!(matches!(l.check(Key::Write(a), t), Decision::Deny { .. }));
        assert_eq!(l.check(Key::Write(b), t), Decision::Allow);
        assert_eq!(l.check(Key::Read(a), t), Decision::Allow);
        assert_eq!(l.check(Key::Anon(ip("10.0.0.1")), t), Decision::Allow);
        assert_eq!(l.check(Key::Anon(ip("10.0.0.2")), t), Decision::Allow);
    }

    #[test]
    fn key_table_never_exceeds_max_keys() {
        let l = RateLimiter::new(cfg(1, 100));
        let t = Instant::now();
        for i in 0..1000u128 {
            let _ = l.check(Key::Write(Uuid::from_u128(i)), t);
            assert!(l.len() <= 100);
        }
        assert_eq!(l.len(), 100);
        // Refilled buckets are pruned first, so a later burst of new keys stays bounded too.
        for i in 0..300u128 {
            let _ = l.check(Key::Read(Uuid::from_u128(i)), t + Duration::from_secs(120));
        }
        assert!(l.len() <= 100);
    }

    #[test]
    fn forwarded_for_is_ignored_unless_the_peer_is_trusted() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("1.2.3.4"),
        );
        let peer = ip("203.0.113.9");
        assert_eq!(client_ip(peer, &h, &[]), peer);
        assert_eq!(client_ip(peer, &h, &[ip("127.0.0.1")]), peer);

        let proxy = ip("127.0.0.1");
        assert_eq!(client_ip(proxy, &h, &[proxy]), ip("1.2.3.4"));
        // A client-supplied left-most value cannot replace the hop the proxy appended.
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("9.9.9.9, 198.51.100.7"),
        );
        assert_eq!(client_ip(proxy, &h, &[proxy]), ip("198.51.100.7"));
        // Garbage stops the walk: the proxy address is used, not a forged value.
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("9.9.9.9, not-an-ip"),
        );
        assert_eq!(client_ip(proxy, &h, &[proxy]), proxy);
        assert_eq!(client_ip(proxy, &HeaderMap::new(), &[proxy]), proxy);
    }
}
