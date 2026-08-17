//! Cards, ranks, suits and decks.
//!
//! `Card` serialises to/from the conventional two-character string form (`"As"`, `"Td"`)
//! so that the bot protocol is human readable.

use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Card(u8);

impl fmt::Debug for Card {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Card {
    pub fn new(rank: Rank, suit: Suit) -> Self {
        Card((rank as u8) * 4 + (suit as u8))
    }

    /// Construct from a raw index in `0..52` (rank-major).
    pub fn from_index(idx: u8) -> Option<Self> {
        if idx < 52 {
            Some(Card(idx))
        } else {
            None
        }
    }

    pub fn index(self) -> u8 {
        self.0
    }

    pub fn rank(self) -> Rank {
        Rank::from_u8(self.0 / 4).expect("card index in range")
    }

    pub fn suit(self) -> Suit {
        match self.0 % 4 {
            0 => Suit::Clubs,
            1 => Suit::Diamonds,
            2 => Suit::Hearts,
            _ => Suit::Spades,
        }
    }

    /// All 52 cards in rank-major order.
    pub fn all() -> Vec<Card> {
        (0..52).map(Card).collect()
    }
}

impl FromStr for Card {
    type Err = CardParseError;

    fn from_str(s: &str) -> Result<Self, CardParseError> {
        let chars: Vec<char> = s.trim().chars().collect();
        if chars.len() != 2 {
            return Err(CardParseError::InvalidLength);
        }
        let rank = Rank::from_char(chars[0]).ok_or(CardParseError::InvalidRank)?;
        let suit = Suit::from_char(chars[1]).ok_or(CardParseError::InvalidSuit)?;
        Ok(Card::new(rank, suit))
    }
}

impl fmt::Display for Card {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", self.rank().to_char(), self.suit().to_char())
    }
}

impl Serialize for Card {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Card {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Rank {
    Two = 0,
    Three = 1,
    Four = 2,
    Five = 3,
    Six = 4,
    Seven = 5,
    Eight = 6,
    Nine = 7,
    Ten = 8,
    Jack = 9,
    Queen = 10,
    King = 11,
    Ace = 12,
}

impl Rank {
    pub const ALL: [Rank; 13] = [
        Rank::Two,
        Rank::Three,
        Rank::Four,
        Rank::Five,
        Rank::Six,
        Rank::Seven,
        Rank::Eight,
        Rank::Nine,
        Rank::Ten,
        Rank::Jack,
        Rank::Queen,
        Rank::King,
        Rank::Ace,
    ];

    pub fn from_u8(v: u8) -> Option<Rank> {
        Rank::ALL.get(v as usize).copied()
    }

    pub fn from_char(c: char) -> Option<Rank> {
        Some(match c.to_ascii_uppercase() {
            '2' => Rank::Two,
            '3' => Rank::Three,
            '4' => Rank::Four,
            '5' => Rank::Five,
            '6' => Rank::Six,
            '7' => Rank::Seven,
            '8' => Rank::Eight,
            '9' => Rank::Nine,
            'T' => Rank::Ten,
            'J' => Rank::Jack,
            'Q' => Rank::Queen,
            'K' => Rank::King,
            'A' => Rank::Ace,
            _ => return None,
        })
    }

    pub fn to_char(self) -> char {
        match self {
            Rank::Two => '2',
            Rank::Three => '3',
            Rank::Four => '4',
            Rank::Five => '5',
            Rank::Six => '6',
            Rank::Seven => '7',
            Rank::Eight => '8',
            Rank::Nine => '9',
            Rank::Ten => 'T',
            Rank::Jack => 'J',
            Rank::Queen => 'Q',
            Rank::King => 'K',
            Rank::Ace => 'A',
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Suit {
    Clubs = 0,
    Diamonds = 1,
    Hearts = 2,
    Spades = 3,
}

impl Suit {
    pub fn from_char(c: char) -> Option<Suit> {
        Some(match c.to_ascii_lowercase() {
            'c' => Suit::Clubs,
            'd' => Suit::Diamonds,
            'h' => Suit::Hearts,
            's' => Suit::Spades,
            _ => return None,
        })
    }

    pub fn to_char(self) -> char {
        match self {
            Suit::Clubs => 'c',
            Suit::Diamonds => 'd',
            Suit::Hearts => 'h',
            Suit::Spades => 's',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CardParseError {
    #[error("card string must be exactly two characters, e.g. \"As\"")]
    InvalidLength,
    #[error("invalid rank character (expected 2-9, T, J, Q, K or A)")]
    InvalidRank,
    #[error("invalid suit character (expected c, d, h or s)")]
    InvalidSuit,
}

/// A deck of cards. Cards are dealt from the *front* (index 0 first), so a deck built with
/// [`Deck::from_cards`] deals in exactly the order given — handy for scripted tests.
#[derive(Clone, Debug)]
pub struct Deck {
    cards: Vec<Card>,
    next: usize,
}

impl Deck {
    /// Full 52-card deck shuffled with a deterministic seed.
    pub fn new(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut cards = Card::all();
        cards.shuffle(&mut rng);
        Deck { cards, next: 0 }
    }

    /// Deck that deals the given cards in order (may be partial; dealing past the end yields `None`).
    pub fn from_cards(cards: Vec<Card>) -> Self {
        Deck { cards, next: 0 }
    }

    /// Like [`Deck::from_cards`] but the given cards are dealt first and the remainder of the 52-card
    /// deck follows in a seeded-random order. Convenient for tests that only care about a few cards.
    pub fn rigged(first: &[Card], seed: u64) -> Self {
        let mut rest = Deck::new(seed).cards;
        rest.retain(|c| !first.contains(c));
        let mut cards = first.to_vec();
        cards.extend(rest);
        Deck { cards, next: 0 }
    }

    pub fn deal(&mut self) -> Option<Card> {
        let c = self.cards.get(self.next).copied();
        if c.is_some() {
            self.next += 1;
        }
        c
    }

    pub fn remaining(&self) -> usize {
        self.cards.len().saturating_sub(self.next)
    }
}

/// Parse a whitespace-separated list of cards, e.g. `"As Kd Qh"`.
pub fn parse_cards(s: &str) -> Result<Vec<Card>, CardParseError> {
    s.split_whitespace().map(|t| t.parse()).collect()
}

/// Convenience macro-free helper for tests: `card("As")`.
pub fn card(s: &str) -> Card {
    s.parse().expect("valid card literal")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_cards() {
        for c in Card::all() {
            let s = c.to_string();
            assert_eq!(s.parse::<Card>().unwrap(), c);
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", s));
            let back: Card = serde_json::from_str(&json).unwrap();
            assert_eq!(back, c);
        }
    }

    #[test]
    fn deck_is_deterministic_and_complete() {
        let mut a = Deck::new(7);
        let mut b = Deck::new(7);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..52 {
            let ca = a.deal().unwrap();
            let cb = b.deal().unwrap();
            assert_eq!(ca, cb);
            assert!(seen.insert(ca));
        }
        assert!(a.deal().is_none());
        assert_eq!(seen.len(), 52);
    }

    #[test]
    fn rigged_deck_deals_requested_cards_first_then_rest() {
        let mut d = Deck::rigged(&[card("As"), card("Ah")], 1);
        assert_eq!(d.deal(), Some(card("As")));
        assert_eq!(d.deal(), Some(card("Ah")));
        let mut seen = std::collections::HashSet::new();
        seen.insert(card("As"));
        seen.insert(card("Ah"));
        while let Some(c) = d.deal() {
            assert!(seen.insert(c));
        }
        assert_eq!(seen.len(), 52);
    }
}
