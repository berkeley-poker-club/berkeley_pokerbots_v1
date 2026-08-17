//! `pokerbots` — local tournament runner, series runner, smoke tester and reference stdio bots.

mod bots;
mod stdio_bot;

use anyhow::{bail, Context, Result};
use bots::{expand, BotSpec};
use clap::{Args, Parser, Subcommand};
use poker_utils::TournamentConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use table_runner::{Player, ProcessBot, SpawnOptions};
use tournament_core::{
    HandRecord, PlayerFactory, SeriesRunner, TournamentDirector, TournamentOutcome,
};

#[derive(Parser)]
#[command(
    name = "pokerbots",
    version,
    about = "Berkeley Pokerbots tournament tools"
)]
struct Cli {
    /// Log filter (e.g. `info`, `tournament_core=debug`).
    #[arg(long, global = true, default_value = "warn")]
    log: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct BotArgs {
    /// Bot spec (repeatable): builtin:<fold|call|raise|random>, python:<file.py>, exec:<cmd args>,
    /// self:<strategy>, or an executable path.
    #[arg(long = "bot", required = true)]
    bots: Vec<String>,
    /// Number of copies of each --bot.
    #[arg(long, default_value_t = 1)]
    copies: usize,
    /// Total number of players (cycles through the --bot specs).
    #[arg(long)]
    players: Option<usize>,
}

#[derive(Args, Clone)]
struct ConfigArgs {
    /// TOML tournament config file (defaults are used for anything unspecified).
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    seed: Option<u64>,
    #[arg(long)]
    table_size: Option<usize>,
    #[arg(long)]
    starting_stack: Option<i64>,
    #[arg(long)]
    timeout_ms: Option<u64>,
    #[arg(long)]
    hands_per_level: Option<u64>,
    /// Disable the wall-clock level trigger (useful for deterministic local runs).
    #[arg(long)]
    no_time_levels: bool,
}

impl ConfigArgs {
    fn load(&self) -> Result<TournamentConfig> {
        let mut cfg = match &self.config {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            None => TournamentConfig::default(),
        };
        if let Some(s) = self.seed {
            cfg.rng_seed = s;
        }
        if let Some(t) = self.table_size {
            cfg.table_size = t;
            cfg.break_threshold = cfg.break_threshold.min(t - 1);
            cfg.min_table_size = cfg.min_table_size.min(t);
        }
        if let Some(s) = self.starting_stack {
            cfg.starting_stack = s;
        }
        if let Some(t) = self.timeout_ms {
            cfg.action_timeout_ms = t;
        }
        if let Some(h) = self.hands_per_level {
            cfg.level_advance.hands_per_level = Some(h);
        }
        if self.no_time_levels {
            cfg.level_advance.max_level_duration_secs = None;
        }
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid config: {}", e.join("; ")))?;
        Ok(cfg)
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a single tournament between the given bots and print placements.
    Tournament {
        #[command(flatten)]
        bots: BotArgs,
        #[command(flatten)]
        config: ConfigArgs,
        /// Print the full outcome as JSON.
        #[arg(long)]
        json: bool,
        /// Write hand histories (JSON lines) to this file.
        #[arg(long)]
        hand_log: Option<PathBuf>,
    },
    /// Run a series of tournaments and print the geometric-mean leaderboard.
    Series {
        #[command(flatten)]
        bots: BotArgs,
        #[command(flatten)]
        config: ConfigArgs,
        /// Number of tournaments (defaults to the config's series_length).
        #[arg(long)]
        length: Option<usize>,
        /// Tournaments to run concurrently.
        #[arg(long, default_value_t = 1)]
        parallel: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        hand_log: Option<PathBuf>,
    },
    /// Run the protocol smoke test against a bot.
    Smoke {
        /// Bot spec (see `tournament --help`).
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = 5000)]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// Act as a bot on stdin/stdout using a built-in strategy.
    StdioBot {
        #[arg(long, default_value = "random")]
        strategy: String,
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },
    /// Load test: a tournament with many bots (in-process or as subprocesses).
    Bench {
        #[arg(long, default_value_t = 200)]
        players: usize,
        /// Strategy mix: `mixed` (call/raise/random) or a single strategy name.
        #[arg(long, default_value = "mixed")]
        strategy: String,
        /// Spawn each bot as a real subprocess (`pokerbots stdio-bot`).
        #[arg(long)]
        process: bool,
        #[command(flatten)]
        config: ConfigArgs,
    },
    /// Print the default tournament configuration as TOML.
    DefaultConfig,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&cli.log).unwrap_or_default())
        .with_writer(std::io::stderr)
        .init();

    match cli.cmd {
        Cmd::Tournament {
            bots,
            config,
            json,
            hand_log,
        } => {
            let cfg = config.load()?;
            let specs = parse_bots(&bots)?;
            if specs.len() < 2 {
                bail!("need at least two bots (use --copies or --players)");
            }
            let (log_tx, log_task) = hand_log_writer(hand_log).await?;
            let mut players = Vec::new();
            let mut names = HashMap::new();
            for (i, s) in specs.iter().enumerate() {
                let id = i as u32 + 1;
                let p = s.spawn(id, cfg.rng_seed, "local").await?;
                names.insert(id, p.display_name());
                players.push(p);
            }
            let start = Instant::now();
            let td = TournamentDirector::new(cfg.clone(), "local", cfg.rng_seed)
                .with_hand_records(log_tx.is_some());
            let out = td.run(players, None, log_tx).await;
            if let Some(t) = log_task {
                let _ = t.await;
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&with_names(&out, &names))?
                );
            } else {
                print_outcome(&out, &names, start.elapsed());
            }
        }
        Cmd::Series {
            bots,
            config,
            length,
            parallel,
            json,
            hand_log,
        } => {
            let cfg = config.load()?;
            let specs = Arc::new(parse_bots(&bots)?);
            if specs.len() < 2 {
                bail!("need at least two bots (use --copies or --players)");
            }
            let names: HashMap<u32, String> = specs
                .iter()
                .enumerate()
                .map(|(i, s)| (i as u32 + 1, format!("{}#{}", s.label(), i + 1)))
                .collect();
            let (log_tx, log_task) = hand_log_writer(hand_log).await?;
            let seed = cfg.rng_seed;
            let factory: PlayerFactory = Arc::new(move |t: usize| {
                let specs = Arc::clone(&specs);
                Box::pin(async move {
                    let mut players: Vec<Arc<dyn Player>> = Vec::new();
                    for (i, s) in specs.iter().enumerate() {
                        match s
                            .spawn(
                                i as u32 + 1,
                                seed ^ (t as u64 * 7919),
                                &format!("series#{}", t + 1),
                            )
                            .await
                        {
                            Ok(p) => players.push(p),
                            Err(e) => eprintln!("warning: {e:#}"),
                        }
                    }
                    players
                })
            });
            let start = Instant::now();
            let runner = SeriesRunner::new(cfg.clone(), "local")
                .parallelism(parallel)
                .record_hands(log_tx.is_some());
            let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let d2 = Arc::clone(&done);
            let total = length.unwrap_or(cfg.series_length);
            let cb: tournament_core::OnTournamentDone = Box::new(move |o| {
                let n = d2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                eprintln!(
                    "tournament {}/{} done: {} hands, winner {:?}, {} ms",
                    n, total, o.total_hands, o.winner, o.duration_ms
                );
            });
            let out = runner.run(length, factory, None, log_tx, Some(cb)).await;
            if let Some(t) = log_task {
                let _ = t.await;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!(
                    "Series of {} tournaments in {:.1}s",
                    out.tournaments.len(),
                    start.elapsed().as_secs_f64()
                );
                println!(
                    "{:<5} {:<28} {:>10} {:>6}",
                    "rank", "player", "geo_mean", "n"
                );
                for s in &out.scores {
                    println!(
                        "{:<5} {:<28} {:>10.3} {:>6}",
                        s.rank,
                        names.get(&s.player_id).cloned().unwrap_or_default(),
                        s.geo_mean,
                        s.tournaments_counted
                    );
                }
            }
        }
        Cmd::Smoke {
            bot,
            timeout_ms,
            json,
        } => {
            let spec = BotSpec::parse(&bot)?;
            let BotSpec::Command { program, args } = spec else {
                bail!("smoke test needs an external bot (python:, exec:, self: or a path)");
            };
            let bot = ProcessBot::spawn(
                &program,
                &args,
                SpawnOptions::new(1, "smoke").session("smoke-test"),
            )
            .await
            .context("spawn")?;
            let report = table_runner::smoke_test(&bot, Duration::from_millis(timeout_ms)).await;
            bot.shutdown().await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{} — {} (first response {} ms, steady {} ms)",
                    if report.passed { "PASSED" } else { "FAILED" },
                    report.message,
                    report.first_latency_ms,
                    report.latency_ms
                );
                if !report.stderr_tail.trim().is_empty() {
                    println!("--- stderr tail ---\n{}", report.stderr_tail.trim_end());
                }
            }
            if !report.passed {
                std::process::exit(2);
            }
        }
        Cmd::StdioBot { strategy, seed } => {
            stdio_bot::run(&strategy, seed)?;
        }
        Cmd::Bench {
            players,
            strategy,
            process,
            config,
        } => {
            let cfg = config.load()?;
            let mut specs = Vec::new();
            for i in 0..players {
                let name = if strategy == "mixed" {
                    ["call", "raise", "random"][i % 3]
                } else {
                    strategy.as_str()
                };
                let s = if process {
                    BotSpec::parse(&format!("self:{name}"))?
                } else {
                    BotSpec::parse(&format!("builtin:{name}"))?
                };
                specs.push(s);
            }
            let spawn_start = Instant::now();
            let mut bots: Vec<Arc<dyn Player>> = Vec::new();
            for (i, s) in specs.iter().enumerate() {
                bots.push(s.spawn(i as u32 + 1, cfg.rng_seed, "bench").await?);
            }
            eprintln!("spawned {} bots in {:?}", bots.len(), spawn_start.elapsed());
            let start = Instant::now();
            let out = TournamentDirector::new(cfg.clone(), "bench", cfg.rng_seed)
                .run(bots, None, None)
                .await;
            let el = start.elapsed();
            println!(
                "players={} hands={} levels={} time={:.2}s hands/s={:.0} aborted={:?}",
                players,
                out.total_hands,
                out.levels_reached,
                el.as_secs_f64(),
                out.total_hands as f64 / el.as_secs_f64().max(1e-9),
                out.aborted
            );
        }
        Cmd::DefaultConfig => {
            print!("{}", toml::to_string_pretty(&TournamentConfig::default())?);
        }
    }
    Ok(())
}

fn parse_bots(args: &BotArgs) -> Result<Vec<BotSpec>> {
    let specs: Vec<BotSpec> = args
        .bots
        .iter()
        .map(|s| BotSpec::parse(s))
        .collect::<Result<_>>()?;
    Ok(expand(&specs, args.copies, args.players))
}

async fn hand_log_writer(
    path: Option<PathBuf>,
) -> Result<(
    Option<tokio::sync::mpsc::Sender<HandRecord>>,
    Option<tokio::task::JoinHandle<()>>,
)> {
    let Some(path) = path else {
        return Ok((None, None));
    };
    let file =
        std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<HandRecord>(1024);
    let task = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(file);
        while let Some(rec) = rx.blocking_recv() {
            let line = serde_json::json!({
                "tournament_id": rec.tournament_id,
                "table_id": rec.table_id,
                "hand_id": rec.hand_id,
                "level": rec.level,
                "result": rec.result,
                "events": rec.events,
            });
            let _ = serde_json::to_writer(&mut w, &line);
            let _ = w.write_all(b"\n");
        }
        let _ = w.flush();
    });
    Ok((Some(tx), Some(task)))
}

fn with_names(out: &TournamentOutcome, names: &HashMap<u32, String>) -> serde_json::Value {
    let mut v = serde_json::to_value(out).unwrap_or_default();
    v["player_names"] = serde_json::to_value(names).unwrap_or_default();
    v
}

fn print_outcome(out: &TournamentOutcome, names: &HashMap<u32, String>, elapsed: Duration) {
    println!(
        "Tournament {}: {} players, {} hands, {} blind levels, {:.2}s{}",
        out.tournament_id,
        out.num_players,
        out.total_hands,
        out.levels_reached,
        elapsed.as_secs_f64(),
        out.aborted
            .as_ref()
            .map(|a| format!(" (ABORTED: {a})"))
            .unwrap_or_default()
    );
    let mut rows: Vec<(usize, u32)> = out
        .placements
        .iter()
        .map(|(p, place)| (*place, *p))
        .collect();
    rows.sort();
    println!("{:<6} {:<30} {:>6}", "place", "player", "hands");
    for (place, pid) in rows {
        println!(
            "{:<6} {:<30} {:>6}",
            place,
            names.get(&pid).cloned().unwrap_or_else(|| pid.to_string()),
            out.hands_played.get(&pid).copied().unwrap_or(0)
        );
    }
}
