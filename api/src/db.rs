//! Pool construction, migrations and the test-database marker guard.

use std::str::FromStr;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::config::{AccessMode, Config, DatabaseUrl};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("database belum ditandai sebagai database uji; mode uji Superadmin ditolak")]
    NotTestDatabase,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

pub async fn connect(url: &DatabaseUrl) -> Result<PgPool, sqlx::Error> {
    let opts = PgConnectOptions::from_str(url.expose())?
        .application_name("itsm-api")
        .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(opts)
        .await
}

/// True only when this database was explicitly marked as a dummy test database.
pub async fn is_marked_test_database(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let marked: Option<bool> = sqlx::query_scalar(
        "SELECT kind = 'test_dummy' AND marked_db = current_database() \
         FROM itsm_deployment_marker WHERE singleton",
    )
    .fetch_optional(pool)
    .await?;
    Ok(marked == Some(true))
}

/// Startup guard: UAT Superadmin mode requires a marked test database.
pub async fn verify_runtime_guard(cfg: &Config, pool: &PgPool) -> Result<(), GuardError> {
    match cfg.access_mode {
        AccessMode::UatSuperadmin => {
            if is_marked_test_database(pool).await? {
                Ok(())
            } else {
                Err(GuardError::NotTestDatabase)
            }
        }
        // Config already refuses SSO; keep this arm fail-closed if that ever changes.
        AccessMode::Sso => Err(GuardError::NotTestDatabase),
        AccessMode::Password => {
            let expected = if cfg.environment == crate::config::Environment::Production {
                "operational"
            } else {
                "test_dummy"
            };
            let valid: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM itsm_deployment_marker WHERE kind = $1 AND marked_db = current_database())")
                .bind(expected).fetch_one(pool).await?;
            if valid {
                Ok(())
            } else {
                Err(GuardError::NotTestDatabase)
            }
        }
    }
}
