//! `table-runner`: the async layer between the pure engine (`poker-utils`) and bots.
//!
//! * [`Player`] — the endpoint abstraction (in-process or subprocess).
//! * [`ProcessBot`] — JSON-lines subprocess implementation of the bot protocol.
//! * [`LocalBot`] + [`Strategy`] — in-process bots (fold / call / raise / random).
//! * [`play_hand`] — drives one hand with deadlines and auto-actions.
//! * [`spawn_table`] — a table task that plays hands and talks to the tournament director.

pub mod driver;
pub mod local_bots;
pub mod player;
pub mod process_bot;
pub mod protocol;
pub mod smoke;
pub mod table;

pub use driver::{broadcast, broadcast_batch, play_hand};
pub use local_bots::{
    strategy_by_name, Behaviour, CallStrategy, FoldStrategy, LocalBot, RaiseStrategy,
    RandomStrategy, Strategy,
};
pub use player::{Player, PlayerError, SharedPlayer};
pub use process_bot::{raise_fd_limit, ProcessBot, SpawnError, SpawnOptions};
pub use protocol::{BotMessage, EngineMessage, PROTOCOL_VERSION};
pub use smoke::{smoke_test, SmokeFailure, SmokeReport};
pub use table::{
    mix, spawn_table, TableCommand, TableConfig, TableEvent, TableHandle, TableId, EVENT_QUEUE,
};
