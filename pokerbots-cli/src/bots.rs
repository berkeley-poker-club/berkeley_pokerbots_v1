//! Bot specifications for the CLI: in-process strategies or external processes.

use anyhow::{anyhow, Context, Result};
use poker_utils::PlayerId;
use std::sync::Arc;
use table_runner::{strategy_by_name, LocalBot, Player, ProcessBot, SpawnOptions, Strategy};

#[derive(Clone, Debug)]
pub enum BotSpec {
    Builtin(String),
    Command { program: String, args: Vec<String> },
}

impl BotSpec {
    /// Parse a spec:
    /// * `builtin:<fold|call|raise|random>` — in-process bot
    /// * `python:<file.py>` or any path ending in `.py` — run with `python3`
    /// * `exec:<program> [args...]` — arbitrary command
    /// * `self:<strategy>` — this binary in `stdio-bot` mode (a real subprocess)
    /// * anything else — executable path
    pub fn parse(s: &str) -> Result<BotSpec> {
        if let Some(name) = s.strip_prefix("builtin:") {
            strategy_by_name(name, 0)
                .ok_or_else(|| anyhow!("unknown builtin strategy '{name}'"))?;
            return Ok(BotSpec::Builtin(name.to_string()));
        }
        if let Some(path) = s.strip_prefix("python:") {
            return Ok(BotSpec::Command {
                program: std::env::var("PYTHON").unwrap_or_else(|_| "python3".into()),
                args: vec![path.to_string()],
            });
        }
        if let Some(rest) = s.strip_prefix("exec:") {
            let mut parts = rest.split_whitespace();
            let program = parts
                .next()
                .ok_or_else(|| anyhow!("empty exec spec"))?
                .to_string();
            return Ok(BotSpec::Command {
                program,
                args: parts.map(|p| p.to_string()).collect(),
            });
        }
        if let Some(name) = s.strip_prefix("self:") {
            strategy_by_name(name, 0).ok_or_else(|| anyhow!("unknown strategy '{name}'"))?;
            let exe = std::env::current_exe().context("current_exe")?;
            return Ok(BotSpec::Command {
                program: exe.to_string_lossy().to_string(),
                args: vec!["stdio-bot".into(), "--strategy".into(), name.to_string()],
            });
        }
        if s.ends_with(".py") {
            return Ok(BotSpec::Command {
                program: std::env::var("PYTHON").unwrap_or_else(|_| "python3".into()),
                args: vec![s.to_string()],
            });
        }
        Ok(BotSpec::Command {
            program: s.to_string(),
            args: vec![],
        })
    }

    pub fn label(&self) -> String {
        match self {
            BotSpec::Builtin(n) => format!("builtin:{n}"),
            BotSpec::Command { program, args } => {
                let base = std::path::Path::new(program)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| program.clone());
                if args.is_empty() {
                    base
                } else {
                    format!("{} {}", base, args.join(" "))
                }
            }
        }
    }

    /// Instantiate this bot as player `id`.
    pub async fn spawn(&self, id: PlayerId, seed: u64, session: &str) -> Result<Arc<dyn Player>> {
        let name = format!("{}#{}", self.label(), id);
        match self {
            BotSpec::Builtin(n) => {
                let strategy: Box<dyn Strategy> =
                    strategy_by_name(n, seed ^ id as u64).expect("validated");
                Ok(Arc::new(LocalBot::new(id, strategy).named(name)))
            }
            BotSpec::Command { program, args } => {
                let bot =
                    ProcessBot::spawn(program, args, SpawnOptions::new(id, name).session(session))
                        .await
                        .with_context(|| format!("spawning bot {}", self.label()))?;
                Ok(Arc::new(bot))
            }
        }
    }
}

/// Expand `--bot` specs with `--copies` and an optional total `--players` (cycling specs).
pub fn expand(specs: &[BotSpec], copies: usize, players: Option<usize>) -> Vec<BotSpec> {
    let mut out = Vec::new();
    for s in specs {
        for _ in 0..copies.max(1) {
            out.push(s.clone());
        }
    }
    if let Some(n) = players {
        if out.is_empty() {
            return out;
        }
        let base = out.clone();
        let mut i = 0;
        while out.len() < n {
            out.push(base[i % base.len()].clone());
            i += 1;
        }
        out.truncate(n);
    }
    out
}
