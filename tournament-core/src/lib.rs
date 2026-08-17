//! `tournament-core`: tournament orchestration on top of `table-runner`.
//!
//! * [`TournamentDirector`] runs one multi-table tournament (table breaking, blind levels,
//!   SPEC placements).
//! * [`SeriesRunner`] runs a series of tournaments and computes geometric-mean scores.
//! * [`scoring`] holds the pure ranking functions.

pub mod director;
pub mod scoring;
pub mod series;

pub use director::{
    build_initial_tables, CancelFlag, Elimination, HandRecord, TournamentDirector,
    TournamentOutcome,
};
pub use poker_utils::{BlindSchedule, LevelAdvance, LevelSpec, PlayerId, TournamentConfig};
pub use scoring::{finalize_placements, geometric_mean_scores, ScoreEntry};
pub use series::{OnTournamentDone, PlayerFactory, SeriesOutcome, SeriesRunner};
