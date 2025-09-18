use poker_utils::GameRules;
use table_runner::{GameTable, TableId, PlayerRegistry};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct Table {
    pub id: TableId,
    pub capacity: usize,
    pub active_count: usize,
    pub status_running: bool,
}


pub struct TableFactory {
    next_table_id: TableId,
}

impl TableFactory {
    pub fn new() -> Self {
        TableFactory { next_table_id: 1 }
    }

    pub fn create_table(&mut self, capacity: usize, rules: GameRules, player_registry: Arc<Mutex<PlayerRegistry>>) -> GameTable {
        let table_id = self.next_table_id;
        self.next_table_id += 1;
        GameTable::new(table_id, capacity, rules, player_registry)
    }
}

impl Default for TableFactory {
    fn default() -> Self {
        Self::new()
    }
}
