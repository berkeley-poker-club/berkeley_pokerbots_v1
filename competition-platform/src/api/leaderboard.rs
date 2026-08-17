//! Public leaderboard and team stats.

use super::auth::AuthTeam;
use super::errors::{ApiError, ApiResult};
use super::SharedState;
use crate::models::{Run, RunKind};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct BoardQuery {
    pub run_id: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

fn board_for_run(state: &SharedState, run: &Run, limit: usize, offset: usize) -> ApiResult<Value> {
    let scores = state.store.scores(&run.id)?;
    let by_team: HashMap<&str, &crate::models::Participant> = run
        .participants
        .iter()
        .map(|p| (p.team_id.as_str(), p))
        .collect();
    let entries: Vec<Value> = scores
        .iter()
        .skip(offset)
        .take(limit)
        .map(|s| {
            let p = by_team.get(s.team_id.as_str());
            json!({
                "rank": s.rank,
                "team_id": s.team_id,
                "team_name": p.map(|p| p.team_name.clone()),
                "submission_id": p.map(|p| p.submission_id.clone()),
                "geo_mean_placement": (s.geo_mean * 1000.0).round() / 1000.0,
                "tournaments_counted": s.tournaments_counted,
            })
        })
        .collect();
    Ok(json!({
        "run_id": run.id,
        "kind": run.kind,
        "updated_at": run.finished_at,
        "participant_count": run.participants.len(),
        "series_length": run.config.series_length,
        "total_entries": scores.len(),
        "entries": entries,
    }))
}

/// `GET /leaderboard`
pub async fn current(
    State(state): State<SharedState>,
    Query(q): Query<BoardQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0);
    let run = match &q.run_id {
        Some(id) => state
            .store
            .run(id)?
            .ok_or_else(|| ApiError::not_found("run"))?,
        None => match state.store.latest_completed_run(Some(RunKind::Nightly))? {
            Some(r) => r,
            None => match state.store.latest_completed_run(None)? {
                Some(r) => r,
                None => {
                    return Ok(Json(json!({
                        "run_id": null,
                        "updated_at": null,
                        "entries": [],
                        "message": "no completed run yet",
                    })))
                }
            },
        },
    };
    Ok(Json(board_for_run(&state, &run, limit, offset)?))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<usize>,
    pub kind: Option<String>,
}

/// `GET /leaderboard/history`
pub async fn history(
    State(state): State<SharedState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(30).clamp(1, 365);
    let kind = q
        .kind
        .as_deref()
        .map(RunKind::parse)
        .or(Some(RunKind::Nightly));
    let runs =
        state
            .store
            .list_runs(Some(crate::models::RunStatus::Completed), kind, limit, None)?;
    let mut boards = Vec::new();
    for r in &runs {
        let mut b = board_for_run(&state, r, 3, 0)?;
        b["top"] = b["entries"].take();
        b.as_object_mut().map(|o| o.remove("entries"));
        boards.push(b);
    }
    Ok(Json(json!({ "boards": boards })))
}

/// `GET /teams/{team_id}/stats`
pub async fn team_stats(
    State(state): State<SharedState>,
    AuthTeam(_caller): AuthTeam,
    Path(team_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let team = state
        .store
        .team(&team_id)?
        .ok_or_else(|| ApiError::not_found("team"))?;
    let active = state.store.active_submission_id(&team.id)?;
    let scores = state.store.team_scores(&team.id, 30)?;
    let placements = state.store.team_placements(&team.id, 200)?;
    let mut per_run: HashMap<String, Vec<usize>> = HashMap::new();
    for p in &placements {
        per_run.entry(p.run_id.clone()).or_default().push(p.place);
    }
    let best = placements.iter().map(|p| p.place).min();
    let avg = if placements.is_empty() {
        None
    } else {
        Some(placements.iter().map(|p| p.place as f64).sum::<f64>() / placements.len() as f64)
    };
    Ok(Json(json!({
        "team_id": team.id,
        "team_name": team.name,
        "active_submission_id": active,
        "suspended": team.suspended,
        "recent_scores": scores.iter().map(|s| json!({
            "run_id": s.run_id, "rank": s.rank, "geo_mean_placement": s.geo_mean, "tournaments_counted": s.tournaments_counted,
        })).collect::<Vec<_>>(),
        "recent_placements": placements.iter().take(50).map(|p| json!({
            "run_id": p.run_id, "tournament_index": p.tournament_index, "place": p.place, "hands_played": p.hands_played,
        })).collect::<Vec<_>>(),
        "summary": {
            "tournaments_played": placements.len(),
            "best_place": best,
            "average_place": avg,
            "wins": placements.iter().filter(|p| p.place == 1).count(),
        },
    })))
}
