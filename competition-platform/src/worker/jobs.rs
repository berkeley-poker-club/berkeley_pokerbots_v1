//! Job handlers.

use super::Worker;
use crate::models::*;
use crate::runs::{finalize_run, maybe_finalize};
use crate::sandbox::BotLaunch;
use anyhow::{anyhow, Context, Result};
use poker_utils::PlayerId;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use table_runner::{
    smoke_test, FoldStrategy, LocalBot, Player, ProcessBot, SmokeFailure, SmokeReport,
};
use tournament_core::{HandRecord, TournamentDirector};

fn payload_str<'a>(job: &'a Job, key: &str) -> Result<&'a str> {
    job.payload
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("job payload missing '{}'", key))
}

fn setup_failure(msg: String) -> SmokeReport {
    SmokeReport {
        passed: false,
        first_latency_ms: 0,
        latency_ms: 0,
        reason: Some(SmokeFailure::SetupFailed),
        message: msg,
        stderr_tail: String::new(),
        actions: vec![],
    }
}

impl Worker {
    fn logs_dir(&self) -> PathBuf {
        self.cfg.storage.logs_dir.clone()
    }

    /// Prepare a submission's artifact and describe how to launch it.
    fn launch_for(
        &self,
        sub: &Submission,
        player_id: PlayerId,
        display_name: String,
        session: &str,
        stderr_log: Option<PathBuf>,
    ) -> Result<BotLaunch> {
        let dir = self
            .artifacts
            .unpack(&sub.id, &sub.artifact_path, &sub.manifest)
            .with_context(|| format!("unpacking artifact for {}", sub.id))?;
        Ok(BotLaunch {
            player_id,
            display_name,
            session: session.to_string(),
            artifact_dir: dir,
            entrypoint: sub.manifest.entrypoint.trim_start_matches("./").to_string(),
            runtime: sub.manifest.effective_runtime().to_string(),
            args: sub.manifest.args.clone(),
            stderr_log,
        })
    }

    // ------------------------------------------------------------ smoke_validate_submission

    pub(super) async fn job_smoke_validate(&self, job: &Job) -> Result<Value> {
        let submission_id = payload_str(job, "submission_id")?;
        let sub = self
            .store
            .submission(submission_id)?
            .ok_or_else(|| anyhow!("submission {} not found", submission_id))?;
        let settings = self.store.settings()?;
        let session = format!("smoke-{}", sub.id);
        let log_dir = self.logs_dir().join("smoke");
        let _ = std::fs::create_dir_all(&log_dir);
        let stderr_log = Some(log_dir.join(format!("{}.log", sub.id)));

        let report =
            match self.launch_for(&sub, 1, format!("smoke:{}", sub.id), &session, stderr_log) {
                Err(e) => setup_failure(format!("{e:#}")),
                Ok(launch) => match self.sandbox.spawn(&launch).await {
                    Err(e) => setup_failure(format!("could not start bot: {e:#}")),
                    Ok(bot) => {
                        let report =
                            smoke_test(&bot, Duration::from_millis(settings.smoke_timeout_ms))
                                .await;
                        bot.shutdown().await;
                        report
                    }
                },
            };
        self.sandbox.cleanup_session(&session).await;
        let status = if report.passed {
            SubmissionStatus::Validated
        } else {
            SubmissionStatus::Rejected
        };
        self.store
            .set_submission_status(&sub.id, status, Some(&report))?;
        tracing::info!(submission = %sub.id, passed = report.passed, reason = ?report.reason, "smoke test finished");
        Ok(serde_json::to_value(&report)?)
    }

    // ------------------------------------------------------------ run_series

    pub(super) async fn job_run_series(&self, job: &Job) -> Result<Value> {
        let run_id = payload_str(job, "run_id")?;
        let run = self
            .store
            .run(run_id)?
            .ok_or_else(|| anyhow!("run {} not found", run_id))?;
        if run.status.is_terminal() {
            return Ok(json!({ "skipped": run.status }));
        }
        if run.participants.len() < 2 {
            self.store.transition_run(
                run_id,
                &[RunStatus::Queued, RunStatus::Running],
                RunStatus::Failed,
                Some("fewer than two active bots at snapshot time"),
            )?;
            self.store.cancel_pending_tournaments(run_id)?;
            return Ok(json!({ "failed": "not enough participants" }));
        }
        self.store
            .transition_run(run_id, &[RunStatus::Queued], RunStatus::Running, None)?;
        let existing: std::collections::HashSet<u64> = self
            .store
            .jobs_for_run(run_id)?
            .into_iter()
            .filter(|j| j.job_type == job_types::RUN_TOURNAMENT)
            .filter_map(|j| j.payload.get("index").and_then(|v| v.as_u64()))
            .collect();
        let priority = if run.kind == RunKind::Nightly { 4 } else { 2 };
        let mut enqueued = 0;
        for t in self.store.tournaments(run_id)? {
            if t.status == TournamentStatus::Queued && !existing.contains(&(t.index as u64)) {
                self.store.enqueue(
                    job_types::RUN_TOURNAMENT,
                    &json!({ "run_id": run_id, "index": t.index }),
                    Some(run_id),
                    priority,
                    2,
                )?;
                enqueued += 1;
            }
        }
        // Edge: nothing to run (all cancelled) → finalize now.
        maybe_finalize(&self.store, run_id)?;
        Ok(json!({ "enqueued": enqueued }))
    }

    // ------------------------------------------------------------ run_tournament

    pub(super) async fn job_run_tournament(&self, job: &Job) -> Result<Value> {
        let run_id = payload_str(job, "run_id")?.to_string();
        let index = job
            .payload
            .get("index")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow!("job payload missing 'index'"))? as usize;
        let run = self
            .store
            .run(&run_id)?
            .ok_or_else(|| anyhow!("run {} not found", run_id))?;
        let trow = self
            .store
            .tournament(&run_id, index)?
            .ok_or_else(|| anyhow!("tournament {}#{} not found", run_id, index))?;
        if run.status.is_terminal() || trow.status.is_terminal() {
            if trow.status == TournamentStatus::Queued {
                self.store.finish_tournament(
                    &run_id,
                    index,
                    TournamentStatus::Cancelled,
                    None,
                    None,
                    Some("run not active"),
                    None,
                    &[],
                )?;
            }
            maybe_finalize(&self.store, &run_id)?;
            return Ok(json!({ "skipped": true }));
        }
        if !self.store.start_tournament(&run_id, index, &self.id)? {
            return Ok(json!({ "skipped": "already terminal" }));
        }

        let session = format!("{}-{}", run_id, index);
        let log_dir = self.logs_dir().join(&run_id).join(index.to_string());
        let _ = std::fs::create_dir_all(&log_dir);

        // ---- spawn every participant (concurrently); failures fall back to an auto-fold bot.
        let mut launches: Vec<(Participant, Result<BotLaunch>)> = Vec::new();
        for p in &run.participants {
            let sub = self.store.submission(&p.submission_id)?;
            let launch = match sub {
                Some(sub) => self.launch_for(
                    &sub,
                    p.player_id,
                    p.team_name.clone(),
                    &session,
                    Some(log_dir.join(format!("{}.stderr.log", sanitize(&p.team_id)))),
                ),
                None => Err(anyhow!("submission {} missing", p.submission_id)),
            };
            launches.push((p.clone(), launch));
        }
        let sandbox = Arc::clone(&self.sandbox);
        let spawned = futures::future::join_all(launches.into_iter().map(|(p, launch)| {
            let sandbox = Arc::clone(&sandbox);
            async move {
                let bot: Result<ProcessBot> = match launch {
                    Ok(l) => sandbox.spawn(&l).await,
                    Err(e) => Err(e),
                };
                (p, bot)
            }
        }))
        .await;
        let mut players: Vec<Arc<dyn Player>> = Vec::with_capacity(spawned.len());
        let mut spawn_failures: Vec<Value> = Vec::new();
        let mut team_by_player: HashMap<PlayerId, String> = HashMap::new();
        for (p, bot) in spawned {
            team_by_player.insert(p.player_id, p.team_id.clone());
            match bot {
                Ok(b) => players.push(Arc::new(b)),
                Err(e) => {
                    tracing::warn!(run = %run_id, index, team = %p.team_id, error = %format!("{e:#}"), "bot spawn failed; seating auto-fold stand-in");
                    spawn_failures.push(json!({ "team_id": p.team_id, "error": format!("{e:#}") }));
                    players.push(Arc::new(
                        LocalBot::new(p.player_id, FoldStrategy)
                            .named(format!("{} (failed to start)", p.team_name)),
                    ));
                }
            }
        }

        // ---- cancellation watcher
        let cancel = Arc::new(AtomicBool::new(false));
        let watcher = {
            let cancel = Arc::clone(&cancel);
            let store = Arc::clone(&self.store);
            let run_id = run_id.clone();
            tokio::spawn(async move {
                let mut t = tokio::time::interval(Duration::from_secs(3));
                loop {
                    t.tick().await;
                    match store.run(&run_id) {
                        Ok(Some(r))
                            if r.status == RunStatus::Cancelled
                                || r.status == RunStatus::Failed =>
                        {
                            cancel.store(true, Ordering::SeqCst);
                            break;
                        }
                        Ok(None) => {
                            cancel.store(true, Ordering::SeqCst);
                            break;
                        }
                        _ => {}
                    }
                }
            })
        };

        // ---- hand log
        let (hand_tx, hand_task) = if run.config.record_hands {
            let path = log_dir.join("hands.jsonl");
            match std::fs::File::create(&path) {
                Ok(file) => {
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<HandRecord>(1024);
                    let task = tokio::task::spawn_blocking(move || {
                        use std::io::Write;
                        let mut w = std::io::BufWriter::new(file);
                        while let Some(rec) = rx.blocking_recv() {
                            let line = json!({
                                "tournament_id": rec.tournament_id,
                                "table_id": rec.table_id,
                                "hand_id": rec.hand_id,
                                "level": rec.level,
                                "result": rec.result,
                                "events": rec.events,
                            });
                            let _ = serde_json::to_writer(&mut w, &line);
                            let _ = w.write_all(b"\n");
                        }
                        let _ = w.flush();
                    });
                    (Some(tx), Some(task))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot create hand log");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // ---- play
        let td = TournamentDirector::new(run.config.tournament.clone(), session.clone(), trow.seed)
            .with_hand_records(hand_tx.is_some());
        let outcome = td.run(players, Some(Arc::clone(&cancel)), hand_tx).await;
        watcher.abort();
        if let Some(t) = hand_task {
            let _ = t.await;
        }
        self.sandbox.cleanup_session(&session).await;

        // ---- persist
        let placements: Vec<PlacementRow> = outcome
            .placements
            .iter()
            .filter_map(|(pid, place)| {
                team_by_player.get(pid).map(|team_id| PlacementRow {
                    run_id: run_id.clone(),
                    tournament_index: index,
                    team_id: team_id.clone(),
                    place: *place,
                    hands_played: outcome.hands_played.get(pid).copied().unwrap_or(0),
                })
            })
            .collect();
        let winner_team = outcome.winner.and_then(|w| team_by_player.get(&w).cloned());
        let (status, error) = match outcome.aborted.as_deref() {
            None | Some("max_hands") => (TournamentStatus::Completed, None),
            Some("cancelled") => (TournamentStatus::Cancelled, Some("cancelled".to_string())),
            Some(other) => (TournamentStatus::Failed, Some(other.to_string())),
        };
        let mut outcome_json = serde_json::to_value(&outcome)?;
        outcome_json["team_by_player"] = serde_json::to_value(&team_by_player)?;
        if !spawn_failures.is_empty() {
            outcome_json["spawn_failures"] = Value::Array(spawn_failures.clone());
        }
        self.store.finish_tournament(
            &run_id,
            index,
            status,
            Some(outcome.total_hands),
            winner_team.as_deref(),
            error.as_deref(),
            Some(&outcome_json),
            if status == TournamentStatus::Completed {
                &placements
            } else {
                &[]
            },
        )?;
        maybe_finalize(&self.store, &run_id)?;
        tracing::info!(run = %run_id, index, hands = outcome.total_hands, ms = outcome.duration_ms, status = ?status, "tournament finished");
        Ok(json!({
            "status": status,
            "total_hands": outcome.total_hands,
            "duration_ms": outcome.duration_ms,
            "winner_team_id": winner_team,
            "spawn_failures": spawn_failures.len(),
        }))
    }

    // ------------------------------------------------------------ finalize_series

    pub(super) async fn job_finalize_series(&self, job: &Job) -> Result<Value> {
        let run_id = payload_str(job, "run_id")?;
        let scores = finalize_run(&self.store, run_id)?;
        tracing::info!(run = %run_id, teams = scores.len(), "leaderboard published");
        Ok(json!({ "teams": scores.len() }))
    }

    /// Called when a job has exhausted its retries.
    pub(super) async fn on_final_failure(&self, job: &Job, error: &str) {
        match job.job_type.as_str() {
            job_types::RUN_TOURNAMENT => {
                if let (Some(run_id), Some(index)) = (
                    job.payload.get("run_id").and_then(|v| v.as_str()),
                    job.payload.get("index").and_then(|v| v.as_u64()),
                ) {
                    let _ = self.store.finish_tournament(
                        run_id,
                        index as usize,
                        TournamentStatus::Failed,
                        None,
                        None,
                        Some(error),
                        None,
                        &[],
                    );
                    let _ = maybe_finalize(&self.store, run_id);
                }
            }
            job_types::RUN_SERIES | job_types::FINALIZE_SERIES => {
                if let Some(run_id) = job.payload.get("run_id").and_then(|v| v.as_str()) {
                    let _ = self.store.transition_run(
                        run_id,
                        &[RunStatus::Queued, RunStatus::Running, RunStatus::Finalizing],
                        RunStatus::Failed,
                        Some(error),
                    );
                }
            }
            job_types::SMOKE_VALIDATE => {
                if let Some(sid) = job.payload.get("submission_id").and_then(|v| v.as_str()) {
                    let report = setup_failure(format!("smoke test could not run: {error}"));
                    let _ = self.store.set_submission_status(
                        sid,
                        SubmissionStatus::Rejected,
                        Some(&report),
                    );
                }
            }
            _ => {}
        }
    }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
