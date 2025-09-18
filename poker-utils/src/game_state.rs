use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use crate::{Card, Deck, HandStrength, Action, ValidAction, ActionError, ActionResult, SeatIndex, PotManager, PotEvent};

pub type PlayerId = usize;

#[derive(Clone, Debug)]
pub struct GameState {
    pub hand_id: u64,
    pub street: Street,
    pub deck: Deck,
    pub board: Vec<Card>,
    pub seats: Vec<SeatState>,
    pub pot_manager: PotManager,
    pub betting_state: BettingState,
    pub current_actor: Option<SeatIndex>,
    pub button_position: SeatIndex,
    pub rules: GameRules,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Street {
    Preflop,
    Flop,
    Turn,
    River,
    Showdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeatState {
    pub player_id: Option<PlayerId>,
    pub stack: i64,
    pub committed_this_street: i64,
    pub total_committed: i64,
    pub hole_cards: Option<[Card; 2]>,
    pub status: SeatStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SeatStatus {
    Empty,
    Active,
    Folded,
    AllIn,
    SittingOut,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BettingState {
    pub to_call: i64,
    pub min_raise: i64,
    pub last_raiser: Option<SeatIndex>,
    pub can_act: HashSet<SeatIndex>,
    pub action_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameRules {
    pub small_blind: i64,
    pub big_blind: i64,
    pub ante: i64,
    pub max_seats: usize,
}

impl GameState {
    pub fn new(hand_id: u64, button_position: SeatIndex, rules: GameRules, seed: u64) -> Self {
        GameState {
            hand_id,
            street: Street::Preflop,
            deck: Deck::new(seed),
            board: Vec::new(),
            seats: vec![SeatState::empty(); rules.max_seats],
            pot_manager: PotManager::new(),
            betting_state: BettingState::new(),
            current_actor: None,
            button_position,
            rules,
        }
    }

    pub fn is_betting_complete(&self) -> bool {
        self.betting_state.can_act.is_empty()
    }

    pub fn active_players(&self) -> Vec<SeatIndex> {
        self.seats.iter()
            .enumerate()
            .filter(|(_, seat)| seat.status == SeatStatus::Active || seat.status == SeatStatus::AllIn)
            .map(|(i, _)| i as SeatIndex)
            .collect()
    }

    pub fn get_next_actor(&self, current: SeatIndex) -> Option<SeatIndex> {
        let active = self.active_players();
        if active.is_empty() {
            return None;
        }

        let current_pos = active.iter().position(|&seat| seat == current)?;
        let next_pos = (current_pos + 1) % active.len();
        Some(active[next_pos])
    }
}

impl SeatState {
    pub fn empty() -> Self {
        SeatState {
            player_id: None,
            stack: 0,
            committed_this_street: 0,
            total_committed: 0,
            hole_cards: None,
            status: SeatStatus::Empty,
        }
    }

    pub fn new_player(player_id: PlayerId, stack: i64) -> Self {
        SeatState {
            player_id: Some(player_id),
            stack,
            committed_this_street: 0,
            total_committed: 0,
            hole_cards: None,
            status: SeatStatus::Active,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, SeatStatus::Active | SeatStatus::AllIn)
    }

    pub fn can_act(&self) -> bool {
        self.status == SeatStatus::Active && self.stack > 0
    }
}

impl BettingState {
    pub fn new() -> Self {
        BettingState {
            to_call: 0,
            min_raise: 0,
            last_raiser: None,
            can_act: HashSet::new(),
            action_count: 0,
        }
    }

    pub fn reset_for_street(&mut self) {
        self.to_call = 0;
        self.action_count = 0;
        self.can_act.clear();
    }
}

impl Default for BettingState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GameEvent {
    HandStarted {
        hand_id: u64,
        button_seat: SeatIndex,
        stacks: Vec<i64>,
    },
    BlindsPosted {
        small_seat: SeatIndex,
        big_seat: SeatIndex,
        small_amount: i64,
        big_amount: i64,
    },
    CardsDealt {
        street: Street,
        cards: Vec<Card>,
    },
    ActionTaken {
        seat: SeatIndex,
        action: ValidAction,
    },
    PotEvent(PotEvent),
    ShowdownRevealed {
        seat: SeatIndex,
        hole_cards: [Card; 2],
        hand_strength: HandStrength,
    },
    HandEnded {
        hand_id: u64,
        winners: Vec<PlayerId>,
    },
    PlayerEliminated {
        player_id: PlayerId,
    },
}

pub struct ActionValidator;

impl ActionValidator {
    pub fn validate_action(
        state: &GameState,
        seat: SeatIndex,
        action: &Action,
    ) -> ActionResult {
        if Some(seat) != state.current_actor {
            return Err(ActionError::NotPlayersTurn);
        }

        let seat_state = &state.seats[seat as usize];
        if !seat_state.can_act() {
            return Err(ActionError::InvalidAction);
        }

        let to_call = std::cmp::max(0, state.betting_state.to_call - seat_state.committed_this_street);

        match action {
            Action::Fold => Ok(ValidAction::new(Action::Fold, 0)),

            Action::Check => {
                if to_call > 0 {
                    Err(ActionError::InvalidAction)
                } else {
                    Ok(ValidAction::new(Action::Check, 0))
                }
            }

            Action::Call => {
                if to_call == 0 {
                    Err(ActionError::InvalidAction)
                } else {
                    let amount = std::cmp::min(to_call, seat_state.stack);
                    Ok(ValidAction::new(Action::Call, amount))
                }
            }

            Action::Bet(amount) => {
                if to_call > 0 || *amount <= 0 {
                    return Err(ActionError::InvalidAction);
                }

                if *amount > seat_state.stack {
                    return Err(ActionError::InsufficientChips);
                }

                let min_bet = state.rules.big_blind;
                if *amount < min_bet && *amount < seat_state.stack {
                    return Err(ActionError::BelowMinimum);
                }

                Ok(ValidAction::new(Action::Bet(*amount), *amount))
            }

            Action::Raise(raise_to) => {
                if to_call == 0 {
                    return Err(ActionError::InvalidAction);
                }

                if *raise_to > seat_state.stack {
                    return Err(ActionError::InsufficientChips);
                }

                let min_raise_to = state.betting_state.to_call + state.betting_state.min_raise;
                if *raise_to < min_raise_to && *raise_to < seat_state.stack {
                    return Err(ActionError::BelowMinimum);
                }

                Ok(ValidAction::new(Action::Raise(*raise_to), *raise_to - seat_state.committed_this_street))
            }

            Action::AllIn => {
                if seat_state.stack <= 0 {
                    return Err(ActionError::InsufficientChips);
                }
                Ok(ValidAction::new(Action::AllIn, seat_state.stack))
            }
        }
    }
}