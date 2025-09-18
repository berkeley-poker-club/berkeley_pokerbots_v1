use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::{Player, ProcessBot, BotError, TableId};
use poker_utils::game_state::PlayerId;

#[derive(Debug, Clone)]
pub enum PlayerState {
    Available,
    Playing { table_id: TableId, seat: u8 },
    InTransit { from_table: TableId, to_table: TableId },
    WaitingQueue { table_id: TableId },
    Disconnected,
}

pub struct PlayerRegistry {
    bots: HashMap<PlayerId, Arc<Mutex<ProcessBot>>>,
    states: HashMap<PlayerId, PlayerState>,
    stacks: HashMap<PlayerId, i64>,
    table_assignments: HashMap<TableId, HashSet<PlayerId>>,
    next_player_id: PlayerId,
}

impl PlayerRegistry {
    pub fn new() -> Self {
        PlayerRegistry {
            bots: HashMap::new(),
            states: HashMap::new(),
            stacks: HashMap::new(),
            table_assignments: HashMap::new(),
            next_player_id: 1,
        }
    }

    pub async fn spawn_bot(&mut self, executable: &str, args: &[&str]) -> Result<PlayerId, BotError> {
        let player_id = self.next_player_id;
        self.next_player_id += 1;

        let bot = ProcessBot::spawn(player_id, executable, args).await?;
        self.bots.insert(player_id, Arc::new(Mutex::new(bot)));
        self.states.insert(player_id, PlayerState::Available);
        self.stacks.insert(player_id, 100); // default starting stack
        Ok(player_id)
    }

    pub fn register_bot(&mut self, bot: ProcessBot) -> PlayerId {
        let player_id = bot.player_id();
        self.bots.insert(player_id, Arc::new(Mutex::new(bot)));
        self.states.insert(player_id, PlayerState::Available);
        self.stacks.insert(player_id, 100); // default starting stack
        player_id
    }

    // shared ownership model
    pub fn get_player_ref(&self, player_id: &PlayerId) -> Option<Arc<Mutex<ProcessBot>>> {
        self.bots.get(player_id).map(Arc::clone)
    }

    pub fn update_player_state(&mut self, player_id: PlayerId, state: PlayerState) {
        if let Some(old_state) = self.states.get(&player_id).cloned() {
            self.remove_from_table_assignment(player_id, &old_state);
        }

        self.add_to_table_assignment(player_id, &state);
        self.states.insert(player_id, state);
    }

    pub fn get_player_state(&self, player_id: &PlayerId) -> Option<&PlayerState> {
        self.states.get(player_id)
    }

    pub fn get_player_stack(&self, player_id: &PlayerId) -> Option<i64> {
        self.stacks.get(player_id).copied()
    }

    pub fn update_player_stack(&mut self, player_id: PlayerId, stack: i64) {
        self.stacks.insert(player_id, stack);
    }

    pub fn get_players_at_table(&self, table_id: TableId) -> Vec<PlayerId> {
        self.table_assignments.get(&table_id)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn get_available_players(&self) -> Vec<PlayerId> {
        self.states.iter()
            .filter_map(|(player_id, state)| {
                if matches!(state, PlayerState::Available) {
                    Some(*player_id)
                } else {
                    None
                }
            })
            .collect()
    }

    // Migration support
    pub fn start_migration(&mut self, player_id: PlayerId, from_table: TableId, to_table: TableId) -> Result<(), String> {
        let current_state = self.states.get(&player_id)
            .ok_or_else(|| format!("Player {} not found", player_id))?;

        match current_state {
            PlayerState::Playing { table_id, .. } if *table_id == from_table => {
                self.update_player_state(player_id, PlayerState::InTransit { from_table, to_table });
                Ok(())
            }
            PlayerState::WaitingQueue { table_id } if *table_id == from_table => {
                self.update_player_state(player_id, PlayerState::InTransit { from_table, to_table });
                Ok(())
            }
            _ => Err(format!("Player {} not at source table {}", player_id, from_table))
        }
    }

    pub fn complete_migration(&mut self, player_id: PlayerId, to_table: TableId, seat: Option<u8>) {
        if let Some(PlayerState::InTransit { to_table: expected_table, .. }) = self.states.get(&player_id) {
            if *expected_table == to_table {
                let new_state = if let Some(seat) = seat {
                    PlayerState::Playing { table_id: to_table, seat }
                } else {
                    PlayerState::WaitingQueue { table_id: to_table }
                };
                self.update_player_state(player_id, new_state);
            }
        }
    }

    pub fn rollback_migration(&mut self, player_id: PlayerId) {
        if let Some(PlayerState::InTransit { from_table, .. }) = self.states.get(&player_id).cloned() {
            // put player back in waiting queue at original table
            self.update_player_state(player_id, PlayerState::WaitingQueue { table_id: from_table });
        }
    }

    pub fn active_players(&self) -> Vec<PlayerId> {
        self.bots.keys().copied().collect()
    }

    pub fn player_count(&self) -> usize {
        self.bots.len()
    }

    pub fn remove_eliminated_player(&mut self, player_id: PlayerId) {
        if let Some(state) = self.states.get(&player_id).cloned() {
            self.remove_from_table_assignment(player_id, &state);
        }
        self.bots.remove(&player_id);
        self.states.remove(&player_id);
        self.stacks.remove(&player_id);
    }

    pub async fn cleanup_dead_bots(&mut self) -> Vec<PlayerId> {
        let mut dead_bots = Vec::new();
        let mut bots_to_remove = Vec::new();

        for (&player_id, bot_arc) in &self.bots {
            let bot = bot_arc.lock().await;
            if !bot.is_alive().await {
                dead_bots.push(player_id);
                bots_to_remove.push(player_id);
            }
        }

        for player_id in bots_to_remove {
            if let Some(state) = self.states.get(&player_id).cloned() {
                self.remove_from_table_assignment(player_id, &state);
            }
            self.bots.remove(&player_id);
            self.states.remove(&player_id);
            self.stacks.remove(&player_id);
        }

        dead_bots
    }

    fn add_to_table_assignment(&mut self, player_id: PlayerId, state: &PlayerState) {
        match state {
            PlayerState::Playing { table_id, .. } |
            PlayerState::WaitingQueue { table_id } => {
                self.table_assignments.entry(*table_id).or_insert_with(HashSet::new).insert(player_id);
            }
            _ => {}
        }
    }

    fn remove_from_table_assignment(&mut self, player_id: PlayerId, state: &PlayerState) {
        match state {
            PlayerState::Playing { table_id, .. } |
            PlayerState::WaitingQueue { table_id } => {
                if let Some(set) = self.table_assignments.get_mut(table_id) {
                    set.remove(&player_id);
                    if set.is_empty() {
                        self.table_assignments.remove(table_id);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Default for PlayerRegistry {
    fn default() -> Self {
        Self::new()
    }
}