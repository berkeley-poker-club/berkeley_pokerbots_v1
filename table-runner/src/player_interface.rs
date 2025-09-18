use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use poker_utils::{
    Action, Card, HandStrength, SeatIndex, Street, SeatStatus
};
use poker_utils::game_state::PlayerId;

#[async_trait]
pub trait Player: Send + Sync {
    async fn notify_event(&self, event: &PublicEvent) -> Result<(), PlayerError>;
    async fn request_action(
        &self,
        context: &DecisionContext,
        legal: &LegalActions,
        timeout_ms: u64,
    ) -> Result<Action, PlayerError>;
    fn player_id(&self) -> PlayerId;
}

struct BotProcess {
    player_id: PlayerId,
    process: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    // timeout_handler: TimeoutManager,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionContext {
    pub hand_id: u64,
    pub street: Street,
    pub my_seat: SeatIndex,
    pub button_seat: SeatIndex,
    pub my_hole_cards: [Card; 2],
    pub board_cards: Vec<Card>,
    pub pot_size: i64,
    pub to_call: i64,
    pub stacks: Vec<i64>,
    pub committed_this_street: Vec<i64>,
    pub seat_statuses: Vec<SeatStatus>,
    pub action_history: Vec<(SeatIndex, Action)>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PublicEvent {
    HandStarted {
        hand_id: u64,
        button_seat: SeatIndex,
        stacks: Vec<i64>,
    },
    BlindsPosted {
        small_seat: SeatIndex,
        big_seat: SeatIndex,
        small: i64,
        big: i64,
    },
    CardsDealt {
        street: Street,
        cards: Vec<Card>,
    },
    ActionTaken {
        seat: SeatIndex,
        action: Action,
    },
    ShowdownRevealed {
        seat: SeatIndex,
        hole_cards: [Card; 2],
        hand_strength: HandStrength,
    },
    PotAwarded {
        pot_amount: i64,
        winners: Vec<PlayerId>,
    },
    HandEnded {
        hand_id: u64,
    },
    PlayerEliminated {
        player_id: PlayerId,
    },
}

#[derive(Debug)]
pub enum PlayerError {
    Timeout,
    CommunicationFailed,
    InvalidResponse,
    Disconnected,
}

impl std::fmt::Display for PlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            PlayerError::Timeout => write!(f, "PlayerError::Timeout"),
            PlayerError::CommunicationFailed => write!(f, "PlayerError::CommunicationFailed"),
            PlayerError::InvalidResponse => write!(f, "PlayerError::InvalidResponse"),
            PlayerError::Disconnected => write!(f, "PlayerError::Disconnected"),
        }
    }
}

impl std::error::Error for PlayerError {}