use async_trait::async_trait;
use std::collections::VecDeque;
use tokio::sync::{mpsc, Mutex};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use poker_utils::{PlayerId, GameRules, GameEvent};
use crate::{GameRunner};

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableEvent {
    HandEnded { table_id: TableId, participants: Vec<PlayerId> },
    PlayerBusted { table_id: TableId, player: PlayerId },
    TableSizes { table_id: TableId, active_count: usize },
    ReadyForReseat { table_id: TableId, open_seats: usize },
    LevelApplied { table_id: TableId, level_id: u32 },
}

#[async_trait]
pub trait TableHandle: Send + Sync {
    fn id(&self) -> TableId;
    fn capacity(&self) -> usize;
    async fn send(&self, cmd: TableCommand) -> anyhow::Result<()>;
    async fn drain_events(&self) -> Vec<TableEvent>;
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
    next_seat: Arc<Mutex<SeatIndex>>,
    hand_counter: Arc<Mutex<u64>>,
}

impl GameTable {
    pub fn new(id: TableId, capacity: usize, rules: GameRules) -> Self {
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
            next_seat: Arc::new(Mutex::new(0)),
            hand_counter: Arc::new(Mutex::new(1)),
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
                            /*
                            let mut runner = runner_clone.lock().await;
                            let player = Box::new(MockPlayer::new(player_id, poker_utils::Action::Check));

                            // Find next available seat
                            let mut seat_found = false;
                            for seat in 0..8 {
                                if runner.seat_player(seat, mock_player.clone(), 1000).is_ok() {
                                    seat_found = true;
                                    break;
                                }
                            }

                            if seat_found {
                                let mut queue = event_queue_clone.lock().await;
                                queue.push_back(TableEvent::ReadyForReseat {
                                    table_id: runner.table_id,
                                    open_seats: 1,
                                });
                            }
                             */
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
                        _ => {}
                    }
                }
                drop(receiver);

                if !paused && !should_close {
                    let mut runner = runner_clone.lock().await;
                    let mut hand_counter = hand_counter_clone.lock().await;
                    let hand_id = *hand_counter;
                    *hand_counter += 1;

                    if let Err(_) = runner.run_hand(hand_id, 0).await {
                        paused = true;
                    }

                    let active_count = runner.get_active_seats().len();
                    let mut queue = event_queue_clone.lock().await;
                    queue.push_back(TableEvent::TableSizes {
                        table_id: runner.table_id,
                        active_count,
                    });
                }

                if should_close {
                    break;
                }

                // tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
        });
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
}