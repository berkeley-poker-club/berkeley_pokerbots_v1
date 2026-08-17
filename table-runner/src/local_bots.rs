//! In-process bots. Used for tests, load testing and as the reference strategies behind the
//! `pokerbots stdio-bot` command.

use crate::player::{Player, PlayerError};
use async_trait::async_trait;
use poker_utils::{Action, DecisionContext, LegalActions, PlayerId, PublicEvent};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::sync::Mutex;
use std::time::Duration;

/// A synchronous decision function. Implement this to get a `Player` for free via [`LocalBot`].
pub trait Strategy: Send {
    fn act(&mut self, ctx: &DecisionContext, legal: &LegalActions) -> Action;
    fn on_event(&mut self, _event: &PublicEvent) {}
    fn name(&self) -> &'static str {
        "custom"
    }
}

/// Always folds (checks when free).
#[derive(Default, Clone, Debug)]
pub struct FoldStrategy;

impl Strategy for FoldStrategy {
    fn act(&mut self, _ctx: &DecisionContext, legal: &LegalActions) -> Action {
        legal.auto_action()
    }
    fn name(&self) -> &'static str {
        "fold"
    }
}

/// Calls everything, never bets.
#[derive(Default, Clone, Debug)]
pub struct CallStrategy;

impl Strategy for CallStrategy {
    fn act(&mut self, _ctx: &DecisionContext, legal: &LegalActions) -> Action {
        if legal.can_check {
            Action::Check
        } else if legal.can_call {
            Action::Call
        } else {
            legal.auto_action()
        }
    }
    fn name(&self) -> &'static str {
        "callstation"
    }
}

/// Bets/raises the minimum whenever allowed, otherwise calls.
#[derive(Default, Clone, Debug)]
pub struct RaiseStrategy;

impl Strategy for RaiseStrategy {
    fn act(&mut self, _ctx: &DecisionContext, legal: &LegalActions) -> Action {
        if legal.can_bet {
            Action::BetTo {
                amount: legal.min_bet_to,
            }
        } else if legal.can_raise {
            Action::RaiseTo {
                amount: legal.min_raise_to,
            }
        } else {
            CallStrategy.act(_ctx, legal)
        }
    }
    fn name(&self) -> &'static str {
        "raiser"
    }
}

/// Uniformly random legal action with random sizing (seeded, deterministic).
#[derive(Clone, Debug)]
pub struct RandomStrategy {
    rng: StdRng,
    pub fold_weight: u32,
    pub passive_weight: u32,
    pub aggressive_weight: u32,
    pub all_in_weight: u32,
}

impl RandomStrategy {
    pub fn new(seed: u64) -> Self {
        RandomStrategy {
            rng: StdRng::seed_from_u64(seed),
            fold_weight: 15,
            passive_weight: 55,
            aggressive_weight: 28,
            all_in_weight: 2,
        }
    }

    /// Random "to" amount in `[min, max]`, strongly biased towards the minimum (cubic).
    fn sized(&mut self, min: i64, max: i64) -> i64 {
        if max <= min {
            return min;
        }
        let r: f64 = self.rng.random::<f64>();
        let frac = r * r * r;
        min + ((max - min) as f64 * frac).round() as i64
    }
}

impl Strategy for RandomStrategy {
    fn act(&mut self, _ctx: &DecisionContext, legal: &LegalActions) -> Action {
        let total =
            self.fold_weight + self.passive_weight + self.aggressive_weight + self.all_in_weight;
        let roll = self.rng.random_range(0..total.max(1));
        let mut acc = self.fold_weight;
        if roll < acc {
            return if legal.can_check {
                Action::Check
            } else {
                Action::Fold
            };
        }
        acc += self.passive_weight;
        if roll < acc {
            return CallStrategy.act(_ctx, legal);
        }
        acc += self.aggressive_weight;
        if roll < acc {
            if legal.can_bet {
                let amt = self.sized(legal.min_bet_to, legal.max_bet_to);
                return Action::BetTo { amount: amt };
            }
            if legal.can_raise {
                let amt = self.sized(legal.min_raise_to, legal.max_raise_to);
                return Action::RaiseTo { amount: amt };
            }
            return CallStrategy.act(_ctx, legal);
        }
        if legal.can_all_in {
            Action::AllIn
        } else {
            CallStrategy.act(_ctx, legal)
        }
    }
    fn name(&self) -> &'static str {
        "random"
    }
}

/// A `Player` wrapping a synchronous [`Strategy`], with optional artificial latency and failure
/// modes for testing the engine's timeout/illegal-action handling.
pub struct LocalBot<S: Strategy> {
    id: PlayerId,
    name: String,
    strategy: Mutex<S>,
    latency: Option<Duration>,
    behaviour: Behaviour,
    alive: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    Normal,
    /// Never answers (simulates a hung bot).
    Hang,
    /// Always returns an illegal `RaiseTo` of a huge amount.
    Illegal,
    /// Reports communication failure (simulates a crash).
    Crash,
}

impl<S: Strategy> LocalBot<S> {
    pub fn new(id: PlayerId, strategy: S) -> Self {
        LocalBot {
            id,
            name: format!("{}-{}", strategy.name(), id),
            strategy: Mutex::new(strategy),
            latency: None,
            behaviour: Behaviour::Normal,
            alive: std::sync::atomic::AtomicBool::new(true),
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = Some(latency);
        self
    }

    pub fn with_behaviour(mut self, behaviour: Behaviour) -> Self {
        self.behaviour = behaviour;
        self
    }
}

#[async_trait]
impl<S: Strategy + 'static> Player for LocalBot<S> {
    fn player_id(&self) -> PlayerId {
        self.id
    }

    fn display_name(&self) -> String {
        self.name.clone()
    }

    async fn notify(&self, event: &PublicEvent) {
        if let Ok(mut s) = self.strategy.lock() {
            s.on_event(event);
        }
    }

    async fn request_action(
        &self,
        ctx: &DecisionContext,
        legal: &LegalActions,
        _timeout_ms: u64,
    ) -> Result<Action, PlayerError> {
        match self.behaviour {
            Behaviour::Hang => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            Behaviour::Crash => {
                return Err(PlayerError::CommunicationFailed("simulated crash".into()))
            }
            Behaviour::Illegal => {
                return Ok(Action::RaiseTo {
                    amount: i64::MAX / 4,
                })
            }
            Behaviour::Normal => {}
        }
        if let Some(l) = self.latency {
            tokio::time::sleep(l).await;
        }
        let action = self
            .strategy
            .lock()
            .map_err(|_| PlayerError::CommunicationFailed("poisoned".into()))?
            .act(ctx, legal);
        Ok(action)
    }

    async fn shutdown(&self) {
        self.alive.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Build a strategy by name: `fold`, `callstation`/`call`, `raiser`/`raise`, `random`.
pub fn strategy_by_name(name: &str, seed: u64) -> Option<Box<dyn Strategy>> {
    Some(match name {
        "fold" => Box::new(FoldStrategy),
        "call" | "callstation" => Box::new(CallStrategy),
        "raise" | "raiser" => Box::new(RaiseStrategy),
        "random" => Box::new(RandomStrategy::new(seed)),
        _ => return None,
    })
}

impl Strategy for Box<dyn Strategy> {
    fn act(&mut self, ctx: &DecisionContext, legal: &LegalActions) -> Action {
        (**self).act(ctx, legal)
    }
    fn on_event(&mut self, event: &PublicEvent) {
        (**self).on_event(event)
    }
    fn name(&self) -> &'static str {
        (**self).name()
    }
}
