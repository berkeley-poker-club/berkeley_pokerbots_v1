//! `tournament-worker`: pulls jobs from the shared store and runs them.

use anyhow::Result;
use clap::Parser;
use competition_platform::{config::PlatformConfig, init_tracing, Services};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "tournament-worker",
    version,
    about = "Pokerbots tournament worker"
)]
struct Cli {
    /// Path to pokerbots.toml
    #[arg(long, default_value = "pokerbots.toml")]
    config: PathBuf,
    /// Jobs to run concurrently (overrides worker.concurrency).
    #[arg(long)]
    concurrency: Option<usize>,
    /// Restrict to these job types (comma-separated).
    #[arg(long)]
    job_types: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing("info");
    if let Some(n) = table_runner::raise_fd_limit() {
        tracing::debug!(open_files = n, "raised fd limit");
    }
    let mut cfg = PlatformConfig::load(Some(&cli.config))?;
    if let Some(c) = cli.concurrency {
        cfg.worker.concurrency = c;
    }
    if let Some(t) = cli.job_types {
        cfg.worker.job_types = t
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let services = Arc::new(Services::open(cfg)?);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = services.worker();
    let handle = tokio::spawn(worker.run(shutdown_rx));
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down worker (waiting for in-flight jobs)");
    let _ = shutdown_tx.send(true);
    let _ = handle.await;
    Ok(())
}
