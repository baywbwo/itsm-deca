//! `itsm-api [serve|migrate|seed-uat]`. Exits non-zero on any guard failure.

use std::process::ExitCode;
use std::sync::Arc;

use itsm_api::config::Config;
use itsm_api::{AppState, app, db, seed};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_owned());
    match run(&command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            tracing::error!(command = %command, reason = %msg, "refusing to continue");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: &str) -> Result<(), String> {
    let config = Config::from_env().map_err(|e| e.to_string())?;
    let pool = db::connect(&config.database_url)
        .await
        .map_err(|e| format!("database connect failed: {e}"))?;

    match command {
        "migrate" => db::MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| format!("migration failed: {e}")),
        "seed-uat" => {
            let confirm = std::env::var("ITSM_SEED_CONFIRM_DB")
                .map_err(|_| "ITSM_SEED_CONFIRM_DB wajib diisi".to_owned())?;
            seed::seed_uat(&pool, &confirm)
                .await
                .map_err(|e| e.to_string())
        }
        "serve" => serve(config, pool).await,
        _ => Err("perintah tidak dikenal".to_owned()),
    }
}

async fn serve(config: Config, pool: sqlx::PgPool) -> Result<(), String> {
    db::verify_runtime_guard(&config, &pool)
        .await
        .map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .map_err(|e| format!("bind failed: {e}"))?;
    tracing::info!(
        bind = %config.bind,
        access_mode = config.access_mode.as_str(),
        "itsm-api listening"
    );
    let state = AppState {
        config: Arc::new(config),
        pool,
    };
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| format!("server error: {e}"))
}
