//! Competition HTTP API (Axum). See `docs/openapi.yaml`.

pub mod admin;
pub mod auth;
pub mod errors;
pub mod leaderboard;
pub mod runs;
pub mod submissions;

use crate::artifacts::ArtifactStore;
use crate::config::PlatformConfig;
use crate::store::Store;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub cfg: PlatformConfig,
    pub store: Arc<Store>,
    pub artifacts: Arc<ArtifactStore>,
    pub started_at: String,
}

pub type SharedState = Arc<AppState>;

pub const OPENAPI_YAML: &str = include_str!("../../../docs/openapi.yaml");

/// Build the full router (mounted at `/api/v1` plus root health endpoints).
pub fn router(state: SharedState) -> Router {
    let max_upload = state.cfg.defaults.max_upload_bytes.max(1024 * 1024) as usize + 64 * 1024;
    let v1 = Router::new()
        // auth & teams
        .route("/auth/register", post(auth::register))
        .route("/me", get(auth::me))
        .route("/teams/{team_id}/rotate-key", post(auth::rotate_key))
        .route("/teams/{team_id}/stats", get(leaderboard::team_stats))
        // submissions
        .route(
            "/submissions",
            post(submissions::upload).get(submissions::list),
        )
        .route(
            "/submissions/{submission_id}",
            get(submissions::get_one).delete(submissions::delete),
        )
        .route(
            "/submissions/{submission_id}/activate",
            post(submissions::activate),
        )
        // runs
        .route("/runs", get(runs::list).post(runs::create))
        .route("/runs/{run_id}", get(runs::get_one))
        .route("/runs/{run_id}/cancel", post(runs::cancel))
        .route("/runs/{run_id}/tournaments", get(runs::tournaments))
        .route("/runs/{run_id}/tournaments/{index}", get(runs::tournament))
        .route(
            "/runs/{run_id}/tournaments/{index}/hands",
            get(runs::hand_log),
        )
        // leaderboard
        .route("/leaderboard", get(leaderboard::current))
        .route("/leaderboard/history", get(leaderboard::history))
        // admin
        .route(
            "/admin/config",
            get(admin::get_config).patch(admin::patch_config),
        )
        .route(
            "/admin/teams",
            post(admin::create_team).get(admin::list_teams),
        )
        .route("/admin/teams/{team_id}/suspend", post(admin::suspend_team))
        .route("/admin/workers", get(admin::workers))
        .route("/admin/revalidate/{submission_id}", post(admin::revalidate))
        .route("/admin/metrics", get(admin::metrics))
        .route("/admin/runs/nightly", post(admin::nightly))
        .route("/admin/runs/{run_id}/jobs", get(admin::run_jobs))
        // misc
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.yaml", get(openapi))
        .layer(DefaultBodyLimit::max(max_upload));

    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/ready", get(ready))
        .nest("/api/v1", v1)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn index() -> Json<serde_json::Value> {
    Json(json!({
        "service": "pokerbots-competition-api",
        "version": env!("CARGO_PKG_VERSION"),
        "api": "/api/v1",
        "openapi": "/api/v1/openapi.yaml",
    }))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> Result<Json<serde_json::Value>, errors::ApiError> {
    let db = state.store.ping().is_ok();
    let artifacts = state.artifacts.root().exists();
    let workers = state.store.workers(120).map(|w| w.len()).unwrap_or(0);
    let ready = db && artifacts;
    let body = json!({
        "ready": ready,
        "database": db,
        "artifact_store": artifacts,
        "workers_alive": workers,
        "started_at": state.started_at,
    });
    if ready {
        Ok(Json(body))
    } else {
        Err(errors::ApiError::new(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "dependencies unavailable",
        )
        .with_details(body))
    }
}

async fn openapi() -> ([(http::HeaderName, &'static str); 1], &'static str) {
    (
        [(
            http::header::CONTENT_TYPE,
            "application/yaml; charset=utf-8",
        )],
        OPENAPI_YAML,
    )
}
