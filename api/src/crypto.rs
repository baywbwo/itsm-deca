//! Secret encryption (R1 I-1): AES-256-GCM with keys from the environment.
//!
//! `ITSM_KEYRING` = `key_id:base64(32 bytes)` entries separated by commas; `ITSM_KEYRING_ACTIVE`
//! = the key_id used for new ciphertext. Older keys stay listed until rotation re-encrypts their
//! rows. Keys and plaintext never reach the database, logs or responses: `Debug` is redacted.

use std::collections::BTreeMap;
use std::fmt;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use zeroize::Zeroize;

pub const ALGORITHM: &str = "aes-256-gcm";
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyringError {
    #[error("ITSM_KEYRING tidak diisi")]
    Missing,
    #[error("ITSM_KEYRING entri ke-{0} tidak valid (format key_id:base64 32 byte)")]
    BadEntry(usize),
    #[error("ITSM_KEYRING memuat key_id ganda")]
    Duplicate,
    #[error("ITSM_KEYRING_ACTIVE tidak ada di ITSM_KEYRING")]
    ActiveMissing,
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("kunci {0} tidak ada di keyring")]
    UnknownKey(String),
    #[error("secret tidak ditemukan")]
    NotFound,
    #[error("secret gagal didekripsi")]
    Decrypt,
    #[error("secret kosong atau terlalu besar")]
    Size,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// What a secret is for; bound into the AEAD associated data so rows cannot be swapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    TotpSeed,
    LlmApiKey,
    McpAuth,
    ImapPassword,
    SmtpPassword,
    /// CP5b: Jira Cloud API token (Basic auth with the configured account email).
    JiraApiToken,
    /// CP5b: Lark custom-app secret (exchanged for a tenant_access_token).
    LarkAppSecret,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TotpSeed => "totp_seed",
            Self::LlmApiKey => "llm_api_key",
            Self::McpAuth => "mcp_auth",
            Self::ImapPassword => "imap_password",
            Self::SmtpPassword => "smtp_password",
            Self::JiraApiToken => "jira_api_token",
            Self::LarkAppSecret => "lark_app_secret",
        }
    }
}

/// Decrypted secret bytes. Never printed; wiped on drop.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(<redacted>)")
    }
}

pub struct Keyring {
    keys: BTreeMap<String, Aes256Gcm>,
    active: String,
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyring")
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .field("active", &self.active)
            .finish()
    }
}

fn valid_key_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Keyring {
    pub fn parse(keyring: Option<&str>, active: Option<&str>) -> Result<Self, KeyringError> {
        let raw = keyring
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(KeyringError::Missing)?;
        let mut keys = BTreeMap::new();
        for (i, entry) in raw.split(',').enumerate() {
            let (id, b64) = entry
                .trim()
                .split_once(':')
                .ok_or(KeyringError::BadEntry(i + 1))?;
            let bytes = STANDARD
                .decode(b64.trim())
                .map_err(|_| KeyringError::BadEntry(i + 1))?;
            if !valid_key_id(id) || bytes.len() != 32 {
                return Err(KeyringError::BadEntry(i + 1));
            }
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&bytes));
            if keys.insert(id.to_owned(), cipher).is_some() {
                return Err(KeyringError::Duplicate);
            }
        }
        let active = active
            .map(str::trim)
            .filter(|a| keys.contains_key(*a))
            .ok_or(KeyringError::ActiveMissing)?
            .to_owned();
        Ok(Self { keys, active })
    }

    pub fn from_env() -> Result<Self, KeyringError> {
        Self::parse(
            std::env::var("ITSM_KEYRING").ok().as_deref(),
            std::env::var("ITSM_KEYRING_ACTIVE").ok().as_deref(),
        )
    }

    pub fn active_key_id(&self) -> &str {
        &self.active
    }

    /// Encrypts with the active key and a fresh random nonce.
    pub fn seal(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<(String, Vec<u8>, Vec<u8>), SecretError> {
        if plaintext.is_empty() || plaintext.len() > 16_000 {
            return Err(SecretError::Size);
        }
        let cipher = &self.keys[&self.active];
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| SecretError::Decrypt)?;
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| SecretError::Decrypt)?;
        Ok((self.active.clone(), nonce.to_vec(), ct))
    }

    pub fn open(
        &self,
        key_id: &str,
        nonce: &[u8],
        ct: &[u8],
        aad: &[u8],
    ) -> Result<SecretBytes, SecretError> {
        let cipher = self
            .keys
            .get(key_id)
            .ok_or_else(|| SecretError::UnknownKey(key_id.to_owned()))?;
        if nonce.len() != NONCE_LEN {
            return Err(SecretError::Decrypt);
        }
        cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
            .map(SecretBytes)
            .map_err(|_| SecretError::Decrypt)
    }
}

fn aad(id: Uuid, workspace_id: Option<Uuid>, purpose: Purpose) -> Vec<u8> {
    let ws = workspace_id.map(|w| w.to_string()).unwrap_or_default();
    format!("itsm-secret/v1|{id}|{ws}|{}", purpose.as_str()).into_bytes()
}

/// Stores a new secret and returns its id. The caller owns the transaction and the audit row.
pub async fn store_secret(
    conn: &mut PgConnection,
    keyring: &Keyring,
    workspace_id: Option<Uuid>,
    purpose: Purpose,
    plaintext: &[u8],
    created_by: Option<Uuid>,
) -> Result<Uuid, SecretError> {
    let id = Uuid::new_v4();
    let (key_id, nonce, ct) = keyring.seal(plaintext, &aad(id, workspace_id, purpose))?;
    sqlx::query(
        "INSERT INTO encrypted_secrets (id, workspace_id, purpose, key_id, algorithm, nonce, ciphertext, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(purpose.as_str())
    .bind(key_id)
    .bind(ALGORITHM)
    .bind(nonce)
    .bind(ct)
    .bind(created_by)
    .execute(conn)
    .await?;
    Ok(id)
}

pub async fn load_secret(
    pool: &PgPool,
    keyring: &Keyring,
    id: Uuid,
    workspace_id: Option<Uuid>,
    purpose: Purpose,
) -> Result<SecretBytes, SecretError> {
    let row: Option<(String, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT key_id, nonce, ciphertext FROM encrypted_secrets \
         WHERE id = $1 AND workspace_id IS NOT DISTINCT FROM $2 AND purpose = $3 AND algorithm = $4",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(purpose.as_str())
    .bind(ALGORITHM)
    .fetch_optional(pool)
    .await?;
    let (key_id, nonce, ct) = row.ok_or(SecretError::NotFound)?;
    keyring.open(&key_id, &nonce, &ct, &aad(id, workspace_id, purpose))
}

// id, workspace_id, purpose, key_id, nonce, ciphertext
type StaleRow = (Uuid, Option<Uuid>, String, String, Vec<u8>, Vec<u8>);

/// Re-encrypts rows sealed with a non-active key (rotation job). Returns the number re-sealed.
pub async fn reseal_stale(pool: &PgPool, keyring: &Keyring) -> Result<u64, SecretError> {
    let rows: Vec<StaleRow> = sqlx::query_as(
        "SELECT id, workspace_id, purpose, key_id, nonce, ciphertext FROM encrypted_secrets \
         WHERE key_id <> $1 AND algorithm = $2 ORDER BY id LIMIT 500",
    )
    .bind(keyring.active_key_id())
    .bind(ALGORITHM)
    .fetch_all(pool)
    .await?;
    let mut n = 0;
    for (id, ws, purpose, key_id, nonce, ct) in rows {
        let purpose = match purpose.as_str() {
            "totp_seed" => Purpose::TotpSeed,
            "llm_api_key" => Purpose::LlmApiKey,
            "mcp_auth" => Purpose::McpAuth,
            "imap_password" => Purpose::ImapPassword,
            "smtp_password" => Purpose::SmtpPassword,
            "jira_api_token" => Purpose::JiraApiToken,
            "lark_app_secret" => Purpose::LarkAppSecret,
            // The DB CHECK lists every purpose; an unknown one stays untouched rather than being
            // re-sealed under the wrong associated data.
            _ => continue,
        };
        let a = aad(id, ws, purpose);
        let plain = keyring.open(&key_id, &nonce, &ct, &a)?;
        let (new_key, new_nonce, new_ct) = keyring.seal(plain.expose(), &a)?;
        n += sqlx::query(
            "UPDATE encrypted_secrets SET key_id = $2, nonce = $3, ciphertext = $4, \
             version = version + 1, rotated_at = now() WHERE id = $1 AND key_id = $5",
        )
        .bind(id)
        .bind(new_key)
        .bind(new_nonce)
        .bind(new_ct)
        .bind(key_id)
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const K1: &str = "k1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const K2: &str = "k2:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    #[test]
    fn keyring_rejects_missing_short_duplicate_and_unknown_active() {
        assert_eq!(
            Keyring::parse(None, Some("k1")).unwrap_err(),
            KeyringError::Missing
        );
        assert_eq!(
            Keyring::parse(Some("k1:AAAA"), Some("k1")).unwrap_err(),
            KeyringError::BadEntry(1)
        );
        assert_eq!(
            Keyring::parse(Some(&format!("{K1},{K1}")), Some("k1")).unwrap_err(),
            KeyringError::Duplicate
        );
        assert_eq!(
            Keyring::parse(Some(K1), Some("k9")).unwrap_err(),
            KeyringError::ActiveMissing
        );
        assert_eq!(
            Keyring::parse(
                Some("K1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
                Some("K1")
            )
            .unwrap_err(),
            KeyringError::BadEntry(1)
        );
    }

    #[test]
    fn seal_open_round_trip_binds_aad_and_never_reuses_nonce() {
        let kr = Keyring::parse(Some(&format!("{K1},{K2}")), Some("k2")).unwrap();
        let (kid, n1, ct) = kr.seal(b"dummy-secret", b"ctx").unwrap();
        assert_eq!(kid, "k2");
        assert_eq!(
            kr.open(&kid, &n1, &ct, b"ctx").unwrap().expose(),
            b"dummy-secret"
        );
        assert!(kr.open(&kid, &n1, &ct, b"other").is_err());
        assert!(kr.open("k1", &n1, &ct, b"ctx").is_err());
        let (_, n2, _) = kr.seal(b"dummy-secret", b"ctx").unwrap();
        assert_ne!(n1, n2);
        assert!(!format!("{kr:?}").contains("AAAA"));
        assert_eq!(
            format!("{:?}", kr.open(&kid, &n1, &ct, b"ctx").unwrap()),
            "SecretBytes(<redacted>)"
        );
    }
}
