//! Data model shared by the store, the API and the workers.

use poker_utils::TournamentConfig;
use serde::{Deserialize, Serialize};
use table_runner::SmokeReport;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub api_key_hash: String,
    pub api_key_prefix: String,
    pub is_admin: bool,
    pub suspended: bool,
    pub created_at: String,
    pub submission_seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMember {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub student_id: Option<String>,
}

/// Bot manifest uploaded with an artifact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    #[serde(default = "default_language")]
    pub language: String,
    /// Path of the entrypoint relative to the artifact root (e.g. `bot.py`, `./bot`).
    pub entrypoint: String,
    /// `native` | `python3` | `node` | `java` | `auto` (by extension).
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Extra command-line arguments passed to the entrypoint.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default = "default_protocol")]
    pub protocol_version: String,
}

fn default_language() -> String {
    "other".into()
}
fn default_runtime() -> String {
    "auto".into()
}
fn default_protocol() -> String {
    "1".into()
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.entrypoint.trim().is_empty() {
            return Err("entrypoint is required".into());
        }
        let ep = self.entrypoint.trim_start_matches("./");
        if ep.starts_with('/') || ep.split('/').any(|c| c == "..") {
            return Err("entrypoint must be a relative path inside the artifact".into());
        }
        if !matches!(
            self.runtime.as_str(),
            "native" | "python3" | "node" | "java" | "auto"
        ) {
            return Err(format!(
                "unsupported runtime '{}' (expected native, python3, node, java or auto)",
                self.runtime
            ));
        }
        if self.protocol_version != "1" {
            return Err(format!(
                "unsupported protocol_version '{}' (this server speaks version 1)",
                self.protocol_version
            ));
        }
        Ok(())
    }

    /// Resolve `auto` runtime by extension.
    pub fn effective_runtime(&self) -> &str {
        if self.runtime != "auto" {
            return &self.runtime;
        }
        let ep = self.entrypoint.to_ascii_lowercase();
        if ep.ends_with(".py") {
            "python3"
        } else if ep.ends_with(".js") || ep.ends_with(".mjs") {
            "node"
        } else if ep.ends_with(".jar") {
            "java"
        } else {
            "native"
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    Pending,
    Validated,
    Rejected,
    Deleted,
}

impl SubmissionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubmissionStatus::Pending => "pending",
            SubmissionStatus::Validated => "validated",
            SubmissionStatus::Rejected => "rejected",
            SubmissionStatus::Deleted => "deleted",
        }
    }
    pub fn parse(s: &str) -> SubmissionStatus {
        match s {
            "validated" => SubmissionStatus::Validated,
            "rejected" => SubmissionStatus::Rejected,
            "deleted" => SubmissionStatus::Deleted,
            _ => SubmissionStatus::Pending,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Submission {
    pub id: String,
    pub team_id: String,
    pub seq: u64,
    pub artifact_path: String,
    pub artifact_sha256: String,
    pub artifact_size: u64,
    pub manifest: Manifest,
    pub status: SubmissionStatus,
    pub protocol_version: String,
    pub smoke_test: Option<SmokeReport>,
    pub created_at: String,
    pub validated_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Nightly,
    Ondemand,
}

impl RunKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunKind::Nightly => "nightly",
            RunKind::Ondemand => "ondemand",
        }
    }
    pub fn parse(s: &str) -> RunKind {
        if s == "nightly" {
            RunKind::Nightly
        } else {
            RunKind::Ondemand
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Finalizing => "finalizing",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> RunStatus {
        match s {
            "running" => RunStatus::Running,
            "finalizing" => RunStatus::Finalizing,
            "completed" => RunStatus::Completed,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            _ => RunStatus::Queued,
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

/// One participant frozen into a run snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Participant {
    pub team_id: String,
    pub team_name: String,
    pub submission_id: String,
    /// Numeric id used inside the engine (1-based, stable for the run).
    pub player_id: u32,
}

/// Effective configuration of a run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunConfig {
    pub series_length: usize,
    pub tournament: TournamentConfig,
    #[serde(default)]
    pub record_hands: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub kind: RunKind,
    pub status: RunStatus,
    pub config: RunConfig,
    pub participants: Vec<Participant>,
    pub created_by: String,
    pub created_at: String,
    pub snapshot_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub nightly_date: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TournamentStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TournamentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TournamentStatus::Queued => "queued",
            TournamentStatus::Running => "running",
            TournamentStatus::Completed => "completed",
            TournamentStatus::Failed => "failed",
            TournamentStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> TournamentStatus {
        match s {
            "running" => TournamentStatus::Running,
            "completed" => TournamentStatus::Completed,
            "failed" => TournamentStatus::Failed,
            "cancelled" => TournamentStatus::Cancelled,
            _ => TournamentStatus::Queued,
        }
    }
    pub fn is_terminal(&self) -> bool {
        !matches!(self, TournamentStatus::Queued | TournamentStatus::Running)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TournamentRow {
    pub run_id: String,
    pub index: usize,
    pub seed: u64,
    pub status: TournamentStatus,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub worker_id: Option<String>,
    pub total_hands: Option<u64>,
    pub winner_team_id: Option<String>,
    pub error: Option<String>,
    /// Full engine outcome (placements keyed by engine player id).
    pub outcome: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlacementRow {
    pub run_id: String,
    pub tournament_index: usize,
    pub team_id: String,
    pub place: usize,
    pub hands_played: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScoreRow {
    pub run_id: String,
    pub team_id: String,
    pub geo_mean: f64,
    pub rank: usize,
    pub tournaments_counted: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> JobStatus {
        match s {
            "running" => JobStatus::Running,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            _ => JobStatus::Queued,
        }
    }
}

pub mod job_types {
    pub const SMOKE_VALIDATE: &str = "smoke_validate_submission";
    pub const RUN_SERIES: &str = "run_series";
    pub const RUN_TOURNAMENT: &str = "run_tournament";
    pub const FINALIZE_SERIES: &str = "finalize_series";
    pub const ALL: &[&str] = &[SMOKE_VALIDATE, RUN_SERIES, RUN_TOURNAMENT, FINALIZE_SERIES];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub job_type: String,
    pub payload: serde_json::Value,
    pub status: JobStatus,
    pub priority: i64,
    pub run_id: Option<String>,
    pub locked_by: Option<String>,
    pub locked_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRow {
    pub id: String,
    pub hostname: String,
    pub started_at: String,
    pub heartbeat_at: String,
    pub running_jobs: u32,
    pub capacity: u32,
    pub info: serde_json::Value,
}

/// Runtime-adjustable platform settings (admin API). Stored in the `settings` table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlatformSettings {
    pub tournament: TournamentConfig,
    pub nightly_enabled: bool,
    /// `HH:MM` in UTC.
    pub nightly_time_utc: String,
    pub nightly_series_length: usize,
    pub ondemand_runs_per_team_per_day: u32,
    pub ondemand_max_series_length: usize,
    /// Wall-clock deadline for the smoke test (interpreter start-up included).
    pub smoke_timeout_ms: u64,
    pub max_upload_bytes: u64,
    pub registration_open: bool,
    pub record_hands: bool,
}

impl Default for PlatformSettings {
    fn default() -> Self {
        PlatformSettings {
            tournament: TournamentConfig::default(),
            nightly_enabled: true,
            nightly_time_utc: "07:00".into(),
            nightly_series_length: 100,
            ondemand_runs_per_team_per_day: 2,
            ondemand_max_series_length: 10,
            smoke_timeout_ms: 10_000,
            max_upload_bytes: 50 * 1024 * 1024,
            registration_open: true,
            record_hands: false,
        }
    }
}
