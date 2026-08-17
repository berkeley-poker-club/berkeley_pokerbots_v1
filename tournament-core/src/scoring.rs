//! Placements and leaderboard scoring (SPEC.md §"Placements, Ties, and Finalization" and
//! §"Leaderboard Update").

use poker_utils::PlayerId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Rank players. The winner (if any) is placed 1st. Everyone else is ranked by hands played,
/// descending; players with the same number of hands played share a rank and the next rank is
/// skipped accordingly (competition ranking: 1, 2, 2, 4).
pub fn finalize_placements(
    eliminated: &[(PlayerId, u64)],
    winner: Option<PlayerId>,
) -> HashMap<PlayerId, usize> {
    let mut placements = HashMap::new();
    let mut place = 1usize;
    if let Some(w) = winner {
        placements.insert(w, 1);
        place = 2;
    }
    let mut sorted: Vec<(PlayerId, u64)> = eliminated.to_vec();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j < sorted.len() && sorted[j].1 == sorted[i].1 {
            j += 1;
        }
        for item in &sorted[i..j] {
            placements.insert(item.0, place);
        }
        place += j - i;
        i = j;
    }
    placements
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreEntry {
    pub player_id: PlayerId,
    /// Geometric mean of placements (lower is better).
    pub geo_mean: f64,
    pub tournaments_counted: usize,
    /// 1-based rank on the leaderboard (ties share a rank).
    pub rank: usize,
}

/// Geometric mean of each player's placements, sorted ascending (best first).
/// Players with no placements are omitted.
pub fn geometric_mean_scores(series: &HashMap<PlayerId, Vec<usize>>) -> Vec<ScoreEntry> {
    let mut entries: Vec<ScoreEntry> = series
        .iter()
        .filter(|(_, places)| !places.is_empty())
        .map(|(&player_id, places)| {
            let sum_ln: f64 = places.iter().map(|&p| (p.max(1) as f64).ln()).sum();
            ScoreEntry {
                player_id,
                geo_mean: (sum_ln / places.len() as f64).exp(),
                tournaments_counted: places.len(),
                rank: 0,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        a.geo_mean
            .total_cmp(&b.geo_mean)
            .then(a.player_id.cmp(&b.player_id))
    });
    let mut rank = 0usize;
    let mut prev: Option<f64> = None;
    for (i, e) in entries.iter_mut().enumerate() {
        match prev {
            Some(p) if (p - e.geo_mean).abs() < 1e-9 => {}
            _ => rank = i + 1,
        }
        e.rank = rank;
        prev = Some(e.geo_mean);
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements_follow_spec_with_ties() {
        // winner 10; eliminated: 1 (30 hands), 2 (30 hands), 3 (12 hands), 4 (5 hands)
        let elim = vec![(4, 5), (3, 12), (1, 30), (2, 30)];
        let p = finalize_placements(&elim, Some(10));
        assert_eq!(p[&10], 1);
        assert_eq!(p[&1], 2);
        assert_eq!(p[&2], 2);
        assert_eq!(p[&3], 4);
        assert_eq!(p[&4], 5);
    }

    #[test]
    fn placements_without_winner() {
        let p = finalize_placements(&[(1, 3), (2, 3)], None);
        assert_eq!(p[&1], 1);
        assert_eq!(p[&2], 1);
    }

    #[test]
    fn geometric_mean_and_ranks() {
        let mut s = HashMap::new();
        s.insert(1, vec![1, 4]); // gm 2
        s.insert(2, vec![2, 2]); // gm 2
        s.insert(3, vec![9, 1]); // gm 3
        s.insert(4, vec![]);
        let scores = geometric_mean_scores(&s);
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].player_id, 1);
        assert_eq!(scores[0].rank, 1);
        assert_eq!(scores[1].player_id, 2);
        assert_eq!(scores[1].rank, 1);
        assert_eq!(scores[2].player_id, 3);
        assert_eq!(scores[2].rank, 3);
        assert!((scores[2].geo_mean - 3.0).abs() < 1e-9);
    }
}
