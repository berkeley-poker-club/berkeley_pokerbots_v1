//! Worker: claims jobs from the queue and executes them (smoke tests, tournaments, finalisation).

pub mod jobs;

use crate::artifacts::ArtifactStore;
use crate::config::PlatformConfig;
use crate::ids::{now_str, worker_id};
use crate::models::{job_types, Job, JobStatus, WorkerRow};
use crate::sandbox::Sandbox;
use crate::store::Store;
use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Semaphore};

pub struct Worker {
    pub id: String,
    pub cfg: PlatformConfig,
    pub store: Arc<Store>,
    pub artifacts: Arc<ArtifactStore>,
    pub sandbox: Arc<dyn Sandbox>,
    running: AtomicU32,
    bots_in_flight: AtomicU32,
    started_at: String,
}

impl Worker {
    pub fn new(
        cfg: PlatformConfig,
        store: Arc<Store>,
        artifacts: Arc<ArtifactStore>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Worker {
        Worker {
            id: worker_id(),
            cfg,
            store,
            artifacts,
            sandbox,
            running: AtomicU32::new(0),
            bots_in_flight: AtomicU32::new(0),
            started_at: now_str(),
        }
    }

    fn job_types(&self) -> Vec<String> {
        if self.cfg.worker.job_types.is_empty() {
            job_types::ALL.iter().map(|s| s.to_string()).collect()
        } else {
            self.cfg.worker.job_types.clone()
        }
    }

    fn heartbeat(&self) {
        let row = WorkerRow {
            id: self.id.clone(),
            hostname: hostname(),
            started_at: self.started_at.clone(),
            heartbeat_at: now_str(),
            running_jobs: self.running.load(Ordering::SeqCst),
            capacity: self.cfg.worker.concurrency as u32,
            info: serde_json::json!({
                "sandbox": self.sandbox.describe(),
                "job_types": self.job_types(),
                "pid": std::process::id(),
                "bots_in_flight": self.bots_in_flight.load(Ordering::SeqCst),
                "max_bots_in_flight": self.cfg.worker.max_bots_in_flight,
            }),
        };
        if let Err(e) = self.store.upsert_worker(&row) {
            tracing::warn!(error = %e, "worker heartbeat failed");
        }
    }

    /// Main loop. Returns when `shutdown` becomes true and all in-flight jobs finished.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let concurrency = self.cfg.worker.concurrency.max(1);
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let poll = Duration::from_millis(self.cfg.worker.poll_interval_ms.max(50));
        let types = self.job_types();
        let type_refs: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
        let mut last_heartbeat = std::time::Instant::now() - Duration::from_secs(3600);
        let mut last_stale = std::time::Instant::now();
        tracing::info!(worker = %self.id, concurrency, sandbox = %self.sandbox.describe(), "worker started");
        loop {
            if *shutdown.borrow() {
                break;
            }
            if last_heartbeat.elapsed()
                >= Duration::from_secs(self.cfg.worker.heartbeat_secs.max(1))
            {
                self.heartbeat();
                last_heartbeat = std::time::Instant::now();
            }
            if last_stale.elapsed() >= Duration::from_secs(60) {
                match self
                    .store
                    .requeue_stale_jobs(self.cfg.worker.stale_job_secs as i64)
                {
                    Ok(n) if n > 0 => tracing::warn!(n, "re-queued stale jobs"),
                    Err(e) => tracing::warn!(error = %e, "requeue_stale_jobs failed"),
                    _ => {}
                }
                last_stale = std::time::Instant::now();
            }
            let permit = match Arc::clone(&semaphore).try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    tokio::select! {
                        _ = tokio::time::sleep(poll) => {}
                        _ = shutdown.changed() => {}
                    }
                    continue;
                }
            };
            let used = self.bots_in_flight.load(Ordering::SeqCst);
            let capacity = self
                .cfg
                .worker
                .max_bots_in_flight
                .saturating_sub(used)
                .max(1);
            let job = match self
                .store
                .claim_job_with_capacity(&self.id, &type_refs, capacity)
            {
                Ok(Some(j)) => j,
                Ok(None) => {
                    drop(permit);
                    tokio::select! {
                        _ = tokio::time::sleep(poll) => {}
                        _ = shutdown.changed() => {}
                    }
                    continue;
                }
                Err(e) => {
                    tracing::error!(error = %e, "claim_job failed");
                    drop(permit);
                    tokio::time::sleep(poll).await;
                    continue;
                }
            };
            let me = Arc::clone(&self);
            me.running.fetch_add(1, Ordering::SeqCst);
            let weight = job.weight;
            me.bots_in_flight.fetch_add(weight, Ordering::SeqCst);
            tokio::spawn(async move {
                me.execute(job).await;
                me.running.fetch_sub(1, Ordering::SeqCst);
                me.bots_in_flight.fetch_sub(weight, Ordering::SeqCst);
                drop(permit);
            });
        }
        // Drain in-flight jobs.
        let _ = semaphore.acquire_many(concurrency as u32).await;
        let _ = self.store.remove_worker(&self.id);
        tracing::info!(worker = %self.id, "worker stopped");
    }

    /// Run one job to completion, with periodic heartbeats and failure bookkeeping.
    pub async fn execute(&self, job: Job) {
        let job_id = job.id;
        let job_type = job.job_type.clone();
        tracing::info!(job_id, %job_type, attempt = job.attempts, "job started");
        let store = Arc::clone(&self.store);
        let hb = tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(10));
            loop {
                t.tick().await;
                let _ = store.heartbeat_job(job_id);
            }
        });
        let result = self.dispatch(&job).await;
        hb.abort();
        match result {
            Ok(v) => {
                if let Err(e) = self.store.complete_job(job_id, Some(&v)) {
                    tracing::error!(job_id, error = %e, "complete_job failed");
                }
                tracing::info!(job_id, %job_type, "job done");
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(job_id, %job_type, error = %msg, "job failed");
                match self.store.fail_job(job_id, &msg, true) {
                    Ok(JobStatus::Failed) => self.on_final_failure(&job, &msg).await,
                    Ok(_) => {}
                    Err(e2) => tracing::error!(job_id, error = %e2, "fail_job failed"),
                }
            }
        }
    }

    async fn dispatch(&self, job: &Job) -> Result<serde_json::Value> {
        match job.job_type.as_str() {
            job_types::SMOKE_VALIDATE => self.job_smoke_validate(job).await,
            job_types::RUN_SERIES => self.job_run_series(job).await,
            job_types::RUN_TOURNAMENT => self.job_run_tournament(job).await,
            job_types::FINALIZE_SERIES => self.job_finalize_series(job).await,
            other => Err(anyhow::anyhow!("unknown job type '{}'", other)),
        }
    }
}

pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string())
}
