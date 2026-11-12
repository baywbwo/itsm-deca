//! `itsm-api [serve|migrate|init-password|seed-uat|reseal [--dry-run]]`. Exits non-zero on any
//! guard failure.

use std::process::ExitCode;
use std::sync::Arc;

use itsm_api::config::Config;
use itsm_api::{AppState, app, db, outbox, seed};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".to_owned());
    let rest: Vec<String> = args.collect();
    match run(&command, &rest).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            tracing::error!(command = %command, reason = %msg, "refusing to continue");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: &str, rest: &[String]) -> Result<(), String> {
    let dry_run = match (command, rest) {
        (_, []) => false,
        ("reseal", [flag]) if flag == "--dry-run" => true,
        _ => return Err("argumen tidak dikenal".to_owned()),
    };
    let config = Config::from_env().map_err(|e| e.to_string())?;
    let pool = db::connect(&config.database_url)
        .await
        .map_err(|e| format!("database connect failed: {e}"))?;

    match command {
        "migrate" => db::MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| format!("migration failed: {e}")),
        "init-password" => itsm_api::password_bootstrap::run(&pool, &config).await,
        "seed-uat" => {
            if config.environment != itsm_api::config::Environment::Uat
                || config.access_mode != itsm_api::config::AccessMode::UatSuperadmin
            {
                return Err("seed-uat only allowed in explicit UAT Superadmin mode".into());
            }
            let confirm = std::env::var("ITSM_SEED_CONFIRM_DB")
                .map_err(|_| "ITSM_SEED_CONFIRM_DB wajib diisi".to_owned())?;
            seed::seed_uat(&pool, &confirm)
                .await
                .map_err(|e| e.to_string())?;
            let n = seed::seed_uat_tickets(&pool, &confirm)
                .await
                .map_err(|e| e.to_string())?;
            println!("seed-uat: {n} tiket dummy baru");
            Ok(())
        }
        // CP5B-03 key rotation; see deploy/RUNBOOK-keyring.md.
        "reseal" => {
            let keyring = config
                .keyring
                .as_deref()
                .ok_or("keyring belum dipasang (ITSM_KEYRING_FILE)")?;
            let report = itsm_api::crypto::reseal(&pool, keyring, dry_run)
                .await
                .map_err(|e| format!("reseal gagal: {e}"))?;
            let line = report.verdict(dry_run)?;
            println!("{line}");
            Ok(())
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
    // CP6b: where outbound calls go (proxy address only, never its credentials).
    match &config.egress_proxy {
        Some(proxy) => tracing::info!(
            egress_proxy = proxy.url(),
            proxy_auth = proxy.credentials().is_some(),
            integrations = config.integrations.mode.as_str(),
            app_allowlist = "jira *.atlassian.net, lark open.larksuite.com + open.feishu.cn, IdP hosts per company",
            "outbound calls through the egress proxy"
        ),
        None => tracing::info!(
            integrations = config.integrations.mode.as_str(),
            "outbound calls direct (no egress proxy; DNS pinned, private addresses refused)"
        ),
    }
    let state = AppState {
        config: Arc::new(config),
        pool,
    };
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker = state
        .config
        .outbox_worker
        .then(|| outbox::spawn(state.clone(), stopped));
    // Peer address feeds the rate limiter for requests without a session.
    let served = axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|e| format!("server error: {e}"));
    // Let an in-progress tick finish; an unsettled claim is swept to unknown on next start.
    let _ = stop.send(true);
    if let Some(worker) = worker {
        let _ = worker.await;
    }
    served
}
