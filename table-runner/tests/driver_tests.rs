use poker_utils::{Deck, HandParams, HandRules, PublicEvent};
use std::sync::Arc;
use std::time::Duration;
use table_runner::{play_hand, Behaviour, CallStrategy, LocalBot, Player, RaiseStrategy};

fn params(stacks: &[i64], button: u8, seed: u64) -> HandParams {
    HandParams {
        hand_id: 1,
        table_id: 7,
        rules: HandRules {
            small_blind: 5,
            big_blind: 10,
            ante: 0,
        },
        button,
        seats: stacks
            .iter()
            .enumerate()
            .map(|(i, &s)| Some((i as u32 + 1, s)))
            .collect(),
        deck: Deck::new(seed),
    }
}

#[tokio::test]
async fn call_stations_reach_showdown() {
    let players: Vec<Option<Arc<dyn Player>>> = (1..=3)
        .map(|i| Some(Arc::new(LocalBot::new(i, CallStrategy)) as Arc<dyn Player>))
        .collect();
    let (result, log) = play_hand(
        params(&[100, 100, 100], 0, 1),
        &players,
        Duration::from_millis(500),
    )
    .await
    .unwrap();
    assert!(result.went_to_showdown);
    assert_eq!(result.seats.iter().map(|s| s.stack).sum::<i64>(), 300);
    assert!(matches!(log.first(), Some(PublicEvent::HandStarted { .. })));
    assert!(matches!(log.last(), Some(PublicEvent::HandEnded { .. })));
    // The private hole-card events are still in the log (once per seat).
    assert_eq!(
        log.iter()
            .filter(|e| matches!(e, PublicEvent::HoleCards { .. }))
            .count(),
        3
    );
}

#[tokio::test]
async fn hung_bot_is_auto_folded_after_timeout() {
    let hung: Arc<dyn Player> =
        Arc::new(LocalBot::new(1, CallStrategy).with_behaviour(Behaviour::Hang));
    let ok: Arc<dyn Player> = Arc::new(LocalBot::new(2, RaiseStrategy));
    let players = vec![Some(hung), Some(ok)];
    let start = std::time::Instant::now();
    let (result, log) = play_hand(
        params(&[100, 100], 0, 2),
        &players,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(log
        .iter()
        .any(|e| matches!(e, PublicEvent::ActionSubstituted { seat: 0, .. })));
    // seat 0 (button/sb) folds preflop after timing out; seat 1 collects the blinds.
    assert!(!result.went_to_showdown);
    assert_eq!(result.seats[1].stack, 105);
}

#[tokio::test]
async fn illegal_and_crashing_bots_are_substituted() {
    let illegal: Arc<dyn Player> =
        Arc::new(LocalBot::new(1, CallStrategy).with_behaviour(Behaviour::Illegal));
    let crash: Arc<dyn Player> =
        Arc::new(LocalBot::new(2, CallStrategy).with_behaviour(Behaviour::Crash));
    let ok: Arc<dyn Player> = Arc::new(LocalBot::new(3, CallStrategy));
    let players = vec![Some(illegal), Some(crash), Some(ok)];
    let (result, log) = play_hand(
        params(&[100, 100, 100], 0, 3),
        &players,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    let subs: Vec<u8> = log
        .iter()
        .filter_map(|e| match e {
            PublicEvent::ActionSubstituted { seat, .. } => Some(*seat),
            _ => None,
        })
        .collect();
    assert!(subs.contains(&0));
    assert!(subs.contains(&1));
    assert_eq!(result.seats.iter().map(|s| s.stack).sum::<i64>(), 300);
}

#[tokio::test]
async fn slow_but_in_time_bot_is_honoured() {
    let slow: Arc<dyn Player> =
        Arc::new(LocalBot::new(1, CallStrategy).with_latency(Duration::from_millis(30)));
    let ok: Arc<dyn Player> = Arc::new(LocalBot::new(2, CallStrategy));
    let players = vec![Some(slow), Some(ok)];
    let (_result, log) = play_hand(
        params(&[100, 100], 0, 4),
        &players,
        Duration::from_millis(500),
    )
    .await
    .unwrap();
    assert!(!log
        .iter()
        .any(|e| matches!(e, PublicEvent::ActionSubstituted { .. })));
}
