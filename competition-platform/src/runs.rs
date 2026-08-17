//! Run lifecycle helpers shared by the API, the scheduler and the workers.

use crate::ids::run_id;
use crate::models::*;
use crate::store::Store;
use anyhow::{bail, Result};
use serde_json::json;
use tournament_core::geometric_mean_scores;

pub struct RunRequest {
    pub kind: RunKind,
    pub series_length: usize,
    pub created_by: String,
    pub nightly_date: Option<String>,
    pub seed: Option<u64>,
}

/// Snapshot the active bots, create the run + tournament rows and enqueue the parent job.
pub fn create_run(store: &Store, settings: &PlatformSettings, req: RunRequest) -> Result<Run> {
    let participants = store.active_bots_snapshot()?;
    let mut tournament = settings.tournament.clone();
    tournament.rng_seed = req.seed.unwrap_or_else(|| rand::random::<u64>() >> 1);
    let config = RunConfig {
        series_length: req.series_length.max(1),
        tournament,
        record_hands: settings.record_hands,
    };
    let id = run_id();
    let run = store.create_run(
        &id,
        req.kind,
        &config,
        &participants,
        &req.created_by,
        req.nightly_date.as_deref(),
    )?;
    let priority = match req.kind {
        RunKind::Nightly => 5,
        RunKind::Ondemand => 3,
    };
    store.enqueue(
        job_types::RUN_SERIES,
        &json!({ "run_id": id }),
        Some(&id),
        priority,
        1,
    )?;
    Ok(run)
}

/// Cancel a run: mark it cancelled, drop queued tournaments/jobs. Running tournaments observe the
/// status change and abort. Returns false if the run was already terminal.
pub fn cancel_run(store: &Store, run_id: &str) -> Result<bool> {
    let ok = store.transition_run(
        run_id,
        &[RunStatus::Queued, RunStatus::Running, RunStatus::Finalizing],
        RunStatus::Cancelled,
        None,
    )?;
    if ok {
        store.cancel_pending_tournaments(run_id)?;
        store.cancel_queued_jobs(run_id)?;
    }
    Ok(ok)
}

/// If every tournament of the run is terminal, move the run to `finalizing` and enqueue the
/// finalize job (exactly once thanks to the compare-and-set).
pub fn maybe_finalize(store: &Store, run_id: &str) -> Result<bool> {
    let (total, _completed, terminal) = store.tournament_counts(run_id)?;
    if total == 0 || terminal < total {
        return Ok(false);
    }
    if store.transition_run(run_id, &[RunStatus::Running], RunStatus::Finalizing, None)? {
        store.enqueue(
            job_types::FINALIZE_SERIES,
            &json!({ "run_id": run_id }),
            Some(run_id),
            8,
            3,
        )?;
        return Ok(true);
    }
    Ok(false)
}

/// Compute geometric-mean scores from the run's placements and publish them.
pub fn finalize_run(store: &Store, run_id: &str) -> Result<Vec<ScoreRow>> {
    let Some(run) = store.run(run_id)? else {
        bail!("run {} not found", run_id);
    };
    if run.status == RunStatus::Cancelled {
        return Ok(vec![]);
    }
    let placements = store.placements(run_id, None)?;
    let (_, completed, _) = store.tournament_counts(run_id)?;
    if completed == 0 {
        store.transition_run(
            run_id,
            &[RunStatus::Finalizing, RunStatus::Running],
            RunStatus::Failed,
            Some("no tournament completed"),
        )?;
        return Ok(vec![]);
    }
    // Map team ids to dense numeric ids for the scoring function.
    let mut ids: Vec<String> = placements.iter().map(|p| p.team_id.clone()).collect();
    ids.sort();
    ids.dedup();
    let mut by_player = std::collections::HashMap::new();
    for p in &placements {
        let pid = ids.iter().position(|t| *t == p.team_id).unwrap() as u32;
        by_player.entry(pid).or_insert_with(Vec::new).push(p.place);
    }
    let scores: Vec<ScoreRow> = geometric_mean_scores(&by_player)
        .into_iter()
        .map(|s| ScoreRow {
            run_id: run_id.to_string(),
            team_id: ids[s.player_id as usize].clone(),
            geo_mean: s.geo_mean,
            rank: s.rank,
            tournaments_counted: s.tournaments_counted,
        })
        .collect();
    store.save_scores(run_id, &scores)?;
    store.transition_run(
        run_id,
        &[RunStatus::Finalizing, RunStatus::Running],
        RunStatus::Completed,
        None,
    )?;
    Ok(scores)
}
