//! US-1.2 / TM-02: TOTP (RFC 6238: HMAC-SHA1, 6 digits, 30 s, ±1 step) and recovery codes.
//!
//! Built only from RustCrypto primitives already in the tree (`hmac`, `sha1`, `sha2`); no custom
//! cryptography. Seeds are sealed with the secret keyring (`crypto`); recovery codes are kept as
//! HMAC-SHA256 with a pepper from the environment. Nothing here logs or prints a secret.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::crypto::Keyring;

pub const SEED_LEN: usize = 20;
pub const DIGITS: u32 = 6;
pub const PERIOD_SECS: u64 = 30;
pub const ISSUER: &str = "ITSM DECA";
pub const RECOVERY_CODE_COUNT: usize = 10;
/// TM-02 (d): wrong codes before the TOTP factor locks, and for how long.
pub const MAX_FAILED: i32 = 5;
pub const LOCK_MINUTES: i32 = 15;
pub const CHALLENGE_TTL_MINUTES: i32 = 5;
pub const ENROLLMENT_TTL_MINUTES: i32 = 10;
const MIN_PEPPER_LEN: usize = 32;

/// Seed keyring and recovery-code pepper. `Debug` never prints key material.
pub struct MfaKeys {
    pub keyring: Keyring,
    pepper: Vec<u8>,
}

impl MfaKeys {
    /// `pepper_b64` is standard or URL-safe base64 of at least 32 bytes.
    pub fn new(keyring: Keyring, pepper_b64: &str) -> Option<Self> {
        let pepper = STANDARD
            .decode(pepper_b64.trim())
            .or_else(|_| URL_SAFE_NO_PAD.decode(pepper_b64.trim()))
            .ok()
            .filter(|p| p.len() >= MIN_PEPPER_LEN)?;
        Some(Self { keyring, pepper })
    }

    /// HMAC-SHA256 of a normalised recovery code (see [`normalize_recovery_code`]).
    pub fn recovery_hash(&self, normalized: &str) -> Vec<u8> {
        // HMAC accepts keys of any length; the empty fallback can never match a stored hash.
        let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(&self.pepper) else {
            return Vec::new();
        };
        mac.update(b"itsm-recovery/v1|");
        mac.update(normalized.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

impl Drop for MfaKeys {
    fn drop(&mut self) {
        self.pepper.zeroize();
    }
}

impl fmt::Debug for MfaKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MfaKeys")
            .field("keyring", &self.keyring)
            .field("pepper", &"<redacted>")
            .finish()
    }
}

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding (the form authenticator apps expect).
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes unpadded RFC 4648 base32 (upper case). `None` on any other character.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = B32.iter().position(|&x| x == c)? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// The TOTP value for one time step, zero-padded to six digits.
pub fn code_at(secret: &[u8], step: u64) -> String {
    // HMAC accepts keys of any length; an empty result never equals a six-digit code.
    let Ok(mut mac) = <Hmac<Sha1> as Mac>::new_from_slice(secret) else {
        return String::new();
    };
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = usize::from(h[19] & 0x0f);
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    format!("{:06}", bin % 10u32.pow(DIGITS))
}

pub fn now_step() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / PERIOD_SECS)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotpFailure {
    /// Not six digits or no match in the ±1 window.
    BadCode,
    /// Matches only a step already used (ASVS 2.8.4).
    Replay,
}

impl TotpFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadCode => "bad_code",
            Self::Replay => "replay",
        }
    }
}

/// Checks `code` against steps `now-1..=now+1`; returns the matched step, which must be above
/// `last_used_step`. Every candidate is compared in constant time.
pub fn verify_totp(
    secret: &[u8],
    code: &str,
    now: u64,
    last_used_step: Option<i64>,
) -> Result<u64, TotpFailure> {
    let code = code.trim();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TotpFailure::BadCode);
    }
    let floor = last_used_step.and_then(|s| u64::try_from(s).ok());
    let mut matched: Option<u64> = None;
    let mut replay = false;
    for step in now.saturating_sub(1)..=now + 1 {
        let hit: bool = code_at(secret, step)
            .as_bytes()
            .ct_eq(code.as_bytes())
            .into();
        if hit {
            if floor.is_some_and(|f| step <= f) {
                replay = true;
            } else {
                matched = Some(step);
            }
        }
    }
    match (matched, replay) {
        (Some(step), _) => Ok(step),
        (None, true) => Err(TotpFailure::Replay),
        (None, false) => Err(TotpFailure::BadCode),
    }
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `otpauth://totp/…` for authenticator apps. Contains the seed: shown once, never logged.
pub fn otpauth_uri(secret: &[u8], account: &str) -> String {
    let issuer = percent_encode(ISSUER);
    format!(
        "otpauth://totp/{issuer}:{}?secret={}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={PERIOD_SECS}",
        percent_encode(account),
        base32_encode(secret)
    )
}

/// A fresh recovery code: 80 random bits, shown as `XXXX-XXXX-XXXX-XXXX` (base32).
pub fn new_recovery_code() -> Option<String> {
    let mut raw = [0u8; 10];
    getrandom::fill(&mut raw).ok()?;
    let s = base32_encode(&raw);
    raw.zeroize();
    Some(format!(
        "{}-{}-{}-{}",
        &s[0..4],
        &s[4..8],
        &s[8..12],
        &s[12..16]
    ))
}

/// Upper case, separators removed; `None` unless exactly 16 base32 characters remain.
pub fn normalize_recovery_code(input: &str) -> Option<String> {
    let s: String = input
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (s.len() == 16 && s.bytes().all(|b| B32.contains(&b))).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SEED: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc6238_sha1_vectors_truncated_to_six_digits() {
        for (t, want) in [
            (59u64, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(code_at(RFC_SEED, t / PERIOD_SECS), want, "t={t}");
        }
    }

    #[test]
    fn base32_round_trips_and_matches_rfc4648() {
        assert_eq!(base32_encode(RFC_SEED), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("MZXW6YTBOI").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(base32_decode("mzxw"), None);
    }

    #[test]
    fn window_is_plus_minus_one_and_replay_is_refused() {
        let now = 1_000_000;
        let prev = code_at(RFC_SEED, now - 1);
        let next = code_at(RFC_SEED, now + 1);
        let far = code_at(RFC_SEED, now + 2);
        assert_eq!(verify_totp(RFC_SEED, &prev, now, None), Ok(now - 1));
        assert_eq!(verify_totp(RFC_SEED, &next, now, None), Ok(now + 1));
        if far != prev && far != next && far != code_at(RFC_SEED, now) {
            assert_eq!(
                verify_totp(RFC_SEED, &far, now, None),
                Err(TotpFailure::BadCode)
            );
        }
        let cur = code_at(RFC_SEED, now);
        assert_eq!(
            verify_totp(RFC_SEED, &cur, now, Some(now as i64)),
            Err(TotpFailure::Replay)
        );
        assert_eq!(
            verify_totp(RFC_SEED, "12345a", now, None),
            Err(TotpFailure::BadCode)
        );
        assert_eq!(
            verify_totp(RFC_SEED, "1234567", now, None),
            Err(TotpFailure::BadCode)
        );
    }

    #[test]
    fn otpauth_uri_carries_parameters_and_encodes_the_label() {
        let uri = otpauth_uri(RFC_SEED, "dummy:cs alfa");
        assert!(uri.starts_with("otpauth://totp/ITSM%20DECA:dummy%3Acs%20alfa?secret="));
        for p in [
            "algorithm=SHA1",
            "digits=6",
            "period=30",
            "issuer=ITSM%20DECA",
        ] {
            assert!(uri.contains(p), "{p}");
        }
    }

    #[test]
    fn recovery_codes_are_random_and_normalise() {
        let a = new_recovery_code().unwrap_or_default();
        let b = new_recovery_code().unwrap_or_default();
        assert_eq!(a.len(), 19);
        assert_ne!(a, b);
        let lower = a.to_lowercase().replace('-', " ");
        assert_eq!(normalize_recovery_code(&lower), Some(a.replace('-', "")));
        assert_eq!(normalize_recovery_code("ABCD-EFGH"), None);
        assert_eq!(normalize_recovery_code("ABCD-EFGH-IJKL-MNO1"), None);
    }

    #[test]
    fn keys_need_a_long_pepper_and_hide_it() {
        let ring = || {
            Keyring::parse(
                Some("k1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
                Some("k1"),
            )
        };
        let short = ring().ok().and_then(|r| MfaKeys::new(r, "c2hvcnQ="));
        assert!(short.is_none());
        let pepper = STANDARD.encode([7u8; 32]);
        let keys = ring().ok().and_then(|r| MfaKeys::new(r, &pepper));
        assert!(keys.is_some());
        if let Some(k) = keys {
            assert_eq!(k.recovery_hash("AAAA").len(), 32);
            assert_ne!(k.recovery_hash("AAAA"), k.recovery_hash("AAAB"));
            assert!(!format!("{k:?}").contains(&pepper));
        }
    }
}
