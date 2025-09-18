use std::collections::HashMap;
use table_runner::{Player, ProcessBot, BotError};

pub type PlayerId = usize;

pub struct PlayerRegistry {
    bots: HashMap<PlayerId, ProcessBot>,
    next_player_id: PlayerId,
}

impl PlayerRegistry {
    pub fn new() -> Self {
        PlayerRegistry {
            bots: HashMap::new(),
            next_player_id: 1,
        }
    }

    pub async fn spawn_bot(&mut self, executable: &str, args: &[&str]) -> Result<PlayerId, BotError> {
        let player_id = self.next_player_id;
        self.next_player_id += 1;

        let bot = ProcessBot::spawn(player_id, executable, args).await?;
        self.bots.insert(player_id, bot);
        Ok(player_id)
    }

    pub fn register_bot(&mut self, bot: ProcessBot) -> PlayerId {
        let player_id = bot.player_id();
        self.bots.insert(player_id, bot);
        player_id
    }

    pub fn get_player(&self, player_id: &PlayerId) -> Option<&dyn Player> {
        self.bots.get(player_id).map(|bot| bot as &dyn Player)
    }

    pub fn get_bot(&self, player_id: &PlayerId) -> Option<&ProcessBot> {
        self.bots.get(player_id)
    }

    pub fn remove_bot(&mut self, player_id: &PlayerId) -> Option<ProcessBot> {
        self.bots.remove(player_id)
    }

    pub fn active_players(&self) -> Vec<PlayerId> {
        self.bots.keys().copied().collect()
    }

    pub fn player_count(&self) -> usize {
        self.bots.len()
    }

    pub async fn cleanup_dead_bots(&mut self) -> Vec<PlayerId> {
        let mut dead_bots = Vec::new();
        let mut bots_to_remove = Vec::new();

        for (&player_id, bot) in &self.bots {
            if !bot.is_alive().await {
                dead_bots.push(player_id);
                bots_to_remove.push(player_id);
            }
        }

        for player_id in bots_to_remove {
            self.bots.remove(&player_id);
        }

        dead_bots
    }
}

impl Default for PlayerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
