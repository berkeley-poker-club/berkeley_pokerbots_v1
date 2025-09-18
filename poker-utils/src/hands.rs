use crate::cards::{Card, Rank, Suit};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HandStrength(u32);

impl HandStrength {
    pub fn new(rank: HandRank, kickers: &[Rank]) -> Self {
        let mut value = (rank as u32) << 20;

        for (i, &kicker) in kickers.iter().enumerate() {
            if i < 5 {
                value |= (kicker as u32) << (16 - i * 4);
            }
        }

        HandStrength(value)
    }

    pub fn rank(&self) -> HandRank {
        match (self.0 >> 20) & 0xF {
            0 => HandRank::HighCard,
            1 => HandRank::Pair,
            2 => HandRank::TwoPair,
            3 => HandRank::ThreeOfAKind,
            4 => HandRank::Straight,
            5 => HandRank::Flush,
            6 => HandRank::FullHouse,
            7 => HandRank::FourOfAKind,
            8 => HandRank::StraightFlush,
            _ => panic!("Invalid hand rank"),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HandRank {
    HighCard = 0,
    Pair = 1,
    TwoPair = 2,
    ThreeOfAKind = 3,
    Straight = 4,
    Flush = 5,
    FullHouse = 6,
    FourOfAKind = 7,
    StraightFlush = 8,
}

pub fn evaluate_hand(hole_cards: &[Card; 2], board: &[Card]) -> HandStrength {
    let mut all_cards = Vec::with_capacity(7);
    all_cards.extend_from_slice(hole_cards);
    all_cards.extend_from_slice(board);

    if all_cards.len() < 5 {
        return HandStrength(0);
    }

    let mut best_strength = HandStrength(0);

    let indices: Vec<usize> = (0..all_cards.len()).collect();
    for combo in combinations(&indices, 5) {
        let five_cards: Vec<Card> = combo.iter().map(|&i| all_cards[i]).collect();
        let strength = evaluate_five_cards(&five_cards);
        if strength > best_strength {
            best_strength = strength;
        }
    }

    best_strength
}

fn evaluate_five_cards(cards: &[Card]) -> HandStrength {
    assert_eq!(cards.len(), 5);

    let mut ranks: Vec<Rank> = cards.iter().map(|c| c.rank()).collect();
    ranks.sort_by(|a, b| b.cmp(a));

    let suits: Vec<Suit> = cards.iter().map(|c| c.suit()).collect();
    let is_flush = suits.iter().all(|&s| s == suits[0]);

    let is_straight = check_straight(&ranks);

    if is_flush && is_straight {
        return HandStrength::new(HandRank::StraightFlush, &[ranks[0]]);
    }

    let rank_counts = count_ranks(&ranks);

    if let Some(four_kind) = rank_counts.iter().find(|(_, &count)| count == 4) {
        let kicker = rank_counts.iter().find(|(_, &count)| count == 1).unwrap().0;
        return HandStrength::new(HandRank::FourOfAKind, &[*four_kind.0, *kicker]);
    }

    let three_kinds: Vec<Rank> = rank_counts.iter()
        .filter(|(_, &count)| count == 3)
        .map(|(&rank, _)| rank)
        .collect();
    let pairs: Vec<Rank> = rank_counts.iter()
        .filter(|(_, &count)| count == 2)
        .map(|(&rank, _)| rank)
        .collect();

    if !three_kinds.is_empty() && !pairs.is_empty() {
        return HandStrength::new(HandRank::FullHouse, &[three_kinds[0], pairs[0]]);
    }

    if is_flush {
        return HandStrength::new(HandRank::Flush, &ranks);
    }

    if is_straight {
        return HandStrength::new(HandRank::Straight, &[ranks[0]]);
    }

    if !three_kinds.is_empty() {
        let mut kickers: Vec<Rank> = rank_counts.iter()
            .filter(|(_, &count)| count == 1)
            .map(|(&rank, _)| rank)
            .collect();
        kickers.sort_by(|a, b| b.cmp(a));
        let mut result = vec![three_kinds[0]];
        result.extend_from_slice(&kickers);
        return HandStrength::new(HandRank::ThreeOfAKind, &result);
    }

    if pairs.len() >= 2 {
        let mut sorted_pairs = pairs;
        sorted_pairs.sort_by(|a, b| b.cmp(a));
        let kicker = rank_counts.iter().find(|(_, &count)| count == 1).unwrap().0;
        return HandStrength::new(HandRank::TwoPair, &[sorted_pairs[0], sorted_pairs[1], *kicker]);
    }

    if pairs.len() == 1 {
        let mut kickers: Vec<Rank> = rank_counts.iter()
            .filter(|(_, &count)| count == 1)
            .map(|(&rank, _)| rank)
            .collect();
        kickers.sort_by(|a, b| b.cmp(a));
        let mut result = vec![pairs[0]];
        result.extend_from_slice(&kickers);
        return HandStrength::new(HandRank::Pair, &result);
    }

    HandStrength::new(HandRank::HighCard, &ranks)
}

fn check_straight(ranks: &[Rank]) -> bool {
    if ranks.len() != 5 {
        return false;
    }

    let rank_values: Vec<u8> = ranks.iter().map(|&r| r as u8).collect();

    if rank_values == [12, 3, 2, 1, 0] {
        return true;
    }

    for i in 1..5 {
        if rank_values[i-1] != rank_values[i] + 1 {
            return false;
        }
    }

    true
}

fn count_ranks(ranks: &[Rank]) -> std::collections::HashMap<Rank, usize> {
    let mut counts = std::collections::HashMap::new();
    for &rank in ranks {
        *counts.entry(rank).or_insert(0) += 1;
    }
    counts
}

fn combinations<T: Clone>(items: &[T], k: usize) -> Vec<Vec<T>> {
    if k == 0 {
        return vec![vec![]];
    }
    if items.is_empty() {
        return vec![];
    }

    let mut result = vec![];
    let first = &items[0];
    let rest = &items[1..];

    for mut combo in combinations(rest, k - 1) {
        combo.insert(0, first.clone());
        result.push(combo);
    }

    result.extend(combinations(rest, k));
    result
}