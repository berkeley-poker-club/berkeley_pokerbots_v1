//! A table is a tokio task that plays hands back-to-back for the players seated at it, taking
//! commands from the tournament director and reporting events back. All commands take effect at
//! hand boundaries; the task never touches shared state, so many tables run concurrently.

use crate::driver::{broadcast, play_hand};
use crate::player::Player;
use poker_utils::{
    Deck, HandParams, HandResult, LevelSpec, PlayerId, PublicEvent, SeatIndex, SeatStatus, SeatView,
};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub type TableId = u64;

#[derive(Clone, Debug)]
pub struct TableConfig {
    pub table_id: TableId,
    pub capacity: usize,
    pub action_timeout: Duration,
    /// Seed for seating and deck shuffles at this table.
    pub seed: u64,
    pub initial_level: LevelSpec,
    /// Include the full public event log of each hand in `HandCompleted` (for hand histories).
    pub record_events: bool,
}

pub enum TableCommand {
    /// Seat a player (takes effect before the next hand; a random open seat is chosen).
    Seat {
        player: Arc<dyn Player>,
        stack: i64,
    },
    /// Change blinds starting with the next hand.
    ApplyBlinds(LevelSpec),
    /// Finish the current hand and then wait until `Resume`.
    PauseAfterHand,
    Resume,
    /// Finish the current hand, then hand back all players and exit.
    Close,
}

#[derive(Clone)]
pub enum TableEvent {
    HandCompleted {
        table_id: TableId,
        hand_id: u64,
        level: u32,
        result: HandResult,
        events: Vec<PublicEvent>,
        /// Players still seated (with chips) after the hand.
        seated_after: usize,
    },
    /// The table is at a hand boundary and paused.
    Paused {
        table_id: TableId,
        seated: usize,
    },
    /// Fewer than two players are seated; the table is waiting for commands.
    Idle {
        table_id: TableId,
        seated: usize,
    },
    /// The table has closed; these players (with their stacks) need new homes.
    Closed {
        table_id: TableId,
        players: Vec<(Arc<dyn Player>, i64)>,
    },
    Error {
        table_id: TableId,
        message: String,
    },
}

impl std::fmt::Debug for TableEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableEvent::HandCompleted {
                table_id,
                hand_id,
                level,
                result,
                seated_after,
                ..
            } => f
                .debug_struct("HandCompleted")
                .field("table_id", table_id)
                .field("hand_id", hand_id)
                .field("level", level)
                .field("participants", &result.participants)
                .field("busted", &result.busted)
                .field("seated_after", seated_after)
                .finish(),
            TableEvent::Paused { table_id, seated } => f
                .debug_struct("Paused")
                .field("table_id", table_id)
                .field("seated", seated)
                .finish(),
            TableEvent::Idle { table_id, seated } => f
                .debug_struct("Idle")
                .field("table_id", table_id)
                .field("seated", seated)
                .finish(),
            TableEvent::Closed { table_id, players } => f
                .debug_struct("Closed")
                .field("table_id", table_id)
                .field(
                    "players",
                    &players
                        .iter()
                        .map(|(p, s)| (p.player_id(), *s))
                        .collect::<Vec<_>>(),
                )
                .finish(),
            TableEvent::Error { table_id, message } => f
                .debug_struct("Error")
                .field("table_id", table_id)
                .field("message", message)
                .finish(),
        }
    }
}

pub struct TableHandle {
    pub id: TableId,
    pub capacity: usize,
    tx: mpsc::UnboundedSender<TableCommand>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl TableHandle {
    pub fn send(&self, cmd: TableCommand) -> bool {
        self.tx.send(cmd).is_ok()
    }

    /// Wait for the table task to finish (after `Close`).
    pub async fn join(&mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }

    pub fn abort(&mut self) {
        if let Some(j) = self.join.take() {
            j.abort();
        }
    }
}

struct Seated {
    player: Arc<dyn Player>,
    stack: i64,
}

struct TableTask {
    cfg: TableConfig,
    rx: mpsc::UnboundedReceiver<TableCommand>,
    events: mpsc::Sender<TableEvent>,
    seats: Vec<Option<Seated>>,
    waiting: VecDeque<(Arc<dyn Player>, i64)>,
    level: LevelSpec,
    paused: bool,
    announced_paused: bool,
    announced_idle: bool,
    closing: bool,
    button: Option<SeatIndex>,
    hand_no: u64,
    rng: StdRng,
}

/// Start a table task. Events are reported on `events`.
/// Events are sent on a bounded channel: a table blocks at the hand boundary if the director
/// falls more than `EVENT_QUEUE` hands behind, which bounds memory and keeps level changes timely.
pub const EVENT_QUEUE: usize = 64;

pub fn spawn_table(cfg: TableConfig, events: mpsc::Sender<TableEvent>) -> TableHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let id = cfg.table_id;
    let capacity = cfg.capacity;
    let mut seats = Vec::with_capacity(capacity);
    for _ in 0..capacity {
        seats.push(None);
    }
    let task = TableTask {
        rng: StdRng::seed_from_u64(cfg.seed),
        level: cfg.initial_level,
        cfg,
        rx,
        events,
        seats,
        waiting: VecDeque::new(),
        paused: false,
        announced_paused: false,
        announced_idle: false,
        closing: false,
        button: None,
        hand_no: 0,
    };
    let join = tokio::spawn(task.run());
    TableHandle {
        id,
        capacity,
        tx,
        join: Some(join),
    }
}

impl TableTask {
    async fn run(mut self) {
        loop {
            self.drain_commands().await;
            if self.closing {
                self.close().await;
                return;
            }
            self.seat_waiting().await;
            let seated = self.seated_count();

            if self.paused {
                if !self.announced_paused {
                    self.announced_paused = true;
                    self.emit(TableEvent::Paused {
                        table_id: self.cfg.table_id,
                        seated,
                    })
                    .await;
                }
                self.wait_command().await;
                continue;
            }
            if seated < 2 {
                if !self.announced_idle {
                    self.announced_idle = true;
                    self.emit(TableEvent::Idle {
                        table_id: self.cfg.table_id,
                        seated,
                    })
                    .await;
                }
                self.wait_command().await;
                continue;
            }
            self.announced_idle = false;
            self.play_one_hand().await;
            // Fairness: in-process bots never yield, so give other tables a turn between hands.
            tokio::task::yield_now().await;
        }
    }

    async fn emit(&mut self, ev: TableEvent) {
        if self.events.send(ev).await.is_err() {
            // Director is gone: shut down.
            self.closing = true;
        }
    }

    async fn drain_commands(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(cmd) => self.handle(cmd).await,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.closing = true;
                    break;
                }
            }
        }
    }

    async fn wait_command(&mut self) {
        match self.rx.recv().await {
            Some(cmd) => self.handle(cmd).await,
            None => self.closing = true,
        }
    }

    async fn handle(&mut self, cmd: TableCommand) {
        match cmd {
            TableCommand::Seat { player, stack } => self.waiting.push_back((player, stack)),
            TableCommand::ApplyBlinds(level) => {
                self.level = level;
                let ev = PublicEvent::BlindLevel {
                    level: level.level_id,
                    small_blind: level.small_blind,
                    big_blind: level.big_blind,
                    ante: level.ante,
                };
                let players = self.players_by_seat();
                broadcast(&players, &ev).await;
            }
            TableCommand::PauseAfterHand => {
                self.paused = true;
            }
            TableCommand::Resume => {
                self.paused = false;
                self.announced_paused = false;
            }
            TableCommand::Close => self.closing = true,
        }
    }

    fn seated_count(&self) -> usize {
        self.seats.iter().filter(|s| s.is_some()).count()
    }

    fn players_by_seat(&self) -> Vec<Option<Arc<dyn Player>>> {
        self.seats
            .iter()
            .map(|s| s.as_ref().map(|x| Arc::clone(&x.player)))
            .collect()
    }

    fn seat_views(&self) -> Vec<SeatView> {
        self.seats
            .iter()
            .enumerate()
            .map(|(i, s)| match s {
                Some(x) => SeatView {
                    seat: i as SeatIndex,
                    player_id: Some(x.player.player_id()),
                    stack: x.stack,
                    committed_street: 0,
                    committed_total: 0,
                    status: SeatStatus::Active,
                },
                None => SeatView {
                    seat: i as SeatIndex,
                    player_id: None,
                    stack: 0,
                    committed_street: 0,
                    committed_total: 0,
                    status: SeatStatus::Empty,
                },
            })
            .collect()
    }

    async fn seat_waiting(&mut self) {
        while !self.waiting.is_empty() {
            let empty: Vec<usize> = self
                .seats
                .iter()
                .enumerate()
                .filter(|(_, s)| s.is_none())
                .map(|(i, _)| i)
                .collect();
            if empty.is_empty() {
                break;
            }
            let (player, stack) = self.waiting.pop_front().expect("non-empty");
            let seat = empty[self.rng.random_range(0..empty.len())];
            self.seats[seat] = Some(Seated {
                player: Arc::clone(&player),
                stack,
            });
            player
                .notify(&PublicEvent::Seated {
                    table_id: self.cfg.table_id,
                    seat: seat as SeatIndex,
                    seats: self.seat_views(),
                })
                .await;
            player
                .notify(&PublicEvent::BlindLevel {
                    level: self.level.level_id,
                    small_blind: self.level.small_blind,
                    big_blind: self.level.big_blind,
                    ante: self.level.ante,
                })
                .await;
        }
    }

    fn next_button(&mut self) -> SeatIndex {
        let occupied: Vec<usize> = self
            .seats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .map(|(i, _)| i)
            .collect();
        match self.button {
            None => occupied[self.rng.random_range(0..occupied.len())] as SeatIndex,
            Some(prev) => {
                let n = self.seats.len();
                for k in 1..=n {
                    let idx = (prev as usize + k) % n;
                    if self.seats[idx].is_some() {
                        return idx as SeatIndex;
                    }
                }
                occupied[0] as SeatIndex
            }
        }
    }

    async fn play_one_hand(&mut self) {
        let button = self.next_button();
        self.button = Some(button);
        self.hand_no += 1;
        let hand_id = (self.cfg.table_id << 32) | self.hand_no;
        let deck = Deck::new(mix(self.cfg.seed, hand_id));
        let params = HandParams {
            hand_id,
            table_id: self.cfg.table_id,
            rules: self.level.rules(),
            button,
            seats: self
                .seats
                .iter()
                .map(|s| s.as_ref().map(|x| (x.player.player_id(), x.stack)))
                .collect(),
            deck,
        };
        let players = self.players_by_seat();
        match play_hand(params, &players, self.cfg.action_timeout).await {
            Ok((result, events)) => {
                for view in &result.seats {
                    if let Some(Some(seat)) = self.seats.get_mut(view.seat as usize) {
                        seat.stack = view.stack;
                    }
                }
                // Remove busted players (announce to everyone at the table first).
                let busted: Vec<(SeatIndex, PlayerId)> = self
                    .seats
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| match s {
                        Some(x) if x.stack <= 0 => Some((i as SeatIndex, x.player.player_id())),
                        _ => None,
                    })
                    .collect();
                for (seat, player_id) in &busted {
                    let ev = PublicEvent::PlayerBusted {
                        seat: *seat,
                        player_id: *player_id,
                    };
                    broadcast(&players, &ev).await;
                }
                for (seat, _) in &busted {
                    self.seats[*seat as usize] = None;
                }
                let seated_after = self.seated_count();
                self.emit(TableEvent::HandCompleted {
                    table_id: self.cfg.table_id,
                    hand_id,
                    level: self.level.level_id,
                    result,
                    events: if self.cfg.record_events {
                        events
                    } else {
                        Vec::new()
                    },
                    seated_after,
                })
                .await;
            }
            Err(e) => {
                self.emit(TableEvent::Error {
                    table_id: self.cfg.table_id,
                    message: e.to_string(),
                })
                .await;
                // Avoid a hot loop on a persistent error: wait for the director.
                self.paused = true;
            }
        }
    }

    async fn close(&mut self) {
        let mut players: Vec<(Arc<dyn Player>, i64)> = Vec::new();
        for (i, s) in self.seats.iter_mut().enumerate() {
            if let Some(x) = s.take() {
                x.player
                    .notify(&PublicEvent::PlayerMoved {
                        seat: i as SeatIndex,
                        player_id: x.player.player_id(),
                    })
                    .await;
                players.push((x.player, x.stack));
            }
        }
        while let Some((p, stack)) = self.waiting.pop_front() {
            players.push((p, stack));
        }
        self.emit(TableEvent::Closed {
            table_id: self.cfg.table_id,
            players,
        })
        .await;
    }
}

/// splitmix64-style mixing of a base seed with a per-hand identifier.
pub fn mix(seed: u64, salt: u64) -> u64 {
    let mut z = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
