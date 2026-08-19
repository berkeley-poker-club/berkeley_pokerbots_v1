//! Platform configuration (`pokerbots.toml` + environment overrides).

use crate::models::PlatformSettings;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlatformConfig {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub worker: WorkerConfig,
    pub sandbox: SandboxConfig,
    pub scheduler: SchedulerConfig,
    pub autoscaler: AutoscalerConfig,
    /// Initial runtime settings, applied when the database has none (later editable via
    /// `PATCH /admin/config`).
    pub defaults: PlatformSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    /// Admin API key (`X-Admin-Key`). If unset, only teams flagged `is_admin` can use admin routes.
    pub admin_key: Option<String>,
    pub public_base_url: Option<String>,
    /// Number of worker loops to run inside the API process (0 = rely on `tournament-worker`).
    pub embedded_workers: usize,
    /// How long `POST /submissions` waits for the smoke test before returning `202 pending`.
    pub smoke_wait_ms: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: "0.0.0.0:8080".into(),
            admin_key: None,
            public_base_url: None,
            embedded_workers: 1,
            smoke_wait_ms: 30_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StorageConfig {
    pub database: PathBuf,
    pub artifacts_dir: PathBuf,
    pub logs_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            database: PathBuf::from("data/pokerbots.db"),
            artifacts_dir: PathBuf::from("data/artifacts"),
            logs_dir: PathBuf::from("data/logs"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkerConfig {
    /// Jobs (tournaments) a worker process runs concurrently.
    pub concurrency: usize,
    /// Maximum bots this worker will host at once. A tournament job with more participants than
    /// the remaining capacity is left for another worker. Must be at least the largest expected
    /// field (e.g. 500).
    pub max_bots_in_flight: u32,
    pub poll_interval_ms: u64,
    pub heartbeat_secs: u64,
    /// Running jobs without a heartbeat for this long are re-queued.
    pub stale_job_secs: u64,
    /// Job types this worker accepts (default: all).
    pub job_types: Vec<String>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            concurrency: 2,
            max_bots_in_flight: 600,
            poll_interval_ms: 1000,
            heartbeat_secs: 20,
            stale_job_secs: 300,
            job_types: vec![],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SandboxConfig {
    /// `process` (rlimits only — development) or `docker` (production).
    pub kind: String,
    pub memory_mb: u64,
    /// CPU-time limit per bot process (seconds); `process` sandbox only.
    pub cpu_seconds: Option<u64>,
    pub max_pids: u64,
    pub max_open_files: u64,
    pub python: String,
    pub node: String,
    pub java: String,
    pub docker: DockerConfig,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            kind: "process".into(),
            memory_mb: 512,
            cpu_seconds: Some(1800),
            max_pids: 64,
            max_open_files: 256,
            python: "python3".into(),
            node: "node".into(),
            java: "java".into(),
            docker: DockerConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DockerConfig {
    pub binary: String,
    pub cpus: f64,
    /// Image per runtime (`python3`, `node`, `java`, `native`).
    pub images: HashMap<String, String>,
    pub extra_args: Vec<String>,
}

impl Default for DockerConfig {
    fn default() -> Self {
        let mut images = HashMap::new();
        images.insert("python3".to_string(), "python:3.12-slim".to_string());
        images.insert("node".to_string(), "node:22-slim".to_string());
        images.insert("java".to_string(), "eclipse-temurin:21-jre".to_string());
        images.insert("native".to_string(), "debian:bookworm-slim".to_string());
        DockerConfig {
            binary: "docker".into(),
            cpus: 1.0,
            images,
            extra_args: vec![],
        }
    }
}

/// How the autoscaler acts on its plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AutoscalerConfig {
    /// `off` (plan only, visible via /admin/autoscale), `processes` (spawn/stop local
    /// `tournament-worker` processes), or `command` (run `scale_command` with `{n}`).
    pub backend: String,
    pub poll_secs: u64,
    /// Path to the tournament-worker binary for the `processes` backend. Defaults to a sibling of
    /// the current executable.
    pub worker_binary: Option<String>,
    /// Shell command template for the `command` backend; `{n}` is replaced by the desired
    /// replica count (e.g. `docker compose -f deploy/docker-compose.yml up -d --scale worker={n} --no-recreate`).
    pub scale_command: Option<String>,
}

impl Default for AutoscalerConfig {
    fn default() -> Self {
        AutoscalerConfig {
            backend: "off".into(),
            poll_secs: 20,
            worker_binary: None,
            scale_command: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SchedulerConfig {
    pub enabled: bool,
    pub poll_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            enabled: true,
            poll_secs: 30,
        }
    }
}

impl PlatformConfig {
    /// Load from a TOML file (if it exists) and apply `POKERBOTS_*` environment overrides.
    pub fn load(path: Option<&Path>) -> Result<PlatformConfig> {
        let mut cfg = match path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            _ => PlatformConfig::default(),
        };
        if let Ok(v) = std::env::var("POKERBOTS_BIND") {
            cfg.server.bind = v;
        }
        if let Ok(v) = std::env::var("POKERBOTS_ADMIN_KEY") {
            if !v.is_empty() {
                cfg.server.admin_key = Some(v);
            }
        }
        if let Ok(v) = std::env::var("POKERBOTS_DATABASE") {
            cfg.storage.database = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("POKERBOTS_ARTIFACTS_DIR") {
            cfg.storage.artifacts_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("POKERBOTS_LOGS_DIR") {
            cfg.storage.logs_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("POKERBOTS_SANDBOX") {
            cfg.sandbox.kind = v;
        }
        if let Ok(v) = std::env::var("POKERBOTS_EMBEDDED_WORKERS") {
            if let Ok(n) = v.parse() {
                cfg.server.embedded_workers = n;
            }
        }
        if let Ok(v) = std::env::var("POKERBOTS_WORKER_CONCURRENCY") {
            if let Ok(n) = v.parse() {
                cfg.worker.concurrency = n;
            }
        }
        if let Ok(v) = std::env::var("POKERBOTS_PUBLIC_BASE_URL") {
            cfg.server.public_base_url = Some(v);
        }
        Ok(cfg)
    }

    pub fn example_toml() -> String {
        toml::to_string_pretty(&PlatformConfig::default()).unwrap_or_default()
    }
}
