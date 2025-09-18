pub mod runner;
pub mod config;
pub mod table_manager;
pub mod player_manager;

// Re-export key types for easier access
pub use config::TournamentConfig;
pub use table_manager::{Table, TableFactory};
pub use player_manager::{PlayerId, PlayerRegistry};
pub use runner::{TournamentDirector, TournamentState, Placements, SeriesResult};

// Re-export from table-runner for convenience
pub use table_runner::{TableId, TableHandle, TableEvent, TableCommand};