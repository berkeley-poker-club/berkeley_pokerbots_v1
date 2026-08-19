//! Runs: list/get/create/cancel + tournament detail.

use super::auth::{Admin, TeamOrAdmin};
use super::errors::{ApiError, ApiResult};
use super::SharedState;
use crate::models::{Run, RunKind, RunStatus, TournamentRow};
use crate::runs::{cancel_run, create_run, RunRequest};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn run_json(state: &SharedState, run: &Run) -> ApiResult<Value> {
    let (total, completed, terminal) = state.store.tournament_counts(&run.id)?;
    Ok(json!({
        "run_id": run.id,
        "kind": run.kind,
        "status": run.status,
        "config": {
            "series_length": run.config.series_length,
            "table_size": run.config.tournament.table_size,
            "seed": run.config.tournament.rng_seed,
            "action_timeout_ms": run.config.tournament.action_timeout_ms,
            "starting_stack": run.config.tournament.starting_stack,
            "record_hands": run.config.record_hands,
            "tournament": run.config.tournament,
        },
        "participant_count": run.participants.len(),
        "created_by": run.created_by,
        "created_at": run.created_at,
        "snapshot_at": run.snapshot_at,
        "started_at": run.started_at,
        "finished_at": run.finished_at,
        "error": run.error,
        "nightly_date": run.nightly_date,
        "progress": {
            "tournaments_total": total,
            "tournaments_completed": completed,
            "tournaments_failed_or_cancelled": terminal - completed,
        },
    }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<usize>,
    /// `created_at` cursor: return runs created before this timestamp.
    pub cursor: Option<String>,
}

/// `GET /runs`
pub async fn list(
    State(state): State<SharedState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let status = q.status.as_deref().map(RunStatus::parse);
    let kind = q.kind.as_deref().map(RunKind::parse);
    let runs = state
        .store
        .list_runs(status, kind, limit, q.cursor.as_deref())?;
    let next_cursor = if runs.len() == limit {
        runs.last().map(|r| r.created_at.clone())
    } else {
        None
    };
    let items = runs
        .iter()
        .map(|r| run_json(&state, r))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(json!({ "runs": items, "next_cursor": next_cursor })))
}

/// `GET /runs/{run_id}`
pub async fn get_one(
    State(state): State<SharedState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let run = state
        .store
        .run(&run_id)?
        .ok_or_else(|| ApiError::not_found("run"))?;
    let mut v = run_json(&state, &run)?;
    v["participants"] = json!(run
        .participants
        .iter()
        .map(|p| json!({ "team_id": p.team_id, "team_name": p.team_name, "submission_id": p.submission_id }))
        .collect::<Vec<_>>());
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct CreateBody {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub series_length: Option<usize>,
    #[serde(default)]
    pub include_inactive: bool,
    #[serde(default)]
    pub seed: Option<u64>,
}

/// `POST /runs` — on-demand run (team: rate limited; admin: unlimited)
pub async fn create(
    State(state): State<SharedState>,
    caller: TeamOrAdmin,
    body: Option<Json<CreateBody>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let settings = state.store.settings()?;
    let mode = body.mode.clone().unwrap_or_else(|| "series".into());
    let mut series_length = match mode.as_str() {
        "single" => 1,
        "series" => body
            .series_length
            .unwrap_or(settings.ondemand_max_series_length.min(10)),
        other => {
            return Err(ApiError::validation(format!(
                "mode must be 'series' or 'single' (got '{other}')"
            )))
        }
    };
    if body.include_inactive {
        return Err(ApiError::validation(
            "include_inactive is not supported: runs always use the active-bot snapshot",
        ));
    }
    let created_by = if caller.is_admin {
        caller
            .team
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_else(|| "admin".into())
    } else {
        let team = caller.team.as_ref().expect("team present when not admin");
        if team.suspended {
            return Err(ApiError::forbidden("team is suspended"));
        }
        let used = state.store.count_recent_ondemand_runs(&team.id, 24)?;
        if used >= settings.ondemand_runs_per_team_per_day {
            return Err(ApiError::rate_limited(format!(
                "on-demand run limit reached ({} per 24h)",
                settings.ondemand_runs_per_team_per_day
            )));
        }
        if series_length > settings.ondemand_max_series_length {
            return Err(ApiError::validation(format!(
                "series_length exceeds the on-demand maximum of {}",
                settings.ondemand_max_series_length
            )));
        }
        team.id.clone()
    };
    if series_length == 0 {
        series_length = 1;
    }
    let kind = match body.kind.as_deref() {
        None | Some("ondemand") => RunKind::Ondemand,
        Some("nightly") if caller.is_admin => RunKind::Nightly,
        Some("nightly") => return Err(ApiError::forbidden("only admins can start nightly runs")),
        Some(other) => return Err(ApiError::validation(format!("unknown kind '{other}'"))),
    };
    let run = create_run(
        &state.store,
        &settings,
        RunRequest {
            kind,
            series_length,
            created_by,
            nightly_date: None,
            seed: body.seed,
        },
    )?;
    let mut v = run_json(&state, &run)?;
    if run.participants.len() < 2 {
        v["warning"] = json!("fewer than two active bots; the run will fail");
    }
    Ok((StatusCode::ACCEPTED, Json(v)))
}

/// `POST /runs/{run_id}/cancel` (admin)
pub async fn cancel(
    State(state): State<SharedState>,
    _admin: Admin,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let run = state
        .store
        .run(&run_id)?
        .ok_or_else(|| ApiError::not_found("run"))?;
    let cancelled = cancel_run(&state.store, &run.id)?;
    if !cancelled {
        return Err(ApiError::conflict(format!(
            "run is already {}",
            run.status.as_str()
        )));
    }
    let run = state
        .store
        .run(&run_id)?
        .ok_or_else(|| ApiError::not_found("run"))?;
    Ok(Json(run_json(&state, &run)?))
}

fn tournament_json(t: &TournamentRow, names: &HashMap<String, String>) -> Value {
    json!({
        "index": t.index,
        "seed": t.seed,
        "status": t.status,
        "started_at": t.started_at,
        "finished_at": t.finished_at,
        "worker_id": t.worker_id,
        "total_hands": t.total_hands,
        "winner_team_id": t.winner_team_id,
        "winner_team_name": t.winner_team_id.as_ref().and_then(|id| names.get(id)),
        "error": t.error,
    })
}

/// `GET /runs/{run_id}/tournaments`
pub async fn tournaments(
    State(state): State<SharedState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let run = state
        .store
        .run(&run_id)?
        .ok_or_else(|| ApiError::not_found("run"))?;
    let names: HashMap<String, String> = run
        .participants
        .iter()
        .map(|p| (p.team_id.clone(), p.team_name.clone()))
        .collect();
    let rows = state.store.tournaments(&run_id)?;
    Ok(Json(json!({
        "run_id": run_id,
        "tournaments": rows.iter().map(|t| tournament_json(t, &names)).collect::<Vec<_>>(),
    })))
}

/// `GET /runs/{run_id}/tournaments/{index}`
pub async fn tournament(
    State(state): State<SharedState>,
    Path((run_id, index)): Path<(String, usize)>,
) -> ApiResult<Json<Value>> {
    let run = state
        .store
        .run(&run_id)?
        .ok_or_else(|| ApiError::not_found("run"))?;
    let names: HashMap<String, String> = run
        .participants
        .iter()
        .map(|p| (p.team_id.clone(), p.team_name.clone()))
        .collect();
    let t = state
        .store
        .tournament(&run_id, index)?
        .ok_or_else(|| ApiError::not_found("tournament"))?;
    let placements = state.store.placements(&run_id, Some(index))?;
    let mut v = tournament_json(&t, &names);
    v["placements"] = json!(placements
        .iter()
        .map(|p| json!({
            "team_id": p.team_id,
            "team_name": names.get(&p.team_id),
            "place": p.place,
            "hands_played": p.hands_played,
        }))
        .collect::<Vec<_>>());
    if let Some(outcome) = &t.outcome {
        v["eliminations"] = outcome.get("eliminations").cloned().unwrap_or(Value::Null);
        v["levels_reached"] = outcome
            .get("levels_reached")
            .cloned()
            .unwrap_or(Value::Null);
        v["duration_ms"] = outcome.get("duration_ms").cloned().unwrap_or(Value::Null);
        v["spawn_failures"] = outcome
            .get("spawn_failures")
            .cloned()
            .unwrap_or(Value::Null);
    }
    if run.config.record_hands {
        v["hand_log_url"] = json!(format!(
            "/api/v1/runs/{}/tournaments/{}/hands",
            run_id, index
        ));
    }
    Ok(Json(v))
}

/// `GET /runs/{run_id}/tournaments/{index}/hands` (admin) — JSON-lines hand history.
pub async fn hand_log(
    State(state): State<SharedState>,
    _admin: Admin,
    Path((run_id, index)): Path<(String, usize)>,
) -> ApiResult<Response> {
    let path = state
        .cfg
        .storage
        .logs_dir
        .join(&run_id)
        .join(index.to_string())
        .join("hands.jsonl");
    if !path.exists() {
        return Err(ApiError::not_found("hand log"));
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(ApiError::internal)?;
    let stream = tokio_util_stream(file);
    Ok((
        [(http::header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

fn tokio_util_stream(
    file: tokio::fs::File,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    futures::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(bytes::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    })
}
