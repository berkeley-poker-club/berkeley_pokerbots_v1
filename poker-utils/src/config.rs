//! Tournament configuration shared by the engine, the tournament director and the platform.

use crate::hand::HandRules;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelSpec {
    pub level_id: u32,
    pub small_blind: i64,
    pub big_blind: i64,
    pub ante: i64,
}

impl LevelSpec {
    pub fn rules(&self) -> HandRules {
        HandRules {
            small_blind: self.small_blind,
            big_blind: self.big_blind,
            ante: self.ante,
        }
    }
}

/// How blinds grow from level to level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlindSchedule {
    /// Big blind multiplied by `multiplier` every level (rounded to an integer, small blind is
    /// half the big blind). Antes start at `ante_starts_at_level` as `ante_fraction_of_bb * bb`.
    Geometric {
        starting_small: i64,
        starting_big: i64,
        multiplier: f64,
        #[serde(default)]
        ante_starts_at_level: Option<u32>,
        #[serde(default)]
        ante_fraction_of_bb: f64,
    },
    /// Explicit levels; the last level repeats forever.
    Levels { levels: Vec<LevelSpec> },
}

impl BlindSchedule {
    pub fn geometric(starting_small: i64, starting_big: i64, multiplier: f64) -> Self {
        BlindSchedule::Geometric {
            starting_small,
            starting_big,
            multiplier,
            ante_starts_at_level: None,
            ante_fraction_of_bb: 0.0,
        }
    }

    pub fn with_antes(self, start_level: u32, fraction_of_bb: f64) -> Self {
        match self {
            BlindSchedule::Geometric {
                starting_small,
                starting_big,
                multiplier,
                ..
            } => BlindSchedule::Geometric {
                starting_small,
                starting_big,
                multiplier,
                ante_starts_at_level: Some(start_level),
                ante_fraction_of_bb: fraction_of_bb,
            },
            other => other,
        }
    }

    pub fn level(&self, level_index: u32) -> LevelSpec {
        match self {
            BlindSchedule::Geometric {
                starting_small,
                starting_big,
                multiplier,
                ante_starts_at_level,
                ante_fraction_of_bb,
            } => {
                let mut big = *starting_big;
                let mut small = *starting_small;
                for _ in 0..level_index {
                    big = ((big as f64) * multiplier).round().max(big as f64 + 1.0) as i64;
                    small = (big / 2).max(1);
                }
                let ante = match ante_starts_at_level {
                    Some(start) if level_index >= *start => {
                        ((big as f64) * ante_fraction_of_bb).round() as i64
                    }
                    _ => 0,
                };
                LevelSpec {
                    level_id: level_index,
                    small_blind: small.min(big),
                    big_blind: big,
                    ante,
                }
            }
            BlindSchedule::Levels { levels } => {
                let idx = (level_index as usize).min(levels.len().saturating_sub(1));
                let mut l = levels.get(idx).copied().unwrap_or(LevelSpec {
                    level_id: level_index,
                    small_blind: 1,
                    big_blind: 2,
                    ante: 0,
                });
                l.level_id = level_index;
                l
            }
        }
    }
}

/// When blind levels advance. Any satisfied trigger advances the level; level changes only take
/// effect at hand boundaries and are synchronised across tables by the tournament director.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LevelAdvance {
    /// Wall-clock cap per level.
    #[serde(default)]
    pub max_level_duration_secs: Option<u64>,
    /// Advance once the average number of hands played per table in the current level reaches
    /// this value.
    #[serde(default)]
    pub hands_per_level: Option<u64>,
    /// `(level_id, min_avg_stack)`: advance to `level_id` once the average stack per active
    /// player is at least `min_avg_stack` chips (average stack only ever grows as players bust).
    #[serde(default)]
    pub avg_stack_thresholds: Vec<(u32, i64)>,
}

impl Default for LevelAdvance {
    fn default() -> Self {
        LevelAdvance {
            max_level_duration_secs: Some(120),
            hands_per_level: Some(20),
            avg_stack_thresholds: vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TournamentConfig {
    /// Maximum players per table (N).
    pub table_size: usize,
    /// A table with this many or fewer players breaks at the next hand boundary (if room exists).
    pub break_threshold: usize,
    /// No starting table may be smaller than this.
    pub min_table_size: usize,
    pub starting_stack: i64,
    pub blinds: BlindSchedule,
    pub level_advance: LevelAdvance,
    /// Time each bot has to answer a decision request.
    pub action_timeout_ms: u64,
    /// Number of tournaments in a nightly series.
    pub series_length: usize,
    pub rng_seed: u64,
    /// Safety valve: if any table plays this many hands the tournament is stopped and the
    /// remaining players are ranked by stack.
    pub max_hands_per_table: Option<u64>,
}

impl Default for TournamentConfig {
    fn default() -> Self {
        TournamentConfig {
            table_size: 9,
            break_threshold: 7,
            min_table_size: 6,
            starting_stack: 1000,
            blinds: BlindSchedule::geometric(5, 10, 1.5),
            level_advance: LevelAdvance::default(),
            action_timeout_ms: 1000,
            series_length: 100,
            rng_seed: 42,
            max_hands_per_table: Some(100_000),
        }
    }
}

impl TournamentConfig {
    pub fn level(&self, level_index: u32) -> LevelSpec {
        self.blinds.level(level_index)
    }

    /// Basic sanity checks. Returns a human-readable list of problems.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        if self.table_size < 2 || self.table_size > 23 {
            errs.push(format!(
                "table_size must be in 2..=23 (got {})",
                self.table_size
            ));
        }
        if self.break_threshold >= self.table_size {
            errs.push("break_threshold must be smaller than table_size".into());
        }
        if self.min_table_size > self.table_size {
            errs.push("min_table_size must not exceed table_size".into());
        }
        if self.min_table_size < 2 {
            errs.push("min_table_size must be at least 2".into());
        }
        if self.starting_stack <= 0 {
            errs.push("starting_stack must be positive".into());
        }
        let l0 = self.level(0);
        if l0.big_blind <= 0 || l0.small_blind < 0 || l0.small_blind > l0.big_blind {
            errs.push("level 0 blinds are invalid".into());
        }
        if self.action_timeout_ms == 0 {
            errs.push("action_timeout_ms must be positive".into());
        }
        if self.series_length == 0 {
            errs.push("series_length must be positive".into());
        }
        if self.level_advance.max_level_duration_secs.is_none()
            && self.level_advance.hands_per_level.is_none()
            && self.level_advance.avg_stack_thresholds.is_empty()
        {
            errs.push("at least one level-advance trigger must be configured".into());
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometric_levels_grow_and_keep_sb_half_bb() {
        let s = BlindSchedule::geometric(5, 10, 1.5).with_antes(2, 0.1);
        let l0 = s.level(0);
        assert_eq!((l0.small_blind, l0.big_blind, l0.ante), (5, 10, 0));
        let l1 = s.level(1);
        assert_eq!((l1.small_blind, l1.big_blind, l1.ante), (7, 15, 0));
        let l2 = s.level(2);
        assert_eq!((l2.small_blind, l2.big_blind, l2.ante), (11, 23, 2));
        for i in 1..30 {
            assert!(s.level(i).big_blind > s.level(i - 1).big_blind);
        }
    }

    #[test]
    fn explicit_levels_clamp() {
        let s = BlindSchedule::Levels {
            levels: vec![
                LevelSpec {
                    level_id: 0,
                    small_blind: 1,
                    big_blind: 2,
                    ante: 0,
                },
                LevelSpec {
                    level_id: 1,
                    small_blind: 2,
                    big_blind: 4,
                    ante: 1,
                },
            ],
        };
        assert_eq!(s.level(5).big_blind, 4);
        assert_eq!(s.level(5).level_id, 5);
    }

    #[test]
    fn default_config_is_valid_and_serialisable() {
        let c = TournamentConfig::default();
        c.validate().unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back: TournamentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        // partial JSON fills defaults
        let partial: TournamentConfig =
            serde_json::from_str(r#"{"table_size": 6, "break_threshold": 4}"#).unwrap();
        assert_eq!(partial.table_size, 6);
        assert_eq!(partial.starting_stack, 1000);
    }
}
