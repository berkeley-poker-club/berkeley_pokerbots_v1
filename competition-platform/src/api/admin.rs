//! Admin endpoints.

use super::auth::{team_json, validate_team_name, Admin};
use super::errors::{ApiError, ApiResult};
use super::runs::run_json;
use super::SharedState;
use crate::ids::{api_key, hash_key, key_prefix, team_id};
use crate::models::{job_types, PlatformSettings, RunKind, TeamMember};
use crate::runs::{create_run, RunRequest};
use axum::extract::{Path, State};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

/// `GET /admin/config`
pub async fn get_config(
    State(state): State<SharedState>,
    _admin: Admin,
) -> ApiResult<Json<PlatformSettings>> {
    Ok(Json(state.store.settings()?))
}

fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge(existing, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// `PATCH /admin/config` — deep-merges the JSON body into the current settings.
pub async fn patch_config(
    State(state): State<SharedState>,
    _admin: Admin,
    Json(patch): Json<Value>,
) -> ApiResult<Json<PlatformSettings>> {
    let current = state.store.settings()?;
    let mut v = serde_json::to_value(&current).map_err(ApiError::internal)?;
    merge(&mut v, &patch);
    let updated: PlatformSettings = serde_json::from_value(v)
        .map_err(|e| ApiError::validation(format!("invalid settings: {e}")))?;
    updated.tournament.validate().map_err(|errs| {
        ApiError::validation(format!("invalid tournament config: {}", errs.join("; ")))
    })?;
    if parse_hhmm(&updated.nightly_time_utc).is_none() {
        return Err(ApiError::validation("nightly_time_utc must be HH:MM (UTC)"));
    }
    if updated.nightly_series_length == 0 {
        return Err(ApiError::validation(
            "nightly_series_length must be positive",
        ));
    }
    state.store.save_settings(&updated)?;
    Ok(Json(updated))
}

pub fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h < 24 && m < 60 {
        Some((h, m))
    } else {
        None
    }
}

#[derive(Deserialize)]
pub struct CreateTeamBody {
    pub team_name: String,
    #[serde(default)]
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub is_admin: bool,
}

/// `POST /admin/teams`
pub async fn create_team(
    State(state): State<SharedState>,
    _admin: Admin,
    Json(body): Json<CreateTeamBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    validate_team_name(&body.team_name)?;
    let key = api_key();
    let team = state.store.create_team(
        &team_id(),
        body.team_name.trim(),
        &hash_key(&key),
        &key_prefix(&key),
        body.is_admin,
        &body.members,
    )?;
    let mut v = team_json(&team, &body.members);
    v["api_key"] = json!(key);
    Ok((StatusCode::CREATED, Json(v)))
}

/// `GET /admin/teams`
pub async fn list_teams(State(state): State<SharedState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let teams = state.store.list_teams()?;
    let mut out = Vec::new();
    for t in &teams {
        let members = state.store.team_members(&t.id)?;
        let mut v = team_json(t, &members);
        v["active_submission_id"] = json!(state.store.active_submission_id(&t.id)?);
        out.push(v);
    }
    Ok(Json(json!({ "teams": out })))
}

#[derive(Deserialize, Default)]
pub struct SuspendBody {
    #[serde(default = "default_true")]
    pub suspended: bool,
}
fn default_true() -> bool {
    true
}

/// `POST /admin/teams/{team_id}/suspend`
pub async fn suspend_team(
    State(state): State<SharedState>,
    _admin: Admin,
    Path(team_id): Path<String>,
    body: Option<Json<SuspendBody>>,
) -> ApiResult<Json<Value>> {
    let suspended = body.map(|b| b.0.suspended).unwrap_or(true);
    state.store.set_suspended(&team_id, suspended)?;
    Ok(Json(json!({ "team_id": team_id, "suspended": suspended })))
}

/// `GET /admin/workers`
pub async fn workers(State(state): State<SharedState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let workers = state.store.workers(120)?;
    let depth = state.store.queue_depth()?;
    Ok(Json(json!({
        "workers": workers,
        "queue": depth.iter().map(|(t, s, n)| json!({ "type": t, "status": s, "count": n })).collect::<Vec<_>>(),
    })))
}

/// `POST /admin/revalidate/{submission_id}`
pub async fn revalidate(
    State(state): State<SharedState>,
    _admin: Admin,
    Path(submission_id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let sub = state
        .store
        .submission(&submission_id)?
        .ok_or_else(|| ApiError::not_found("submission"))?;
    let job_id = state.store.enqueue(
        job_types::SMOKE_VALIDATE,
        &json!({ "submission_id": sub.id }),
        None,
        10,
        2,
    )?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "submission_id": sub.id, "job_id": job_id })),
    ))
}

/// `GET /admin/autoscale` — the current scaling plan and the last action the autoscaler took.
pub async fn autoscale(State(state): State<SharedState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let settings = state.store.settings()?;
    let plan = crate::autoscale::plan(&state.store, &settings.autoscale)?;
    let status = state.store.get_setting("autoscale_status")?;
    Ok(Json(json!({
        "settings": settings.autoscale,
        "plan": plan,
        "last_action": status,
    })))
}

/// `GET /admin/metrics`
pub async fn metrics(State(state): State<SharedState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let mut v = state.store.counts()?;
    v["workers_alive"] = json!(state.store.workers(120)?.len());
    v["queue"] = json!(state
        .store
        .queue_depth()?
        .iter()
        .map(|(t, s, n)| json!({ "type": t, "status": s, "count": n }))
        .collect::<Vec<_>>());
    v["started_at"] = json!(state.started_at);
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct NightlyBody {
    #[serde(default)]
    pub series_length: Option<usize>,
    #[serde(default)]
    pub seed: Option<u64>,
}

/// `POST /admin/runs/nightly`
pub async fn nightly(
    State(state): State<SharedState>,
    admin: Admin,
    body: Option<Json<NightlyBody>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let settings = state.store.settings()?;
    let run = create_run(
        &state.store,
        &settings,
        RunRequest {
            kind: RunKind::Nightly,
            series_length: body.series_length.unwrap_or(settings.nightly_series_length),
            created_by: admin.team.map(|t| t.id).unwrap_or_else(|| "admin".into()),
            nightly_date: None,
            seed: body.seed,
        },
    )?;
    Ok((StatusCode::ACCEPTED, Json(run_json(&state, &run)?)))
}

/// `GET /admin/runs/{run_id}/jobs`
pub async fn run_jobs(
    State(state): State<SharedState>,
    _admin: Admin,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let jobs = state.store.jobs_for_run(&run_id)?;
    Ok(Json(json!({ "run_id": run_id, "jobs": jobs })))
}
