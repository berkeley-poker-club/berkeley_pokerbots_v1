//! Wire format of the bot protocol (JSON Lines over stdin/stdout). See `docs/BOT_PROTOCOL.md`.

use poker_utils::{Action, DecisionContext, LegalActions, PlayerId, PublicEvent};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1";

/// Engine → bot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum EngineMessage {
    /// First message after the process starts.
    Hello {
        protocol_version: String,
        player_id: PlayerId,
        /// Free-form identifier of the run (tournament id, "smoke-test", ...).
        session: String,
    },
    NotifyEvent {
        event: PublicEvent,
    },
    RequestAction {
        request_id: String,
        deadline_ms: u64,
        context: DecisionContext,
        legal: LegalActions,
    },
    /// Last message; the process should exit promptly.
    Goodbye,
}

/// Bot → engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BotMessage {
    Action {
        request_id: String,
        action: Action,
    },
    /// Optional acknowledgement of `hello`; ignored by the engine.
    HelloAck {
        #[serde(default)]
        name: Option<String>,
    },
    /// Free-form log line, echoed to the bot's stderr log by the engine.
    Log {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_wire_format_matches_docs() {
        let m = BotMessage::Action {
            request_id: "req_1".into(),
            action: Action::RaiseTo { amount: 200 },
        };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"action","request_id":"req_1","action":{"kind":"RaiseTo","amount":200}}"#
        );
        let parsed: BotMessage =
            serde_json::from_str(r#"{"type":"action","request_id":"r","action":{"kind":"Fold"}}"#)
                .unwrap();
        assert_eq!(
            parsed,
            BotMessage::Action {
                request_id: "r".into(),
                action: Action::Fold
            }
        );
    }
}
