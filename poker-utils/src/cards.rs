use serde::{Deserialize, Serialize};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use std::fmt;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Card(u8);

impl Card {
    pub fn new(rank: Rank, suit: Suit) -> Self {
        Card((rank as u8) * 4 + (suit as u8))
    }

    pub fn rank(self) -> Rank {
        match self.0 / 4 {
            0 => Rank::Two,
            1 => Rank::Three,
            2 => Rank::Four,
            3 => Rank::Five,
            4 => Rank::Six,
            5 => Rank::Seven,
            6 => Rank::Eight,
            7 => Rank::Nine,
            8 => Rank::Ten,
            9 => Rank::Jack,
            10 => Rank::Queen,
            11 => Rank::King,
            12 => Rank::Ace,
            _ => panic!("Invalid card value"),
        }
    }

    pub fn suit(self) -> Suit {
        match self.0 % 4 {
            0 => Suit::Clubs,
            1 => Suit::Diamonds,
            2 => Suit::Hearts,
            3 => Suit::Spades,
            _ => unreachable!(),
        }
    }

    pub fn from_str(s: &str) -> Result<Self, CardParseError> {
        if s.len() != 2 {
            return Err(CardParseError::InvalidLength);
        }

        let chars: Vec<char> = s.chars().collect();
        let rank = match chars[0] {
            '2' => Rank::Two,
            '3' => Rank::Three,
            '4' => Rank::Four,
            '5' => Rank::Five,
            '6' => Rank::Six,
            '7' => Rank::Seven,
            '8' => Rank::Eight,
            '9' => Rank::Nine,
            'T' | 't' => Rank::Ten,
            'J' | 'j' => Rank::Jack,
            'Q' | 'q' => Rank::Queen,
            'K' | 'k' => Rank::King,
            'A' | 'a' => Rank::Ace,
            _ => return Err(CardParseError::InvalidRank),
        };

        let suit = match chars[1] {
            'c' | 'C' => Suit::Clubs,
            'd' | 'D' => Suit::Diamonds,
            'h' | 'H' => Suit::Hearts,
            's' | 'S' => Suit::Spades,
            _ => return Err(CardParseError::InvalidSuit),
        };

        Ok(Card::new(rank, suit))
    }
}

impl fmt::Display for Card {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let rank_char = match self.rank() {
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
        };

        let suit_char = match self.suit() {
            Suit::Clubs => 'c',
            Suit::Diamonds => 'd',
            Suit::Hearts => 'h',
            Suit::Spades => 's',
        };

        write!(f, "{}{}", rank_char, suit_char)
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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Suit {
    Clubs = 0,
    Diamonds = 1,
    Hearts = 2,
    Spades = 3,
}

#[derive(Debug)]
pub enum CardParseError {
    InvalidLength,
    InvalidRank,
    InvalidSuit,
}

impl fmt::Display for CardParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CardParseError::InvalidLength => write!(f, "CardParseError::InvalidLength"),
            CardParseError::InvalidRank => write!(f, "CardParseError::InvalidRank"),
            CardParseError::InvalidSuit => write!(f, "CardParseError::InvalidSuit"),
        }
    }
}

impl std::error::Error for CardParseError {}

#[derive(Clone, Debug)]
pub struct Deck {
    cards: Vec<Card>,
    rng: StdRng,
}

impl Deck {
    pub fn new(seed: u64) -> Self {
        let mut cards = Vec::with_capacity(52);
        for rank in 0..13 {
            for suit in 0..4 {
                cards.push(Card(rank * 4 + suit));
            }
        }

        let mut deck = Deck {
            cards,
            rng: StdRng::seed_from_u64(seed),
        };
        deck.shuffle();
        deck
    }

    pub fn shuffle(&mut self) {
        self.cards.shuffle(&mut self.rng);
    }

    pub fn deal(&mut self) -> Option<Card> {
        self.cards.pop()
    }

    pub fn remaining(&self) -> usize {
        self.cards.len()
    }

    pub fn reset(&mut self) {
        self.cards.clear();
        for rank in 0..13 {
            for suit in 0..4 {
                self.cards.push(Card(rank * 4 + suit));
            }
        }
        self.shuffle();
    }
}