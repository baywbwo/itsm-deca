//! Runtime configuration. Every security-relevant value must be set explicitly;
//! anything missing, unknown or contradictory refuses to start (fail closed).

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Uat,
    Production,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Limited UAT: direct Superadmin entry with a persistent banner, dummy data only.
    UatSuperadmin,
    Password,
    /// Office SSO. Not implemented yet, so it can never start.
    Sso,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessMode::UatSuperadmin => "uat_superadmin",
            AccessMode::Sso => "sso",
            AccessMode::Password => "password",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{0} wajib diisi")]
    Missing(&'static str),
    #[error("{name} bernilai tidak dikenal")]
    Invalid { name: &'static str },
    #[error("production tidak boleh memakai mode uji Superadmin")]
    UatInProduction,
    #[error("mode akses SSO belum tersedia; server menolak start")]
    SsoUnavailable,
    /// CP5B-04: a key secret given as a plain env var in production (visible to `docker
    /// inspect`, `/proc/<pid>/environ` and crash dumps).
    #[error("{0} tidak boleh lewat env di production; pakai {0}_FILE (file secret)")]
    SecretFromEnv(&'static str),
}

/// Database URL wrapper that never prints its value (it may contain credentials).
#[derive(Clone)]
pub struct DatabaseUrl(String);

impl DatabaseUrl {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseUrl(<redacted>)")
    }
}

/// Where clamd listens. Only an operator-provided address is used; nothing is discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClamdAddr {
    Tcp(String),
    Unix(PathBuf),
}

/// Default upload cap (the baseline's proposed 10 MiB; final value is configuration).
pub const DEFAULT_ATTACHMENT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Highest cap the server accepts from configuration.
pub const ATTACHMENT_MAX_BYTES_CEILING: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub environment: Environment,
    pub access_mode: AccessMode,
    pub database_url: DatabaseUrl,
    pub bind: SocketAddr,
    pub cookie_secure: bool,
    /// Root for quarantine and stored attachments. Absent = uploads/downloads refused (503).
    pub attachment_dir: Option<PathBuf>,
    /// clamd address. Absent = every upload ends as scanner_unavailable (503), never bound.
    pub clamd: Option<ClamdAddr>,
    pub attachment_max_bytes: u64,
    /// Background outbox worker (simulated delivery). `ITSM_OUTBOX_WORKER=off` disables it.
    pub outbox_worker: bool,
    /// Worker poll interval in ms (`ITSM_OUTBOX_POLL_MS`, 200–60000, default 2000).
    pub outbox_poll_ms: u64,
    /// Most rows one operational report export may hold (`ITSM_REPORT_MAX_ROWS`, 1–200000,
    /// default 50000). A larger result is recorded as a failed run, never truncated.
    pub report_max_rows: i64,
    /// Per-process request rate limits (see `rate_limit`).
    pub rate_limit: RateLimitConfig,
    /// `ITSM_MFA=on|off` (default off, owner 2026-11-12: plain password login). Off = no login
    /// challenge, no pre-MFA session, no step-up, MFA endpoints answer 404 and the keys below are
    /// neither required nor loaded. Stored factors stay in the database for a later re-enable.
    pub mfa_enabled: bool,
    /// US-1.2 MFA keys (only with `ITSM_MFA=on`): the keyring + `ITSM_KEYRING_ACTIVE` (TOTP seeds) and
    /// the recovery-code HMAC pepper, each read by [`secret_input`]. Required in production; absent in UAT
    /// = MFA endpoints answer 503 and users who must use MFA cannot finish signing in.
    pub mfa: Option<std::sync::Arc<crate::mfa::MfaKeys>>,
    /// CP5b provider integrations (see [`IntegrationsConfig`]).
    pub integrations: IntegrationsConfig,
    /// Secret keyring for stored provider credentials: `ITSM_KEYRING_FILE` (or, outside production,
    /// `ITSM_KEYRING`) + `ITSM_KEYRING_ACTIVE`, loaded whenever set (independent of MFA). Absent = credentials cannot be saved and live
    /// connections cannot be activated (clear 503 `keyring_missing`); set but malformed = no start.
    pub keyring: Option<std::sync::Arc<crate::crypto::Keyring>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationsMode {
    /// Real Jira Cloud / Lark requests (default in production).
    Live,
    /// No network request at all; outcomes are configured per connection (tests, CI, UAT only).
    Simulation,
}

impl IntegrationsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrationsMode::Live => "live",
            IntegrationsMode::Simulation => "simulation",
        }
    }
}

/// Default budget for one provider operation (DNS + every request of a connection test).
pub const DEFAULT_INTEGRATIONS_TIMEOUT_MS: u64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationsConfig {
    /// `ITSM_INTEGRATIONS=live|simulation`. Unset = `simulation` in UAT, `live` in production;
    /// production refuses `simulation`; any other value refuses to start.
    pub mode: IntegrationsMode,
    /// `ITSM_INTEGRATIONS_LAB=on|off` (default off, refused in production): allows `http://` and
    /// loopback/private provider addresses, for local stubs only. Off = https to public IPs only.
    pub lab: bool,
    /// `ITSM_INTEGRATIONS_TIMEOUT_MS` (500–8000, default 5000): budget of one provider operation,
    /// kept below the 10 s request timeout so a connection test always answers.
    pub timeout_ms: u64,
}

/// Token-bucket limits per minute. Each bucket holds up to one minute of requests and refills
/// continuously. Authenticated requests are keyed by user; requests without a session by the
/// trusted peer address. In-memory, per process: N replicas allow up to N times these limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// `ITSM_RATE_LIMIT_READ_PER_MIN`: GET/HEAD per user (1–100000, default 1200).
    pub read_per_min: u32,
    /// `ITSM_RATE_LIMIT_WRITE_PER_MIN`: unsafe methods per user (1–100000, default 120).
    pub write_per_min: u32,
    /// `ITSM_RATE_LIMIT_ANON_PER_MIN`: requests without a session per peer (1–100000, default 30).
    pub anon_per_min: u32,
    /// `ITSM_RATE_LIMIT_MAX_KEYS`: most buckets kept in memory (100–1000000, default 10000).
    pub max_keys: usize,
    /// `ITSM_TRUSTED_PROXIES`: comma-separated exact IPs whose `X-Forwarded-For` is honoured
    /// (at most 16). Empty (default) = `X-Forwarded-For` is ignored.
    pub trusted_proxies: Vec<IpAddr>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            read_per_min: 1200,
            write_per_min: 120,
            anon_per_min: 30,
            max_keys: 10_000,
            trusted_proxies: Vec::new(),
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_map(&std::env::vars().collect())
    }

    pub fn from_map(vars: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let get = |name: &'static str| {
            vars.get(name)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .ok_or(ConfigError::Missing(name))
        };

        let environment = match get("ITSM_ENV")? {
            "uat" => Environment::Uat,
            "production" => Environment::Production,
            _ => return Err(ConfigError::Invalid { name: "ITSM_ENV" }),
        };
        let access_mode = match get("ITSM_ACCESS_MODE")? {
            "uat_superadmin" => AccessMode::UatSuperadmin,
            "sso" => AccessMode::Sso,
            "password" => AccessMode::Password,
            _ => {
                return Err(ConfigError::Invalid {
                    name: "ITSM_ACCESS_MODE",
                });
            }
        };
        match (environment, access_mode) {
            (Environment::Production, AccessMode::UatSuperadmin) => {
                return Err(ConfigError::UatInProduction);
            }
            (_, AccessMode::Sso) => return Err(ConfigError::SsoUnavailable),
            (Environment::Uat, AccessMode::UatSuperadmin) | (_, AccessMode::Password) => {}
        }

        let database_url = DatabaseUrl(get("DATABASE_URL")?.to_owned());
        let bind = match vars.get("ITSM_BIND").map(|v| v.trim()) {
            None | Some("") => SocketAddr::from(([127, 0, 0, 1], 8080)),
            Some(v) => v
                .parse()
                .map_err(|_| ConfigError::Invalid { name: "ITSM_BIND" })?,
        };
        let cookie_secure = match vars.get("ITSM_COOKIE_SECURE").map(|v| v.trim()) {
            None | Some("") | Some("true") => true,
            Some("false") => false,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    name: "ITSM_COOKIE_SECURE",
                });
            }
        };

        if access_mode == AccessMode::Password && !cookie_secure {
            return Err(ConfigError::Invalid {
                name: "ITSM_COOKIE_SECURE",
            });
        }
        let optional = |name: &str| {
            vars.get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let attachment_dir = match optional("ITSM_ATTACHMENT_DIR") {
            None => None,
            Some(v) => {
                let p = PathBuf::from(v);
                if !p.is_absolute() {
                    return Err(ConfigError::Invalid {
                        name: "ITSM_ATTACHMENT_DIR",
                    });
                }
                Some(p)
            }
        };
        let clamd = match optional("ITSM_CLAMD_ADDR") {
            None => None,
            Some(v) => Some(parse_clamd(&v).ok_or(ConfigError::Invalid {
                name: "ITSM_CLAMD_ADDR",
            })?),
        };
        let attachment_max_bytes = match optional("ITSM_ATTACHMENT_MAX_BYTES") {
            None => DEFAULT_ATTACHMENT_MAX_BYTES,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=ATTACHMENT_MAX_BYTES_CEILING).contains(n))
                .ok_or(ConfigError::Invalid {
                    name: "ITSM_ATTACHMENT_MAX_BYTES",
                })?,
        };

        let outbox_worker = match optional("ITSM_OUTBOX_WORKER").as_deref() {
            None | Some("on") => true,
            Some("off") => false,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    name: "ITSM_OUTBOX_WORKER",
                });
            }
        };
        let outbox_poll_ms = match optional("ITSM_OUTBOX_POLL_MS") {
            None => 2000,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| (200..=60_000).contains(n))
                .ok_or(ConfigError::Invalid {
                    name: "ITSM_OUTBOX_POLL_MS",
                })?,
        };

        let report_max_rows = match optional("ITSM_REPORT_MAX_ROWS") {
            None => 50_000,
            Some(v) => v
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=200_000).contains(n))
                .ok_or(ConfigError::Invalid {
                    name: "ITSM_REPORT_MAX_ROWS",
                })?,
        };

        let defaults = RateLimitConfig::default();
        let per_min = |name: &'static str, default: u32| match optional(name) {
            None => Ok(default),
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=100_000).contains(n))
                .ok_or(ConfigError::Invalid { name }),
        };
        let rate_limit = RateLimitConfig {
            read_per_min: per_min("ITSM_RATE_LIMIT_READ_PER_MIN", defaults.read_per_min)?,
            write_per_min: per_min("ITSM_RATE_LIMIT_WRITE_PER_MIN", defaults.write_per_min)?,
            anon_per_min: per_min("ITSM_RATE_LIMIT_ANON_PER_MIN", defaults.anon_per_min)?,
            max_keys: match optional("ITSM_RATE_LIMIT_MAX_KEYS") {
                None => defaults.max_keys,
                Some(v) => v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (100..=1_000_000).contains(n))
                    .ok_or(ConfigError::Invalid {
                        name: "ITSM_RATE_LIMIT_MAX_KEYS",
                    })?,
            },
            trusted_proxies: match optional("ITSM_TRUSTED_PROXIES") {
                None => Vec::new(),
                Some(v) => parse_proxies(&v).ok_or(ConfigError::Invalid {
                    name: "ITSM_TRUSTED_PROXIES",
                })?,
            },
        };

        let mfa_enabled = match optional("ITSM_MFA").as_deref() {
            None | Some("off") => false,
            Some("on") => true,
            Some(_) => return Err(ConfigError::Invalid { name: "ITSM_MFA" }),
        };
        let (keyring_raw, keyring_src) =
            secret_input(vars, "ITSM_KEYRING", "ITSM_KEYRING_FILE", environment)?;
        let keyring_raw = keyring_raw.as_ref().map(|s| s.as_str());
        let keyring_active = optional("ITSM_KEYRING_ACTIVE");
        let mfa = match mfa_enabled {
            false => None,
            true => {
                let (pepper, pepper_src) = secret_input(
                    vars,
                    "ITSM_MFA_RECOVERY_PEPPER",
                    "ITSM_MFA_RECOVERY_PEPPER_FILE",
                    environment,
                )?;
                match (keyring_raw, keyring_active.as_deref(), pepper) {
                    (None, None, None) if environment == Environment::Uat => None,
                    (keyring, active, pepper) => {
                        let keyring = crate::crypto::Keyring::parse(keyring, active)
                            .map_err(|e| keyring_error(e, keyring_src))?;
                        let pepper = pepper.ok_or(ConfigError::Missing(pepper_src))?;
                        Some(std::sync::Arc::new(
                            crate::mfa::MfaKeys::new(keyring, &pepper)
                                .ok_or(ConfigError::Invalid { name: pepper_src })?,
                        ))
                    }
                }
            }
        };

        let integrations = IntegrationsConfig {
            mode: match (optional("ITSM_INTEGRATIONS").as_deref(), environment) {
                (None, Environment::Uat) | (Some("simulation"), Environment::Uat) => {
                    IntegrationsMode::Simulation
                }
                (None, Environment::Production) | (Some("live"), _) => IntegrationsMode::Live,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "ITSM_INTEGRATIONS",
                    });
                }
            },
            lab: match (optional("ITSM_INTEGRATIONS_LAB").as_deref(), environment) {
                (None | Some("off"), _) => false,
                (Some("on"), Environment::Uat) => true,
                _ => {
                    return Err(ConfigError::Invalid {
                        name: "ITSM_INTEGRATIONS_LAB",
                    });
                }
            },
            timeout_ms: match optional("ITSM_INTEGRATIONS_TIMEOUT_MS") {
                None => DEFAULT_INTEGRATIONS_TIMEOUT_MS,
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (500..=8000).contains(n))
                    .ok_or(ConfigError::Invalid {
                        name: "ITSM_INTEGRATIONS_TIMEOUT_MS",
                    })?,
            },
        };
        let keyring = match (keyring_raw, keyring_active.as_deref()) {
            (None, None) => None,
            (keyring, active) => Some(std::sync::Arc::new(
                crate::crypto::Keyring::parse(keyring, active)
                    .map_err(|e| keyring_error(e, keyring_src))?,
            )),
        };

        Ok(Config {
            environment,
            access_mode,
            database_url,
            bind,
            cookie_secure,
            attachment_dir,
            clamd,
            attachment_max_bytes,
            outbox_worker,
            outbox_poll_ms,
            report_max_rows,
            rate_limit,
            mfa_enabled,
            mfa,
            integrations,
            keyring,
        })
    }
}

fn keyring_error(e: crate::crypto::KeyringError, source: &'static str) -> ConfigError {
    match e {
        crate::crypto::KeyringError::Missing => ConfigError::Missing(source),
        crate::crypto::KeyringError::ActiveMissing => ConfigError::Missing("ITSM_KEYRING_ACTIVE"),
        _ => ConfigError::Invalid { name: source },
    }
}

/// Largest secret file accepted; a keyring of many keys is a few KB.
const MAX_SECRET_FILE_BYTES: u64 = 64 * 1024;

/// CP5B-04: a key secret comes from `<name>_FILE` (a mounted Docker/compose secret, read once at
/// start and wiped from memory) or, outside production, from `<name>` itself. Production refuses
/// the plain variable; setting both is ambiguous and refused everywhere. An empty file counts as
/// unset. Also returns the variable name to report in errors.
fn secret_input(
    vars: &HashMap<String, String>,
    name: &'static str,
    file_name: &'static str,
    environment: Environment,
) -> Result<(Option<Zeroizing<String>>, &'static str), ConfigError> {
    let set = |n: &str| vars.get(n).filter(|v| !v.trim().is_empty());
    match (set(file_name), set(name)) {
        (Some(_), Some(_)) => Err(ConfigError::Invalid { name: file_name }),
        (Some(path), None) => read_secret_file(path.trim())
            .map(|v| (v, file_name))
            .ok_or(ConfigError::Invalid { name: file_name }),
        (None, Some(_)) if environment == Environment::Production => {
            Err(ConfigError::SecretFromEnv(name))
        }
        (None, Some(v)) => Ok((Some(Zeroizing::new(v.trim().to_owned())), name)),
        (None, None) if environment == Environment::Production => Ok((None, file_name)),
        (None, None) => Ok((None, name)),
    }
}

/// Reads a secret file: a regular file, at most 64 KB, UTF-8, not writable by other users.
/// `None` = unusable (reported as an invalid `_FILE`); `Some(None)` = empty.
fn read_secret_file(path: &str) -> Option<Option<Zeroizing<String>>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SECRET_FILE_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o002 != 0 {
            return None;
        }
    }
    let bytes = Zeroizing::new(std::fs::read(path).ok()?);
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    Some((!text.is_empty()).then(|| Zeroizing::new(text.to_owned())))
}

/// Comma-separated exact IP addresses (no ranges), 1–16 entries, no blanks.
fn parse_proxies(v: &str) -> Option<Vec<IpAddr>> {
    let ips = v
        .split(',')
        .map(|s| s.trim().parse::<IpAddr>().ok())
        .collect::<Option<Vec<_>>>()?;
    (1..=16).contains(&ips.len()).then_some(ips)
}

/// `tcp://host:port` or `unix:/absolute/path` (also `unix:///absolute/path`).
fn parse_clamd(v: &str) -> Option<ClamdAddr> {
    if let Some(rest) = v.strip_prefix("tcp://") {
        let (host, port) = rest.rsplit_once(':')?;
        let port_ok = port.parse::<u16>().ok().filter(|p| *p > 0).is_some();
        let host_ok = !host.is_empty()
            && host.bytes().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'[' | b']' | b':')
            });
        return (port_ok && host_ok).then(|| ClamdAddr::Tcp(rest.to_owned()));
    }
    let path = v
        .strip_prefix("unix://")
        .or_else(|| v.strip_prefix("unix:"))?;
    let p = PathBuf::from(path);
    p.is_absolute().then_some(ClamdAddr::Unix(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("ITSM_ENV", "uat"),
            ("ITSM_ACCESS_MODE", "uat_superadmin"),
            ("DATABASE_URL", "postgres://u:secret@localhost/db"),
        ]
    }

    #[test]
    fn valid_uat_config_loads_with_secure_cookie_by_default() {
        let cfg = Config::from_map(&vars(&base())).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.cookie_secure && c.environment == Environment::Uat));
    }

    #[test]
    fn empty_environment_is_rejected() {
        assert_eq!(
            Config::from_map(&HashMap::new()).err(),
            Some(ConfigError::Missing("ITSM_ENV"))
        );
    }

    #[test]
    fn each_required_variable_is_enforced() {
        for name in ["ITSM_ENV", "ITSM_ACCESS_MODE", "DATABASE_URL"] {
            let v: Vec<_> = base().into_iter().filter(|(k, _)| *k != name).collect();
            assert_eq!(
                Config::from_map(&vars(&v)).err(),
                Some(ConfigError::Missing(name))
            );
        }
    }

    #[test]
    fn node_env_style_values_do_not_unlock_anything() {
        let mut v = base();
        v[0] = ("ITSM_ENV", "development");
        v.push(("NODE_ENV", "test"));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Invalid { name: "ITSM_ENV" })
        );
    }

    const KEY: &str = "k1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const PEPPER: &str = "cGVwcGVyLXBlcHBlci1wZXBwZXItcGVwcGVyLXBlcHBlcg==";

    /// Writes a 0600 secret file unique to this test process and tag; returns its path.
    fn secret_file(tag: &str, content: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("itsm-cfg-{}-{tag}", std::process::id()));
        assert!(std::fs::write(&path, content).is_ok());
        assert!(std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).is_ok());
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn production_password_mode_requires_mfa_keys() {
        let prod = vec![
            ("ITSM_ENV", "production"),
            ("ITSM_ACCESS_MODE", "password"),
            ("DATABASE_URL", "postgres://x"),
            ("ITSM_MFA", "on"),
        ];
        assert_eq!(
            Config::from_map(&vars(&prod)).err(),
            Some(ConfigError::Missing("ITSM_KEYRING_FILE"))
        );
        let key_file = secret_file("mfa-key", KEY);
        let short_file = secret_file("mfa-short", "c2hvcnQ=");
        let pepper_file = secret_file("mfa-pepper", PEPPER);
        let mut v = prod.clone();
        v.push(("ITSM_KEYRING_FILE", &key_file));
        v.push(("ITSM_KEYRING_ACTIVE", "k1"));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Missing("ITSM_MFA_RECOVERY_PEPPER_FILE"))
        );
        v.push(("ITSM_MFA_RECOVERY_PEPPER_FILE", &short_file));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Invalid {
                name: "ITSM_MFA_RECOVERY_PEPPER_FILE"
            })
        );
        v.pop();
        v.push(("ITSM_MFA_RECOVERY_PEPPER_FILE", &pepper_file));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.mfa.is_some()));
        let printed = format!("{cfg:?}");
        assert!(!printed.contains(PEPPER) && !printed.contains("AAAAAAAA"));
        for f in [key_file, short_file, pepper_file] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn uat_without_mfa_keys_starts_but_partial_keys_are_refused() {
        let mut on = base();
        on.push(("ITSM_MFA", "on"));
        let cfg = Config::from_map(&vars(&on)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.mfa.is_none() && c.mfa_enabled));
        let mut v = on.clone();
        v.push(("ITSM_MFA_RECOVERY_PEPPER", PEPPER));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Missing("ITSM_KEYRING"))
        );
        let mut v = on.clone();
        v.push(("ITSM_KEYRING", "k1:bad"));
        v.push(("ITSM_KEYRING_ACTIVE", "k1"));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Invalid {
                name: "ITSM_KEYRING"
            })
        );
    }

    #[test]
    fn mfa_is_off_by_default_and_then_needs_no_keys() {
        let prod = vec![
            ("ITSM_ENV", "production"),
            ("ITSM_ACCESS_MODE", "password"),
            ("DATABASE_URL", "postgres://x"),
        ];
        let cfg = Config::from_map(&vars(&prod)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if !c.mfa_enabled && c.mfa.is_none()));
        let mut v = prod.clone();
        v.push(("ITSM_MFA", "off"));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if !c.mfa_enabled && c.mfa.is_none()));
        let mut v = prod.clone();
        v.push(("ITSM_MFA", "yes"));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Invalid { name: "ITSM_MFA" })
        );
    }

    #[test]
    fn production_rejects_uat_superadmin() {
        let mut v = base();
        v[0] = ("ITSM_ENV", "production");
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::UatInProduction)
        );
    }

    #[test]
    fn sso_is_not_available_in_any_environment() {
        for env in ["uat", "production"] {
            let v = vec![
                ("ITSM_ENV", env),
                ("ITSM_ACCESS_MODE", "sso"),
                ("DATABASE_URL", "postgres://x"),
            ];
            assert_eq!(
                Config::from_map(&vars(&v)).err(),
                Some(ConfigError::SsoUnavailable)
            );
        }
    }

    #[test]
    fn unknown_values_are_rejected() {
        let mut v = base();
        v[1] = ("ITSM_ACCESS_MODE", "open");
        assert!(Config::from_map(&vars(&v)).is_err());
        let mut v = base();
        v.push(("ITSM_COOKIE_SECURE", "yes"));
        assert!(Config::from_map(&vars(&v)).is_err());
    }

    #[test]
    fn database_url_is_redacted_in_debug_output() {
        let cfg = Config::from_map(&vars(&base())).map_err(|e| e.to_string());
        let printed = format!("{cfg:?}");
        assert!(!printed.contains("secret"));
        assert!(printed.contains("<redacted>"));
    }

    #[test]
    fn attachment_settings_are_optional_and_fail_closed_when_malformed() {
        let cfg = Config::from_map(&vars(&base())).map_err(|e| e.to_string());
        assert!(
            matches!(&cfg, Ok(c) if c.attachment_dir.is_none() && c.clamd.is_none()
            && c.attachment_max_bytes == DEFAULT_ATTACHMENT_MAX_BYTES)
        );

        // D13 compose default: an empty address = no scanner (uploads refused), not a start error.
        let mut v = base();
        v.push(("ITSM_CLAMD_ADDR", ""));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.clamd.is_none()));

        let mut v = base();
        v.push(("ITSM_ATTACHMENT_DIR", "/srv/itsm/att"));
        v.push(("ITSM_CLAMD_ADDR", "tcp://127.0.0.1:3310"));
        v.push(("ITSM_ATTACHMENT_MAX_BYTES", "1048576"));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(
            matches!(&cfg, Ok(c) if c.clamd == Some(ClamdAddr::Tcp("127.0.0.1:3310".into()))
            && c.attachment_max_bytes == 1_048_576)
        );
        assert_eq!(
            parse_clamd("unix:///run/clamd.sock"),
            Some(ClamdAddr::Unix(PathBuf::from("/run/clamd.sock")))
        );

        for (name, bad) in [
            ("ITSM_ATTACHMENT_DIR", "relative/dir"),
            ("ITSM_CLAMD_ADDR", "127.0.0.1:3310"),
            ("ITSM_CLAMD_ADDR", "tcp://host:0"),
            ("ITSM_CLAMD_ADDR", "unix:relative.sock"),
            ("ITSM_ATTACHMENT_MAX_BYTES", "0"),
            ("ITSM_OUTBOX_WORKER", "yes"),
            ("ITSM_OUTBOX_POLL_MS", "10"),
            ("ITSM_ATTACHMENT_MAX_BYTES", "999999999"),
        ] {
            let mut v = base();
            v.push((name, bad));
            assert_eq!(
                Config::from_map(&vars(&v)).err(),
                Some(ConfigError::Invalid { name }),
                "{name}={bad}"
            );
        }
    }

    #[test]
    fn rate_limits_default_and_reject_out_of_range_values() {
        let cfg = Config::from_map(&vars(&base())).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.rate_limit == RateLimitConfig::default()));

        let mut v = base();
        v.push(("ITSM_RATE_LIMIT_READ_PER_MIN", "600"));
        v.push(("ITSM_RATE_LIMIT_WRITE_PER_MIN", "60"));
        v.push(("ITSM_RATE_LIMIT_ANON_PER_MIN", "5"));
        v.push(("ITSM_RATE_LIMIT_MAX_KEYS", "500"));
        v.push(("ITSM_TRUSTED_PROXIES", "127.0.0.1, ::1"));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.rate_limit.read_per_min == 600
            && c.rate_limit.write_per_min == 60
            && c.rate_limit.anon_per_min == 5
            && c.rate_limit.max_keys == 500
            && c.rate_limit.trusted_proxies.len() == 2));

        for (name, bad) in [
            ("ITSM_RATE_LIMIT_READ_PER_MIN", "0"),
            ("ITSM_RATE_LIMIT_READ_PER_MIN", "100001"),
            ("ITSM_RATE_LIMIT_WRITE_PER_MIN", "-1"),
            ("ITSM_RATE_LIMIT_ANON_PER_MIN", "ten"),
            ("ITSM_RATE_LIMIT_MAX_KEYS", "99"),
            ("ITSM_RATE_LIMIT_MAX_KEYS", "1000001"),
            ("ITSM_TRUSTED_PROXIES", "10.0.0.0/8"),
            ("ITSM_TRUSTED_PROXIES", "127.0.0.1,,::1"),
            ("ITSM_TRUSTED_PROXIES", "proxy.local"),
        ] {
            let mut v = base();
            v.push((name, bad));
            assert_eq!(
                Config::from_map(&vars(&v)).err(),
                Some(ConfigError::Invalid { name }),
                "{name}={bad}"
            );
        }
    }

    #[test]
    fn integrations_mode_defaults_per_environment_and_fails_closed() {
        let cfg = Config::from_map(&vars(&base())).map_err(|e| e.to_string());
        assert!(
            matches!(&cfg, Ok(c) if c.integrations.mode == IntegrationsMode::Simulation
            && !c.integrations.lab
            && c.integrations.timeout_ms == DEFAULT_INTEGRATIONS_TIMEOUT_MS
            && c.keyring.is_none())
        );
        let prod = vec![
            ("ITSM_ENV", "production"),
            ("ITSM_ACCESS_MODE", "password"),
            ("DATABASE_URL", "postgres://x"),
        ];
        let cfg = Config::from_map(&vars(&prod)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.integrations.mode == IntegrationsMode::Live));
        let mut v = base();
        v.push(("ITSM_INTEGRATIONS", "live"));
        v.push(("ITSM_INTEGRATIONS_LAB", "on"));
        v.push(("ITSM_INTEGRATIONS_TIMEOUT_MS", "800"));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(
            matches!(&cfg, Ok(c) if c.integrations.mode == IntegrationsMode::Live
            && c.integrations.lab && c.integrations.timeout_ms == 800)
        );

        for (env, name, bad) in [
            (&prod, "ITSM_INTEGRATIONS", "simulation"),
            (&prod, "ITSM_INTEGRATIONS_LAB", "on"),
            (&base(), "ITSM_INTEGRATIONS", "off"),
            (&base(), "ITSM_INTEGRATIONS", "LIVE"),
            (&base(), "ITSM_INTEGRATIONS_LAB", "yes"),
            (&base(), "ITSM_INTEGRATIONS_TIMEOUT_MS", "499"),
            (&base(), "ITSM_INTEGRATIONS_TIMEOUT_MS", "8001"),
            (&base(), "ITSM_KEYRING", "k1:bad"),
        ] {
            let mut v = env.clone();
            v.push((name, bad));
            if name == "ITSM_KEYRING" {
                v.push(("ITSM_KEYRING_ACTIVE", "k1"));
            }
            assert_eq!(
                Config::from_map(&vars(&v)).err(),
                Some(ConfigError::Invalid { name }),
                "{name}={bad}"
            );
        }

        // The credential keyring loads without MFA and never prints its key material.
        let key_file = secret_file("integrations-key", KEY);
        let mut v = prod.clone();
        v.push(("ITSM_KEYRING_FILE", &key_file));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Missing("ITSM_KEYRING_ACTIVE"))
        );
        v.push(("ITSM_KEYRING_ACTIVE", "k1"));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.keyring.is_some() && c.mfa.is_none()));
        assert!(!format!("{cfg:?}").contains("AAAAAAAA"));
        let _ = std::fs::remove_file(key_file);
    }

    #[test]
    fn cp5b04_production_takes_key_secrets_only_from_files() {
        use std::os::unix::fs::PermissionsExt;
        let prod = vec![
            ("ITSM_ENV", "production"),
            ("ITSM_ACCESS_MODE", "password"),
            ("DATABASE_URL", "postgres://x"),
            ("ITSM_KEYRING_ACTIVE", "k1"),
        ];
        // Plain env vars are visible through docker inspect and /proc: refused in production.
        let mut v = prod.clone();
        v.push(("ITSM_KEYRING", KEY));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::SecretFromEnv("ITSM_KEYRING"))
        );
        let mut v = prod.clone();
        v.push(("ITSM_MFA", "on"));
        v.push(("ITSM_MFA_RECOVERY_PEPPER", PEPPER));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::SecretFromEnv("ITSM_MFA_RECOVERY_PEPPER"))
        );

        // A mounted secret file loads (surrounding whitespace ignored) and never prints.
        let good = secret_file("cp5b04-good", &format!("  {KEY}\n"));
        let mut v = prod.clone();
        v.push(("ITSM_KEYRING_FILE", &good));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(
            matches!(&cfg, Ok(c) if c.keyring.as_ref().is_some_and(|k| k.active_key_id() == "k1"))
        );
        assert!(!format!("{cfg:?}").contains("AAAAAAAA"));

        // Empty file = unset (live integrations then answer keyring_missing).
        let empty = secret_file("cp5b04-empty", "\n");
        let mut v = prod.clone();
        v.pop();
        v.push(("ITSM_KEYRING_FILE", &empty));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.keyring.is_none()));

        // Unusable files refuse to start, named by the _FILE variable.
        let bad = secret_file("cp5b04-bad", "k1:bad");
        let open = secret_file("cp5b04-open", KEY);
        assert!(std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o666)).is_ok());
        let missing = format!("{}-missing", good);
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        for path in [&bad, &open, &missing, &dir] {
            let mut v = prod.clone();
            v.push(("ITSM_KEYRING_FILE", path));
            assert_eq!(
                Config::from_map(&vars(&v)).err(),
                Some(ConfigError::Invalid {
                    name: "ITSM_KEYRING_FILE"
                }),
                "{path}"
            );
        }

        // UAT still accepts the env var, but never both sources at once.
        let mut v = base();
        v.push(("ITSM_KEYRING", KEY));
        v.push(("ITSM_KEYRING_ACTIVE", "k1"));
        let cfg = Config::from_map(&vars(&v)).map_err(|e| e.to_string());
        assert!(matches!(&cfg, Ok(c) if c.keyring.is_some()));
        v.push(("ITSM_KEYRING_FILE", &good));
        assert_eq!(
            Config::from_map(&vars(&v)).err(),
            Some(ConfigError::Invalid {
                name: "ITSM_KEYRING_FILE"
            })
        );
        for f in [good, empty, bad, open] {
            let _ = std::fs::remove_file(f);
        }
    }
}
