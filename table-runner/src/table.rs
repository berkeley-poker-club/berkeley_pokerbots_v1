use async_trait::async_trait;
use std::collections::VecDeque;
use tokio::sync::{mpsc, Mutex};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use poker_utils::{GameRules, GameEvent};
use poker_utils::game_state::PlayerId;
use crate::{GameRunner, PlayerRegistry, PlayerState};
use rand::{seq::SliceRandom, thread_rng};

pub type TableId = u64;
pub type SeatIndex = u8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableCommand {
    CreateTable { capacity: usize },
    SeatPlayer { player_id: PlayerId },
    ApplyBlinds { level_id: u32, small_blind: i64, big_blind: i64, ante: i64 },
    PauseAfterHand,
    Resume,
    CloseAfterHand,
    ExtractPlayer { player_id: PlayerId },
    ExtractAllPlayers,
    FinishHandAndExtract { players: Vec<PlayerId> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableEvent {
    HandEnded { table_id: TableId, participants: Vec<PlayerId> },
    PlayerBusted { table_id: TableId, player: PlayerId },
    TableSizes { table_id: TableId, active_count: usize },
    ReadyForReseat { table_id: TableId, open_seats: usize },
    LevelApplied { table_id: TableId, level_id: u32 },
    PlayerExtracted { table_id: TableId, player_id: PlayerId }, // player ready for migration
    ExtractionFailed { table_id: TableId, player_id: PlayerId, reason: String },
    AllPlayersExtracted { table_id: TableId, players: Vec<PlayerId> },
    HandFinishedExtractionReady { table_id: TableId, players: Vec<PlayerId> }, // hand done; ready to move players
}

#[async_trait]
pub trait TableHandle: Send + Sync {
    fn id(&self) -> TableId;
    fn capacity(&self) -> usize;
    async fn send(&self, cmd: TableCommand) -> anyhow::Result<()>;
    async fn drain_events(&self) -> Vec<TableEvent>;
    async fn total_player_count(&self) -> usize;
    async fn start(&self);
}

pub struct GameTable {
    id: TableId,
    capacity: usize,
    runner: Arc<Mutex<GameRunner>>,
    command_receiver: Arc<Mutex<mpsc::UnboundedReceiver<TableCommand>>>,
    command_sender: mpsc::UnboundedSender<TableCommand>,
    event_queue: Arc<Mutex<VecDeque<TableEvent>>>,
    running: Arc<Mutex<bool>>,
    rules: GameRules,
    waiting_queue: Arc<Mutex<VecDeque<PlayerId>>>,
    hand_counter: Arc<Mutex<u64>>,
    player_registry: Arc<Mutex<PlayerRegistry>>,
    pending_extractions: Arc<Mutex<Vec<PlayerId>>>,
    extract_all_pending: Arc<Mutex<bool>>,
    extract_after_hand: Arc<Mutex<Vec<PlayerId>>>,
}

impl GameTable {
    pub fn new(id: TableId, capacity: usize, rules: GameRules, player_registry: Arc<Mutex<PlayerRegistry>>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<GameEvent>();

        let event_queue = Arc::new(Mutex::new(VecDeque::new()));
        let event_queue_clone = Arc::clone(&event_queue);

        // convert GameEvents to TableEvents
        tokio::spawn(async move {
            while let Some(game_event) = event_rx.recv().await {
                let table_event = match game_event {
                    GameEvent::HandEnded { winners, .. } => {
                        Some(TableEvent::HandEnded {
                            table_id: id,
                            participants: winners,
                        })
                    }
                    GameEvent::PlayerEliminated { player_id } => {
                        Some(TableEvent::PlayerBusted {
                            table_id: id,
                            player: player_id,
                        })
                    }
                    _ => None,
                };

                if let Some(event) = table_event {
                    let mut queue = event_queue_clone.lock().await;
                    queue.push_back(event);
                }
            }
        });

        GameTable {
            id,
            capacity,
            runner: Arc::new(Mutex::new(GameRunner::new(id, rules.clone(), event_tx, 28))),
            command_receiver: Arc::new(Mutex::new(cmd_rx)),
            command_sender: cmd_tx,
            event_queue: Arc::new(Mutex::new(VecDeque::new())),
            running: Arc::new(Mutex::new(false)),
            rules,
            waiting_queue: Arc::new(Mutex::new(VecDeque::new())),
            hand_counter: Arc::new(Mutex::new(1)),
            player_registry,
            pending_extractions: Arc::new(Mutex::new(Vec::new())),
            extract_all_pending: Arc::new(Mutex::new(false)),
            extract_after_hand: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn start(&self) {
        let mut running = self.running.lock().await;
        *running = true;

        let running_clone = Arc::clone(&self.running);
        let runner_clone = Arc::clone(&self.runner);
        let cmd_receiver_clone = Arc::clone(&self.command_receiver);
        let event_queue_clone = Arc::clone(&self.event_queue);
        let hand_counter_clone = Arc::clone(&self.hand_counter);
        let waiting_queue_clone = Arc::clone(&self.waiting_queue);
        let player_registry_clone = Arc::clone(&self.player_registry);
        let pending_extractions_clone = Arc::clone(&self.pending_extractions);
        let extract_all_pending_clone = Arc::clone(&self.extract_all_pending);
        let extract_after_hand_clone = Arc::clone(&self.extract_after_hand);

        tokio::spawn(async move {
            let mut paused = false;
            let mut should_close = false;

            loop {
                let running = *running_clone.lock().await;
                if !running && !should_close {
                    break;
                }

                let mut receiver = cmd_receiver_clone.lock().await;
                while let Ok(cmd) = receiver.try_recv() {
                    match cmd {
                        TableCommand::SeatPlayer { player_id } => {
                            let mut waiting_queue = waiting_queue_clone.lock().await;
                            waiting_queue.push_back(player_id);
                        }
                        TableCommand::ApplyBlinds { level_id, small_blind, big_blind, ante } => {
                            let mut runner = runner_clone.lock().await;
                            runner.state.rules.small_blind = small_blind;
                            runner.state.rules.big_blind = big_blind;
                            runner.state.rules.ante = ante;

                            let mut queue = event_queue_clone.lock().await;
                            queue.push_back(TableEvent::LevelApplied {
                                table_id: runner.table_id,
                                level_id,
                            });
                        }
                        TableCommand::PauseAfterHand => {
                            paused = true;
                        }
                        TableCommand::Resume => {
                            paused = false;
                        }
                        TableCommand::CloseAfterHand => {
                            should_close = true;
                        }
                        TableCommand::ExtractPlayer { player_id } => {
                            let mut extractions = pending_extractions_clone.lock().await;
                            extractions.push(player_id);
                        }
                        TableCommand::ExtractAllPlayers => {
                            let mut extract_all = extract_all_pending_clone.lock().await;
                            *extract_all = true;
                        }
                        TableCommand::FinishHandAndExtract { players } => {
                            let mut extract_after = extract_after_hand_clone.lock().await;
                            extract_after.extend(players);
                        }
                        _ => {}
                    }
                }
                drop(receiver);

                if !paused && !should_close {
                    Self::process_waiting_queue(&runner_clone, &waiting_queue_clone, &player_registry_clone).await;

                    let mut runner = runner_clone.lock().await;
                    let mut hand_counter = hand_counter_clone.lock().await;
                    let hand_id = *hand_counter;
                    *hand_counter += 1;

                    if let Err(_) = runner.run_hand(hand_id, 0).await {
                        paused = true;
                    }

                    // Handle extractions after hand completion
                    Self::process_extractions(
                        &runner_clone,
                        &event_queue_clone,
                        &pending_extractions_clone,
                        &extract_all_pending_clone,
                        &extract_after_hand_clone,
                        &player_registry_clone,
                    ).await;

                    let active_count = runner.get_active_seats().len();
                    let waiting_count = waiting_queue_clone.lock().await.len();
                    let mut queue = event_queue_clone.lock().await;
                    queue.push_back(TableEvent::TableSizes {
                        table_id: runner.table_id,
                        active_count: active_count + waiting_count,
                    });
                }

                if should_close {
                    break;
                }

                // tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
        });
    }

    async fn process_waiting_queue(
        runner: &Arc<Mutex<GameRunner>>,
        waiting_queue: &Arc<Mutex<VecDeque<PlayerId>>>,
        player_registry: &Arc<Mutex<PlayerRegistry>>,
    ) {
        let mut queue = waiting_queue.lock().await;
        if queue.is_empty() {
            return;
        }

        let mut runner = runner.lock().await;
        let mut available_seats = Vec::new();

        for seat_idx in 0..runner.state.seats.len() {
            if runner.state.seats[seat_idx].player_id.is_none() {
                available_seats.push(seat_idx as SeatIndex);
            }
        }

        available_seats.shuffle(&mut thread_rng());

        let seat_count = std::cmp::min(available_seats.len(), queue.len());
        for i in 0..seat_count {
            if let Some(player_id) = queue.pop_front() {
                let seat = available_seats[i];
                let mut registry = player_registry.lock().await;
                let stack = registry.get_player_stack(&player_id).unwrap_or(1000);
                if let Some(player) = registry.get_player_ref(&player_id) {
                    registry.update_player_state(player_id, PlayerState::Playing {
                        table_id: runner.table_id,
                        seat
                    });
                    drop(registry);

                    if let Ok(()) = runner.seat_player(seat, player, stack).await {
                    } else {
                        let mut registry = player_registry.lock().await;
                        registry.update_player_state(player_id, PlayerState::Available);
                        break;
                    }
                } else {
                    continue;
                }
            }
        }
    }

    async fn process_extractions(
        runner: &Arc<Mutex<GameRunner>>,
        event_queue: &Arc<Mutex<VecDeque<TableEvent>>>,
        pending_extractions: &Arc<Mutex<Vec<PlayerId>>>,
        extract_all_pending: &Arc<Mutex<bool>>,
        extract_after_hand: &Arc<Mutex<Vec<PlayerId>>>,
        player_registry: &Arc<Mutex<PlayerRegistry>>,
    ) {
        let table_id = {
            let runner = runner.lock().await;
            runner.table_id
        };

        let should_extract_all = {
            let mut extract_all = extract_all_pending.lock().await;
            if *extract_all {
                *extract_all = false;
                true
            } else {
                false
            }
        };

        let mut players_to_extract = Vec::new();

        if should_extract_all {
            let runner = runner.lock().await;
            for (_seat_idx, seat_state) in runner.state.seats.iter().enumerate() {
                if let Some(player_id) = seat_state.player_id {
                    players_to_extract.push(player_id);
                }
            }
        } else {
            let mut extractions = pending_extractions.lock().await;
            players_to_extract.extend(extractions.drain(..));

            let mut extract_after = extract_after_hand.lock().await;
            players_to_extract.extend(extract_after.drain(..));
        }

        for player_id in &players_to_extract {
            if let Some(seat_idx) = Self::find_player_seat(&runner, *player_id).await {
                let current_stack = {
                    let runner = runner.lock().await;
                    if seat_idx < runner.state.seats.len() {
                        runner.state.seats[seat_idx].stack
                    } else {
                        0
                    }
                };

                let mut registry = player_registry.lock().await;
                registry.update_player_stack(*player_id, current_stack);
                drop(registry);

                let mut runner = runner.lock().await;
                if seat_idx < runner.state.seats.len() {
                    runner.state.seats[seat_idx].player_id = None;
                    runner.state.seats[seat_idx].stack = 0;
                    runner.remove_player_from_seat(seat_idx as u8);
                }
                drop(runner);

                // Update player state to available for re-seating
                let mut registry = player_registry.lock().await;
                registry.update_player_state(*player_id, PlayerState::Available);
                drop(registry);

                // Send extraction event
                let mut queue = event_queue.lock().await;
                queue.push_back(TableEvent::PlayerExtracted {
                    table_id,
                    player_id: *player_id
                });
            } else {
                let mut queue = event_queue.lock().await;
                queue.push_back(TableEvent::ExtractionFailed {
                    table_id,
                    player_id: *player_id,
                    reason: "Player not found at table".to_string()
                });
            }
        }

        if should_extract_all && !players_to_extract.is_empty() {
            let mut queue = event_queue.lock().await;
            queue.push_back(TableEvent::AllPlayersExtracted {
                table_id,
                players: players_to_extract
            });
        }
    }

    async fn find_player_seat(runner: &Arc<Mutex<GameRunner>>, player_id: PlayerId) -> Option<usize> {
        let runner = runner.lock().await;
        for (seat_idx, seat_state) in runner.state.seats.iter().enumerate() {
            if seat_state.player_id == Some(player_id) {
                return Some(seat_idx);
            }
        }
        None
    }
}

#[async_trait]
impl TableHandle for GameTable {
    fn id(&self) -> TableId {
        self.id
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    async fn send(&self, cmd: TableCommand) -> anyhow::Result<()> {
        self.command_sender.send(cmd)
            .map_err(|e| anyhow::anyhow!("send command failed: {}", e))
    }

    async fn drain_events(&self) -> Vec<TableEvent> {
        let mut queue = self.event_queue.lock().await;
        let events: Vec<_> = queue.drain(..).collect();
        events
    }

    async fn total_player_count(&self) -> usize {
        let runner = self.runner.lock().await;
        let active_count = runner.get_active_seats().len();
        let waiting_count = self.waiting_queue.lock().await.len();
        active_count + waiting_count
    }

    async fn start(&self) {
        GameTable::start(self).await;
    }
}