use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use crate::hands::HandStrength;

pub type PlayerId = usize;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PotManager {
    pub main_pot: Pot,
    pub side_pots: Vec<Pot>,
    pub total_committed: HashMap<PlayerId, i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pot {
    pub amount: i64,
    pub eligible_players: HashSet<PlayerId>,
    pub cap_per_player: Option<i64>,
}

impl PotManager {
    pub fn new() -> Self {
        PotManager {
            main_pot: Pot {
                amount: 0,
                eligible_players: HashSet::new(),
                cap_per_player: None,
            },
            side_pots: Vec::new(),
            total_committed: HashMap::new(),
        }
    }

    pub fn commit_chips(&mut self, player: PlayerId, amount: i64) {
        *self.total_committed.entry(player).or_insert(0) += amount;
        self.main_pot.amount += amount;
        self.main_pot.eligible_players.insert(player);
    }

    pub fn build_side_pots(&mut self) -> Vec<PotEvent> {
        if self.total_committed.is_empty() {
            return vec![];
        }

        let mut events = vec![];

        let mut commitments: Vec<(PlayerId, i64)> = self.total_committed.iter()
            .map(|(&player, &amount)| (player, amount))
            .collect();
        commitments.sort_by_key(|(_, amount)| *amount);

        let mut new_pots = vec![];
        let mut previous_level = 0i64;

        for (i, &(_, commitment_level)) in commitments.iter().enumerate() {
            if commitment_level > previous_level {
                let delta = commitment_level - previous_level;
                let eligible_count = commitments.len() - i;
                let pot_amount = delta * (eligible_count as i64);

                let eligible_players: HashSet<PlayerId> = commitments[i..]
                    .iter()
                    .map(|(player, _)| *player)
                    .collect();

                let pot = Pot {
                    amount: pot_amount,
                    eligible_players,
                    cap_per_player: Some(commitment_level),
                };

                events.push(PotEvent::SidePotCreated {
                    amount: pot_amount,
                    eligible_players: pot.eligible_players.clone(),
                });
                new_pots.push(pot);

                previous_level = commitment_level;
            }
        }

        if new_pots.is_empty() {
            new_pots.push(Pot {
                amount: self.main_pot.amount,
                eligible_players: self.main_pot.eligible_players.clone(),
                cap_per_player: None,
            });
        }

        self.main_pot = new_pots.remove(0);
        self.side_pots = new_pots;

        events
    }

    pub fn distribute_to_winners(&mut self, winners: &[(PlayerId, HandStrength)]) -> Vec<PotEvent> {
        let mut events = vec![];
        let mut all_pots = vec![self.main_pot.clone()];
        all_pots.extend(self.side_pots.clone());

        for pot in all_pots {
            if pot.amount == 0 {
                continue;
            }

            let eligible_winners: Vec<(PlayerId, HandStrength)> = winners
                .iter()
                .filter(|(player, _)| pot.eligible_players.contains(player))
                .cloned()
                .collect();

            if eligible_winners.is_empty() {
                continue;
            }

            let best_hand_strength = eligible_winners
                .iter()
                .map(|(_, strength)| *strength)
                .max()
                .unwrap();

            let actual_winners: Vec<PlayerId> = eligible_winners
                .iter()
                .filter(|(_, strength)| *strength == best_hand_strength)
                .map(|(player, _)| *player)
                .collect();

            let per_winner = pot.amount / (actual_winners.len() as i64);
            let remainder = pot.amount % (actual_winners.len() as i64);

            for (i, &winner) in actual_winners.iter().enumerate() {
                let amount = per_winner + if (i as i64) < remainder { 1 } else { 0 };
                events.push(PotEvent::PotAwarded {
                    player: winner,
                    amount,
                });
            }
        }

        self.main_pot.amount = 0;
        self.side_pots.clear();
        self.total_committed.clear();

        events
    }

    pub fn total_pot_size(&self) -> i64 {
        self.main_pot.amount + self.side_pots.iter().map(|p| p.amount).sum::<i64>()
    }
}

impl Default for PotManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PotEvent {
    SidePotCreated {
        amount: i64,
        eligible_players: HashSet<PlayerId>,
    },
    PotAwarded {
        player: PlayerId,
        amount: i64,
    },
}