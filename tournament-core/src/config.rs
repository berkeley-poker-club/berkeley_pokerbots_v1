use serde::{Deserialize, Serialize};
use poker_utils::{LevelSpec, GameRules};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TournamentConfig {
    pub table_size: usize,
    pub break_threshold: usize,
    pub min_table_size: usize,
    pub blind_generator: BlindGenerator,
    pub avg_stack_thresholds: Vec<(u32, i64)>,
    pub max_level_duration_secs: u64,
    pub series_length: usize,
    pub rng_seed: u64,
    pub initial_stack: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlindGenerator {
    starting_small: i64,
    starting_big: i64,
    multiplier: f64,
    ante_starts_at_level: Option<usize>,
    ante_percentage: f64, // percentage of big blind
}

impl BlindGenerator {
    pub fn doubling() -> Self {
        Self {
            starting_small: 1,
            starting_big: 2,
            multiplier: 2.0,
            ante_starts_at_level: None,
            ante_percentage: 0.0,
        }
    }

    pub fn with_starting_blinds(mut self, small: i64, big: i64) -> Self {
        self.starting_small = small;
        self.starting_big = big;
        self
    }

    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = multiplier;
        self
    }

    pub fn with_antes(mut self, start_level: usize, percentage: f64) -> Self {
        self.ante_starts_at_level = Some(start_level);
        self.ante_percentage = percentage;
        self
    }

    pub fn generate_level(&self, level_index: usize) -> LevelSpec {
        let mut small = self.starting_small;
        let mut big = self.starting_big;

        for _ in 0..level_index {
            small = (small as f64 * self.multiplier).round() as i64;
            big = (big as f64 * self.multiplier).round() as i64;
        }

        let ante = if let Some(ante_level) = self.ante_starts_at_level {
            if level_index >= ante_level {
                (big as f64 * self.ante_percentage).round() as i64
            } else {
                0
            }
        } else {
            0
        };

        LevelSpec {
            level_id: level_index as u32,
            small_blind: small,
            big_blind: big,
            ante,
        }
    }
}

impl TournamentConfig {

    pub fn with_doubling_blinds(mut self) -> Self {
        self.blind_generator = BlindGenerator::doubling();
        self
    }

    pub fn with_custom_blinds(mut self, generator: BlindGenerator) -> Self {
        self.blind_generator = generator;
        self
    }

    pub fn to_game_rules(&self, level_index: usize) -> GameRules {
        let level = self.blind_generator.generate_level(level_index);
        GameRules {
            small_blind: level.small_blind,
            big_blind: level.big_blind,
            ante: level.ante,
            max_seats: self.table_size,
        }
    }
}

impl Default for TournamentConfig {
    fn default() -> Self {
        Self {
            table_size: 9,
            break_threshold: 7,
            min_table_size: 6,
            blind_generator: BlindGenerator::doubling(),
            avg_stack_thresholds: vec![],
            max_level_duration_secs: 120,
            series_length: 100,
            rng_seed: 28,
            initial_stack: 100,
        }
    }
}