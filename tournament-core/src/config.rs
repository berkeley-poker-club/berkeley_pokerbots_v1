use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LevelSpec {
    pub level_id: u32,
    pub small_blind: i64,
    pub big_blind: i64,
    pub ante: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TournamentConfig {
    pub table_size: usize,
    pub break_threshold: usize,
    pub min_table_size: usize,
    pub blind_levels: Vec<LevelSpec>,
    pub avg_stack_thresholds: Vec<(u32, i64)>,
    pub max_level_duration_secs: u64,
    pub series_length: usize,
    pub rng_seed: u64,
}

impl Default for TournamentConfig {
    fn default() -> Self {
        Self {
            table_size: 9,
            break_threshold: 7,
            min_table_size: 6,
            blind_levels: vec![LevelSpec{ level_id:0, small_blind:1, big_blind:2, ante:0 }],
            avg_stack_thresholds: vec![],
            max_level_duration_secs: 120,
            series_length: 100,
            rng_seed: 28,
        }
    }
}
