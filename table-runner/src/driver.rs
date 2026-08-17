//! Drives a single hand: connects the pure engine to `Player`s with deadlines and auto-actions.

use crate::player::{Player, PlayerError};
use poker_utils::{Hand, HandError, HandParams, HandResult, PublicEvent, SeatIndex};
use std::sync::Arc;
use std::time::Duration;

/// Deliver an event to the seated players. `HoleCards` is private and only goes to its owner.
pub async fn broadcast(players: &[Option<Arc<dyn Player>>], event: &PublicEvent) {
    if let PublicEvent::HoleCards { seat, .. } = event {
        if let Some(Some(p)) = players.get(*seat as usize) {
            p.notify(event).await;
        }
        return;
    }
    for p in players.iter().flatten() {
        p.notify(event).await;
    }
}

/// Play one hand to completion. `players` is indexed by seat and must have an entry for every
/// dealt-in seat in `params`. Every emitted event is delivered to the players and appended to the
/// returned log. Timeouts, crashes and illegal actions are replaced by check/fold and announced
/// with an `ActionSubstituted` event.
pub async fn play_hand(
    params: HandParams,
    players: &[Option<Arc<dyn Player>>],
    action_timeout: Duration,
) -> Result<(HandResult, Vec<PublicEvent>), HandError> {
    let mut hand = Hand::start(params)?;
    let mut log: Vec<PublicEvent> = Vec::new();
    flush(&mut hand, players, &mut log).await;

    while let Some(seat) = hand.actor() {
        let ctx = hand.decision_context(seat).expect("actor has context");
        let legal = hand.legal_actions(seat).expect("actor has legal actions");
        let player = players.get(seat as usize).and_then(|p| p.clone());
        let timeout_ms = action_timeout.as_millis() as u64;

        let response = match player {
            Some(p) => {
                match tokio::time::timeout(
                    action_timeout,
                    p.request_action(&ctx, &legal, timeout_ms),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(PlayerError::Timeout),
                }
            }
            None => Err(PlayerError::Dead),
        };

        let (action, substituted_reason) = match response {
            Ok(a) => {
                if legal.allows(&a) {
                    (a, None)
                } else {
                    (legal.auto_action(), Some(format!("illegal action {:?}", a)))
                }
            }
            Err(e) => (legal.auto_action(), Some(e.to_string())),
        };

        if let Some(reason) = substituted_reason {
            let ev = PublicEvent::ActionSubstituted {
                seat,
                reason,
                substituted: action,
            };
            broadcast(players, &ev).await;
            log.push(ev);
        }

        // The auto action is always legal; a rejection here would be an engine bug.
        if let Err(e) = hand.apply(seat, action) {
            tracing::error!(?e, ?action, seat, "engine rejected a legal action");
            let fallback = legal.auto_action();
            hand.apply(seat, fallback)
                .expect("auto action must be legal");
        }
        flush(&mut hand, players, &mut log).await;
    }

    let result = hand.result().cloned().expect("hand complete");
    Ok((result, log))
}

/// Deliver a burst of events to every seated player in one batch each. Private `HoleCards`
/// events are only included for their owner.
pub async fn broadcast_batch(players: &[Option<Arc<dyn Player>>], events: &[PublicEvent]) {
    if events.is_empty() {
        return;
    }
    let has_private = events
        .iter()
        .any(|e| matches!(e, PublicEvent::HoleCards { .. }));
    for (seat, p) in players.iter().enumerate() {
        let Some(p) = p else { continue };
        if has_private {
            let mine: Vec<PublicEvent> = events
                .iter()
                .filter(|e| match e {
                    PublicEvent::HoleCards { seat: s, .. } => *s as usize == seat,
                    _ => true,
                })
                .cloned()
                .collect();
            p.notify_batch(&mine).await;
        } else {
            p.notify_batch(events).await;
        }
    }
}

async fn flush(hand: &mut Hand, players: &[Option<Arc<dyn Player>>], log: &mut Vec<PublicEvent>) {
    let events = hand.take_events();
    broadcast_batch(players, &events).await;
    log.extend(events);
}

/// Convenience for tests: which seat indices are occupied.
pub fn occupied_seats(players: &[Option<Arc<dyn Player>>]) -> Vec<SeatIndex> {
    players
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_some())
        .map(|(i, _)| i as SeatIndex)
        .collect()
}
