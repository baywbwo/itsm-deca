//! Pool construction, migrations and the test-database marker guard.

use std::str::FromStr;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::config::{AccessMode, Config, DatabaseUrl, DbPoolConfig};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("database belum ditandai sebagai database uji; mode uji Superadmin ditolak")]
    NotTestDatabase,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error(
        "skema database belum dimigrasi (versi {0} belum diterapkan); jalankan `migrate` sebagai pemilik skema dulu"
    )]
    Pending(i64),
    #[error("migrasi {0} tercatat gagal di database; perbaiki lalu jalankan `migrate` ulang")]
    Dirty(i64),
    #[error("migrasi {0} di database berbeda dari binary ini (checksum)")]
    Mismatch(i64),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// CP8: `serve` and `worker` never migrate in production (the runtime role cannot), so they
/// refuse a schema that lacks any migration embedded in this binary, has a failed one, or has
/// one with a different checksum. A schema with extra, newer migrations is accepted: that is the
/// expand phase of the next release while this one is still rolling.
pub async fn verify_schema(pool: &PgPool) -> Result<(), SchemaError> {
    let applied: Vec<(i64, bool, Vec<u8>)> =
        match sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P01") => Vec::new(),
            Err(e) => return Err(e.into()),
        };
    if let Some((v, _, _)) = applied.iter().find(|(_, ok, _)| !ok) {
        return Err(SchemaError::Dirty(*v));
    }
    for m in MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
    {
        match applied.iter().find(|(v, _, _)| *v == m.version) {
            None => return Err(SchemaError::Pending(m.version)),
            Some((_, _, sum)) if sum.as_slice() != m.checksum.as_ref() => {
                return Err(SchemaError::Mismatch(m.version));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Pool for one process. `application_name` tells API and worker connections apart in
/// `pg_stat_activity` (`itsm-api` / `itsm-worker`).
pub async fn connect(
    url: &DatabaseUrl,
    pool: &DbPoolConfig,
    application_name: &str,
) -> Result<PgPool, sqlx::Error> {
    let opts = PgConnectOptions::from_str(url.expose())?
        .application_name(application_name)
        .options([("statement_timeout", "5000"), ("lock_timeout", "3000")]);
    pool_options(pool).connect_with(opts).await
}

pub fn pool_options(pool: &DbPoolConfig) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(pool.max_connections)
        .acquire_timeout(pool.acquire_timeout)
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
