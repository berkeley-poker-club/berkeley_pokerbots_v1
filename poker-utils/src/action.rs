//! Player actions and the legal-action summary handed to bots.

use serde::{Deserialize, Serialize};

pub type SeatIndex = u8;
pub type PlayerId = u32;

/// An action chosen by a player. Bet/raise amounts are **"to" amounts**: the total number of chips
/// the player will have committed on the current street after the action.
///
/// JSON form (bot protocol): `{"kind":"Fold"}`, `{"kind":"RaiseTo","amount":200}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Action {
    Fold,
    Check,
    Call,
    BetTo { amount: i64 },
    RaiseTo { amount: i64 },
    AllIn,
}

impl Action {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Action::Fold => "Fold",
            Action::Check => "Check",
            Action::Call => "Call",
            Action::BetTo { .. } => "BetTo",
            Action::RaiseTo { .. } => "RaiseTo",
            Action::AllIn => "AllIn",
        }
    }
}

/// The set of legal actions for the acting player, with all the numbers needed to size them.
///
/// * `call_amount` — chips the player must add to call (may be less than `to_call` when calling
///   all-in for less).
/// * `min_bet_to`/`max_bet_to` — valid `BetTo` totals when no bet has been made this street.
/// * `min_raise_to`/`max_raise_to` — valid `RaiseTo` totals when facing a bet.
/// * `all_in_to` — the "to" amount corresponding to shoving the whole stack.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalActions {
    pub can_fold: bool,
    pub can_check: bool,
    pub can_call: bool,
    pub can_bet: bool,
    pub can_raise: bool,
    pub can_all_in: bool,
    pub to_call: i64,
    pub call_amount: i64,
    pub min_bet_to: i64,
    pub max_bet_to: i64,
    pub min_raise_to: i64,
    pub max_raise_to: i64,
    pub all_in_to: i64,
}

impl LegalActions {
    /// The action the engine substitutes when a player times out, crashes or acts illegally:
    /// check if possible, otherwise fold.
    pub fn auto_action(&self) -> Action {
        if self.can_check {
            Action::Check
        } else {
            Action::Fold
        }
    }

    /// Whether `action` is one of the legal actions (with a valid amount).
    pub fn allows(&self, action: &Action) -> bool {
        match action {
            Action::Fold => self.can_fold,
            Action::Check => self.can_check,
            Action::Call => self.can_call,
            Action::BetTo { amount } => {
                self.can_bet && *amount >= self.min_bet_to && *amount <= self.max_bet_to
            }
            Action::RaiseTo { amount } => {
                self.can_raise && *amount >= self.min_raise_to && *amount <= self.max_raise_to
            }
            Action::AllIn => self.can_all_in,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum ActionError {
    #[error("it is not this seat's turn to act")]
    NotYourTurn,
    #[error("hand is already complete")]
    HandComplete,
    #[error("cannot check when facing a bet")]
    CannotCheck,
    #[error("nothing to call")]
    NothingToCall,
    #[error("cannot bet: there is already a bet this street (use RaiseTo)")]
    CannotBet,
    #[error("cannot raise: no bet to raise (use BetTo) or raising is closed for this seat")]
    CannotRaise,
    #[error("amount {amount} is below the minimum {min}")]
    BelowMinimum { amount: i64, min: i64 },
    #[error("amount {amount} exceeds the maximum {max}")]
    AboveMaximum { amount: i64, max: i64 },
    #[error("seat {0} is not in the hand")]
    SeatNotInHand(SeatIndex),
}
