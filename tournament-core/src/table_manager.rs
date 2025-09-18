use poker_utils::GameRules;
use table_runner::{GameTable, TableId, TableHandle, TableEvent, TableCommand};

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

    pub fn create_table(&mut self, capacity: usize, rules: GameRules) -> GameTable {
        let table_id = self.next_table_id;
        self.next_table_id += 1;
        GameTable::new(table_id, capacity, rules)
    }
}

impl Default for TableFactory {
    fn default() -> Self {
        Self::new()
    }
}
