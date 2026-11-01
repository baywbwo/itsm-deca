//! Runtime configuration. Every security-relevant value must be set explicitly;
//! anything missing, unknown or contradictory refuses to start (fail closed).

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Uat,
    Production,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Limited UAT: direct Superadmin entry with a persistent banner, dummy data only.
    UatSuperadmin,
    /// Office SSO. Not implemented yet, so it can never start.
    Sso,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessMode::UatSuperadmin => "uat_superadmin",
            AccessMode::Sso => "sso",
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
            (Environment::Uat, AccessMode::UatSuperadmin) => {}
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
        })
    }
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
}
