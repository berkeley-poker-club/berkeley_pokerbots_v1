//! `competition-api`: HTTP API server (+ optional embedded workers and the nightly scheduler).

use anyhow::{Context, Result};
use clap::Parser;
use competition_platform::{config::PlatformConfig, init_tracing, scheduler, Services};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "competition-api",
    version,
    about = "Pokerbots competition API server"
)]
struct Cli {
    /// Path to pokerbots.toml
    #[arg(long, default_value = "pokerbots.toml")]
    config: PathBuf,
    /// Override bind address.
    #[arg(long)]
    bind: Option<String>,
    /// Override number of embedded workers.
    #[arg(long)]
    workers: Option<usize>,
    /// Print an example configuration file and exit.
    #[arg(long)]
    print_example_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.print_example_config {
        print!("{}", PlatformConfig::example_toml());
        return Ok(());
    }
    init_tracing("info,tower_http=info");
    let mut cfg = PlatformConfig::load(Some(&cli.config))?;
    if let Some(b) = cli.bind {
        cfg.server.bind = b;
    }
    if let Some(w) = cli.workers {
        cfg.server.embedded_workers = w;
    }
    if cfg.server.admin_key.is_none() {
        tracing::warn!("no admin key configured (server.admin_key / POKERBOTS_ADMIN_KEY); admin routes only accept admin teams");
    }
    let services = Arc::new(Services::open(cfg.clone())?);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let mut tasks = Vec::new();
    for _ in 0..cfg.server.embedded_workers {
        let w = services.worker();
        tasks.push(tokio::spawn(w.run(shutdown_rx.clone())));
    }
    if cfg.scheduler.enabled {
        tasks.push(tokio::spawn(scheduler::run_scheduler(
            Arc::clone(&services.store),
            cfg.scheduler.poll_secs,
            shutdown_rx.clone(),
        )));
    }

    let app = competition_platform::api::router(services.app_state());
    let listener = tokio::net::TcpListener::bind(&cfg.server.bind)
        .await
        .with_context(|| format!("binding {}", cfg.server.bind))?;
    tracing::info!(bind = %cfg.server.bind, workers = cfg.server.embedded_workers, sandbox = %services.sandbox.describe(), "competition-api listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    let _ = shutdown_tx.send(true);
    for t in tasks {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(30), t).await;
    }
    Ok(())
}
