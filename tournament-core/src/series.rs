//! Series runner: N tournaments over the same players, scored by geometric-mean placement.

use crate::director::{CancelFlag, HandRecord, TournamentDirector, TournamentOutcome};
use crate::scoring::{geometric_mean_scores, ScoreEntry};
use futures::stream::{FuturesUnordered, StreamExt};
use poker_utils::{PlayerId, TournamentConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use table_runner::Player;
use tokio::sync::mpsc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Callback invoked after each tournament completes.
pub type OnTournamentDone = Box<dyn FnMut(&TournamentOutcome) + Send>;

/// Creates a fresh set of players for tournament `index` (bots are spawned per tournament).
pub type PlayerFactory = Arc<dyn Fn(usize) -> BoxFuture<Vec<Arc<dyn Player>>> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesOutcome {
    pub series_id: String,
    pub tournaments: Vec<TournamentOutcome>,
    /// Sorted best-first.
    pub scores: Vec<ScoreEntry>,
    pub placements_by_player: HashMap<PlayerId, Vec<usize>>,
}

pub struct SeriesRunner {
    pub cfg: TournamentConfig,
    pub series_id: String,
    /// How many tournaments run concurrently.
    pub parallelism: usize,
    pub record_hands: bool,
}

impl SeriesRunner {
    pub fn new(cfg: TournamentConfig, series_id: impl Into<String>) -> Self {
        SeriesRunner {
            cfg,
            series_id: series_id.into(),
            parallelism: 1,
            record_hands: false,
        }
    }

    pub fn parallelism(mut self, p: usize) -> Self {
        self.parallelism = p.max(1);
        self
    }

    pub fn record_hands(mut self, on: bool) -> Self {
        self.record_hands = on;
        self
    }

    /// Run `length` tournaments (default `cfg.series_length`), calling `on_done` after each.
    pub async fn run(
        &self,
        length: Option<usize>,
        factory: PlayerFactory,
        cancel: Option<CancelFlag>,
        hand_log: Option<mpsc::Sender<HandRecord>>,
        mut on_done: Option<OnTournamentDone>,
    ) -> SeriesOutcome {
        let length = length.unwrap_or(self.cfg.series_length);
        let mut outcomes: Vec<Option<TournamentOutcome>> = vec![None; length];
        let mut in_flight = FuturesUnordered::new();
        let mut next = 0usize;

        let launch = |i: usize| {
            let cfg = self.cfg.clone();
            let id = format!("{}#{}", self.series_id, i + 1);
            let seed = table_runner::mix(cfg.rng_seed, i as u64 + 1);
            let factory = Arc::clone(&factory);
            let cancel = cancel.clone();
            let hand_log = hand_log.clone();
            let record = self.record_hands;
            async move {
                let players = factory(i).await;
                let td = TournamentDirector::new(cfg, id, seed).with_hand_records(record);
                let outcome = td.run(players, cancel, hand_log).await;
                (i, outcome)
            }
        };

        while next < length && in_flight.len() < self.parallelism {
            in_flight.push(launch(next));
            next += 1;
        }
        while let Some((i, outcome)) = in_flight.next().await {
            if let Some(cb) = on_done.as_mut() {
                cb(&outcome);
            }
            let cancelled = outcome.aborted.as_deref() == Some("cancelled");
            outcomes[i] = Some(outcome);
            if !cancelled && next < length {
                in_flight.push(launch(next));
                next += 1;
            }
        }

        let tournaments: Vec<TournamentOutcome> = outcomes.into_iter().flatten().collect();
        let mut by_player: HashMap<PlayerId, Vec<usize>> = HashMap::new();
        for t in &tournaments {
            if t.aborted.is_some() && t.aborted.as_deref() != Some("max_hands") {
                continue;
            }
            for (p, place) in &t.placements {
                by_player.entry(*p).or_default().push(*place);
            }
        }
        let scores = geometric_mean_scores(&by_player);
        SeriesOutcome {
            series_id: self.series_id.clone(),
            tournaments,
            scores,
            placements_by_player: by_player,
        }
    }
}
