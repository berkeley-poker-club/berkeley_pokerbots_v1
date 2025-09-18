use serde::{Deserialize, Serialize};

pub type SeatIndex = u8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    Fold,
    Check,
    Call,
    Bet(i64),
    Raise(i64),
    AllIn,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegalActions {
    pub can_fold: bool,
    pub can_check: bool,
    pub can_call: bool,
    pub to_call: i64,
    pub min_bet: Option<i64>,
    pub min_raise_to: Option<i64>,
    pub max_bet_or_raise: i64,
    pub is_all_in_situation: bool,
}

#[derive(Debug)]
pub enum ActionError {
    NotPlayersTurn,
    InsufficientChips,
    BelowMinimum,
    InvalidAction,
    GameNotInProgress,
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ActionError::NotPlayersTurn => write!(f, "ActionError::NotPlayersTurn"),
            ActionError::InsufficientChips => write!(f, "ActionError::InsufficientChips"),
            ActionError::BelowMinimum => write!(f, "ActionError::BelowMinimum"),
            ActionError::InvalidAction => write!(f, "ActionError::InvalidAction"),
            ActionError::GameNotInProgress => write!(f, "ActionError::GameNotInProgress"),
        }
    }
}

impl std::error::Error for ActionError {}

pub type ActionResult = Result<ValidAction, ActionError>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidAction {
    pub action: Action,
    pub amount: i64,
}

impl ValidAction {
    pub fn new(action: Action, amount: i64) -> Self {
        ValidAction { action, amount }
    }
}