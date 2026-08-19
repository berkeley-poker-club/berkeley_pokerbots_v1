//! Authentication: team API keys (`X-Api-Key` / `Authorization: Bearer`) and the admin key.

use super::errors::{ApiError, ApiResult};
use super::SharedState;
use crate::ids::{api_key, hash_key, key_prefix, team_id};
use crate::models::{Team, TeamMember};
use axum::extract::{FromRequestParts, Path, State};
use axum::Json;
use http::request::Parts;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

fn bearer_or_key(parts: &Parts) -> Option<String> {
    if let Some(v) = parts.headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    if let Some(v) = parts
        .headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        {
            if !rest.trim().is_empty() {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

fn admin_key(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get("x-admin-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// An authenticated team.
pub struct AuthTeam(pub Team);

impl FromRequestParts<SharedState> for AuthTeam {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, ApiError> {
        let key = bearer_or_key(parts).ok_or_else(|| {
            ApiError::unauthorized("missing API key (X-Api-Key or Authorization: Bearer)")
        })?;
        let team = state
            .store
            .team_by_key_hash(&hash_key(&key))
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::unauthorized("invalid API key"))?;
        Ok(AuthTeam(team))
    }
}

/// Admin access: `X-Admin-Key` matching the configured key, or a team flagged `is_admin`.
pub struct Admin {
    pub team: Option<Team>,
}

impl FromRequestParts<SharedState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, ApiError> {
        if let (Some(provided), Some(expected)) =
            (admin_key(parts), state.cfg.server.admin_key.as_ref())
        {
            if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                return Ok(Admin { team: None });
            }
            return Err(ApiError::forbidden("invalid admin key"));
        }
        if let Some(key) = bearer_or_key(parts) {
            if let Some(team) = state
                .store
                .team_by_key_hash(&hash_key(&key))
                .map_err(ApiError::internal)?
            {
                if team.is_admin {
                    return Ok(Admin { team: Some(team) });
                }
                return Err(ApiError::forbidden("admin role required"));
            }
        }
        Err(ApiError::unauthorized(
            "admin credentials required (X-Admin-Key or an admin team key)",
        ))
    }
}

/// Either an authenticated team or an admin.
pub struct TeamOrAdmin {
    pub team: Option<Team>,
    pub is_admin: bool,
}

impl FromRequestParts<SharedState> for TeamOrAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, ApiError> {
        if let (Some(provided), Some(expected)) =
            (admin_key(parts), state.cfg.server.admin_key.as_ref())
        {
            if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                return Ok(TeamOrAdmin {
                    team: None,
                    is_admin: true,
                });
            }
            return Err(ApiError::forbidden("invalid admin key"));
        }
        let AuthTeam(team) = AuthTeam::from_request_parts(parts, state).await?;
        let is_admin = team.is_admin;
        Ok(TeamOrAdmin {
            team: Some(team),
            is_admin,
        })
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------- handlers

#[derive(Deserialize)]
pub struct RegisterBody {
    pub team_name: String,
    #[serde(default)]
    pub members: Vec<TeamMember>,
}

pub fn validate_team_name(name: &str) -> Result<(), ApiError> {
    let n = name.trim();
    if n.len() < 2 || n.len() > 40 {
        return Err(ApiError::validation("team_name must be 2–40 characters"));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' || c == '.')
    {
        return Err(ApiError::validation(
            "team_name may contain letters, digits, spaces, '-', '_' and '.'",
        ));
    }
    Ok(())
}

pub fn team_json(team: &Team, members: &[TeamMember]) -> Value {
    json!({
        "team_id": team.id,
        "team_name": team.name,
        "api_key_prefix": team.api_key_prefix,
        "is_admin": team.is_admin,
        "suspended": team.suspended,
        "created_at": team.created_at,
        "members": members,
    })
}

/// `POST /auth/register`
pub async fn register(
    State(state): State<SharedState>,
    Json(body): Json<RegisterBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let settings = state.store.settings()?;
    if !settings.registration_open {
        return Err(ApiError::forbidden(
            "registration is closed; ask an admin to provision your team",
        ));
    }
    validate_team_name(&body.team_name)?;
    let key = api_key();
    let team = state.store.create_team(
        &team_id(),
        body.team_name.trim(),
        &hash_key(&key),
        &key_prefix(&key),
        false,
        &body.members,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "team_id": team.id,
            "team_name": team.name,
            "api_key": key,
            "created_at": team.created_at,
        })),
    ))
}

/// `GET /me`
pub async fn me(
    State(state): State<SharedState>,
    AuthTeam(team): AuthTeam,
) -> ApiResult<Json<Value>> {
    let members = state.store.team_members(&team.id)?;
    let active = state.store.active_submission_id(&team.id)?;
    let settings = state.store.settings()?;
    let used = state.store.count_recent_ondemand_runs(&team.id, 24)?;
    let mut v = team_json(&team, &members);
    v["active_submission_id"] = json!(active);
    v["rate_limits"] = json!({
        "ondemand_runs_per_day": settings.ondemand_runs_per_team_per_day,
        "ondemand_runs_remaining": settings.ondemand_runs_per_team_per_day.saturating_sub(used),
        "ondemand_max_series_length": settings.ondemand_max_series_length,
    });
    Ok(Json(v))
}

/// `POST /teams/{team_id}/rotate-key`
pub async fn rotate_key(
    State(state): State<SharedState>,
    caller: TeamOrAdmin,
    Path(team_id): Path<String>,
) -> ApiResult<Json<Value>> {
    if !caller.is_admin
        && caller
            .team
            .as_ref()
            .map(|t| t.id != team_id)
            .unwrap_or(true)
    {
        return Err(ApiError::forbidden("you can only rotate your own key"));
    }
    let key = api_key();
    state
        .store
        .rotate_key(&team_id, &hash_key(&key), &key_prefix(&key))?;
    Ok(Json(json!({ "team_id": team_id, "api_key": key })))
}
