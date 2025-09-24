use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use poker_utils::{
    GameState, GameRules, Street, SeatIndex, SeatStatus, Action, ValidAction,
    ActionValidator, GameEvent, evaluate_hand, Card
};
use poker_utils::game_state::PlayerId;
use crate::player_interface::{Player, DecisionContext, LegalActions, PublicEvent, PlayerError};

pub struct GameRunner {
    pub table_id: u64,
    pub state: GameState,
    players: HashMap<SeatIndex, Arc<Mutex<dyn Player>>>,
    event_sink: mpsc::UnboundedSender<GameEvent>,
}

impl GameRunner {
    pub fn new(
        table_id: u64,
        rules: GameRules,
        event_sink: mpsc::UnboundedSender<GameEvent>,
        seed: u64,
    ) -> Self {
        GameRunner {
            table_id,
            state: GameState::new(0, 0, rules, seed),
            players: HashMap::new(),
            event_sink,
        }
    }

    pub async fn seat_player(&mut self, seat: SeatIndex, player: Arc<Mutex<dyn Player>>, stack: i64) -> Result<(), String> {
        if seat >= self.state.rules.max_seats as SeatIndex {
            return Err("seat out of bounds".to_string());
        }

        let seat_state = &mut self.state.seats[seat as usize];
        if seat_state.player_id.is_some() {
            return Err("seat occupied".to_string());
        }

        let player_id = {
            let player_guard = player.lock().await;
            player_guard.player_id()
        };
        *seat_state = poker_utils::SeatState::new_player(player_id, stack);
        self.players.insert(seat, player);

        Ok(())
    }

    pub async fn run_hand(&mut self, hand_id: u64, button_seat: SeatIndex) -> Result<(), GameError> {
        self.state.hand_id = hand_id;
        self.state.button_position = button_seat;
        self.state.street = Street::Preflop;
        self.state.deck.reset();
        self.state.board.clear();

        for seat in &mut self.state.seats {
            if seat.is_active() {
                seat.committed_this_street = 0;
                seat.total_committed = 0;
                seat.hole_cards = None;
            }
        }

        self.broadcast_event(PublicEvent::HandStarted {
            hand_id,
            button_seat,
            stacks: self.state.seats.iter().map(|s| s.stack).collect(),
        }).await;

        self.post_blinds().await?;
        self.deal_hole_cards().await?;

        for street in [Street::Preflop, Street::Flop, Street::Turn, Street::River] {
            if self.should_skip_street() {
                break;
            }

            if street != Street::Preflop {
                self.deal_community_cards(street).await?;
            }

            self.run_betting_round(street).await?;

            if self.count_active_players() <= 1 {
                self.award_uncontested_pot().await?;
                return Ok(());
            }
        }

        self.run_showdown().await?;
        Ok(())
    }

    async fn post_blinds(&mut self) -> Result<(), GameError> {
        let active_seats = self.get_active_seats();
        if active_seats.len() < 2 {
            return Err(GameError::InsufficientPlayers);
        }

        let button_pos = active_seats.iter().position(|&s| s == self.state.button_position)
            .unwrap_or(0);

        let small_blind_seat = active_seats[(button_pos + 1) % active_seats.len()];
        let big_blind_seat = active_seats[(button_pos + 2) % active_seats.len()];

        let sb_amount = std::cmp::min(self.state.rules.small_blind, self.state.seats[small_blind_seat as usize].stack);
        self.state.seats[small_blind_seat as usize].stack -= sb_amount;
        self.state.seats[small_blind_seat as usize].committed_this_street += sb_amount;
        self.state.pot_manager.commit_chips(self.state.seats[small_blind_seat as usize].player_id.unwrap(), sb_amount);

        let bb_amount = std::cmp::min(self.state.rules.big_blind, self.state.seats[big_blind_seat as usize].stack);
        self.state.seats[big_blind_seat as usize].stack -= bb_amount;
        self.state.seats[big_blind_seat as usize].committed_this_street += bb_amount;
        self.state.pot_manager.commit_chips(self.state.seats[big_blind_seat as usize].player_id.unwrap(), bb_amount);

        self.state.betting_state.to_call = bb_amount;
        self.state.betting_state.min_raise = self.state.rules.big_blind;

        self.broadcast_event(PublicEvent::BlindsPosted {
            small_seat: small_blind_seat,
            big_seat: big_blind_seat,
            small: sb_amount,
            big: bb_amount,
        }).await;

        Ok(())
    }

    async fn deal_hole_cards(&mut self) -> Result<(), GameError> {
        for seat in &mut self.state.seats {
            if seat.is_active() {
                let card1 = self.state.deck.deal().ok_or(GameError::DeckError)?;
                let card2 = self.state.deck.deal().ok_or(GameError::DeckError)?;
                seat.hole_cards = Some([card1, card2]);
            }
        }

        self.broadcast_event(PublicEvent::CardsDealt {
            street: Street::Preflop,
            cards: vec![],
        }).await;

        Ok(())
    }

    async fn deal_community_cards(&mut self, street: Street) -> Result<(), GameError> {
        let cards_to_deal = match street {
            Street::Flop => 3,
            Street::Turn | Street::River => 1,
            _ => 0,
        };

        let mut new_cards = Vec::new();
        for _ in 0..cards_to_deal {
            let card = self.state.deck.deal().ok_or(GameError::DeckError)?;
            self.state.board.push(card);
            new_cards.push(card);
        }

        self.broadcast_event(PublicEvent::CardsDealt {
            street,
            cards: new_cards,
        }).await;

        Ok(())
    }

    async fn run_betting_round(&mut self, street: Street) -> Result<(), GameError> {
        self.state.street = street;
        self.reset_betting_round();

        while !self.state.is_betting_complete() {
            let actor_seat = self.state.current_actor.ok_or(GameError::NoActor)?;

            if !self.state.seats[actor_seat as usize].can_act() {
                self.advance_actor();
                continue;
            }

            let context = self.build_decision_context(actor_seat);
            let legal_actions = self.get_legal_actions(actor_seat);

            let action_result = self.request_player_action(actor_seat, &context, &legal_actions).await;

            match action_result {
                Ok(action) => {
                    match ActionValidator::validate_action(&self.state, actor_seat, &action) {
                        Ok(valid_action) => {
                            self.apply_action(actor_seat, &valid_action).await;
                            self.advance_actor();
                        }
                        Err(_) => {
                            // autofold on invalid action
                            let fold_action = ValidAction::new(Action::Fold, 0);
                            self.apply_action(actor_seat, &fold_action).await;
                            self.advance_actor();
                        }
                    }
                }
                Err(_) => {
                    // autofold on timeout/error
                    let fold_action = ValidAction::new(Action::Fold, 0);
                    self.apply_action(actor_seat, &fold_action).await;
                    self.advance_actor();
                }
            }
        }

        self.move_committed_to_pots();
        Ok(())
    }

    async fn run_showdown(&mut self) -> Result<(), GameError> {
        let active_players = self.get_active_seats();
        let mut winners = Vec::new();

        for &seat in &active_players {
            let seat_state = &self.state.seats[seat as usize];
            if let Some(hole_cards) = seat_state.hole_cards {
                let hand_strength = evaluate_hand(&hole_cards, &self.state.board);

                if let Some(player_id) = seat_state.player_id {
                    winners.push((player_id, hand_strength));

                    self.broadcast_event(PublicEvent::ShowdownRevealed {
                        seat,
                        hole_cards,
                        hand_strength,
                    }).await;
                }
            }
        }

        let pot_events = self.state.pot_manager.distribute_to_winners(&winners);
        for event in pot_events {
            match event {
                poker_utils::PotEvent::PotAwarded { player, amount } => {
                    self.broadcast_event(PublicEvent::PotAwarded {
                        pot_amount: amount,
                        winners: vec![player],
                    }).await;

                    if let Some(seat) = self.find_player_seat(player) {
                        self.state.seats[seat as usize].stack += amount;
                    }
                }
                _ => {}
            }
        }

        self.broadcast_event(PublicEvent::HandEnded {
            hand_id: self.state.hand_id,
        }).await;

        Ok(())
    }

    fn reset_betting_round(&mut self) {
        self.state.betting_state.reset_for_street();

        let active_seats = self.get_active_seats();
        for seat in active_seats {
            if self.state.seats[seat as usize].can_act() {
                self.state.betting_state.can_act.insert(seat);
            }
        }

        self.state.current_actor = self.get_first_actor();
    }

    fn get_first_actor(&self) -> Option<SeatIndex> {
        let active_seats = self.get_active_seats();
        if active_seats.is_empty() {
            return None;
        }

        let button_pos = active_seats.iter().position(|&s| s == self.state.button_position)
            .unwrap_or(0);

        let first_pos = if self.state.street == Street::Preflop {
            (button_pos + 3) % active_seats.len() // preflop -> utg
        } else {
            (button_pos + 1) % active_seats.len() // postflop -> sb
        };

        Some(active_seats[first_pos])
    }

    fn advance_actor(&mut self) {
        if let Some(current) = self.state.current_actor {
            self.state.betting_state.can_act.remove(&current);

            if !self.state.betting_state.can_act.is_empty() {
                self.state.current_actor = self.state.betting_state.can_act.iter().next().copied();
            } else {
                self.state.current_actor = None;
            }
        }
    }

    async fn apply_action(&mut self, seat: SeatIndex, valid_action: &ValidAction) {
        let seat_state = &mut self.state.seats[seat as usize];

        match &valid_action.action {
            Action::Fold => {
                seat_state.status = SeatStatus::Folded;
            }
            Action::Check => {
                // no chips
            }
            Action::Call | Action::Bet(_) | Action::Raise(_) | Action::AllIn => {
                seat_state.stack -= valid_action.amount;
                seat_state.committed_this_street += valid_action.amount;
                seat_state.total_committed += valid_action.amount;

                if let Some(player_id) = seat_state.player_id {
                    self.state.pot_manager.commit_chips(player_id, valid_action.amount);
                }

                if seat_state.stack == 0 {
                    seat_state.status = SeatStatus::AllIn;
                }

                if matches!(valid_action.action, Action::Bet(_) | Action::Raise(_)) {
                    self.state.betting_state.to_call = seat_state.committed_this_street;
                    self.state.betting_state.last_raiser = Some(seat);

                    let active_seats = self.get_active_seats();
                    for active_seat in active_seats {
                        if active_seat != seat && self.state.seats[active_seat as usize].can_act() {
                            self.state.betting_state.can_act.insert(active_seat);
                        }
                    }
                }
            }
        }

        self.broadcast_event(PublicEvent::ActionTaken {
            seat,
            action: valid_action.action.clone(),
        }).await;
    }

    fn move_committed_to_pots(&mut self) {
        for seat in &mut self.state.seats {
            if seat.committed_this_street > 0 {
                seat.committed_this_street = 0;
            }
        }
    }

    fn build_decision_context(&self, seat: SeatIndex) -> DecisionContext {
        let seat_state = &self.state.seats[seat as usize];

        DecisionContext {
            hand_id: self.state.hand_id,
            street: self.state.street.clone(),
            my_seat: seat,
            button_seat: self.state.button_position,
            my_hole_cards: seat_state.hole_cards.unwrap_or([Card::new(poker_utils::Rank::Two, poker_utils::Suit::Clubs); 2]),
            board_cards: self.state.board.clone(),
            pot_size: self.state.pot_manager.total_pot_size(),
            to_call: std::cmp::max(0, self.state.betting_state.to_call - seat_state.committed_this_street),
            stacks: self.state.seats.iter().map(|s| s.stack).collect(),
            committed_this_street: self.state.seats.iter().map(|s| s.committed_this_street).collect(),
            seat_statuses: self.state.seats.iter().map(|s| s.status.clone()).collect(),
            action_history: vec![], // TODO: track action history maybe?
        }
    }

    fn get_legal_actions(&self, seat: SeatIndex) -> LegalActions {
        let seat_state = &self.state.seats[seat as usize];
        let to_call = std::cmp::max(0, self.state.betting_state.to_call - seat_state.committed_this_street);

        LegalActions {
            can_fold: true,
            can_check: to_call == 0,
            can_call: to_call > 0 && to_call <= seat_state.stack,
            to_call,
            min_bet: if to_call == 0 { Some(self.state.rules.big_blind) } else { None },
            min_raise_to: if to_call > 0 { Some(self.state.betting_state.to_call + self.state.betting_state.min_raise) } else { None },
            max_bet_or_raise: seat_state.stack,
            is_all_in_situation: seat_state.stack <= to_call,
        }
    }

    async fn request_player_action(
        &self,
        seat: SeatIndex,
        context: &DecisionContext,
        legal: &LegalActions,
    ) -> Result<Action, PlayerError> {
        if let Some(player) = self.players.get(&seat) {
            let player_guard = player.lock().await;
            player_guard.request_action(context, legal, 1000).await
        } else {
            Err(PlayerError::CommunicationFailed)
        }
    }

    async fn broadcast_event(&self, event: PublicEvent) {
        for player in self.players.values() {
            let player_guard = player.lock().await;
            let _ = player_guard.notify_event(&event).await;
        }
    }

    pub fn get_active_seats(&self) -> Vec<SeatIndex> {
        self.state.seats.iter()
            .enumerate()
            .filter(|(_, seat)| seat.is_active())
            .map(|(i, _)| i as SeatIndex)
            .collect()
    }

    fn count_active_players(&self) -> usize {
        self.state.seats.iter()
            .filter(|seat| matches!(seat.status, SeatStatus::Active | SeatStatus::AllIn))
            .count()
    }

    fn should_skip_street(&self) -> bool {
        self.count_active_players() <= 1
    }

    async fn award_uncontested_pot(&mut self) -> Result<(), GameError> {
        let active_seats = self.get_active_seats();
        if let Some(&winner_seat) = active_seats.first() {
            let pot_amount = self.state.pot_manager.total_pot_size();
            self.state.seats[winner_seat as usize].stack += pot_amount;

            if let Some(player_id) = self.state.seats[winner_seat as usize].player_id {
                self.broadcast_event(PublicEvent::PotAwarded {
                    pot_amount,
                    winners: vec![player_id],
                }).await;
            }
        }

        Ok(())
    }

    fn find_player_seat(&self, player_id: PlayerId) -> Option<SeatIndex> {
        self.state.seats.iter()
            .enumerate()
            .find(|(_, seat)| seat.player_id == Some(player_id))
            .map(|(i, _)| i as SeatIndex)
    }

    pub fn remove_player_from_seat(&mut self, seat_idx: SeatIndex) -> Option<Arc<Mutex<dyn Player>>> {
        self.players.remove(&seat_idx)
    }
}

#[derive(Debug)]
pub enum GameError {
    InsufficientPlayers,
    DeckError,
    NoActor,
    InvalidSeat,
}

impl std::fmt::Display for GameError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            GameError::InsufficientPlayers => write!(f, "GameError::InsufficientPlayers"),
            GameError::DeckError => write!(f, "GameError::DeckError"),
            GameError::NoActor => write!(f, "GameError::NoActor"),
            GameError::InvalidSeat => write!(f, "GameError::InvalidSeat"),
        }
    }
}

impl std::error::Error for GameError {}