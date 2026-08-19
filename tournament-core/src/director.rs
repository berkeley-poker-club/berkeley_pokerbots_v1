//! The tournament director (TD): runs one multi-table tournament to completion.
//!
//! Responsibilities (SPEC.md §"Single Tournament Orchestration"):
//! * build ⌈T/N⌉ tables with sizes differing by at most one and seat players randomly;
//! * consume table events: count hands played, record eliminations, track stacks;
//! * break tables that fall to `break_threshold` players (one at a time, only when the other
//!   tables have room), redistributing players to the emptiest tables for the next hand;
//! * advance blind levels (hands / time / average-stack triggers), synchronised across tables by
//!   pausing every table at a hand boundary before applying the new level;
//! * finish when one player remains and compute SPEC placements.

use crate::scoring::finalize_placements;
use poker_utils::{HandResult, PlayerId, PublicEvent, TournamentConfig};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use table_runner::{
    mix, spawn_table, Player, TableCommand, TableConfig, TableEvent, TableHandle, TableId,
    EVENT_QUEUE,
};
use tokio::sync::mpsc;

/// One completed hand, for hand-history sinks.
#[derive(Clone, Debug)]
pub struct HandRecord {
    pub tournament_id: String,
    pub table_id: TableId,
    pub hand_id: u64,
    pub level: u32,
    pub result: HandResult,
    pub events: Vec<PublicEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Elimination {
    pub player_id: PlayerId,
    pub hands_played: u64,
    /// 0-based order in which the player left the tournament.
    pub order: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TournamentOutcome {
    pub tournament_id: String,
    pub seed: u64,
    pub num_players: usize,
    /// 1 = winner. Every participant has an entry unless the tournament was cancelled.
    pub placements: HashMap<PlayerId, usize>,
    pub hands_played: HashMap<PlayerId, u64>,
    pub eliminations: Vec<Elimination>,
    pub winner: Option<PlayerId>,
    pub total_hands: u64,
    pub levels_reached: u32,
    pub duration_ms: u64,
    /// Present when the tournament was cut short (`"cancelled"`, `"max_hands"`, ...).
    pub aborted: Option<String>,
    /// Final chip counts (busted players hold 0). Always sums to `num_players * starting_stack`.
    #[serde(default)]
    pub final_stacks: HashMap<PlayerId, i64>,
}

struct TableInfo {
    handle: TableHandle,
    /// Players assigned to this table (seated or waiting to be seated).
    assigned: HashSet<PlayerId>,
    hands_in_level: u64,
    hands_total: u64,
    errors: u32,
    closing: bool,
}

pub struct TournamentDirector {
    cfg: TournamentConfig,
    tournament_id: String,
    seed: u64,
    record_hands: bool,
}

/// Cooperative cancellation flag shared with the caller.
pub type CancelFlag = Arc<AtomicBool>;

impl TournamentDirector {
    pub fn new(cfg: TournamentConfig, tournament_id: impl Into<String>, seed: u64) -> Self {
        TournamentDirector {
            cfg,
            tournament_id: tournament_id.into(),
            seed,
            record_hands: false,
        }
    }

    /// Record full hand histories and send them to the `hand_log` channel passed to `run`.
    pub fn with_hand_records(mut self, on: bool) -> Self {
        self.record_hands = on;
        self
    }

    /// Run the tournament to completion. `players` must have distinct player ids.
    pub async fn run(
        &self,
        players: Vec<Arc<dyn Player>>,
        cancel: Option<CancelFlag>,
        hand_log: Option<mpsc::Sender<HandRecord>>,
    ) -> TournamentOutcome {
        let started = Instant::now();
        let cfg = &self.cfg;
        let n = players.len();
        let mut outcome = TournamentOutcome {
            tournament_id: self.tournament_id.clone(),
            seed: self.seed,
            num_players: n,
            placements: HashMap::new(),
            hands_played: players.iter().map(|p| (p.player_id(), 0)).collect(),
            eliminations: Vec::new(),
            winner: None,
            total_hands: 0,
            levels_reached: 0,
            duration_ms: 0,
            aborted: None,
            final_stacks: HashMap::new(),
        };
        let by_id: HashMap<PlayerId, Arc<dyn Player>> = players
            .iter()
            .map(|p| (p.player_id(), Arc::clone(p)))
            .collect();
        debug_assert_eq!(by_id.len(), n, "player ids must be distinct");

        for p in &players {
            p.notify(&PublicEvent::TournamentStarted {
                tournament_id: self.tournament_id.clone(),
                player_id: p.player_id(),
                num_players: n,
                starting_stack: cfg.starting_stack,
            })
            .await;
        }

        if n <= 1 {
            outcome.winner = players.first().map(|p| p.player_id());
            outcome.placements = finalize_placements(&[], outcome.winner);
            for p in &players {
                p.notify(&PublicEvent::TournamentEnded {
                    tournament_id: self.tournament_id.clone(),
                    winner: outcome.winner,
                    your_placement: Some(1),
                })
                .await;
                p.shutdown().await;
            }
            outcome.final_stacks = players
                .iter()
                .map(|p| (p.player_id(), cfg.starting_stack))
                .collect();
            outcome.duration_ms = started.elapsed().as_millis() as u64;
            return outcome;
        }

        // ---- build tables
        let mut rng = StdRng::seed_from_u64(self.seed);
        let ids: Vec<PlayerId> = players.iter().map(|p| p.player_id()).collect();
        let groups = build_initial_tables(&ids, cfg.table_size, &mut rng);
        let (ev_tx, mut ev_rx) = mpsc::channel::<TableEvent>(EVENT_QUEUE * groups.len().max(1));
        let level0 = cfg.level(0);
        let mut tables: HashMap<TableId, TableInfo> = HashMap::new();
        for (i, group) in groups.iter().enumerate() {
            let table_id = (i + 1) as TableId;
            let handle = spawn_table(
                TableConfig {
                    table_id,
                    capacity: cfg.table_size,
                    action_timeout: Duration::from_millis(cfg.action_timeout_ms),
                    seed: mix(self.seed, table_id),
                    initial_level: level0,
                    record_events: self.record_hands && hand_log.is_some(),
                },
                ev_tx.clone(),
            );
            let mut assigned = HashSet::new();
            for pid in group {
                handle.send(TableCommand::Seat {
                    player: Arc::clone(&by_id[pid]),
                    stack: cfg.starting_stack,
                });
                assigned.insert(*pid);
            }
            tables.insert(
                table_id,
                TableInfo {
                    handle,
                    assigned,
                    hands_in_level: 0,
                    hands_total: 0,
                    errors: 0,
                    closing: false,
                },
            );
        }
        drop(ev_tx);

        // ---- state
        let mut active: HashSet<PlayerId> = ids.iter().copied().collect();
        let mut stacks: HashMap<PlayerId, i64> =
            ids.iter().map(|&p| (p, cfg.starting_stack)).collect();
        let total_chips = cfg.starting_stack * n as i64;
        let mut level_index: u32 = 0;
        let mut level_started = Instant::now();
        let mut pending_level: Option<HashSet<TableId>> = None;
        let mut pending_close: Option<TableId> = None;
        let mut shutdowns: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // ---- main loop
        loop {
            if active.len() <= 1 {
                break;
            }
            if let Some(c) = &cancel {
                if c.load(Ordering::SeqCst) {
                    outcome.aborted = Some("cancelled".into());
                    break;
                }
            }
            tokio::select! {
                ev = ev_rx.recv() => {
                    let Some(ev) = ev else { break; };
                    match ev {
                        TableEvent::HandCompleted { table_id, hand_id, level, result, events, .. } => {
                            outcome.total_hands += 1;
                            if let Some(t) = tables.get_mut(&table_id) {
                                t.hands_total += 1;
                                if level == level_index {
                                    t.hands_in_level += 1;
                                }
                            }
                            for p in &result.participants {
                                *outcome.hands_played.entry(*p).or_insert(0) += 1;
                            }
                            for view in &result.seats {
                                if let Some(pid) = view.player_id {
                                    stacks.insert(pid, view.stack);
                                }
                            }
                            for &p in &result.busted {
                                if active.remove(&p) {
                                    let hp = outcome.hands_played.get(&p).copied().unwrap_or(0);
                                    let order = outcome.eliminations.len();
                                    outcome.eliminations.push(Elimination { player_id: p, hands_played: hp, order });
                                    if let Some(t) = tables.get_mut(&table_id) {
                                        t.assigned.remove(&p);
                                    }
                                    if let Some(player) = by_id.get(&p) {
                                        let player = Arc::clone(player);
                                        shutdowns.push(tokio::spawn(async move { player.shutdown().await }));
                                    }
                                }
                            }
                            if let Some(log) = &hand_log {
                                let _ = log.send(HandRecord {
                                    tournament_id: self.tournament_id.clone(),
                                    table_id,
                                    hand_id,
                                    level,
                                    result,
                                    events,
                                }).await;
                            }
                            if let Some(cap) = cfg.max_hands_per_table {
                                if tables.get(&table_id).map(|t| t.hands_total >= cap).unwrap_or(false) {
                                    outcome.aborted = Some("max_hands".into());
                                    break;
                                }
                            }
                        }
                        TableEvent::Paused { table_id, .. } => {
                            if let Some(waiting) = pending_level.as_mut() {
                                waiting.remove(&table_id);
                                if waiting.is_empty() {
                                    pending_level = None;
                                    level_index += 1;
                                    outcome.levels_reached = level_index;
                                    let lvl = cfg.level(level_index);
                                    for t in tables.values_mut() {
                                        t.hands_in_level = 0;
                                        t.handle.send(TableCommand::ApplyBlinds(lvl));
                                        t.handle.send(TableCommand::Resume);
                                    }
                                    level_started = Instant::now();
                                }
                            } else {
                                // Not expecting a pause (e.g. table paused itself after an error).
                                if let Some(t) = tables.get(&table_id) {
                                    t.handle.send(TableCommand::Resume);
                                }
                            }
                        }
                        TableEvent::Idle { .. } => {}
                        TableEvent::Closed { table_id, players } => {
                            if let Some(mut t) = tables.remove(&table_id) {
                                t.handle.join().await;
                            }
                            if pending_close == Some(table_id) {
                                pending_close = None;
                            }
                            if let Some(waiting) = pending_level.as_mut() {
                                waiting.remove(&table_id);
                            }
                            for (player, stack) in players {
                                let pid = player.player_id();
                                match emptiest_table_with_room(&tables) {
                                    Some(dest) => {
                                        let t = tables.get_mut(&dest).expect("exists");
                                        t.assigned.insert(pid);
                                        debug_assert!(t.assigned.len() <= t.handle.capacity);
                                        t.handle.send(TableCommand::Seat { player, stack });
                                    }
                                    None => {
                                        // Should be impossible: we only close when room exists and
                                        // close one table at a time. Fail loudly rather than lose a
                                        // player.
                                        tracing::error!(pid, "no table has room for a moved player");
                                        outcome.aborted = Some("internal: no room to reseat".into());
                                    }
                                }
                            }
                            // The pending level change may now be complete.
                            if matches!(&pending_level, Some(w) if w.is_empty()) {
                                pending_level = None;
                                level_index += 1;
                                outcome.levels_reached = level_index;
                                let lvl = cfg.level(level_index);
                                for t in tables.values_mut() {
                                    t.hands_in_level = 0;
                                    t.handle.send(TableCommand::ApplyBlinds(lvl));
                                    t.handle.send(TableCommand::Resume);
                                }
                                level_started = Instant::now();
                            }
                        }
                        TableEvent::Error { table_id, message } => {
                            tracing::error!(table_id, %message, "table error");
                            if let Some(t) = tables.get_mut(&table_id) {
                                t.errors += 1;
                                if t.errors > 3 {
                                    outcome.aborted = Some(format!("table {} failed repeatedly: {}", table_id, message));
                                    break;
                                }
                                t.handle.send(TableCommand::Resume);
                            }
                        }
                    }
                }
                _ = tick.tick() => {}
            }
            if outcome.aborted.is_some() {
                break;
            }

            // ---- level advancement
            if pending_level.is_none() && !tables.is_empty() {
                let la = &cfg.level_advance;
                let hands_trigger = la
                    .hands_per_level
                    .map(|h| {
                        let total: u64 = tables.values().map(|t| t.hands_in_level).sum();
                        total >= h * tables.len() as u64
                    })
                    .unwrap_or(false);
                let time_trigger = la
                    .max_level_duration_secs
                    .map(|s| level_started.elapsed() >= Duration::from_secs(s))
                    .unwrap_or(false);
                let avg = if active.is_empty() {
                    0
                } else {
                    total_chips / active.len() as i64
                };
                let stack_trigger = la
                    .avg_stack_thresholds
                    .iter()
                    .any(|(lvl, min_avg)| *lvl == level_index + 1 && avg >= *min_avg);
                if hands_trigger || time_trigger || stack_trigger {
                    let waiting: HashSet<TableId> = tables.keys().copied().collect();
                    for t in tables.values() {
                        t.handle.send(TableCommand::PauseAfterHand);
                    }
                    pending_level = Some(waiting);
                }
            }

            // ---- table breaking (one at a time, never during a level change)
            if pending_close.is_none() && pending_level.is_none() && tables.len() > 1 {
                let mut candidates: Vec<(usize, TableId)> = tables
                    .iter()
                    .filter(|(_, t)| {
                        !t.closing
                            && !t.assigned.is_empty()
                            && t.assigned.len() <= cfg.break_threshold
                    })
                    .map(|(id, t)| (t.assigned.len(), *id))
                    .collect();
                candidates.sort();
                for (size, id) in candidates {
                    let room: usize = tables
                        .iter()
                        .filter(|(tid, t)| **tid != id && !t.closing)
                        .map(|(_, t)| cfg.table_size.saturating_sub(t.assigned.len()))
                        .sum();
                    if room >= size {
                        let t = tables.get_mut(&id).expect("exists");
                        t.closing = true;
                        t.handle.send(TableCommand::Close);
                        pending_close = Some(id);
                        break;
                    }
                }
            }
        }

        // ---- teardown
        for t in tables.values() {
            t.handle.send(TableCommand::Close);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !tables.is_empty() && Instant::now() < deadline {
            match tokio::time::timeout_at(deadline.into(), ev_rx.recv()).await {
                Ok(Some(TableEvent::Closed { table_id, players })) => {
                    if let Some(mut t) = tables.remove(&table_id) {
                        t.handle.join().await;
                    }
                    for (p, stack) in players {
                        stacks.insert(p.player_id(), stack);
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        for (_, mut t) in tables.drain() {
            t.handle.abort();
        }

        // ---- placements
        if outcome.aborted.is_none() {
            outcome.winner = active.iter().next().copied();
            outcome.placements = finalize_placements(
                &outcome
                    .eliminations
                    .iter()
                    .map(|e| (e.player_id, e.hands_played))
                    .collect::<Vec<_>>(),
                outcome.winner,
            );
        } else if outcome.aborted.as_deref() == Some("max_hands") {
            // Safety valve: survivors ranked by stack ahead of everyone eliminated.
            let mut survivors: Vec<PlayerId> = active.iter().copied().collect();
            survivors.sort_by(|a, b| stacks[b].cmp(&stacks[a]).then(a.cmp(b)));
            let elim: Vec<(PlayerId, u64)> = outcome
                .eliminations
                .iter()
                .map(|e| (e.player_id, e.hands_played))
                .collect();
            let mut placements = finalize_placements(&elim, None);
            for v in placements.values_mut() {
                *v += survivors.len();
            }
            for (i, p) in survivors.iter().enumerate() {
                placements.insert(*p, i + 1);
            }
            outcome.winner = survivors.first().copied();
            outcome.placements = placements;
        }

        for p in &players {
            if active.contains(&p.player_id()) {
                p.notify(&PublicEvent::TournamentEnded {
                    tournament_id: self.tournament_id.clone(),
                    winner: outcome.winner,
                    your_placement: outcome.placements.get(&p.player_id()).copied(),
                })
                .await;
                let p = Arc::clone(p);
                shutdowns.push(tokio::spawn(async move { p.shutdown().await }));
            }
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            futures::future::join_all(shutdowns),
        )
        .await;

        outcome.final_stacks = stacks;
        outcome.duration_ms = started.elapsed().as_millis() as u64;
        outcome
    }
}

/// Split shuffled players over ⌈n / capacity⌉ tables round-robin, so sizes differ by at most one.
pub fn build_initial_tables(
    players: &[PlayerId],
    capacity: usize,
    rng: &mut StdRng,
) -> Vec<Vec<PlayerId>> {
    let mut bag = players.to_vec();
    bag.shuffle(rng);
    let capacity = capacity.max(2);
    let num_tables = bag.len().div_ceil(capacity).max(1);
    let mut tables: Vec<Vec<PlayerId>> = vec![Vec::new(); num_tables];
    for (i, p) in bag.into_iter().enumerate() {
        tables[i % num_tables].push(p);
    }
    tables
}

fn emptiest_table_with_room(tables: &HashMap<TableId, TableInfo>) -> Option<TableId> {
    tables
        .iter()
        .filter(|(_, t)| !t.closing && t.assigned.len() < t.handle.capacity)
        .min_by_key(|(id, t)| (t.assigned.len(), **id))
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_tables_are_balanced() {
        let mut rng = StdRng::seed_from_u64(1);
        for n in 2..=200u32 {
            let players: Vec<PlayerId> = (1..=n).collect();
            let tables = build_initial_tables(&players, 9, &mut rng);
            assert_eq!(tables.len(), (n as usize).div_ceil(9));
            let max = tables.iter().map(|t| t.len()).max().unwrap();
            let min = tables.iter().map(|t| t.len()).min().unwrap();
            assert!(max - min <= 1, "n={n}: {max} vs {min}");
            assert!(max <= 9);
            let total: usize = tables.iter().map(|t| t.len()).sum();
            assert_eq!(total, n as usize);
        }
    }
}
