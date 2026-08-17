//! `poker-utils`: cards, hand evaluation, and a pure No-Limit Hold'em hand engine.
//!
//! Nothing in this crate performs I/O or knows about players; see `table-runner` for the async
//! driver that connects the engine to bots, and `tournament-core` for tournament orchestration.

pub mod action;
pub mod cards;
pub mod config;
pub mod events;
pub mod hand;
pub mod hands;

pub use action::{Action, ActionError, LegalActions, PlayerId, SeatIndex};
pub use cards::{card, parse_cards, Card, CardParseError, Deck, Rank, Suit};
pub use config::{BlindSchedule, LevelAdvance, LevelSpec, TournamentConfig};
pub use events::{
    DecisionContext, HistoryEntry, PotAward, PublicEvent, SeatStatus, SeatView, Street,
};
pub use hand::{Hand, HandError, HandParams, HandResult, HandRules, SeatState};
pub use hands::{evaluate_cards, evaluate_hand, HandCategory, HandStrength};
