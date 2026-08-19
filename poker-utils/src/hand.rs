//! A single hand of No-Limit Texas Hold'em as a pure, synchronous state machine.
//!
//! The engine has no notion of players, timeouts or I/O: a driver (see `table-runner`) asks
//! [`Hand::actor`] who is to act, builds a [`DecisionContext`] and [`LegalActions`] for them,
//! obtains an [`Action`] however it likes, and feeds it back through [`Hand::apply`]. Everything
//! observable is emitted as [`PublicEvent`]s which the driver drains with [`Hand::take_events`].
//!
//! Rules implemented (see `SPEC.md`):
//! * antes then blinds; heads-up the button posts the small blind and acts first preflop;
//! * short blinds are posted all-in for less, the big blind still sets the price to call;
//! * bet/raise amounts are "to" amounts; minimum raise is the last full raise increment
//!   (initially the big blind); an all-in raise smaller than a full raise does not reopen the
//!   action for players who already acted since the last full raise (they may only call/fold);
//! * the big blind (and small blind) retain their option even when nobody raises;
//! * once at most one player can still act, remaining streets are dealt without betting;
//! * side pots are built from total contributions (antes included); uncalled chips are returned
//!   through the pot mechanism (the over-bettor is the sole eligible seat for that pot layer);
//! * split pots give odd chips to the first winner clockwise from the button.

use crate::action::{Action, ActionError, LegalActions, PlayerId, SeatIndex};
use crate::cards::{Card, Deck};
use crate::events::{
    DecisionContext, HistoryEntry, PotAward, PublicEvent, SeatStatus, SeatView, Street,
};
use crate::hands::{evaluate_hand, HandStrength};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandRules {
    pub small_blind: i64,
    pub big_blind: i64,
    pub ante: i64,
}

#[derive(Clone, Debug)]
pub struct SeatState {
    pub player_id: Option<PlayerId>,
    pub stack: i64,
    pub committed_street: i64,
    pub committed_total: i64,
    pub hole_cards: Option<[Card; 2]>,
    pub status: SeatStatus,
    /// The value of `full_raises` when this seat last acted on the current street.
    /// `None` means the seat has not yet acted this street (posting a blind is not acting).
    acted_at: Option<u32>,
    dealt_in: bool,
}

impl SeatState {
    fn empty() -> Self {
        SeatState {
            player_id: None,
            stack: 0,
            committed_street: 0,
            committed_total: 0,
            hole_cards: None,
            status: SeatStatus::Empty,
            acted_at: None,
            dealt_in: false,
        }
    }

    pub fn view(&self, seat: SeatIndex) -> SeatView {
        SeatView {
            seat,
            player_id: self.player_id,
            stack: self.stack,
            committed_street: self.committed_street,
            committed_total: self.committed_total,
            status: self.status,
        }
    }

    fn can_act(&self) -> bool {
        self.status == SeatStatus::Active && self.stack > 0
    }

    fn contending(&self) -> bool {
        matches!(self.status, SeatStatus::Active | SeatStatus::AllIn)
    }
}

/// Parameters to start a hand.
#[derive(Clone, Debug)]
pub struct HandParams {
    pub hand_id: u64,
    pub table_id: u64,
    pub rules: HandRules,
    pub button: SeatIndex,
    /// One entry per physical seat: `Some((player_id, stack))` for occupied seats. Seats with a
    /// zero stack are treated as empty for this hand.
    pub seats: Vec<Option<(PlayerId, i64)>>,
    pub deck: Deck,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandError {
    #[error("a hand needs at least two players with chips (got {0})")]
    NotEnoughPlayers(usize),
    #[error("button seat {0} is not an occupied seat with chips")]
    ButtonNotDealtIn(SeatIndex),
    #[error("deck has {have} cards but {need} are required")]
    DeckTooSmall { have: usize, need: usize },
    #[error("invalid blinds: small={small_blind} big={big_blind} ante={ante}")]
    InvalidBlinds {
        small_blind: i64,
        big_blind: i64,
        ante: i64,
    },
}

/// Outcome of a completed hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandResult {
    pub hand_id: u64,
    pub seats: Vec<SeatView>,
    /// Every player that was dealt into the hand.
    pub participants: Vec<PlayerId>,
    /// Players who finished the hand with zero chips.
    pub busted: Vec<PlayerId>,
    pub awards: Vec<PotAward>,
    pub went_to_showdown: bool,
    pub board: Vec<Card>,
}

#[derive(Clone, Debug)]
pub struct Hand {
    hand_id: u64,
    table_id: u64,
    rules: HandRules,
    seats: Vec<SeatState>,
    button: SeatIndex,
    street: Street,
    board: Vec<Card>,
    deck: Deck,
    bet_level: i64,
    min_raise: i64,
    full_raises: u32,
    last_aggressor: Option<SeatIndex>,
    actor: Option<SeatIndex>,
    history: Vec<HistoryEntry>,
    events: Vec<PublicEvent>,
    events_cursor: usize,
    result: Option<HandResult>,
}

impl Hand {
    /// Start a hand: posts antes and blinds, deals hole cards and determines the first actor.
    pub fn start(params: HandParams) -> Result<Hand, HandError> {
        let HandParams {
            hand_id,
            table_id,
            rules,
            button,
            seats: seat_inputs,
            deck,
        } = params;

        if rules.big_blind <= 0
            || rules.small_blind < 0
            || rules.ante < 0
            || rules.small_blind > rules.big_blind
        {
            return Err(HandError::InvalidBlinds {
                small_blind: rules.small_blind,
                big_blind: rules.big_blind,
                ante: rules.ante,
            });
        }

        let mut seats: Vec<SeatState> = seat_inputs
            .iter()
            .map(|s| match s {
                Some((pid, stack)) if *stack > 0 => SeatState {
                    player_id: Some(*pid),
                    stack: *stack,
                    status: SeatStatus::Active,
                    dealt_in: true,
                    ..SeatState::empty()
                },
                Some((pid, _)) => SeatState {
                    player_id: Some(*pid),
                    ..SeatState::empty()
                },
                None => SeatState::empty(),
            })
            .collect();

        let dealt: Vec<SeatIndex> = seats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.dealt_in)
            .map(|(i, _)| i as SeatIndex)
            .collect();
        if dealt.len() < 2 {
            return Err(HandError::NotEnoughPlayers(dealt.len()));
        }
        if !dealt.contains(&button) {
            return Err(HandError::ButtonNotDealtIn(button));
        }
        let need = dealt.len() * 2 + 5;
        if deck.remaining() < need {
            return Err(HandError::DeckTooSmall {
                have: deck.remaining(),
                need,
            });
        }

        let mut hand = Hand {
            hand_id,
            table_id,
            rules,
            seats: Vec::new(),
            button,
            street: Street::Preflop,
            board: Vec::new(),
            deck,
            bet_level: 0,
            min_raise: rules.big_blind,
            full_raises: 0,
            last_aggressor: None,
            actor: None,
            history: Vec::new(),
            events: Vec::new(),
            events_cursor: 0,
            result: None,
        };
        std::mem::swap(&mut hand.seats, &mut seats);

        hand.events.push(PublicEvent::HandStarted {
            hand_id,
            table_id,
            button,
            small_blind: rules.small_blind,
            big_blind: rules.big_blind,
            ante: rules.ante,
            seats: hand.seat_views(),
        });

        // Antes (in order starting left of the button).
        if rules.ante > 0 {
            for seat in hand.rotation_from(hand.next_dealt_in(button)) {
                let amount = hand.commit(seat, rules.ante, false);
                let all_in = hand.seats[seat as usize].status == SeatStatus::AllIn;
                hand.events.push(PublicEvent::AntePosted {
                    seat,
                    amount,
                    all_in,
                });
            }
        }

        // Blinds.
        let heads_up = dealt.len() == 2;
        let sb_seat = if heads_up {
            button
        } else {
            hand.next_dealt_in(button)
        };
        let bb_seat = hand.next_dealt_in(sb_seat);
        let sb_amount = hand.commit(sb_seat, rules.small_blind, true);
        hand.events.push(PublicEvent::BlindPosted {
            seat: sb_seat,
            amount: sb_amount,
            big: false,
            all_in: hand.seats[sb_seat as usize].status == SeatStatus::AllIn,
        });
        let bb_amount = hand.commit(bb_seat, rules.big_blind, true);
        hand.events.push(PublicEvent::BlindPosted {
            seat: bb_seat,
            amount: bb_amount,
            big: true,
            all_in: hand.seats[bb_seat as usize].status == SeatStatus::AllIn,
        });
        hand.bet_level = rules.big_blind;
        hand.min_raise = rules.big_blind;
        hand.full_raises = 0;

        // Hole cards: two to each seat, one seat at a time, starting left of the button.
        for seat in hand.rotation_from(hand.next_dealt_in(button)) {
            let c1 = hand.deck.deal().expect("deck size checked");
            let c2 = hand.deck.deal().expect("deck size checked");
            hand.seats[seat as usize].hole_cards = Some([c1, c2]);
            hand.events.push(PublicEvent::HoleCards {
                seat,
                cards: [c1, c2],
            });
        }

        // First to act preflop: heads-up the button/small blind, otherwise the seat after the BB.
        let first = if heads_up {
            sb_seat
        } else {
            hand.next_dealt_in(bb_seat)
        };
        hand.actor = hand.first_needing_from(first);
        if hand.actor.is_none() {
            hand.end_street();
        }
        Ok(hand)
    }

    // ------------------------------------------------------------------ queries

    pub fn hand_id(&self) -> u64 {
        self.hand_id
    }

    pub fn table_id(&self) -> u64 {
        self.table_id
    }

    pub fn rules(&self) -> HandRules {
        self.rules
    }

    pub fn button(&self) -> SeatIndex {
        self.button
    }

    pub fn street(&self) -> Street {
        self.street
    }

    pub fn board(&self) -> &[Card] {
        &self.board
    }

    pub fn seats(&self) -> &[SeatState] {
        &self.seats
    }

    pub fn seat_views(&self) -> Vec<SeatView> {
        self.seats
            .iter()
            .enumerate()
            .map(|(i, s)| s.view(i as SeatIndex))
            .collect()
    }

    /// The seat that must act next, or `None` when the hand is complete.
    pub fn actor(&self) -> Option<SeatIndex> {
        self.actor
    }

    pub fn is_complete(&self) -> bool {
        self.result.is_some()
    }

    pub fn result(&self) -> Option<&HandResult> {
        self.result.as_ref()
    }

    pub fn pot_total(&self) -> i64 {
        self.seats.iter().map(|s| s.committed_total).sum()
    }

    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// All events emitted so far.
    pub fn events(&self) -> &[PublicEvent] {
        &self.events
    }

    /// Events emitted since the previous call.
    pub fn take_events(&mut self) -> Vec<PublicEvent> {
        let out = self.events[self.events_cursor..].to_vec();
        self.events_cursor = self.events.len();
        out
    }

    /// Legal actions for `seat`; `None` unless it is that seat's turn.
    pub fn legal_actions(&self, seat: SeatIndex) -> Option<LegalActions> {
        if self.actor != Some(seat) {
            return None;
        }
        let s = &self.seats[seat as usize];
        let to_call = (self.bet_level - s.committed_street).max(0);
        let max_to = s.committed_street + s.stack;
        let can_check = to_call == 0;
        let can_call = to_call > 0;
        let can_bet = self.bet_level == 0 && s.stack > 0;
        let can_raise = self.bet_level > 0 && max_to > self.bet_level && self.may_raise(seat);
        let min_bet_to = self.rules.big_blind.min(max_to).max(1);
        let min_raise_to = (self.bet_level + self.min_raise).min(max_to);
        let can_all_in = s.stack > 0 && (can_bet || can_raise || max_to <= self.bet_level);
        Some(LegalActions {
            can_fold: true,
            can_check,
            can_call,
            can_bet,
            can_raise,
            can_all_in,
            to_call,
            call_amount: to_call.min(s.stack),
            min_bet_to: if can_bet { min_bet_to } else { 0 },
            max_bet_to: if can_bet { max_to } else { 0 },
            min_raise_to: if can_raise { min_raise_to } else { 0 },
            max_raise_to: if can_raise { max_to } else { 0 },
            all_in_to: max_to,
        })
    }

    /// Full decision context for `seat`; `None` unless it is that seat's turn.
    pub fn decision_context(&self, seat: SeatIndex) -> Option<DecisionContext> {
        if self.actor != Some(seat) {
            return None;
        }
        let s = &self.seats[seat as usize];
        Some(DecisionContext {
            hand_id: self.hand_id,
            table_id: self.table_id,
            street: self.street,
            my_seat: seat,
            my_player_id: s.player_id.unwrap_or(0),
            button: self.button,
            small_blind: self.rules.small_blind,
            big_blind: self.rules.big_blind,
            ante: self.rules.ante,
            my_hole_cards: s.hole_cards.expect("actor was dealt in"),
            board: self.board.clone(),
            pot: self.pot_total(),
            bet_level: self.bet_level,
            to_call: (self.bet_level - s.committed_street).max(0),
            min_raise: self.min_raise,
            my_stack: s.stack,
            my_committed_street: s.committed_street,
            my_committed_total: s.committed_total,
            seats: self.seat_views(),
            history: self.history.clone(),
        })
    }

    // ------------------------------------------------------------------ mutation

    /// Apply an action for `seat`. Returns the normalised action actually applied (e.g. `AllIn`
    /// becomes `RaiseTo`/`BetTo`/`Call`, a `Call` for less than the full amount stays `Call`).
    pub fn apply(&mut self, seat: SeatIndex, action: Action) -> Result<Action, ActionError> {
        if self.result.is_some() {
            return Err(ActionError::HandComplete);
        }
        if self.actor != Some(seat) {
            return Err(ActionError::NotYourTurn);
        }
        let s = &self.seats[seat as usize];
        if !s.can_act() {
            return Err(ActionError::SeatNotInHand(seat));
        }
        let to_call = (self.bet_level - s.committed_street).max(0);
        let max_to = s.committed_street + s.stack;

        let normalized = match action {
            Action::Fold => Action::Fold,
            Action::Check => {
                if to_call > 0 {
                    return Err(ActionError::CannotCheck);
                }
                Action::Check
            }
            Action::Call => {
                if to_call == 0 {
                    return Err(ActionError::NothingToCall);
                }
                Action::Call
            }
            Action::BetTo { amount } => {
                if self.bet_level != 0 {
                    return Err(ActionError::CannotBet);
                }
                if amount > max_to {
                    return Err(ActionError::AboveMaximum {
                        amount,
                        max: max_to,
                    });
                }
                let min = self.rules.big_blind.min(max_to).max(1);
                if amount < min {
                    return Err(ActionError::BelowMinimum { amount, min });
                }
                Action::BetTo { amount }
            }
            Action::RaiseTo { amount } => {
                if self.bet_level == 0 || max_to <= self.bet_level || !self.may_raise(seat) {
                    return Err(ActionError::CannotRaise);
                }
                if amount > max_to {
                    return Err(ActionError::AboveMaximum {
                        amount,
                        max: max_to,
                    });
                }
                let min = (self.bet_level + self.min_raise).min(max_to);
                if amount < min {
                    return Err(ActionError::BelowMinimum { amount, min });
                }
                Action::RaiseTo { amount }
            }
            Action::AllIn => {
                if max_to <= self.bet_level {
                    Action::Call
                } else if self.bet_level == 0 {
                    Action::BetTo { amount: max_to }
                } else if self.may_raise(seat) {
                    Action::RaiseTo { amount: max_to }
                } else {
                    return Err(ActionError::CannotRaise);
                }
            }
        };

        // ---- effects
        let mut added = 0;
        match normalized {
            Action::Fold => {
                self.seats[seat as usize].status = SeatStatus::Folded;
                self.seats[seat as usize].acted_at = Some(self.full_raises);
            }
            Action::Check => {
                self.seats[seat as usize].acted_at = Some(self.full_raises);
            }
            Action::Call => {
                added = self.commit(seat, to_call, true);
                self.seats[seat as usize].acted_at = Some(self.full_raises);
            }
            Action::BetTo { amount } | Action::RaiseTo { amount } => {
                let increment = amount - self.bet_level;
                added = self.commit(
                    seat,
                    amount - self.seats[seat as usize].committed_street,
                    true,
                );
                debug_assert_eq!(self.seats[seat as usize].committed_street, amount);
                if increment >= self.min_raise {
                    self.min_raise = increment;
                    self.full_raises += 1;
                }
                self.bet_level = amount;
                self.last_aggressor = Some(seat);
                self.seats[seat as usize].acted_at = Some(self.full_raises);
            }
            Action::AllIn => unreachable!("AllIn is normalised above"),
        }
        let all_in = self.seats[seat as usize].status == SeatStatus::AllIn;
        let committed_street = self.seats[seat as usize].committed_street;
        self.history.push(HistoryEntry {
            street: self.street,
            seat,
            action: normalized,
            amount: added,
            all_in,
        });
        self.events.push(PublicEvent::ActionTaken {
            seat,
            street: self.street,
            action: normalized,
            amount: added,
            all_in,
            committed_street,
        });

        if self.contenders().len() <= 1 {
            self.finish_hand();
            return Ok(normalized);
        }

        match self.first_needing_from(self.next_dealt_in(seat)) {
            Some(next) => self.actor = Some(next),
            None => self.end_street(),
        }
        Ok(normalized)
    }

    // ------------------------------------------------------------------ internals

    /// Move up to `amount` chips from the seat's stack into the pot.
    fn commit(&mut self, seat: SeatIndex, amount: i64, counts_toward_street: bool) -> i64 {
        let s = &mut self.seats[seat as usize];
        let amount = amount.min(s.stack).max(0);
        s.stack -= amount;
        s.committed_total += amount;
        if counts_toward_street {
            s.committed_street += amount;
        }
        if s.stack == 0 && s.status == SeatStatus::Active {
            s.status = SeatStatus::AllIn;
        }
        amount
    }

    fn may_raise(&self, seat: SeatIndex) -> bool {
        self.seats[seat as usize].acted_at != Some(self.full_raises)
    }

    fn needs_to_act(&self, seat: SeatIndex) -> bool {
        let s = &self.seats[seat as usize];
        s.can_act() && (s.acted_at != Some(self.full_raises) || s.committed_street < self.bet_level)
    }

    /// Next dealt-in seat clockwise *after* `seat` (may wrap to `seat` itself).
    fn next_dealt_in(&self, seat: SeatIndex) -> SeatIndex {
        let n = self.seats.len();
        for k in 1..=n {
            let idx = (seat as usize + k) % n;
            if self.seats[idx].dealt_in {
                return idx as SeatIndex;
            }
        }
        seat
    }

    /// Dealt-in seats in clockwise order starting at `start` (inclusive).
    fn rotation_from(&self, start: SeatIndex) -> Vec<SeatIndex> {
        let n = self.seats.len();
        (0..n)
            .map(|k| ((start as usize + k) % n) as SeatIndex)
            .filter(|&i| self.seats[i as usize].dealt_in)
            .collect()
    }

    fn first_needing_from(&self, start: SeatIndex) -> Option<SeatIndex> {
        self.rotation_from(start)
            .into_iter()
            .find(|&s| self.needs_to_act(s))
    }

    fn contenders(&self) -> Vec<SeatIndex> {
        self.seats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contending())
            .map(|(i, _)| i as SeatIndex)
            .collect()
    }

    fn end_street(&mut self) {
        for s in self.seats.iter_mut() {
            s.committed_street = 0;
            s.acted_at = None;
        }
        self.bet_level = 0;
        self.min_raise = self.rules.big_blind;
        self.full_raises = 0;
        self.last_aggressor = None;
        self.actor = None;

        let Some(next) = self.street.next() else {
            self.finish_hand();
            return;
        };
        self.street = next;
        let count = match next {
            Street::Flop => 3,
            _ => 1,
        };
        let mut cards = Vec::with_capacity(count);
        for _ in 0..count {
            let c = self.deck.deal().expect("deck size checked");
            self.board.push(c);
            cards.push(c);
        }
        self.events.push(PublicEvent::BoardDealt {
            street: next,
            cards,
            board: self.board.clone(),
            pot: self.pot_total(),
        });

        let can_act = self.seats.iter().filter(|s| s.can_act()).count();
        if can_act >= 2 {
            self.actor = self.first_needing_from(self.next_dealt_in(self.button));
            if self.actor.is_none() {
                self.end_street();
            }
        } else {
            self.end_street();
        }
    }

    fn finish_hand(&mut self) {
        self.actor = None;
        let contenders = self.contenders();
        let pots = self.build_pots(&contenders);
        let showdown = contenders.len() > 1;

        let mut strengths: Vec<Option<HandStrength>> = vec![None; self.seats.len()];
        if showdown {
            for seat in self.rotation_from(self.next_dealt_in(self.button)) {
                if !contenders.contains(&seat) {
                    continue;
                }
                let cards = self.seats[seat as usize]
                    .hole_cards
                    .expect("contender has cards");
                let strength = evaluate_hand(&cards, &self.board);
                strengths[seat as usize] = Some(strength);
                self.events.push(PublicEvent::ShowdownRevealed {
                    seat,
                    cards,
                    strength,
                    description: strength.to_string(),
                });
            }
        }

        let order = self.rotation_from(self.next_dealt_in(self.button));
        let mut awards = Vec::new();
        for (pot_index, (amount, eligible)) in pots.into_iter().enumerate() {
            let winners: Vec<SeatIndex> = if showdown {
                let best = eligible
                    .iter()
                    .filter_map(|&s| strengths[s as usize])
                    .max()
                    .expect("at least one eligible contender");
                order
                    .iter()
                    .copied()
                    .filter(|s| eligible.contains(s) && strengths[*s as usize] == Some(best))
                    .collect()
            } else {
                eligible.clone()
            };
            let n = winners.len() as i64;
            let share = amount / n;
            let remainder = amount % n;
            let mut winner_amounts = Vec::new();
            for (i, &w) in winners.iter().enumerate() {
                let extra = if (i as i64) < remainder { 1 } else { 0 };
                let got = share + extra;
                self.seats[w as usize].stack += got;
                winner_amounts.push((w, got));
            }
            let award = PotAward {
                pot_index,
                amount,
                eligible_seats: eligible,
                winners: winner_amounts,
            };
            self.events.push(PublicEvent::PotAwarded(award.clone()));
            awards.push(award);
        }

        let seats = self.seat_views();
        self.events.push(PublicEvent::HandEnded {
            hand_id: self.hand_id,
            table_id: self.table_id,
            seats: seats.clone(),
        });
        let participants: Vec<PlayerId> = self
            .seats
            .iter()
            .filter(|s| s.dealt_in)
            .filter_map(|s| s.player_id)
            .collect();
        let busted: Vec<PlayerId> = self
            .seats
            .iter()
            .filter(|s| s.dealt_in && s.stack == 0)
            .filter_map(|s| s.player_id)
            .collect();
        self.result = Some(HandResult {
            hand_id: self.hand_id,
            seats,
            participants,
            busted,
            awards,
            went_to_showdown: showdown,
            board: self.board.clone(),
        });
    }

    /// Build (amount, eligible seats) pots from total contributions. Layers are defined by the
    /// distinct contribution levels of the contenders; folded players' chips fall into whichever
    /// layers they reached; anything above the top contender level goes to the top pot.
    fn build_pots(&self, contenders: &[SeatIndex]) -> Vec<(i64, Vec<SeatIndex>)> {
        let mut levels: Vec<i64> = contenders
            .iter()
            .map(|&s| self.seats[s as usize].committed_total)
            .collect();
        levels.sort_unstable();
        levels.dedup();

        let mut pots: Vec<(i64, Vec<SeatIndex>)> = Vec::new();
        let mut prev = 0i64;
        for &level in &levels {
            let layer = level - prev;
            let amount: i64 = self
                .seats
                .iter()
                .map(|s| (s.committed_total - prev).clamp(0, layer))
                .sum();
            let eligible: Vec<SeatIndex> = contenders
                .iter()
                .copied()
                .filter(|&s| self.seats[s as usize].committed_total >= level)
                .collect();
            if amount > 0 {
                pots.push((amount, eligible));
            }
            prev = level;
        }
        // Chips committed by folded players beyond the top contender level.
        let leftover: i64 = self
            .seats
            .iter()
            .map(|s| (s.committed_total - prev).max(0))
            .sum();
        if leftover > 0 {
            if let Some(last) = pots.last_mut() {
                last.0 += leftover;
            } else {
                pots.push((leftover, contenders.to_vec()));
            }
        }
        pots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::card;

    fn rules() -> HandRules {
        HandRules {
            small_blind: 5,
            big_blind: 10,
            ante: 0,
        }
    }

    fn start(stacks: &[i64], button: SeatIndex, deck: Deck) -> Hand {
        let seats = stacks
            .iter()
            .enumerate()
            .map(|(i, &s)| Some((i as PlayerId + 1, s)))
            .collect();
        Hand::start(HandParams {
            hand_id: 1,
            table_id: 1,
            rules: rules(),
            button,
            seats,
            deck,
        })
        .unwrap()
    }

    /// Chips in play: stacks plus (while the hand is live) chips committed to the pot.
    fn total_chips(h: &Hand) -> i64 {
        if h.is_complete() {
            h.seats.iter().map(|s| s.stack).sum()
        } else {
            h.seats.iter().map(|s| s.stack + s.committed_total).sum()
        }
    }

    #[test]
    fn three_handed_blinds_and_first_actor() {
        let h = start(&[100, 100, 100], 0, Deck::new(1));
        // button 0, sb 1, bb 2 -> utg is seat 0
        assert_eq!(h.actor(), Some(0));
        assert_eq!(h.seats[1].committed_street, 5);
        assert_eq!(h.seats[2].committed_street, 10);
        assert_eq!(h.pot_total(), 15);
        let legal = h.legal_actions(0).unwrap();
        assert_eq!(legal.to_call, 10);
        assert!(legal.can_call && !legal.can_check && !legal.can_bet && legal.can_raise);
        assert_eq!(legal.min_raise_to, 20);
        assert_eq!(legal.max_raise_to, 100);
    }

    #[test]
    fn heads_up_button_posts_small_blind_and_acts_first() {
        let h = start(&[100, 100], 1, Deck::new(1));
        assert_eq!(h.seats[1].committed_street, 5, "button posts SB");
        assert_eq!(h.seats[0].committed_street, 10);
        assert_eq!(h.actor(), Some(1));
        let mut h = h;
        h.apply(1, Action::Call).unwrap();
        // BB has the option
        assert_eq!(h.actor(), Some(0));
        h.apply(0, Action::Check).unwrap();
        assert_eq!(h.street(), Street::Flop);
        // postflop the non-button acts first
        assert_eq!(h.actor(), Some(0));
    }

    #[test]
    fn big_blind_option_after_limps() {
        let mut h = start(&[100, 100, 100], 0, Deck::new(2));
        h.apply(0, Action::Call).unwrap();
        h.apply(1, Action::Call).unwrap();
        assert_eq!(h.actor(), Some(2));
        let legal = h.legal_actions(2).unwrap();
        assert!(legal.can_check && legal.can_raise);
        h.apply(2, Action::RaiseTo { amount: 30 }).unwrap();
        // action reopens for 0 and 1
        assert_eq!(h.actor(), Some(0));
        assert_eq!(h.legal_actions(0).unwrap().to_call, 20);
        assert_eq!(h.legal_actions(0).unwrap().min_raise_to, 50);
    }

    #[test]
    fn min_raise_tracks_last_full_raise() {
        let mut h = start(&[1000, 1000, 1000], 0, Deck::new(3));
        h.apply(0, Action::RaiseTo { amount: 30 }).unwrap(); // +20
        assert_eq!(h.legal_actions(1).unwrap().min_raise_to, 50);
        h.apply(1, Action::RaiseTo { amount: 100 }).unwrap(); // +70
        assert_eq!(h.legal_actions(2).unwrap().min_raise_to, 170);
        assert_eq!(
            h.apply(2, Action::RaiseTo { amount: 150 }),
            Err(ActionError::BelowMinimum {
                amount: 150,
                min: 170
            })
        );
    }

    #[test]
    fn short_all_in_does_not_reopen_action() {
        // seat0 button, seat1 sb, seat2 bb, seat3 utg (short)
        let mut h = start(&[1000, 1000, 1000, 55], 0, Deck::new(4));
        assert_eq!(h.actor(), Some(3));
        h.apply(3, Action::Call).unwrap(); // utg limps 10 (45 left)
        h.apply(0, Action::RaiseTo { amount: 40 }).unwrap(); // full raise (+30)
        h.apply(1, Action::Call).unwrap(); // sb calls 40
        h.apply(2, Action::Call).unwrap(); // bb calls 40
                                           // utg shoves all-in to 55: increment 15 < min raise 30 => short
        h.apply(3, Action::AllIn).unwrap();
        assert_eq!(h.seats[3].status, SeatStatus::AllIn);
        // seat0 must respond but may not re-raise
        assert_eq!(h.actor(), Some(0));
        let legal = h.legal_actions(0).unwrap();
        assert!(legal.can_call);
        assert!(!legal.can_raise, "short all-in must not reopen raising");
        assert_eq!(legal.to_call, 15);
        assert_eq!(
            h.apply(0, Action::RaiseTo { amount: 100 }),
            Err(ActionError::CannotRaise)
        );
        h.apply(0, Action::Call).unwrap();
        h.apply(1, Action::Call).unwrap();
        h.apply(2, Action::Call).unwrap();
        assert_eq!(h.street(), Street::Flop);
    }

    #[test]
    fn full_all_in_raise_reopens_action() {
        let mut h = start(&[1000, 1000, 1000, 70], 0, Deck::new(5));
        h.apply(3, Action::Call).unwrap();
        h.apply(0, Action::RaiseTo { amount: 40 }).unwrap();
        h.apply(1, Action::Call).unwrap();
        h.apply(2, Action::Call).unwrap();
        h.apply(3, Action::AllIn).unwrap(); // to 70: increment 30 >= 30 => full raise
        let legal = h.legal_actions(0).unwrap();
        assert!(legal.can_raise);
        assert_eq!(legal.min_raise_to, 100);
    }

    #[test]
    fn fold_to_one_awards_pot_without_showdown() {
        let mut h = start(&[100, 100, 100], 0, Deck::new(6));
        h.apply(0, Action::Fold).unwrap();
        h.apply(1, Action::Fold).unwrap();
        assert!(h.is_complete());
        let r = h.result().unwrap();
        assert!(!r.went_to_showdown);
        assert_eq!(h.seats[2].stack, 105);
        assert_eq!(h.seats[1].stack, 95);
        assert_eq!(total_chips(&h), 300);
        assert!(!h
            .events()
            .iter()
            .any(|e| matches!(e, PublicEvent::ShowdownRevealed { .. })));
    }

    #[test]
    fn side_pots_three_way_all_in() {
        // Rig: seat0 gets AA, seat1 KK, seat2 22; board 3 4 5 9 T (no straight/flush)
        let deck = Deck::from_cards(vec![
            // dealt starting left of button (button 0): seat1, seat2, seat0
            card("Kh"),
            card("Kd"), // seat1
            card("2h"),
            card("2d"), // seat2
            card("Ah"),
            card("Ad"), // seat0
            card("3c"),
            card("4d"),
            card("5s"),
            card("9c"),
            card("Tc"),
        ]);
        let mut h = start(&[300, 100, 200], 0, deck);
        // preflop: seat0 utg shoves 300, seat1 (sb, 100) calls all-in, seat2 (bb, 200) calls all-in
        h.apply(0, Action::AllIn).unwrap();
        h.apply(1, Action::AllIn).unwrap();
        h.apply(2, Action::AllIn).unwrap();
        assert!(h.is_complete());
        let r = h.result().unwrap();
        assert!(r.went_to_showdown);
        // pots: main 100*3=300 (all), side 100*2=200 (0,2), uncalled 100 (0)
        assert_eq!(r.awards.len(), 3);
        assert_eq!(r.awards[0].amount, 300);
        assert_eq!(r.awards[1].amount, 200);
        assert_eq!(r.awards[2].amount, 100);
        assert_eq!(h.seats[0].stack, 600);
        assert_eq!(h.seats[1].stack, 0);
        assert_eq!(h.seats[2].stack, 0);
        assert_eq!(r.busted, vec![2, 3]);
        assert_eq!(total_chips(&h), 600);
    }

    #[test]
    fn side_pot_short_stack_wins_main_only() {
        // seat1 (short, sb) has AA and wins main pot only; seat2 KK beats seat0 QQ for side pot.
        let deck = Deck::from_cards(vec![
            card("Ah"),
            card("Ad"), // seat1
            card("Kh"),
            card("Kd"), // seat2
            card("Qh"),
            card("Qd"), // seat0
            card("3c"),
            card("4d"),
            card("5s"),
            card("9c"),
            card("Tc"),
        ]);
        let mut h = start(&[500, 50, 500], 0, deck);
        h.apply(0, Action::RaiseTo { amount: 200 }).unwrap();
        h.apply(1, Action::AllIn).unwrap(); // call for 50 total
        h.apply(2, Action::Call).unwrap(); // 200
                                           // flop: seat2 acts first (left of button among can-act), checks around to river
        while !h.is_complete() {
            let a = h.actor().unwrap();
            h.apply(a, Action::Check).unwrap();
        }
        let r = h.result().unwrap();
        assert_eq!(r.awards.len(), 2);
        assert_eq!(r.awards[0].amount, 150); // 50*3
        assert_eq!(r.awards[0].winners, vec![(1, 150)]);
        assert_eq!(r.awards[1].amount, 300); // 150*2
        assert_eq!(r.awards[1].winners, vec![(2, 300)]);
        assert_eq!(h.seats[1].stack, 150);
        assert_eq!(h.seats[2].stack, 600);
        assert_eq!(h.seats[0].stack, 300);
        assert_eq!(total_chips(&h), 1050);
    }

    #[test]
    fn split_pot_heads_up_even_split() {
        // Both players play the board (broadway straight); hole cards are irrelevant.
        let deck = Deck::from_cards(vec![
            card("2h"),
            card("3d"), // seat1
            card("2c"),
            card("3s"), // seat0
            card("Ac"),
            card("Kd"),
            card("Qs"),
            card("Jc"),
            card("Th"),
        ]);
        let mut h = start(&[100, 100], 0, deck);
        h.apply(0, Action::RaiseTo { amount: 21 }).unwrap();
        h.apply(1, Action::Call).unwrap();
        while !h.is_complete() {
            let a = h.actor().unwrap();
            h.apply(a, Action::Check).unwrap();
        }
        let r = h.result().unwrap();
        assert_eq!(r.awards.len(), 1);
        assert_eq!(r.awards[0].amount, 42);
        assert_eq!(r.awards[0].winners.len(), 2);
        assert_eq!(h.seats[0].stack, 100);
        assert_eq!(h.seats[1].stack, 100);
    }

    #[test]
    fn split_pot_odd_chip_goes_to_first_winner_left_of_button() {
        // seat0 button, seat1 sb, seat2 bb. seat1 folds preflop leaving 5 dead chips, so the pot
        // (10 + 5 + 10 = 25) is odd. seats 0 and 2 both play the board.
        let deck = Deck::from_cards(vec![
            card("2h"),
            card("3d"), // seat1
            card("2c"),
            card("3s"), // seat2
            card("7c"),
            card("8s"), // seat0
            card("Ac"),
            card("Kd"),
            card("Qs"),
            card("Jc"),
            card("Th"),
        ]);
        let mut h = start(&[100, 100, 100], 0, deck);
        h.apply(0, Action::Call).unwrap();
        h.apply(1, Action::Fold).unwrap();
        h.apply(2, Action::Check).unwrap();
        while !h.is_complete() {
            let a = h.actor().unwrap();
            h.apply(a, Action::Check).unwrap();
        }
        let r = h.result().unwrap();
        assert_eq!(r.awards.len(), 1);
        assert_eq!(r.awards[0].amount, 25);
        // first winner clockwise from the button is seat 2
        assert_eq!(r.awards[0].winners, vec![(2, 13), (0, 12)]);
        assert_eq!(total_chips(&h), 300);
    }

    #[test]
    fn all_in_preflop_runs_out_board_without_further_action() {
        let mut h = start(&[100, 100], 0, Deck::new(9));
        h.apply(0, Action::AllIn).unwrap();
        h.apply(1, Action::Call).unwrap();
        assert!(h.is_complete());
        assert_eq!(h.board().len(), 5);
        assert!(h.result().unwrap().went_to_showdown);
        assert_eq!(total_chips(&h), 200);
    }

    #[test]
    fn short_blind_posts_all_in_and_bb_still_sets_price() {
        // seat2 (bb) only has 4 chips
        let mut h = start(&[100, 100, 4], 0, Deck::new(10));
        assert_eq!(h.seats[2].status, SeatStatus::AllIn);
        assert_eq!(h.seats[2].committed_street, 4);
        let legal = h.legal_actions(0).unwrap();
        assert_eq!(legal.to_call, 10);
        h.apply(0, Action::Call).unwrap();
        h.apply(1, Action::Call).unwrap();
        // bb is all-in and cannot act; heads-up between 0 and 1 continues on the flop
        assert_eq!(h.street(), Street::Flop);
        assert_eq!(h.actor(), Some(1));
    }

    #[test]
    fn antes_go_into_pot_and_side_pots() {
        let seats = vec![Some((1, 100)), Some((2, 100)), Some((3, 100))];
        let mut h = Hand::start(HandParams {
            hand_id: 1,
            table_id: 1,
            rules: HandRules {
                small_blind: 5,
                big_blind: 10,
                ante: 2,
            },
            button: 0,
            seats,
            deck: Deck::new(11),
        })
        .unwrap();
        assert_eq!(h.pot_total(), 6 + 15);
        assert_eq!(h.seats[0].committed_street, 0);
        assert_eq!(h.seats[0].committed_total, 2);
        h.apply(0, Action::Fold).unwrap();
        h.apply(1, Action::Fold).unwrap();
        assert_eq!(h.seats[2].stack, 100 - 2 - 10 + 21);
        assert_eq!(total_chips(&h), 300);
    }

    #[test]
    fn illegal_actions_are_rejected_without_state_change() {
        let mut h = start(&[100, 100, 100], 0, Deck::new(12));
        let before = h.clone();
        assert_eq!(h.apply(1, Action::Fold), Err(ActionError::NotYourTurn));
        assert_eq!(h.apply(0, Action::Check), Err(ActionError::CannotCheck));
        assert_eq!(
            h.apply(0, Action::BetTo { amount: 50 }),
            Err(ActionError::CannotBet)
        );
        assert_eq!(
            h.apply(0, Action::RaiseTo { amount: 15 }),
            Err(ActionError::BelowMinimum {
                amount: 15,
                min: 20
            })
        );
        assert_eq!(
            h.apply(0, Action::RaiseTo { amount: 500 }),
            Err(ActionError::AboveMaximum {
                amount: 500,
                max: 100
            })
        );
        assert_eq!(h.actor(), before.actor());
        assert_eq!(h.pot_total(), before.pot_total());
        assert_eq!(h.events().len(), before.events().len());
    }

    #[test]
    fn everyone_folds_to_bb_ends_hand_immediately() {
        let mut h = start(&[100; 6], 2, Deck::new(13));
        // button 2, sb 3, bb 4, utg 5
        let mut count = 0;
        while !h.is_complete() {
            let a = h.actor().unwrap();
            h.apply(a, Action::Fold).unwrap();
            count += 1;
        }
        assert_eq!(count, 5);
        assert_eq!(h.seats[4].stack, 105);
    }

    #[test]
    fn events_are_delivered_incrementally() {
        let mut h = start(&[100, 100], 0, Deck::new(14));
        let first = h.take_events();
        assert!(matches!(first[0], PublicEvent::HandStarted { .. }));
        assert!(h.take_events().is_empty());
        h.apply(0, Action::Call).unwrap();
        let ev = h.take_events();
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0], PublicEvent::ActionTaken { seat: 0, .. }));
    }

    #[test]
    fn button_must_be_dealt_in() {
        let seats = vec![Some((1, 100)), None, Some((2, 100))];
        let err = Hand::start(HandParams {
            hand_id: 1,
            table_id: 1,
            rules: rules(),
            button: 1,
            seats,
            deck: Deck::new(1),
        })
        .unwrap_err();
        assert_eq!(err, HandError::ButtonNotDealtIn(1));
    }

    #[test]
    fn random_play_conserves_chips_and_terminates() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(99);
        for iter in 0..2000u64 {
            let n = rng.random_range(2..=9usize);
            let stacks: Vec<i64> = (0..n).map(|_| rng.random_range(1..=300)).collect();
            let seats: Vec<Option<(PlayerId, i64)>> = stacks
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    if rng.random_range(0..10) == 0 && n > 2 {
                        None
                    } else {
                        Some((i as PlayerId + 1, s))
                    }
                })
                .collect();
            let dealt: Vec<usize> = seats
                .iter()
                .enumerate()
                .filter(|(_, s)| s.is_some())
                .map(|(i, _)| i)
                .collect();
            if dealt.len() < 2 {
                continue;
            }
            let button = dealt[rng.random_range(0..dealt.len())] as SeatIndex;
            let total: i64 = seats.iter().flatten().map(|s| s.1).sum();
            let mut h = Hand::start(HandParams {
                hand_id: iter,
                table_id: 1,
                rules: HandRules {
                    small_blind: 5,
                    big_blind: 10,
                    ante: if iter % 3 == 0 { 1 } else { 0 },
                },
                button,
                seats,
                deck: Deck::new(iter),
            })
            .unwrap();
            let mut steps = 0;
            while let Some(a) = h.actor() {
                steps += 1;
                assert!(steps < 500, "hand did not terminate");
                let legal = h.legal_actions(a).unwrap();
                let choice = rng.random_range(0..6);
                let action = match choice {
                    0 => Action::Fold,
                    1 if legal.can_check => Action::Check,
                    2 if legal.can_call => Action::Call,
                    3 if legal.can_bet => Action::BetTo {
                        amount: rng.random_range(legal.min_bet_to..=legal.max_bet_to),
                    },
                    4 if legal.can_raise => Action::RaiseTo {
                        amount: rng.random_range(legal.min_raise_to..=legal.max_raise_to),
                    },
                    5 if legal.can_all_in => Action::AllIn,
                    _ => legal.auto_action(),
                };
                assert!(
                    legal.allows(&action),
                    "generated action {:?} not allowed by {:?}",
                    action,
                    legal
                );
                h.apply(a, action).unwrap();
            }
            assert!(h.is_complete());
            assert_eq!(
                total_chips(&h),
                total,
                "chips not conserved in hand {}",
                iter
            );
            let awarded: i64 = h.result().unwrap().awards.iter().map(|a| a.amount).sum();
            let contributed: i64 = h.seats.iter().map(|s| s.committed_total).sum();
            assert_eq!(awarded, contributed);
        }
    }
}
