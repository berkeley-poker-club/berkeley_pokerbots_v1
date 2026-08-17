//! Job handlers.

use super::Worker;
use crate::ids::now_str;
use crate::models::*;
use crate::runs::{finalize_run, maybe_finalize};
use crate::sandbox::BotLaunch;
use anyhow::{anyhow, Context, Result};
use poker_utils::PlayerId;
use poker_utils::{Deck, HandParams, PublicEvent};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use table_runner::{
    play_hand, smoke_test, CallStrategy, FoldStrategy, LocalBot, Player, ProcessBot, RaiseStrategy,
    SmokeFailure, SmokeReport,
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

    // ------------------------------------------------------------ validation pipeline

    /// Validate a submission through three stages, each recorded as a [`CheckReport`]:
    /// 1. **build** — run the manifest's build command (or a default syntax/compile check),
    /// 2. **smoke** — the protocol smoke test,
    /// 3. **trial** — a short 3-handed match against reference bots, measuring how often the
    ///    engine had to substitute actions (timeouts / illegal moves / crashes).
    ///
    /// On success the submission becomes `validated` (and is auto-activated when requested).
    pub(super) async fn job_smoke_validate(&self, job: &Job) -> Result<Value> {
        let submission_id = payload_str(job, "submission_id")?;
        let sub = self
            .store
            .submission(submission_id)?
            .ok_or_else(|| anyhow!("submission {} not found", submission_id))?;
        let settings = self.store.settings()?;
        let v = &settings.validation;
        let mut checks: Vec<CheckReport> = vec![
            CheckReport::pending(CheckStage::Build),
            CheckReport::pending(CheckStage::Smoke),
            CheckReport::pending(CheckStage::Trial),
        ];
        let save = |store: &crate::store::Store,
                    checks: &[CheckReport],
                    status: Option<SubmissionStatus>| {
            let _ = store.set_submission_checks(&sub.id, checks, status);
        };

        // ---- unpack once
        let unpacked = self
            .artifacts
            .unpack(&sub.id, &sub.artifact_path, &sub.manifest)
            .with_context(|| format!("unpacking artifact for {}", sub.id));
        let unpacked = match unpacked {
            Ok(d) => d,
            Err(e) => {
                checks[0].status = CheckStatus::Failed;
                checks[0].summary = format!("{e:#}");
                checks[0].finished_at = Some(now_str());
                let report = setup_failure(format!("{e:#}"));
                save(&self.store, &checks, None);
                self.store.set_submission_status(
                    &sub.id,
                    SubmissionStatus::Rejected,
                    Some(&report),
                )?;
                return Ok(json!({ "passed": false, "stage": "build", "message": report.message }));
            }
        };
        let runtime = sub.manifest.effective_runtime().to_string();
        let entrypoint = sub.manifest.entrypoint.trim_start_matches("./").to_string();

        // ---- stage 1: build / compile check
        if v.build_enabled {
            checks[0].status = CheckStatus::Running;
            checks[0].started_at = Some(now_str());
            save(&self.store, &checks, Some(SubmissionStatus::Building));
            let argv: Vec<String> = if !sub.manifest.build.is_empty() {
                sub.manifest.build.clone()
            } else {
                match runtime.as_str() {
                    "python3" => vec![
                        "python3".into(),
                        "-m".into(),
                        "py_compile".into(),
                        entrypoint.clone(),
                    ],
                    "node" => vec!["node".into(), "--check".into(), entrypoint.clone()],
                    _ => vec![],
                }
            };
            if argv.is_empty() {
                checks[0].status = CheckStatus::Skipped;
                checks[0].summary = format!("no build step for runtime '{runtime}'");
            } else {
                let timeout = Duration::from_secs(
                    sub.manifest
                        .build_timeout_secs
                        .unwrap_or(v.build_timeout_secs)
                        .clamp(1, 3600),
                );
                let out = self
                    .sandbox
                    .run_command(&unpacked, &argv, &runtime, timeout, true)
                    .await;
                match out {
                    Ok(o) if o.ok() => {
                        checks[0].status = CheckStatus::Passed;
                        checks[0].summary = format!("`{}` succeeded", argv.join(" "));
                        checks[0].details =
                            json!({ "stdout_tail": o.stdout_tail, "stderr_tail": o.stderr_tail });
                    }
                    Ok(o) => {
                        checks[0].status = CheckStatus::Failed;
                        checks[0].summary = if o.timed_out {
                            format!("build timed out after {}s", timeout.as_secs())
                        } else {
                            format!("`{}` exited with code {:?}", argv.join(" "), o.exit_code)
                        };
                        checks[0].details =
                            json!({ "stdout_tail": o.stdout_tail, "stderr_tail": o.stderr_tail });
                    }
                    Err(e) => {
                        checks[0].status = CheckStatus::Failed;
                        checks[0].summary = format!("build could not run: {e:#}");
                    }
                }
            }
            checks[0].finished_at = Some(now_str());
            if checks[0].status == CheckStatus::Failed {
                let report = SmokeReport {
                    passed: false,
                    first_latency_ms: 0,
                    latency_ms: 0,
                    reason: Some(SmokeFailure::SetupFailed),
                    message: format!("build failed: {}", checks[0].summary),
                    stderr_tail: checks[0]
                        .details
                        .get("stderr_tail")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    actions: vec![],
                };
                save(&self.store, &checks, None);
                self.store.set_submission_status(
                    &sub.id,
                    SubmissionStatus::Rejected,
                    Some(&report),
                )?;
                return Ok(
                    json!({ "passed": false, "stage": "build", "message": checks[0].summary }),
                );
            }
        } else {
            checks[0].status = CheckStatus::Skipped;
            checks[0].summary = "build stage disabled".into();
        }

        // ---- stage 2: protocol smoke test
        checks[1].status = CheckStatus::Running;
        checks[1].started_at = Some(now_str());
        save(&self.store, &checks, Some(SubmissionStatus::SmokeTesting));
        let session = format!("smoke-{}", sub.id);
        let log_dir = self.logs_dir().join("smoke");
        let _ = std::fs::create_dir_all(&log_dir);
        let launch = BotLaunch {
            player_id: 1,
            display_name: format!("smoke:{}", sub.id),
            session: session.clone(),
            artifact_dir: unpacked.clone(),
            entrypoint: entrypoint.clone(),
            runtime: runtime.clone(),
            args: sub.manifest.args.clone(),
            stderr_log: Some(log_dir.join(format!("{}.log", sub.id))),
        };
        let report = match self.sandbox.spawn(&launch).await {
            Err(e) => setup_failure(format!("could not start bot: {e:#}")),
            Ok(bot) => {
                let r = smoke_test(&bot, Duration::from_millis(settings.smoke_timeout_ms)).await;
                bot.shutdown().await;
                r
            }
        };
        self.sandbox.cleanup_session(&session).await;
        checks[1].status = if report.passed {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        };
        checks[1].summary = report.message.clone();
        checks[1].details = json!({
            "reason": report.reason,
            "first_latency_ms": report.first_latency_ms,
            "latency_ms": report.latency_ms,
        });
        checks[1].finished_at = Some(now_str());
        if !report.passed {
            save(&self.store, &checks, None);
            self.store
                .set_submission_status(&sub.id, SubmissionStatus::Rejected, Some(&report))?;
            return Ok(json!({ "passed": false, "stage": "smoke", "message": report.message }));
        }

        // ---- stage 3: trial run against reference bots
        if v.trial_enabled && v.trial_hands > 0 {
            checks[2].status = CheckStatus::Running;
            checks[2].started_at = Some(now_str());
            save(&self.store, &checks, Some(SubmissionStatus::TrialRunning));
            let trial_session = format!("trial-{}", sub.id);
            let launch = BotLaunch {
                session: trial_session.clone(),
                display_name: format!("trial:{}", sub.id),
                ..launch
            };
            match self.sandbox.spawn(&launch).await {
                Err(e) => {
                    checks[2].status = CheckStatus::Failed;
                    checks[2].summary = format!("could not start bot for trial: {e:#}");
                }
                Ok(bot) => {
                    let bot = Arc::new(bot);
                    let outcome = trial_match(
                        Arc::clone(&bot) as Arc<dyn Player>,
                        v.trial_hands,
                        settings.tournament.action_timeout_ms,
                    )
                    .await;
                    bot.shutdown().await;
                    let rate = outcome.substitution_rate();
                    checks[2].details = json!({
                        "hands": outcome.hands,
                        "decisions": outcome.decisions,
                        "substitutions": outcome.substitutions,
                        "substitution_rate": rate,
                        "max_substitution_rate": v.trial_max_substitution_rate,
                        "bot_survived": outcome.bot_alive,
                    });
                    if !outcome.bot_alive {
                        checks[2].status = CheckStatus::Failed;
                        checks[2].summary = format!(
                            "bot process died during the trial (hand {}/{})",
                            outcome.hands, v.trial_hands
                        );
                    } else if rate > v.trial_max_substitution_rate {
                        checks[2].status = CheckStatus::Failed;
                        checks[2].summary = format!(
                            "{:.0}% of decisions timed out or were illegal (limit {:.0}%)",
                            rate * 100.0,
                            v.trial_max_substitution_rate * 100.0
                        );
                    } else if outcome.substitutions > 0 {
                        checks[2].status = CheckStatus::Warned;
                        checks[2].summary = format!(
                            "passed with {} substituted decision(s) out of {} over {} hands",
                            outcome.substitutions, outcome.decisions, outcome.hands
                        );
                    } else {
                        checks[2].status = CheckStatus::Passed;
                        checks[2].summary = format!(
                            "clean: {} decisions over {} hands, no timeouts or illegal actions",
                            outcome.decisions, outcome.hands
                        );
                    }
                }
            }
            self.sandbox.cleanup_session(&trial_session).await;
            checks[2].finished_at = Some(now_str());
            if checks[2].status == CheckStatus::Failed {
                let fail = SmokeReport {
                    passed: false,
                    first_latency_ms: report.first_latency_ms,
                    latency_ms: report.latency_ms,
                    reason: Some(SmokeFailure::Timeout),
                    message: format!("trial run failed: {}", checks[2].summary),
                    stderr_tail: report.stderr_tail.clone(),
                    actions: report.actions.clone(),
                };
                save(&self.store, &checks, None);
                self.store.set_submission_status(
                    &sub.id,
                    SubmissionStatus::Rejected,
                    Some(&fail),
                )?;
                return Ok(
                    json!({ "passed": false, "stage": "trial", "message": checks[2].summary }),
                );
            }
        } else {
            checks[2].status = CheckStatus::Skipped;
            checks[2].summary = "trial stage disabled".into();
        }

        // ---- validated (and optionally auto-activated)
        save(&self.store, &checks, None);
        self.store
            .set_submission_status(&sub.id, SubmissionStatus::Validated, Some(&report))?;
        let mut activated = false;
        if sub.auto_activate && v.allow_auto_activate {
            self.store.activate(&sub.team_id, &sub.id)?;
            activated = true;
        }
        tracing::info!(submission = %sub.id, activated, "validation pipeline passed");
        Ok(json!({ "passed": true, "activated": activated }))
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
                self.store.enqueue_weighted(
                    job_types::RUN_TOURNAMENT,
                    &json!({ "run_id": run_id, "index": t.index }),
                    Some(run_id),
                    priority,
                    2,
                    run.participants.len() as u32,
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

/// Outcome of the trial stage: N three-handed hands of (bot, call station, min-raiser).
pub struct TrialOutcome {
    pub hands: u32,
    pub decisions: u32,
    pub substitutions: u32,
    pub bot_alive: bool,
}

impl TrialOutcome {
    pub fn substitution_rate(&self) -> f64 {
        if self.decisions == 0 {
            1.0
        } else {
            self.substitutions as f64 / self.decisions as f64
        }
    }
}

/// Play `hands` quick 3-handed hands with the candidate in seat 0. Stacks reset every hand so the
/// bot always has decisions to make; the button rotates.
pub async fn trial_match(bot: Arc<dyn Player>, hands: u32, action_timeout_ms: u64) -> TrialOutcome {
    let call: Arc<dyn Player> = Arc::new(LocalBot::new(2, CallStrategy).named("trial-call"));
    let raise: Arc<dyn Player> = Arc::new(LocalBot::new(3, RaiseStrategy).named("trial-raise"));
    let players: Vec<Option<Arc<dyn Player>>> =
        vec![Some(Arc::clone(&bot)), Some(call), Some(raise)];
    let mut out = TrialOutcome {
        hands: 0,
        decisions: 0,
        substitutions: 0,
        bot_alive: true,
    };
    for i in 0..hands {
        let params = HandParams {
            hand_id: i as u64 + 1,
            table_id: 0,
            rules: poker_utils::HandRules {
                small_blind: 5,
                big_blind: 10,
                ante: 0,
            },
            button: (i % 3) as u8,
            seats: vec![Some((1, 1000)), Some((2, 1000)), Some((3, 1000))],
            deck: Deck::new(0xC0FFEE ^ i as u64),
        };
        match play_hand(params, &players, Duration::from_millis(action_timeout_ms)).await {
            Ok((_result, log)) => {
                out.hands += 1;
                for ev in &log {
                    match ev {
                        PublicEvent::ActionTaken { seat: 0, .. } => out.decisions += 1,
                        PublicEvent::ActionSubstituted { seat: 0, .. } => {
                            out.decisions += 1;
                            out.substitutions += 1;
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => break,
        }
        if !bot.is_alive() {
            out.bot_alive = false;
            break;
        }
    }
    out
}
