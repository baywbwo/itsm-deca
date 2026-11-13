//! `itsm-api [serve|worker|migrate|init-password|seed-uat|reseal [--dry-run]]`. Exits non-zero
//! on any guard failure. `worker` runs the outbox worker without the HTTP API (CP7b).

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
    let application_name = if command == "worker" {
        "itsm-worker"
    } else {
        "itsm-api"
    };
    let pool = db::connect(&config.database_url, &config.db_pool, application_name)
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
        "serve" => serve(config, pool, Role::Api).await,
        "worker" => serve(config, pool, Role::Worker).await,
        _ => Err("perintah tidak dikenal".to_owned()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// HTTP API (+ the outbox worker unless `ITSM_OUTBOX_WORKER=off`).
    Api,
    /// Outbox worker only: no API listener. Any number may run; claims use SKIP LOCKED leases.
    Worker,
}

async fn serve(mut config: Config, pool: sqlx::PgPool, role: Role) -> Result<(), String> {
    // CP8: migrations are a separate step (production runs them as the schema owner).
    db::verify_schema(&pool).await.map_err(|e| e.to_string())?;
    db::verify_runtime_guard(&config, &pool)
        .await
        .map_err(|e| e.to_string())?;
    let listener = match role {
        Role::Api => {
            let l = tokio::net::TcpListener::bind(config.bind)
                .await
                .map_err(|e| format!("bind failed: {e}"))?;
            tracing::info!(
                bind = %config.bind,
                access_mode = config.access_mode.as_str(),
                db_max_connections = config.db_pool.max_connections,
                outbox_worker = config.outbox_worker,
                "itsm-api listening"
            );
            Some(l)
        }
        Role::Worker => {
            // The worker process exists to run the worker; the flag is for API processes.
            config.outbox_worker = true;
            tracing::info!(
                access_mode = config.access_mode.as_str(),
                db_max_connections = config.db_pool.max_connections,
                "itsm-worker starting (no API listener)"
            );
            None
        }
    };
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
    // CP7a: metrics on their own listener (internal network only), bound before serving so a
    // wrong address stops the start instead of failing silently later.
    let metrics_listener = match config.metrics_bind {
        Some(addr) => {
            let l = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|e| format!("metrics bind failed: {e}"))?;
            tracing::info!(metrics_bind = %addr, "metrics listening (/metrics)");
            Some(l)
        }
        None => None,
    };
    let state = AppState {
        config: Arc::new(config),
        pool,
    };
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let stop = Arc::new(stop);
    // SIGTERM (docker stop, orchestrators) or Ctrl-C: stop accepting, finish in-flight requests.
    tokio::spawn({
        let stop = stop.clone();
        async move {
            let signal = shutdown_signal().await;
            tracing::info!(signal, "shutdown requested");
            let _ = stop.send(true);
        }
    });
    let worker = state
        .config
        .outbox_worker
        .then(|| outbox::spawn(state.clone(), stopped.clone()));
    let metrics = metrics_listener.map(|l| {
        let app = itsm_api::observe::metrics_app(state.clone());
        let stopped = stopped.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(l, app)
                .with_graceful_shutdown(wait_stop(stopped))
                .await
            {
                tracing::error!(error = %e, "metrics server stopped");
            }
        })
    });
    let served = match listener {
        // Peer address feeds the rate limiter for requests without a session.
        Some(listener) => axum::serve(
            listener,
            app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(wait_stop(stopped))
        .await
        .map_err(|e| format!("server error: {e}")),
        None => {
            wait_stop(stopped).await;
            Ok(())
        }
    };
    // Let an in-progress tick finish; an unsettled claim is swept to unknown on next start.
    let _ = stop.send(true);
    if let Some(worker) = worker {
        let _ = worker.await;
    }
    if let Some(metrics) = metrics {
        let _ = metrics.await;
    }
    served
}

async fn wait_stop(mut stopped: tokio::sync::watch::Receiver<bool>) {
    let _ = stopped.wait_for(|stop| *stop).await;
}

/// Resolves on SIGTERM or SIGINT (Ctrl-C) and names which one.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = tokio::signal::ctrl_c() => "SIGINT",
            },
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler unavailable; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}
