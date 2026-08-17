//! Bot sandboxes: how a submission is turned into a running `ProcessBot`.
//!
//! * [`ProcessSandbox`] — plain subprocess with resource limits (`setrlimit`) and its own process
//!   group. Adequate for development and trusted environments; **not** an isolation boundary.
//! * [`DockerSandbox`] — one container per bot: no network, memory/CPU/pids limits, read-only
//!   root filesystem, dropped capabilities, unprivileged user, artifact mounted read-only.
//!
//! Both speak the same JSON-lines protocol over the process's stdin/stdout.

use crate::config::SandboxConfig;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use poker_utils::PlayerId;
use std::path::PathBuf;
use std::sync::Arc;
use table_runner::{ProcessBot, SpawnOptions};
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct BotLaunch {
    pub player_id: PlayerId,
    pub display_name: String,
    /// Identifier for this tournament/run; used for container labels and the `hello` message.
    pub session: String,
    pub artifact_dir: PathBuf,
    pub entrypoint: String,
    /// `native` | `python3` | `node` | `java`
    pub runtime: String,
    pub args: Vec<String>,
    pub stderr_log: Option<PathBuf>,
}

/// Result of a one-shot build/command run inside the sandbox.
#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

impl CommandOutput {
    pub fn ok(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn spawn(&self, launch: &BotLaunch) -> Result<ProcessBot>;
    /// Run a one-shot command (e.g. a build) inside the same isolation as a bot, in the artifact
    /// directory, with the given timeout. `runtime` selects the environment (docker image);
    /// `writable` allows the command to write build outputs into the artifact directory.
    async fn run_command(
        &self,
        artifact_dir: &std::path::Path,
        argv: &[String],
        runtime: &str,
        timeout: std::time::Duration,
        writable: bool,
    ) -> Result<CommandOutput>;
    /// Best-effort cleanup of anything left over from `session` (e.g. stray containers).
    async fn cleanup_session(&self, _session: &str) {}
    fn describe(&self) -> String;
}

async fn capture_command(mut cmd: Command, timeout: std::time::Duration) -> Result<CommandOutput> {
    use tokio::io::AsyncReadExt;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let read = async {
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut stdout).await;
        }
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut stderr).await;
        }
    };
    let status = tokio::select! {
        s = child.wait() => { read.await; Some(s?) }
        _ = tokio::time::sleep(timeout) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };
    let tail =
        |b: &[u8]| String::from_utf8_lossy(&b[b.len().saturating_sub(8 * 1024)..]).into_owned();
    Ok(CommandOutput {
        exit_code: status.as_ref().and_then(|s| s.code()),
        timed_out: status.is_none(),
        stdout_tail: tail(&stdout),
        stderr_tail: tail(&stderr),
    })
}

pub fn build_sandbox(cfg: &SandboxConfig) -> Result<Arc<dyn Sandbox>> {
    match cfg.kind.as_str() {
        "process" => Ok(Arc::new(ProcessSandbox { cfg: cfg.clone() })),
        "docker" => Ok(Arc::new(DockerSandbox { cfg: cfg.clone() })),
        other => Err(anyhow!(
            "unknown sandbox kind '{}' (expected process or docker)",
            other
        )),
    }
}

/// Command line (program, args) for a runtime, relative to the artifact directory.
fn runtime_command(
    cfg: &SandboxConfig,
    launch: &BotLaunch,
    in_container: bool,
) -> (String, Vec<String>) {
    let ep = launch.entrypoint.trim_start_matches("./").to_string();
    let ep_path = if in_container {
        format!("/bot/{ep}")
    } else {
        launch.artifact_dir.join(&ep).to_string_lossy().to_string()
    };
    let mut args = Vec::new();
    let program = match launch.runtime.as_str() {
        "python3" => {
            args.push(ep_path);
            if in_container {
                "python3".to_string()
            } else {
                cfg.python.clone()
            }
        }
        "node" => {
            args.push(ep_path);
            if in_container {
                "node".to_string()
            } else {
                cfg.node.clone()
            }
        }
        "java" => {
            args.push("-jar".into());
            args.push(ep_path);
            if in_container {
                "java".to_string()
            } else {
                cfg.java.clone()
            }
        }
        _ => ep_path,
    };
    args.extend(launch.args.iter().cloned());
    (program, args)
}

fn spawn_options(launch: &BotLaunch) -> SpawnOptions {
    let mut opts = SpawnOptions::new(launch.player_id, launch.display_name.clone())
        .session(launch.session.clone());
    if let Some(p) = &launch.stderr_log {
        opts = opts.stderr_log(p.clone());
    }
    opts
}

// ---------------------------------------------------------------- process sandbox

pub struct ProcessSandbox {
    cfg: SandboxConfig,
}

#[async_trait]
impl Sandbox for ProcessSandbox {
    async fn spawn(&self, launch: &BotLaunch) -> Result<ProcessBot> {
        let (program, args) = runtime_command(&self.cfg, launch, false);
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&launch.artifact_dir)
            .env_clear()
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            )
            .env("HOME", std::env::temp_dir())
            .env("LANG", "C.UTF-8")
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("POKERBOTS_PLAYER_ID", launch.player_id.to_string())
            .env("POKERBOTS_SESSION", &launch.session);
        #[cfg(unix)]
        {
            let cpu = self.cfg.cpu_seconds;
            let nofile = self.cfg.max_open_files;
            let mem = self.cfg.memory_mb;
            // SAFETY: only async-signal-safe libc calls are made in the child before exec.
            unsafe {
                cmd.pre_exec(move || {
                    libc::setsid();
                    let set = |res: libc::c_int, v: u64| {
                        let lim = libc::rlimit {
                            rlim_cur: v as libc::rlim_t,
                            rlim_max: v as libc::rlim_t,
                        };
                        libc::setrlimit(res, &lim);
                    };
                    set(libc::RLIMIT_CORE, 0);
                    if nofile > 0 {
                        set(libc::RLIMIT_NOFILE, nofile);
                    }
                    if let Some(c) = cpu {
                        set(libc::RLIMIT_CPU, c);
                    }
                    // Address-space limits break interpreters/JVMs on macOS and are unreliable on
                    // Linux; only apply on Linux and only when generous.
                    #[cfg(target_os = "linux")]
                    if mem >= 256 {
                        set(libc::RLIMIT_AS, mem * 1024 * 1024 * 4);
                    }
                    #[cfg(not(target_os = "linux"))]
                    let _ = mem;
                    Ok(())
                });
            }
        }
        let mut opts = spawn_options(launch);
        opts.kill_process_group = true;
        ProcessBot::spawn_command(cmd, opts)
            .await
            .with_context(|| format!("spawning {} {:?}", program, args))
    }

    async fn run_command(
        &self,
        artifact_dir: &std::path::Path,
        argv: &[String],
        _runtime: &str,
        timeout: std::time::Duration,
        _writable: bool,
    ) -> Result<CommandOutput> {
        let (program, args) = split_argv(argv)?;
        // Map generic interpreter names to the configured local ones.
        let program = match program.as_str() {
            "python3" => self.cfg.python.clone(),
            "node" => self.cfg.node.clone(),
            "java" => self.cfg.java.clone(),
            other => other.to_string(),
        };
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(artifact_dir)
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            )
            .env("HOME", std::env::temp_dir());
        capture_command(cmd, timeout).await
    }

    fn describe(&self) -> String {
        format!(
            "process sandbox (rlimits: cpu={:?}s nofile={} mem={}MB)",
            self.cfg.cpu_seconds, self.cfg.max_open_files, self.cfg.memory_mb
        )
    }
}

fn split_argv(argv: &[String]) -> Result<(String, Vec<String>)> {
    let program = argv
        .first()
        .ok_or_else(|| anyhow!("empty command"))?
        .clone();
    Ok((program, argv[1..].to_vec()))
}

// ---------------------------------------------------------------- docker sandbox

pub struct DockerSandbox {
    cfg: SandboxConfig,
}

impl DockerSandbox {
    fn image_for(&self, runtime: &str) -> String {
        self.cfg
            .docker
            .images
            .get(runtime)
            .cloned()
            .or_else(|| self.cfg.docker.images.get("native").cloned())
            .unwrap_or_else(|| "debian:bookworm-slim".to_string())
    }

    fn container_name(session: &str, player_id: PlayerId) -> String {
        let s: String = session
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        format!("pb-{}-{}", s, player_id)
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn spawn(&self, launch: &BotLaunch) -> Result<ProcessBot> {
        let (program, args) = runtime_command(&self.cfg, launch, true);
        let name = Self::container_name(&launch.session, launch.player_id);
        let mem = format!("{}m", self.cfg.memory_mb);
        let mut cmd = Command::new(&self.cfg.docker.binary);
        cmd.arg("run")
            .arg("--rm")
            .arg("-i")
            .arg("--name")
            .arg(&name)
            .arg("--label")
            .arg(format!("pokerbots.session={}", launch.session))
            .arg("--network")
            .arg("none")
            .arg("--memory")
            .arg(&mem)
            .arg("--memory-swap")
            .arg(&mem)
            .arg("--cpus")
            .arg(format!("{}", self.cfg.docker.cpus))
            .arg("--pids-limit")
            .arg(self.cfg.max_pids.to_string())
            .arg("--read-only")
            .arg("--tmpfs")
            .arg("/tmp:rw,size=64m,mode=1777")
            .arg("--cap-drop")
            .arg("ALL")
            .arg("--security-opt")
            .arg("no-new-privileges")
            .arg("--user")
            .arg("65534:65534")
            .arg("-v")
            .arg(format!("{}:/bot:ro", launch.artifact_dir.display()))
            .arg("-w")
            .arg("/bot")
            .arg("-e")
            .arg("PYTHONUNBUFFERED=1")
            .arg("-e")
            .arg("PYTHONDONTWRITEBYTECODE=1")
            .arg("-e")
            .arg("HOME=/tmp")
            .arg("-e")
            .arg(format!("POKERBOTS_PLAYER_ID={}", launch.player_id))
            .arg("-e")
            .arg(format!("POKERBOTS_SESSION={}", launch.session));
        for extra in &self.cfg.docker.extra_args {
            cmd.arg(extra);
        }
        cmd.arg(self.image_for(&launch.runtime))
            .arg(&program)
            .args(&args);
        ProcessBot::spawn_command(cmd, spawn_options(launch))
            .await
            .with_context(|| format!("docker run {} for {}", name, launch.display_name))
    }

    async fn run_command(
        &self,
        artifact_dir: &std::path::Path,
        argv: &[String],
        runtime: &str,
        timeout: std::time::Duration,
        writable: bool,
    ) -> Result<CommandOutput> {
        let (program, args) = split_argv(argv)?;
        let mem = format!("{}m", self.cfg.memory_mb.max(512));
        let mount = if writable {
            format!("{}:/bot:rw", artifact_dir.display())
        } else {
            format!("{}:/bot:ro", artifact_dir.display())
        };
        let mut cmd = Command::new(&self.cfg.docker.binary);
        cmd.arg("run")
            .arg("--rm")
            .arg("--network")
            .arg("none")
            .arg("--memory")
            .arg(&mem)
            .arg("--cpus")
            .arg(format!("{}", self.cfg.docker.cpus.max(1.0)))
            .arg("--pids-limit")
            .arg((self.cfg.max_pids * 4).to_string())
            .arg("--cap-drop")
            .arg("ALL")
            .arg("--security-opt")
            .arg("no-new-privileges")
            .arg("-v")
            .arg(&mount)
            .arg("-w")
            .arg("/bot")
            .arg("-e")
            .arg("HOME=/tmp")
            .arg(self.image_for(runtime))
            .arg(&program)
            .args(&args);
        capture_command(cmd, timeout).await
    }

    async fn cleanup_session(&self, session: &str) {
        // Kill any container still carrying this session label (clients were killed, etc.).
        let out = Command::new(&self.cfg.docker.binary)
            .args([
                "ps",
                "-q",
                "--filter",
                &format!("label=pokerbots.session={session}"),
            ])
            .output()
            .await;
        if let Ok(out) = out {
            let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if !ids.is_empty() {
                let _ = Command::new(&self.cfg.docker.binary)
                    .arg("kill")
                    .args(&ids)
                    .output()
                    .await;
            }
        }
    }

    fn describe(&self) -> String {
        format!(
            "docker sandbox (mem={}MB cpus={} pids={} network=none read-only)",
            self.cfg.memory_mb, self.cfg.docker.cpus, self.cfg.max_pids
        )
    }
}
