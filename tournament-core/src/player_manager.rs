use std::collections::HashMap;
use table_runner::{Player};

pub type PlayerId = usize;

pub struct PlayerRegistry {
    players: HashMap<PlayerId, Box<dyn Player>>,
    next_player_id: PlayerId,
}

impl PlayerRegistry {
    pub fn new() -> Self {
        PlayerRegistry {
            players: HashMap::new(),
            next_player_id: 1,
        }
    }

    pub fn register_player(&mut self, player: Box<dyn Player>) -> PlayerId {
        let player_id = player.player_id();
        self.players.insert(player_id, player);
        player_id
    }

    pub fn get_player(&self, player_id: &PlayerId) -> Option<&Box<dyn Player>> {
        self.players.get(player_id)
    }

    pub fn remove_player(&mut self, player_id: &PlayerId) -> Option<Box<dyn Player>> {
        self.players.remove(player_id)
    }

    pub fn active_players(&self) -> Vec<PlayerId> {
        self.players.keys().copied().collect()
    }

    pub fn player_count(&self) -> usize {
        self.players.len()
    }
}

impl Default for PlayerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
