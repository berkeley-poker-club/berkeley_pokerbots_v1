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
    /// Optional build command run once inside the sandbox before validation (e.g.
    /// `["cargo","build","--release","--offline"]`, `["make"]`). Interpreted languages get a
    /// default syntax check when this is empty.
    #[serde(default)]
    pub build: Vec<String>,
    #[serde(default)]
    pub build_timeout_secs: Option<u64>,
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
        if self.build.len() > 64 || self.build.iter().any(|a| a.len() > 512) {
            return Err("build command is too long".into());
        }
        if let Some(t) = self.build_timeout_secs {
            if t == 0 || t > 3600 {
                return Err("build_timeout_secs must be in 1..=3600".into());
            }
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

/// Lifecycle of a submission through the validation pipeline:
/// `pending → building → smoke_testing → trial_running → validated | rejected`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    Pending,
    Building,
    SmokeTesting,
    TrialRunning,
    Validated,
    Rejected,
    Deleted,
}

impl SubmissionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubmissionStatus::Pending => "pending",
            SubmissionStatus::Building => "building",
            SubmissionStatus::SmokeTesting => "smoke_testing",
            SubmissionStatus::TrialRunning => "trial_running",
            SubmissionStatus::Validated => "validated",
            SubmissionStatus::Rejected => "rejected",
            SubmissionStatus::Deleted => "deleted",
        }
    }
    pub fn parse(s: &str) -> SubmissionStatus {
        match s {
            "building" => SubmissionStatus::Building,
            "smoke_testing" => SubmissionStatus::SmokeTesting,
            "trial_running" => SubmissionStatus::TrialRunning,
            "validated" => SubmissionStatus::Validated,
            "rejected" => SubmissionStatus::Rejected,
            "deleted" => SubmissionStatus::Deleted,
            _ => SubmissionStatus::Pending,
        }
    }
    /// Still moving through the pipeline.
    pub fn in_progress(&self) -> bool {
        matches!(
            self,
            SubmissionStatus::Pending
                | SubmissionStatus::Building
                | SubmissionStatus::SmokeTesting
                | SubmissionStatus::TrialRunning
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStage {
    /// Build / compile / syntax check.
    Build,
    /// Protocol smoke test.
    Smoke,
    /// Short trial run against reference bots (crashes, timeouts, latency).
    Trial,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pending,
    Running,
    Passed,
    /// Passed with warnings (visible to the team and admins).
    Warned,
    Failed,
    Skipped,
}

/// Result of one pipeline stage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CheckReport {
    pub stage: CheckStage,
    pub status: CheckStatus,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub details: serde_json::Value,
}

impl CheckReport {
    pub fn pending(stage: CheckStage) -> Self {
        CheckReport {
            stage,
            status: CheckStatus::Pending,
            started_at: None,
            finished_at: None,
            summary: String::new(),
            details: serde_json::Value::Null,
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
    /// Per-stage validation reports (scan, build, smoke, trial).
    pub checks: Vec<CheckReport>,
    /// Activate automatically once validated.
    pub auto_activate: bool,
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
    /// Full validation pipeline for a submission (scan → build → smoke → trial).
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
    /// Capacity units the job needs while running (bots for a tournament; 1 otherwise).
    pub weight: u32,
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
    /// Validation pipeline knobs.
    pub validation: ValidationSettings,
    /// Capacity planning / autoscaling knobs.
    pub autoscale: AutoscaleSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ValidationSettings {
    /// Run the build / compile / syntax-check stage.
    pub build_enabled: bool,
    pub build_timeout_secs: u64,
    pub trial_enabled: bool,
    /// Number of 3-handed hands played against reference bots in the trial run.
    pub trial_hands: u32,
    /// Maximum fraction of the bot's decisions that may be substituted (timeouts/illegal) to pass.
    pub trial_max_substitution_rate: f64,
    /// Whether an upload with `activate=true` may auto-activate once validated.
    pub allow_auto_activate: bool,
}

impl Default for ValidationSettings {
    fn default() -> Self {
        ValidationSettings {
            build_enabled: true,
            build_timeout_secs: 300,
            trial_enabled: true,
            trial_hands: 30,
            trial_max_substitution_rate: 0.2,
            allow_auto_activate: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AutoscaleSettings {
    pub enabled: bool,
    pub min_workers: u32,
    pub max_workers: u32,
    /// Assumed job slots per worker replica (should match `worker.concurrency`).
    pub worker_concurrency: u32,
    /// Assumed bot capacity per worker replica (should match `worker.max_bots_in_flight`).
    pub max_bots_per_worker: u32,
    /// A nightly series must finish within this many hours of its start.
    pub nightly_deadline_hours: f64,
    /// An on-demand run should finish within this many minutes.
    pub ondemand_deadline_minutes: f64,
    /// Fallback estimate of tournament duration when no history exists: seconds per participant.
    pub est_secs_per_participant: f64,
    /// Minimum seconds between scale-down steps.
    pub scale_down_cooldown_secs: u64,
    /// Never plan below this many concurrent job slots while work is queued.
    pub min_slots_when_busy: u32,
}

impl Default for AutoscaleSettings {
    fn default() -> Self {
        AutoscaleSettings {
            enabled: true,
            min_workers: 1,
            max_workers: 8,
            worker_concurrency: 2,
            max_bots_per_worker: 1200,
            nightly_deadline_hours: 6.0,
            ondemand_deadline_minutes: 30.0,
            est_secs_per_participant: 1.5,
            scale_down_cooldown_secs: 600,
            min_slots_when_busy: 1,
        }
    }
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
            validation: ValidationSettings::default(),
            autoscale: AutoscaleSettings::default(),
        }
    }
}
