//! `competition-platform`: the Pokerbots competition service.
//!
//! * [`store`] — SQLite persistence (teams, submissions, runs, tournaments, placements, jobs).
//! * [`artifacts`] — bot artifact storage and unpacking.
//! * [`sandbox`] — how bots are launched (plain process with rlimits, or Docker).
//! * [`worker`] — job execution: smoke tests, tournaments, series finalisation.
//! * [`api`] — the HTTP API (Axum).
//! * [`scheduler`] — nightly run creation.

pub mod api;
pub mod artifacts;
pub mod autoscale;
pub mod config;
pub mod ids;
pub mod models;
pub mod runs;
pub mod sandbox;
pub mod scheduler;
pub mod store;
pub mod worker;

use anyhow::Result;
use std::sync::Arc;

/// Everything a process needs to serve the API and/or run workers.
pub struct Services {
    pub cfg: config::PlatformConfig,
    pub store: Arc<store::Store>,
    pub artifacts: Arc<artifacts::ArtifactStore>,
    pub sandbox: Arc<dyn sandbox::Sandbox>,
}

impl Services {
    pub fn open(cfg: config::PlatformConfig) -> Result<Services> {
        let store = Arc::new(store::Store::open(&cfg.storage.database)?);
        // Seed settings from the config file the first time.
        if store.get_setting("platform")?.is_none() {
            store.save_settings(&cfg.defaults)?;
        }
        let artifacts = Arc::new(artifacts::ArtifactStore::new(&cfg.storage.artifacts_dir)?);
        std::fs::create_dir_all(&cfg.storage.logs_dir)?;
        let sandbox = sandbox::build_sandbox(&cfg.sandbox)?;
        Ok(Services {
            cfg,
            store,
            artifacts,
            sandbox,
        })
    }

    pub fn app_state(&self) -> api::SharedState {
        Arc::new(api::AppState {
            cfg: self.cfg.clone(),
            store: Arc::clone(&self.store),
            artifacts: Arc::clone(&self.artifacts),
            started_at: ids::now_str(),
        })
    }

    pub fn worker(&self) -> Arc<worker::Worker> {
        Arc::new(worker::Worker::new(
            self.cfg.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.artifacts),
            Arc::clone(&self.sandbox),
        ))
    }
}

pub fn init_tracing(default: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
