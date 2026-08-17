//! Hand evaluation for Texas Hold'em (best five of up to seven cards).

use crate::cards::{Card, Rank};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Totally ordered hand strength. Higher is better. Encodes the category in the top bits and up
/// to five tie-break ranks (4 bits each) below it, so a plain integer comparison is a correct
/// poker comparison.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HandStrength(u32);

impl HandStrength {
    pub fn new(category: HandCategory, tiebreak: &[Rank]) -> Self {
        let mut value = (category as u32) << 20;
        for (i, &r) in tiebreak.iter().take(5).enumerate() {
            value |= (r as u32) << (16 - i * 4);
        }
        HandStrength(value)
    }

    pub fn category(&self) -> HandCategory {
        HandCategory::from_u32((self.0 >> 20) & 0xF)
    }

    pub fn raw(&self) -> u32 {
        self.0
    }

    /// The tie-break ranks in significance order (e.g. `[Ace, King]` for a full house aces over kings).
    pub fn tiebreak(&self) -> Vec<Rank> {
        let mut out = Vec::new();
        let n = match self.category() {
            HandCategory::HighCard | HandCategory::Flush => 5,
            HandCategory::Pair => 4,
            HandCategory::TwoPair | HandCategory::ThreeOfAKind => 3,
            HandCategory::FullHouse | HandCategory::FourOfAKind => 2,
            HandCategory::Straight | HandCategory::StraightFlush => 1,
        };
        for i in 0..n {
            let v = (self.0 >> (16 - i * 4)) & 0xF;
            if let Some(r) = Rank::from_u8(v as u8) {
                out.push(r);
            }
        }
        out
    }
}

impl fmt::Display for HandStrength {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let tb: String = self.tiebreak().iter().map(|r| r.to_char()).collect();
        write!(f, "{}({})", self.category(), tb)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HandCategory {
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

impl HandCategory {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => HandCategory::HighCard,
            1 => HandCategory::Pair,
            2 => HandCategory::TwoPair,
            3 => HandCategory::ThreeOfAKind,
            4 => HandCategory::Straight,
            5 => HandCategory::Flush,
            6 => HandCategory::FullHouse,
            7 => HandCategory::FourOfAKind,
            _ => HandCategory::StraightFlush,
        }
    }
}

impl fmt::Display for HandCategory {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            HandCategory::HighCard => "HighCard",
            HandCategory::Pair => "Pair",
            HandCategory::TwoPair => "TwoPair",
            HandCategory::ThreeOfAKind => "ThreeOfAKind",
            HandCategory::Straight => "Straight",
            HandCategory::Flush => "Flush",
            HandCategory::FullHouse => "FullHouse",
            HandCategory::FourOfAKind => "FourOfAKind",
            HandCategory::StraightFlush => "StraightFlush",
        };
        f.write_str(s)
    }
}

/// Evaluate the best five-card hand from two hole cards and 3–5 board cards.
/// With fewer than five cards total, returns the best partial hand (used only for display).
pub fn evaluate_hand(hole_cards: &[Card; 2], board: &[Card]) -> HandStrength {
    let mut all: Vec<Card> = Vec::with_capacity(7);
    all.extend_from_slice(hole_cards);
    all.extend_from_slice(board);
    evaluate_cards(&all)
}

/// Evaluate the best five-card hand from any 5–7 cards.
pub fn evaluate_cards(cards: &[Card]) -> HandStrength {
    let n = cards.len();
    if n < 5 {
        return partial_strength(cards);
    }
    if n == 5 {
        return evaluate_five(cards);
    }
    let mut best = HandStrength(0);
    // choose 5 of n by excluding n-5 cards; n <= 7 so at most 21 combinations.
    let mut idx = [0usize; 5];
    for a in 0..n {
        idx[0] = a;
        for b in (a + 1)..n {
            idx[1] = b;
            for c in (b + 1)..n {
                idx[2] = c;
                for d in (c + 1)..n {
                    idx[3] = d;
                    for e in (d + 1)..n {
                        idx[4] = e;
                        let five = [cards[a], cards[b], cards[c], cards[d], cards[e]];
                        let s = evaluate_five(&five);
                        if s > best {
                            best = s;
                        }
                    }
                }
            }
        }
    }
    best
}

fn partial_strength(cards: &[Card]) -> HandStrength {
    let mut ranks: Vec<Rank> = cards.iter().map(|c| c.rank()).collect();
    ranks.sort_by(|a, b| b.cmp(a));
    let (groups, _) = group_ranks(&ranks);
    if let Some(&(r, 2)) = groups.first() {
        let kickers: Vec<Rank> = groups.iter().skip(1).map(|g| g.0).collect();
        let mut tb = vec![r];
        tb.extend(kickers);
        return HandStrength::new(HandCategory::Pair, &tb);
    }
    HandStrength::new(HandCategory::HighCard, &ranks)
}

/// (rank, count) groups sorted by count desc then rank desc; and ranks sorted desc.
fn group_ranks(sorted_desc: &[Rank]) -> (Vec<(Rank, u8)>, Vec<Rank>) {
    let mut groups: Vec<(Rank, u8)> = Vec::with_capacity(5);
    for &r in sorted_desc {
        if let Some(g) = groups.iter_mut().find(|g| g.0 == r) {
            g.1 += 1;
        } else {
            groups.push((r, 1));
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    (groups, sorted_desc.to_vec())
}

fn evaluate_five(cards: &[Card]) -> HandStrength {
    debug_assert_eq!(cards.len(), 5);
    let mut ranks: Vec<Rank> = cards.iter().map(|c| c.rank()).collect();
    ranks.sort_by(|a, b| b.cmp(a));
    let is_flush = cards.iter().all(|c| c.suit() == cards[0].suit());
    let straight_high = straight_high_card(&ranks);

    if let Some(high) = straight_high {
        if is_flush {
            return HandStrength::new(HandCategory::StraightFlush, &[high]);
        }
    }

    let (groups, _) = group_ranks(&ranks);
    let counts: Vec<u8> = groups.iter().map(|g| g.1).collect();

    if counts[0] == 4 {
        return HandStrength::new(HandCategory::FourOfAKind, &[groups[0].0, groups[1].0]);
    }
    if counts[0] == 3 && counts[1] == 2 {
        return HandStrength::new(HandCategory::FullHouse, &[groups[0].0, groups[1].0]);
    }
    if is_flush {
        return HandStrength::new(HandCategory::Flush, &ranks);
    }
    if let Some(high) = straight_high {
        return HandStrength::new(HandCategory::Straight, &[high]);
    }
    if counts[0] == 3 {
        return HandStrength::new(
            HandCategory::ThreeOfAKind,
            &[groups[0].0, groups[1].0, groups[2].0],
        );
    }
    if counts[0] == 2 && counts[1] == 2 {
        return HandStrength::new(
            HandCategory::TwoPair,
            &[groups[0].0, groups[1].0, groups[2].0],
        );
    }
    if counts[0] == 2 {
        return HandStrength::new(
            HandCategory::Pair,
            &[groups[0].0, groups[1].0, groups[2].0, groups[3].0],
        );
    }
    HandStrength::new(HandCategory::HighCard, &ranks)
}

/// Returns the high card of the straight if the five distinct ranks form one. The wheel
/// (A-2-3-4-5) is a five-high straight.
fn straight_high_card(sorted_desc: &[Rank]) -> Option<Rank> {
    let v: Vec<u8> = sorted_desc.iter().map(|&r| r as u8).collect();
    if v == [12, 3, 2, 1, 0] {
        return Some(Rank::Five);
    }
    for i in 1..5 {
        if v[i - 1] != v[i] + 1 {
            return None;
        }
    }
    Some(sorted_desc[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::parse_cards;

    fn ev(s: &str) -> HandStrength {
        evaluate_cards(&parse_cards(s).unwrap())
    }

    #[test]
    fn categories() {
        assert_eq!(ev("As Ks Qs Js Ts").category(), HandCategory::StraightFlush);
        assert_eq!(ev("As Ad Ah Ac Ts").category(), HandCategory::FourOfAKind);
        assert_eq!(ev("As Ad Ah Kc Ks").category(), HandCategory::FullHouse);
        assert_eq!(ev("As 2s 5s Js Ts").category(), HandCategory::Flush);
        assert_eq!(ev("9d 8s 7s 6s 5s").category(), HandCategory::Straight);
        assert_eq!(ev("As Ad Ah 3c Ts").category(), HandCategory::ThreeOfAKind);
        assert_eq!(ev("As Ad Kh Kc Ts").category(), HandCategory::TwoPair);
        assert_eq!(ev("As Ad 2h Kc Ts").category(), HandCategory::Pair);
        assert_eq!(ev("As 3d 2h Kc Ts").category(), HandCategory::HighCard);
    }

    #[test]
    fn wheel_is_five_high_and_loses_to_six_high() {
        let wheel = ev("As 2d 3h 4c 5s");
        let six = ev("2s 3d 4h 5c 6s");
        let broadway = ev("As Kd Qh Jc Ts");
        assert_eq!(wheel.category(), HandCategory::Straight);
        assert!(wheel < six);
        assert!(six < broadway);
        assert_eq!(wheel.tiebreak(), vec![Rank::Five]);
    }

    #[test]
    fn steel_wheel_loses_to_higher_straight_flush() {
        let sw = ev("As 2s 3s 4s 5s");
        let sf6 = ev("2h 3h 4h 5h 6h");
        assert_eq!(sw.category(), HandCategory::StraightFlush);
        assert!(sw < sf6);
    }

    #[test]
    fn kicker_ordering() {
        assert!(ev("As Ad Kh 5c 2s") > ev("As Ad Qh Jc Ts"));
        assert!(ev("Ks Kd Qh Qc 2s") > ev("Ks Kd Jh Jc As"));
        assert!(ev("As Ks Qs 9s 2s") > ev("As Ks Qs 8s 7s"));
        assert_eq!(ev("As Kd Qh Jc 9s"), ev("Ah Kc Qd Js 9d"));
    }

    #[test]
    fn best_of_seven() {
        // Board gives a flush; hole cards irrelevant
        let s = evaluate_hand(
            &[crate::cards::card("2c"), crate::cards::card("3d")],
            &parse_cards("As Ks Qs Js 9s").unwrap(),
        );
        assert_eq!(s.category(), HandCategory::Flush);
        // Pocket pair + board pair = two pair; picks the best kicker
        let s = evaluate_hand(
            &[crate::cards::card("9c"), crate::cards::card("9d")],
            &parse_cards("Ah Ac 5s 4d 2h").unwrap(),
        );
        assert_eq!(s.category(), HandCategory::TwoPair);
        assert_eq!(s.tiebreak(), vec![Rank::Ace, Rank::Nine, Rank::Five]);
        // Straight using both hole cards across a 7-card set
        let s = evaluate_hand(
            &[crate::cards::card("6c"), crate::cards::card("7d")],
            &parse_cards("8h 9c Ts 2d 2h").unwrap(),
        );
        assert_eq!(s.category(), HandCategory::Straight);
        assert_eq!(s.tiebreak(), vec![Rank::Ten]);
    }

    /// Every one of the C(52,5) = 2,598,960 five-card hands, checked against the classic
    /// frequency table. This is an independent check of the evaluator's category logic.
    #[test]
    fn five_card_category_distribution_matches_known_counts() {
        let all = crate::cards::Card::all();
        let mut counts = [0u64; 9];
        let mut best_seen = HandStrength(0);
        for a in 0..52 {
            for b in (a + 1)..52 {
                for c in (b + 1)..52 {
                    for d in (c + 1)..52 {
                        for e in (d + 1)..52 {
                            let s = evaluate_five(&[all[a], all[b], all[c], all[d], all[e]]);
                            counts[s.category() as usize] += 1;
                            if s > best_seen {
                                best_seen = s;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(counts.iter().sum::<u64>(), 2_598_960);
        assert_eq!(counts[HandCategory::StraightFlush as usize], 40);
        assert_eq!(counts[HandCategory::FourOfAKind as usize], 624);
        assert_eq!(counts[HandCategory::FullHouse as usize], 3_744);
        assert_eq!(counts[HandCategory::Flush as usize], 5_108);
        assert_eq!(counts[HandCategory::Straight as usize], 10_200);
        assert_eq!(counts[HandCategory::ThreeOfAKind as usize], 54_912);
        assert_eq!(counts[HandCategory::TwoPair as usize], 123_552);
        assert_eq!(counts[HandCategory::Pair as usize], 1_098_240);
        assert_eq!(counts[HandCategory::HighCard as usize], 1_302_540);
        // The best possible hand is a royal flush.
        assert_eq!(best_seen.category(), HandCategory::StraightFlush);
        assert_eq!(best_seen.tiebreak(), vec![Rank::Ace]);
    }

    /// Distinct strengths within a category must order by the classic tie-break rules; check the
    /// number of distinct 5-card hand classes (7462) as a whole-evaluator fingerprint.
    #[test]
    fn number_of_distinct_hand_classes_is_7462() {
        let all = crate::cards::Card::all();
        let mut classes = std::collections::HashSet::new();
        for a in 0..52 {
            for b in (a + 1)..52 {
                for c in (b + 1)..52 {
                    for d in (c + 1)..52 {
                        for e in (d + 1)..52 {
                            classes
                                .insert(evaluate_five(&[all[a], all[b], all[c], all[d], all[e]]).0);
                        }
                    }
                }
            }
        }
        assert_eq!(classes.len(), 7462);
    }

    #[test]
    fn full_house_prefers_higher_trips() {
        let a = evaluate_cards(&parse_cards("As Ad Ah Kc Ks Kd 2c").unwrap());
        assert_eq!(a.category(), HandCategory::FullHouse);
        assert_eq!(a.tiebreak(), vec![Rank::Ace, Rank::King]);
        let b = evaluate_cards(&parse_cards("Ks Kd Kh Ac As 3d 2c").unwrap());
        assert_eq!(b.tiebreak(), vec![Rank::King, Rank::Ace]);
        assert!(a > b);
    }
}
