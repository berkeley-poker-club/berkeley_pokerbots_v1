//! Public events broadcast to every player at a table, plus the per-decision context.
//!
//! These types are the wire format of the bot protocol (see `docs/BOT_PROTOCOL.md`).

use crate::action::{Action, PlayerId, SeatIndex};
use crate::cards::Card;
use crate::hands::HandStrength;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Street {
    Preflop,
    Flop,
    Turn,
    River,
}

impl Street {
    pub fn next(self) -> Option<Street> {
        match self {
            Street::Preflop => Some(Street::Flop),
            Street::Flop => Some(Street::Turn),
            Street::Turn => Some(Street::River),
            Street::River => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SeatStatus {
    Empty,
    Active,
    Folded,
    AllIn,
}

/// Snapshot of one seat as shown to players.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatView {
    pub seat: SeatIndex,
    pub player_id: Option<PlayerId>,
    pub stack: i64,
    pub committed_street: i64,
    pub committed_total: i64,
    pub status: SeatStatus,
}

/// One entry of the public action history for the current hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub street: Street,
    pub seat: SeatIndex,
    pub action: Action,
    /// Chips added to the pot by this action.
    pub amount: i64,
    pub all_in: bool,
}

/// A pot awarded at the end of a hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PotAward {
    pub pot_index: usize,
    pub amount: i64,
    pub eligible_seats: Vec<SeatIndex>,
    /// (seat, chips received)
    pub winners: Vec<(SeatIndex, i64)>,
}

/// Events published by the engine. Every event carries `kind` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PublicEvent {
    // ---- tournament / table level ----
    /// Sent once to a bot when it is spawned for a tournament.
    TournamentStarted {
        tournament_id: String,
        player_id: PlayerId,
        num_players: usize,
        starting_stack: i64,
    },
    /// Sent when a player is seated at a table (start of tournament or after a table break).
    Seated {
        table_id: u64,
        seat: SeatIndex,
        seats: Vec<SeatView>,
    },
    /// Blind level applied at this table (effective for subsequent hands).
    BlindLevel {
        level: u32,
        small_blind: i64,
        big_blind: i64,
        ante: i64,
    },
    /// A player at this table has run out of chips and left the table.
    PlayerBusted {
        seat: SeatIndex,
        player_id: PlayerId,
    },
    /// A player left this table because the table is breaking (they will be reseated elsewhere).
    PlayerMoved {
        seat: SeatIndex,
        player_id: PlayerId,
    },
    /// The tournament is over. Sent to every bot still alive; the process may then exit.
    TournamentEnded {
        tournament_id: String,
        winner: Option<PlayerId>,
        /// This recipient's placement (1 = winner).
        your_placement: Option<usize>,
    },

    // ---- hand level ----
    HandStarted {
        hand_id: u64,
        table_id: u64,
        button: SeatIndex,
        small_blind: i64,
        big_blind: i64,
        ante: i64,
        seats: Vec<SeatView>,
    },
    AntePosted {
        seat: SeatIndex,
        amount: i64,
        all_in: bool,
    },
    BlindPosted {
        seat: SeatIndex,
        amount: i64,
        /// `true` for the big blind, `false` for the small blind.
        big: bool,
        all_in: bool,
    },
    /// **Private**: only delivered to the seat that owns the cards.
    HoleCards {
        seat: SeatIndex,
        cards: [Card; 2],
    },
    ActionTaken {
        seat: SeatIndex,
        street: Street,
        action: Action,
        /// Chips added to the pot by this action.
        amount: i64,
        all_in: bool,
        /// Total the seat has committed on this street after acting.
        committed_street: i64,
    },
    /// The player timed out / crashed / acted illegally and the engine substituted `substituted`.
    ActionSubstituted {
        seat: SeatIndex,
        reason: String,
        substituted: Action,
    },
    BoardDealt {
        street: Street,
        cards: Vec<Card>,
        board: Vec<Card>,
        pot: i64,
    },
    ShowdownRevealed {
        seat: SeatIndex,
        cards: [Card; 2],
        strength: HandStrength,
        description: String,
    },
    PotAwarded(PotAward),
    HandEnded {
        hand_id: u64,
        table_id: u64,
        seats: Vec<SeatView>,
    },
}

/// Everything a bot needs to make a decision. Amounts are absolute chip counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionContext {
    pub hand_id: u64,
    pub table_id: u64,
    pub street: Street,
    pub my_seat: SeatIndex,
    pub my_player_id: PlayerId,
    pub button: SeatIndex,
    pub small_blind: i64,
    pub big_blind: i64,
    pub ante: i64,
    pub my_hole_cards: [Card; 2],
    pub board: Vec<Card>,
    /// Total chips in the middle (all streets, all seats).
    pub pot: i64,
    /// Current bet level on this street (max committed by any seat).
    pub bet_level: i64,
    /// Chips this seat must add to call (0 if checking is possible).
    pub to_call: i64,
    /// Size of the last full raise (the minimum raise increment).
    pub min_raise: i64,
    pub my_stack: i64,
    pub my_committed_street: i64,
    pub my_committed_total: i64,
    pub seats: Vec<SeatView>,
    pub history: Vec<HistoryEntry>,
}
