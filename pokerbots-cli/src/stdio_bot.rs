//! Run a built-in strategy as a protocol-speaking subprocess (stdin/stdout JSON lines).

use anyhow::{anyhow, Result};
use std::io::{BufRead, Write};
use table_runner::{strategy_by_name, BotMessage, EngineMessage, Strategy};

pub fn run(strategy_name: &str, seed: u64) -> Result<()> {
    let mut strategy: Box<dyn Strategy> = strategy_by_name(strategy_name, seed)
        .ok_or_else(|| anyhow!("unknown strategy {strategy_name}"))?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: EngineMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("stdio-bot: bad message: {e}");
                continue;
            }
        };
        match msg {
            EngineMessage::Hello { .. } => {}
            EngineMessage::NotifyEvent { event } => strategy.on_event(&event),
            EngineMessage::RequestAction {
                request_id,
                context,
                legal,
                ..
            } => {
                let action = strategy.act(&context, &legal);
                let reply = BotMessage::Action { request_id, action };
                serde_json::to_writer(&mut out, &reply)?;
                out.write_all(b"\n")?;
                out.flush()?;
            }
            EngineMessage::Goodbye => break,
        }
    }
    Ok(())
}
