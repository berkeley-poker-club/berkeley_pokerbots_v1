use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use crate::player_manager::PlayerId;

pub type TableId = u64;
pub type SeatIndex = u8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableCommand {
    CreateTable { capacity: usize },
    SeatPlayer { player_id: usize },
    ApplyBlinds { level_id: u32, small_blind: i64, big_blind: i64, ante: i64 },
    PauseAfterHand,
    Resume,
    CloseAfterHand,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TableEvent {
    HandEnded { table_id: TableId, participants: Vec<PlayerId> },
    PlayerBusted { table_id: TableId, player: PlayerId },
    TableSizes  { table_id: TableId, active_count: PlayerId },
    ReadyForReseat { table_id: TableId, open_seats: PlayerId },
    LevelApplied { table_id: TableId, level_id: PlayerId },
}

#[derive(Clone, Debug)]
pub struct Table {
    pub id: TableId,
    pub capacity: usize,
    pub active_count: usize,
    pub status_running: bool,
}

#[async_trait]
pub trait TableHandle: Send + Sync {
    fn id(&self) -> TableId;
    fn capacity(&self) -> usize;
    async fn send(&self, cmd: TableCommand) -> anyhow::Result<()>;
    async fn drain_events(&self) -> Vec<TableEvent>;
}
