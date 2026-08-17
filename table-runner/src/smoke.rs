//! Protocol smoke test: does a bot start, speak JSON lines and answer a decision request with a
//! legal action in time? Used on submission upload and by `pokerbots smoke`.

use crate::player::{Player, PlayerError};
use crate::process_bot::ProcessBot;
use poker_utils::{Action, Deck, Hand, HandParams, HandRules, PublicEvent};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeFailure {
    Timeout,
    InvalidJson,
    IllegalAction,
    Crashed,
    /// The bot could not be prepared or started (bad archive, missing entrypoint, spawn error).
    SetupFailed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmokeReport {
    pub passed: bool,
    /// Round-trip latency of the first decision request (includes interpreter start-up).
    pub first_latency_ms: u64,
    /// Round-trip latency of a second request (steady state).
    pub latency_ms: u64,
    pub reason: Option<SmokeFailure>,
    pub message: String,
    pub stderr_tail: String,
    pub actions: Vec<Action>,
}

/// Run the smoke test against an already-spawned bot. Two decision requests are issued from a
/// synthetic 3-handed hand; both must be answered legally within `timeout`.
pub async fn smoke_test(bot: &ProcessBot, timeout: Duration) -> SmokeReport {
    let mut report = SmokeReport {
        passed: false,
        first_latency_ms: 0,
        latency_ms: 0,
        reason: None,
        message: String::new(),
        stderr_tail: String::new(),
        actions: Vec::new(),
    };
    let my_seat: u8 = 0;
    let params = HandParams {
        hand_id: 1,
        table_id: 0,
        rules: HandRules {
            small_blind: 5,
            big_blind: 10,
            ante: 0,
        },
        button: 1,
        seats: vec![
            Some((bot.player_id(), 1000)),
            Some((bot.player_id().wrapping_add(1), 1000)),
            Some((bot.player_id().wrapping_add(2), 1000)),
        ],
        deck: Deck::new(12345),
    };
    // Button is seat 1, so seat 2 posts SB, seat 0 posts BB and seat 1 acts first. Fold seat 1
    // and have seat 2 call, so the bot (seat 0, BB) gets the option: check/raise both legal.
    let mut hand = Hand::start(params).expect("valid smoke hand");
    for ev in hand.take_events() {
        if matches!(ev, PublicEvent::HoleCards { seat, .. } if seat != my_seat) {
            continue;
        }
        bot.notify(&ev).await;
    }
    hand.apply(1, Action::Fold).expect("legal");
    hand.apply(2, Action::Call).expect("legal");
    for ev in hand.take_events() {
        bot.notify(&ev).await;
    }
    debug_assert_eq!(hand.actor(), Some(my_seat));

    for round in 0..2 {
        let ctx = hand.decision_context(my_seat).expect("actor");
        let legal = hand.legal_actions(my_seat).expect("actor");
        let start = Instant::now();
        let res = tokio::time::timeout(
            timeout,
            bot.request_action(&ctx, &legal, timeout.as_millis() as u64),
        )
        .await;
        let elapsed = start.elapsed().as_millis() as u64;
        if round == 0 {
            report.first_latency_ms = elapsed;
        } else {
            report.latency_ms = elapsed;
        }
        let action = match res {
            Err(_) | Ok(Err(PlayerError::Timeout)) => {
                report.reason = Some(if bot.is_alive() {
                    SmokeFailure::Timeout
                } else {
                    SmokeFailure::Crashed
                });
                report.message = format!(
                    "no answer to request {} within {} ms",
                    round + 1,
                    timeout.as_millis()
                );
                break;
            }
            Ok(Err(PlayerError::Dead)) | Ok(Err(PlayerError::CommunicationFailed(_))) => {
                report.reason = Some(SmokeFailure::Crashed);
                report.message = "bot process exited or closed its pipes".into();
                break;
            }
            Ok(Err(PlayerError::InvalidResponse(m))) => {
                report.reason = Some(SmokeFailure::InvalidJson);
                report.message = m;
                break;
            }
            Ok(Ok(a)) => a,
        };
        report.actions.push(action);
        if !legal.allows(&action) {
            report.reason = Some(SmokeFailure::IllegalAction);
            report.message = format!(
                "action {:?} is not legal here; legal actions: {:?}",
                action, legal
            );
            break;
        }
        // Keep the bot to act: on round 0 the bot may check or raise. If it checks, the flop is
        // dealt and seat 2 acts first; make seat 2 check so the bot acts again.
        hand.apply(my_seat, action).expect("validated");
        for ev in hand.take_events() {
            bot.notify(&ev).await;
        }
        if round == 0 {
            // Advance until it is the bot's turn again (at most a couple of opponent actions).
            let mut guard = 0;
            while hand.actor().is_some() && hand.actor() != Some(my_seat) && guard < 8 {
                let s = hand.actor().unwrap();
                let l = hand.legal_actions(s).unwrap();
                let a = if l.can_check {
                    Action::Check
                } else {
                    Action::Call
                };
                hand.apply(s, a).expect("legal");
                for ev in hand.take_events() {
                    bot.notify(&ev).await;
                }
                guard += 1;
            }
            if hand.actor() != Some(my_seat) {
                // Hand ended (e.g. bot shoved and everyone called); one legal answer is enough.
                report.latency_ms = report.first_latency_ms;
                break;
            }
        }
    }

    if report.reason.is_none() {
        report.passed = true;
        report.message = "ok".into();
        if report.invalid_lines_hint(bot) {
            report.message = format!(
                "ok (but {} unparseable line(s) on stdout — keep stdout for protocol messages only)",
                bot.invalid_line_count()
            );
        }
    }
    report.stderr_tail = bot.stderr_tail();
    report
}

impl SmokeReport {
    fn invalid_lines_hint(&self, bot: &ProcessBot) -> bool {
        bot.invalid_line_count() > 0
    }
}
