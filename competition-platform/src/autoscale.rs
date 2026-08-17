//! Autoscaler: sizes the worker fleet from queued work and deadlines, so a nightly or on-demand
//! series with up to `max_workers × max_bots_per_worker` bots finishes on time without manual
//! resizing.
//!
//! **Demand model.** For every active run we estimate the per-tournament duration — the run's own
//! completed tournaments first, then history of similar-sized runs, then a configured
//! seconds-per-participant fallback — and compute how many tournaments must run in parallel for
//! the remaining work to finish before the run's deadline (nightly: `nightly_deadline_hours`
//! after start; on-demand: `ondemand_deadline_minutes`). Job slots and bot capacity are both
//! constraints: `workers = max(slots/worker_concurrency, concurrent_bots/max_bots_per_worker)`,
//! clamped to `[min_workers, max_workers]`.
//!
//! **Backends.** `off` (plan only, visible at `GET /admin/autoscale`), `processes` (spawn/stop
//! local `tournament-worker` processes — single-box autoscaling), `command` (run a shell template
//! with `{n}`, e.g. `docker compose up -d --scale worker={n}` or a cloud ASG script).
//!
//! Scale-up applies immediately; scale-down waits `scale_down_cooldown_secs` and stops workers
//! gracefully (SIGINT → the worker finishes in-flight jobs, then exits).

use crate::config::PlatformConfig;
use crate::ids::{now_str, parse_time};
use crate::models::{job_types, AutoscaleSettings, RunKind, RunStatus};
use crate::store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunDemand {
    pub run_id: String,
    pub kind: RunKind,
    pub participants: usize,
    pub tournaments_remaining: usize,
    pub est_tournament_secs: f64,
    /// Where the estimate came from: `run_history` | `global_history` | `configured_fallback`.
    pub estimate_source: String,
    pub deadline: String,
    pub time_left_secs: f64,
    /// Tournaments that must run in parallel to make the deadline.
    pub slots_needed: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoscalePlan {
    pub at: String,
    pub enabled: bool,
    pub desired_workers: u32,
    pub slots_needed: u32,
    pub concurrent_bots_needed: u32,
    pub validation_jobs_queued: u64,
    pub runs: Vec<RunDemand>,
    pub reason: String,
}

/// Parallelism needed to finish `remaining` tournaments of `est_secs` each within `time_left_secs`.
pub fn slots_for(remaining: usize, est_secs: f64, time_left_secs: f64) -> u32 {
    if remaining == 0 {
        return 0;
    }
    let total_work = remaining as f64 * est_secs.max(1.0);
    let slots = (total_work / time_left_secs.max(1.0)).ceil() as usize;
    slots.clamp(1, remaining) as u32
}

/// Workers needed for the given slot and bot demand under the configured per-worker capacity.
pub fn workers_for(slots: u32, concurrent_bots: u32, s: &AutoscaleSettings) -> u32 {
    let by_slots = slots.div_ceil(s.worker_concurrency.max(1));
    let by_bots = concurrent_bots.div_ceil(s.max_bots_per_worker.max(1));
    by_slots
        .max(by_bots)
        .clamp(s.min_workers, s.max_workers.max(s.min_workers))
}

/// Compute the current plan from the store.
pub fn plan(store: &Store, settings: &AutoscaleSettings) -> Result<AutoscalePlan> {
    let now = crate::ids::now();
    let mut runs = Vec::new();
    let mut slots_total: u32 = 0;
    let mut bots_total: u64 = 0;

    for run in store.active_runs()? {
        if run.status == RunStatus::Finalizing {
            continue; // one cheap job left
        }
        let (total, _completed, terminal) = store.tournament_counts(&run.id)?;
        let remaining = total.saturating_sub(terminal);
        if remaining == 0 {
            continue;
        }
        let participants = run.participants.len();
        let (n_done, mean_secs, _last) = store.tournament_duration_stats(&run.id)?;
        let (est, source) = if n_done >= 1 && mean_secs > 0.0 {
            (mean_secs, "run_history")
        } else if let Some(h) = store.historical_tournament_secs(participants)? {
            (h, "global_history")
        } else {
            (
                (settings.est_secs_per_participant * participants as f64).max(30.0),
                "configured_fallback",
            )
        };
        let start = run
            .started_at
            .as_deref()
            .or(Some(run.created_at.as_str()))
            .and_then(parse_time)
            .unwrap_or(now);
        let deadline = match run.kind {
            RunKind::Nightly => {
                start + chrono::Duration::seconds((settings.nightly_deadline_hours * 3600.0) as i64)
            }
            RunKind::Ondemand => {
                start
                    + chrono::Duration::seconds((settings.ondemand_deadline_minutes * 60.0) as i64)
            }
        };
        let time_left = (deadline - now).num_milliseconds() as f64 / 1000.0;
        let slots = slots_for(remaining, est, time_left);
        slots_total = slots_total.saturating_add(slots);
        bots_total += slots as u64 * participants as u64;
        runs.push(RunDemand {
            run_id: run.id.clone(),
            kind: run.kind,
            participants,
            tournaments_remaining: remaining,
            est_tournament_secs: (est * 10.0).round() / 10.0,
            estimate_source: source.into(),
            deadline: crate::ids::fmt_time(&deadline),
            time_left_secs: (time_left * 10.0).round() / 10.0,
            slots_needed: slots,
        });
    }

    // Validation / finalize jobs need at least one live worker.
    let queued = store.queued_work()?;
    let validation_jobs: u64 = queued
        .iter()
        .filter(|(_, t, _, _)| {
            t == job_types::SMOKE_VALIDATE
                || t == job_types::FINALIZE_SERIES
                || t == job_types::RUN_SERIES
        })
        .map(|(_, _, n, _)| *n as u64)
        .sum();
    if validation_jobs > 0 && slots_total < settings.min_slots_when_busy {
        slots_total = settings.min_slots_when_busy;
    }

    let bots_needed = bots_total.min(u32::MAX as u64) as u32;
    let desired = if slots_total == 0 && validation_jobs == 0 {
        settings.min_workers
    } else {
        workers_for(slots_total.max(1), bots_needed, settings)
    };
    let reason = if runs.is_empty() && validation_jobs == 0 {
        "idle".to_string()
    } else {
        format!(
            "{} active run(s) needing {} slot(s) / {} concurrent bots; {} validation job(s)",
            runs.len(),
            slots_total,
            bots_needed,
            validation_jobs
        )
    };
    Ok(AutoscalePlan {
        at: now_str(),
        enabled: settings.enabled,
        desired_workers: desired,
        slots_needed: slots_total,
        concurrent_bots_needed: bots_needed,
        validation_jobs_queued: validation_jobs,
        runs,
        reason,
    })
}

// ---------------------------------------------------------------- backends

enum Backend {
    Off,
    Processes {
        binary: PathBuf,
        config: PathBuf,
        children: Vec<tokio::process::Child>,
    },
    Command {
        template: String,
        last_applied: Option<u32>,
    },
}

pub struct Autoscaler {
    cfg: PlatformConfig,
    config_path: PathBuf,
    store: Arc<Store>,
    backend: Backend,
    last_change: Instant,
    current: u32,
}

impl Autoscaler {
    pub fn new(cfg: PlatformConfig, config_path: PathBuf, store: Arc<Store>) -> Result<Self> {
        let backend = match cfg.autoscaler.backend.as_str() {
            "processes" => {
                let binary = match &cfg.autoscaler.worker_binary {
                    Some(b) => PathBuf::from(b),
                    None => {
                        let exe = std::env::current_exe()?;
                        exe.with_file_name("tournament-worker")
                    }
                };
                Backend::Processes {
                    binary,
                    config: config_path.clone(),
                    children: Vec::new(),
                }
            }
            "command" => Backend::Command {
                template: cfg.autoscaler.scale_command.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "autoscaler.backend = \"command\" requires autoscaler.scale_command"
                    )
                })?,
                last_applied: None,
            },
            _ => Backend::Off,
        };
        Ok(Autoscaler {
            cfg,
            config_path,
            store,
            backend,
            last_change: Instant::now() - Duration::from_secs(3600),
            current: 0,
        })
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Off => "off",
            Backend::Processes { .. } => "processes",
            Backend::Command { .. } => "command",
        }
    }

    async fn apply(&mut self, desired: u32) -> Result<()> {
        match &mut self.backend {
            Backend::Off => {}
            Backend::Processes {
                binary,
                config,
                children,
            } => {
                // Reap exited children first.
                children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
                self.current = children.len() as u32;
                while (children.len() as u32) < desired {
                    let child = tokio::process::Command::new(&*binary)
                        .arg("--config")
                        .arg(&*config)
                        .env(
                            "RUST_LOG",
                            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
                        )
                        .spawn()?;
                    tracing::info!(pid = child.id(), "autoscaler: started tournament-worker");
                    children.push(child);
                }
                while (children.len() as u32) > desired {
                    if let Some(child) = children.pop() {
                        if let Some(pid) = child.id() {
                            tracing::info!(pid, "autoscaler: stopping tournament-worker (SIGINT, drains in-flight jobs)");
                            #[cfg(unix)]
                            // SAFETY: plain kill(2) with a valid pid.
                            unsafe {
                                libc::kill(pid as i32, libc::SIGINT);
                            }
                            // Reap in the background; the worker exits after draining.
                            tokio::spawn(async move {
                                let mut child = child;
                                let _ =
                                    tokio::time::timeout(Duration::from_secs(600), child.wait())
                                        .await;
                                let _ = child.start_kill();
                            });
                        }
                    }
                }
                self.current = children.len() as u32;
            }
            Backend::Command {
                template,
                last_applied,
            } => {
                if *last_applied != Some(desired) {
                    let cmd = template.replace("{n}", &desired.to_string());
                    tracing::info!(%cmd, "autoscaler: applying scale command");
                    let out = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .output()
                        .await?;
                    if out.status.success() {
                        *last_applied = Some(desired);
                        self.current = desired;
                    } else {
                        tracing::error!(
                            code = ?out.status.code(),
                            stderr = %String::from_utf8_lossy(&out.stderr),
                            "autoscaler: scale command failed"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// One tick: plan, apply (respecting the scale-down cooldown), record status.
    pub async fn tick(&mut self) {
        let settings = match self.store.settings() {
            Ok(s) => s.autoscale,
            Err(e) => {
                tracing::warn!(error = %e, "autoscaler: cannot load settings");
                return;
            }
        };
        let plan = match plan(&self.store, &settings) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "autoscaler: planning failed");
                return;
            }
        };
        let mut target = plan.desired_workers;
        let mut action = "hold";
        if !settings.enabled || matches!(self.backend, Backend::Off) {
            action = "plan_only";
        } else if target > self.current {
            action = "scale_up";
        } else if target < self.current {
            // Scale down only after a quiet period since the *last change in either direction*,
            // so a fresh scale-up is given time to prove itself before being unwound.
            if self.last_change.elapsed() >= Duration::from_secs(settings.scale_down_cooldown_secs)
            {
                action = "scale_down";
            } else {
                target = self.current;
                action = "cooldown";
            }
        }
        if matches!(action, "scale_up" | "scale_down") {
            let before = self.current;
            if let Err(e) = self.apply(target).await {
                tracing::error!(error = %e, "autoscaler: apply failed");
            } else if self.current != before {
                self.last_change = Instant::now();
            }
            tracing::info!(from = before, to = self.current, desired = plan.desired_workers, reason = %plan.reason, "autoscaler: {action}");
        }
        let status = serde_json::json!({
            "backend": self.backend_name(),
            "action": action,
            "current_workers": self.current,
            "plan": plan,
        });
        let _ = self.store.set_setting("autoscale_status", &status);
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let poll = Duration::from_secs(self.cfg.autoscaler.poll_secs.max(5));
        tracing::info!(backend = self.backend_name(), config = %self.config_path.display(), "autoscaler started");
        loop {
            if *shutdown.borrow() {
                break;
            }
            self.tick().await;
            tokio::select! {
                _ = tokio::time::sleep(poll) => {}
                _ = shutdown.changed() => {}
            }
        }
        // Stop all managed workers on shutdown.
        if let Backend::Processes { children, .. } = &mut self.backend {
            for child in children.iter() {
                if let Some(pid) = child.id() {
                    #[cfg(unix)]
                    // SAFETY: plain kill(2).
                    unsafe {
                        libc::kill(pid as i32, libc::SIGINT);
                    }
                }
            }
            for child in children.iter_mut() {
                let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
                let _ = child.start_kill();
            }
        }
        tracing::info!("autoscaler stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Participant, RunConfig};

    #[test]
    fn slots_math() {
        // 100 tournaments × 60s within 6h → 2 slots
        assert_eq!(slots_for(100, 60.0, 6.0 * 3600.0), 1);
        // 100 × 600s within 6h → ceil(60000/21600) = 3
        assert_eq!(slots_for(100, 600.0, 6.0 * 3600.0), 3);
        // Deadline already passed → all remaining in parallel (capped)
        assert_eq!(slots_for(10, 300.0, 1.0), 10);
        assert_eq!(slots_for(0, 300.0, 1.0), 0);
        // Never more slots than tournaments
        assert_eq!(slots_for(2, 10_000.0, 1.0), 2);
    }

    #[test]
    fn workers_math_respects_both_constraints_and_clamps() {
        let s = AutoscaleSettings {
            min_workers: 1,
            max_workers: 8,
            worker_concurrency: 2,
            max_bots_per_worker: 600,
            ..Default::default()
        };
        // slot-bound: 6 slots / 2 per worker = 3
        assert_eq!(workers_for(6, 100, &s), 3);
        // bot-bound: 2 slots but 500 bots each = 1000 bots → 2 workers
        assert_eq!(workers_for(2, 1000, &s), 2);
        // clamped to max
        assert_eq!(workers_for(100, 60_000, &s), 8);
        // clamped to min
        assert_eq!(workers_for(0, 0, &s), 1);
    }

    #[test]
    fn plan_uses_fallback_estimate_and_deadlines() {
        let store = Store::open_memory().unwrap();
        let participants: Vec<Participant> = (1..=200u32)
            .map(|i| Participant {
                team_id: format!("tm_{i}"),
                team_name: format!("team {i}"),
                submission_id: format!("sub_{i}"),
                player_id: i,
            })
            .collect();
        let cfg = RunConfig {
            series_length: 100,
            tournament: Default::default(),
            record_hands: false,
        };
        store
            .create_run(
                "run_n",
                RunKind::Nightly,
                &cfg,
                &participants,
                "scheduler",
                None,
            )
            .unwrap();
        let settings = AutoscaleSettings {
            est_secs_per_participant: 3.0, // 600s per 200-player tournament
            nightly_deadline_hours: 6.0,
            worker_concurrency: 2,
            max_bots_per_worker: 600,
            min_workers: 1,
            max_workers: 8,
            ..Default::default()
        };
        let p = plan(&store, &settings).unwrap();
        assert_eq!(p.runs.len(), 1);
        let d = &p.runs[0];
        assert_eq!(d.tournaments_remaining, 100);
        assert_eq!(d.estimate_source, "configured_fallback");
        assert!((d.est_tournament_secs - 600.0).abs() < 1.0);
        // 100 × 600s = 60000s of work in ~21600s → 3 slots; 3 slots × 200 bots = 600 bots
        assert_eq!(d.slots_needed, 3);
        assert_eq!(p.slots_needed, 3);
        assert_eq!(p.concurrent_bots_needed, 600);
        // 3 slots / 2-per-worker = 2 workers; bots 600/600 = 1 → max = 2
        assert_eq!(p.desired_workers, 2);

        // Nothing active → min workers
        crate::runs::cancel_run(&store, "run_n").unwrap();
        let p2 = plan(&store, &settings).unwrap();
        assert_eq!(p2.desired_workers, settings.min_workers);
        assert_eq!(p2.reason, "idle");
    }

    #[test]
    fn plan_counts_validation_jobs() {
        let store = Store::open_memory().unwrap();
        store
            .enqueue(
                job_types::SMOKE_VALIDATE,
                &serde_json::json!({"submission_id": "x"}),
                None,
                10,
                1,
            )
            .unwrap();
        let settings = AutoscaleSettings::default();
        let p = plan(&store, &settings).unwrap();
        assert_eq!(p.validation_jobs_queued, 1);
        assert!(p.desired_workers >= 1);
        assert_ne!(p.reason, "idle");
    }
}
