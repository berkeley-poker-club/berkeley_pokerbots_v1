//! The `Player` abstraction: anything that can observe events and answer decision requests.

use async_trait::async_trait;
use poker_utils::{Action, DecisionContext, LegalActions, PlayerId, PublicEvent};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlayerError {
    #[error("player did not answer before the deadline")]
    Timeout,
    #[error("communication with the player failed: {0}")]
    CommunicationFailed(String),
    #[error("player sent an unparseable response: {0}")]
    InvalidResponse(String),
    #[error("player process is not running")]
    Dead,
}

/// A participant endpoint. Implementations must be cheap to call concurrently for *different*
/// players; the engine never issues two concurrent `request_action`s to the same player.
#[async_trait]
pub trait Player: Send + Sync {
    fn player_id(&self) -> PlayerId;

    /// Human-readable name for logs and leaderboards.
    fn display_name(&self) -> String {
        format!("player-{}", self.player_id())
    }

    /// Deliver a public event. Must not block for long; failures are ignored by the engine.
    async fn notify(&self, event: &PublicEvent);

    /// Deliver several events at once (in order). Implementations may batch I/O; the default
    /// simply calls [`Player::notify`] for each event.
    async fn notify_batch(&self, events: &[PublicEvent]) {
        for e in events {
            self.notify(e).await;
        }
    }

    /// Ask for a decision. `timeout_ms` is advisory — the engine enforces its own deadline.
    async fn request_action(
        &self,
        ctx: &DecisionContext,
        legal: &LegalActions,
        timeout_ms: u64,
    ) -> Result<Action, PlayerError>;

    /// Called once the player is no longer needed (eliminated or tournament over).
    async fn shutdown(&self) {}

    /// Whether the endpoint is still usable (e.g. process alive).
    fn is_alive(&self) -> bool {
        true
    }
}

pub type SharedPlayer = Arc<dyn Player>;
