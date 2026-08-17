//! Submission upload / list / activate / delete.

use super::auth::AuthTeam;
use super::errors::{ApiError, ApiResult};
use super::SharedState;
use crate::models::{job_types, Manifest, Submission, SubmissionStatus};
use axum::extract::{Multipart, Path, Query, State};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

pub fn submission_json(s: &Submission, active_id: Option<&str>) -> Value {
    json!({
        "submission_id": s.id,
        "team_id": s.team_id,
        "seq": s.seq,
        "status": s.status,
        "protocol_version": s.protocol_version,
        "manifest": s.manifest,
        "artifact_sha256": s.artifact_sha256,
        "artifact_size": s.artifact_size,
        "created_at": s.created_at,
        "validated_at": s.validated_at,
        "smoke_test": s.smoke_test,
        "checks": s.checks,
        "auto_activate": s.auto_activate,
        "in_progress": s.status.in_progress(),
        "active": active_id == Some(s.id.as_str()),
    })
}

/// `POST /submissions` (multipart: `artifact` file + `manifest` JSON string)
pub async fn upload(
    State(state): State<SharedState>,
    AuthTeam(team): AuthTeam,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if team.suspended {
        return Err(ApiError::forbidden("team is suspended"));
    }
    let settings = state.store.settings()?;
    let mut artifact: Option<Vec<u8>> = None;
    let mut manifest: Option<Manifest> = None;
    let mut auto_activate = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::validation(format!("invalid multipart body: {e}")))?
    {
        match field.name().unwrap_or("") {
            "artifact" => {
                let bytes = field.bytes().await.map_err(|e| {
                    ApiError::payload_too_large(format!("artifact upload failed: {e}"))
                })?;
                if bytes.len() as u64 > settings.max_upload_bytes {
                    return Err(ApiError::payload_too_large(format!(
                        "artifact is {} bytes; the limit is {} bytes",
                        bytes.len(),
                        settings.max_upload_bytes
                    )));
                }
                artifact = Some(bytes.to_vec());
            }
            "activate" => {
                let text = field.text().await.unwrap_or_default();
                auto_activate = matches!(text.trim(), "1" | "true" | "on" | "yes");
            }
            "manifest" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::validation(format!("manifest part unreadable: {e}")))?;
                let m: Manifest = serde_json::from_str(&text).map_err(|e| {
                    ApiError::validation(format!("manifest is not valid JSON: {e}"))
                })?;
                manifest = Some(m);
            }
            other => {
                tracing::debug!(field = other, "ignoring unknown multipart field");
            }
        }
    }
    let artifact = artifact.ok_or_else(|| ApiError::validation("missing 'artifact' file part"))?;
    let manifest = manifest.ok_or_else(|| ApiError::validation("missing 'manifest' JSON part"))?;
    if artifact.is_empty() {
        return Err(ApiError::validation("artifact is empty"));
    }
    manifest.validate().map_err(ApiError::validation)?;

    let artifacts = state.artifacts.clone();
    let sub = state.store.create_submission(
        &team.id,
        |id| {
            // Path is decided by the artifact store; save happens right after with the same id.
            format!(
                "{}/artifact.{}",
                id,
                if artifact.starts_with(b"PK\x03\x04") {
                    "zip"
                } else {
                    "bin"
                }
            )
        },
        &crate::ids::sha256_hex(&artifact),
        artifact.len() as u64,
        &manifest,
        auto_activate && settings.validation.allow_auto_activate,
    )?;
    let saved = artifacts
        .save(&sub.id, &artifact)
        .map_err(ApiError::internal)?;
    debug_assert_eq!(saved.relative_path, sub.artifact_path);

    state.store.enqueue(
        job_types::SMOKE_VALIDATE,
        &json!({ "submission_id": sub.id }),
        None,
        10,
        2,
    )?;

    // Wait (bounded) for the validation pipeline so the uploader gets an immediate verdict.
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(state.cfg.server.smoke_wait_ms);
    let mut current = sub.clone();
    while current.status.in_progress() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(s) = state.store.submission(&sub.id)? {
            current = s;
        }
    }
    let active = state.store.active_submission_id(&team.id)?;
    match current.status {
        SubmissionStatus::Validated => Ok((
            StatusCode::CREATED,
            Json(submission_json(&current, active.as_deref())),
        )),
        SubmissionStatus::Rejected => {
            let report = current.smoke_test.clone();
            let failed = current
                .checks
                .iter()
                .find(|c| c.status == crate::models::CheckStatus::Failed);
            Err(
                ApiError::validation("bot failed validation").with_details(json!({
                    "submission_id": current.id,
                    "stage": failed.map(|c| c.stage),
                    "stage_summary": failed.map(|c| c.summary.clone()),
                    "checks": current.checks,
                    "reason": report.as_ref().and_then(|r| r.reason.clone()),
                    "message": report.as_ref().map(|r| r.message.clone()),
                    "stderr_tail": report.as_ref().map(|r| r.stderr_tail.clone()),
                    "actions": report.as_ref().map(|r| r.actions.clone()),
                })),
            )
        }
        _ => {
            let mut v = submission_json(&current, active.as_deref());
            v["hint"] = json!("validation still running; poll GET /submissions/{submission_id}");
            Ok((StatusCode::ACCEPTED, Json(v)))
        }
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    /// Sequence number cursor: return submissions with `seq < cursor`.
    #[serde(default)]
    pub cursor: Option<u64>,
}

/// `GET /submissions`
pub async fn list(
    State(state): State<SharedState>,
    AuthTeam(team): AuthTeam,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let subs = state.store.list_submissions(&team.id, limit, q.cursor)?;
    let active = state.store.active_submission_id(&team.id)?;
    let next_cursor = if subs.len() == limit {
        subs.last().map(|s| s.seq)
    } else {
        None
    };
    Ok(Json(json!({
        "submissions": subs.iter().map(|s| submission_json(s, active.as_deref())).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
        "active_submission_id": active,
    })))
}

fn load_owned(state: &SharedState, team_id: &str, submission_id: &str) -> ApiResult<Submission> {
    let sub = state
        .store
        .submission(submission_id)?
        .ok_or_else(|| ApiError::not_found("submission"))?;
    if sub.team_id != team_id {
        return Err(ApiError::not_found("submission"));
    }
    Ok(sub)
}

/// `GET /submissions/{submission_id}`
pub async fn get_one(
    State(state): State<SharedState>,
    AuthTeam(team): AuthTeam,
    Path(submission_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = load_owned(&state, &team.id, &submission_id)?;
    let active = state.store.active_submission_id(&team.id)?;
    Ok(Json(submission_json(&sub, active.as_deref())))
}

/// `POST /submissions/{submission_id}/activate`
pub async fn activate(
    State(state): State<SharedState>,
    AuthTeam(team): AuthTeam,
    Path(submission_id): Path<String>,
) -> ApiResult<Json<Value>> {
    if team.suspended {
        return Err(ApiError::forbidden("team is suspended"));
    }
    let sub = load_owned(&state, &team.id, &submission_id)?;
    if sub.status != SubmissionStatus::Validated {
        return Err(ApiError::conflict(format!(
            "submission is '{}'; only validated submissions can be activated",
            sub.status.as_str()
        )));
    }
    let previous = state.store.activate(&team.id, &sub.id)?;
    Ok(Json(json!({
        "team_id": team.id,
        "active_submission_id": sub.id,
        "previous_submission_id": previous,
    })))
}

/// `DELETE /submissions/{submission_id}` — soft delete of inactive submissions.
pub async fn delete(
    State(state): State<SharedState>,
    AuthTeam(team): AuthTeam,
    Path(submission_id): Path<String>,
) -> ApiResult<StatusCode> {
    let sub = load_owned(&state, &team.id, &submission_id)?;
    let active = state.store.active_submission_id(&team.id)?;
    if active.as_deref() == Some(sub.id.as_str()) {
        return Err(ApiError::conflict(
            "cannot delete the active submission; activate another one first",
        ));
    }
    state
        .store
        .set_submission_status(&sub.id, SubmissionStatus::Deleted, None)?;
    Ok(StatusCode::NO_CONTENT)
}
