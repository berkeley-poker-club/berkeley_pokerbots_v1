use poker_utils::LevelSpec;
use std::sync::Arc;
use std::time::Duration;
use table_runner::{
    spawn_table, CallStrategy, LocalBot, Player, RaiseStrategy, TableCommand, TableConfig,
    TableEvent,
};
use tokio::sync::mpsc;

fn cfg(id: u64) -> TableConfig {
    TableConfig {
        table_id: id,
        capacity: 6,
        action_timeout: Duration::from_millis(200),
        seed: id,
        initial_level: LevelSpec {
            level_id: 0,
            small_blind: 5,
            big_blind: 10,
            ante: 0,
        },
        record_events: true,
    }
}

async fn next_event(rx: &mut mpsc::Receiver<TableEvent>) -> TableEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("event within 10s")
        .expect("channel open")
}

#[tokio::test]
async fn table_plays_until_one_player_remains_then_idles() {
    let (tx, mut rx) = mpsc::channel(64);
    let table = spawn_table(cfg(1), tx);
    for i in 1..=3u32 {
        let p: Arc<dyn Player> = Arc::new(LocalBot::new(i, RaiseStrategy));
        assert!(table.send(TableCommand::Seat {
            player: p,
            stack: 50
        }));
    }
    let mut hands = 0;
    let mut busted = Vec::new();
    loop {
        match next_event(&mut rx).await {
            TableEvent::HandCompleted {
                result,
                seated_after,
                events,
                ..
            } => {
                hands += 1;
                assert!(!events.is_empty());
                busted.extend(result.busted.clone());
                if seated_after <= 1 {
                    // Idle must follow.
                    assert!(matches!(
                        next_event(&mut rx).await,
                        TableEvent::Idle { seated: 1, .. }
                    ));
                    break;
                }
            }
            TableEvent::Error { message, .. } => panic!("table error: {message}"),
            other => panic!("unexpected event {:?}", other),
        }
        assert!(hands < 500, "tournament did not converge");
    }
    assert_eq!(busted.len(), 2);
    let mut table = table;
    table.send(TableCommand::Close);
    match next_event(&mut rx).await {
        TableEvent::Closed { players, .. } => {
            assert_eq!(players.len(), 1);
            assert_eq!(players[0].1, 150, "winner holds all the chips");
        }
        other => panic!("unexpected {:?}", other),
    }
    table.join().await;
}

#[tokio::test]
async fn pause_resume_and_blind_changes_apply_at_hand_boundaries() {
    let (tx, mut rx) = mpsc::channel(64);
    let mut table = spawn_table(cfg(2), tx);
    for i in 1..=2u32 {
        // Slow bots so the test can issue commands between hands deterministically.
        let p: Arc<dyn Player> =
            Arc::new(LocalBot::new(i, CallStrategy).with_latency(Duration::from_millis(20)));
        table.send(TableCommand::Seat {
            player: p,
            stack: 10_000,
        });
    }
    // Let one hand complete, then pause.
    let first = next_event(&mut rx).await;
    assert!(matches!(first, TableEvent::HandCompleted { level: 0, .. }));
    table.send(TableCommand::PauseAfterHand);
    // Drain until Paused; every completed hand before that is at level 0.
    loop {
        match next_event(&mut rx).await {
            TableEvent::HandCompleted { level, .. } => assert_eq!(level, 0),
            TableEvent::Paused { seated, .. } => {
                assert_eq!(seated, 2);
                break;
            }
            other => panic!("unexpected {:?}", other),
        }
    }
    // While paused, no hands are played.
    assert!(tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .is_err());
    table.send(TableCommand::ApplyBlinds(LevelSpec {
        level_id: 3,
        small_blind: 50,
        big_blind: 100,
        ante: 10,
    }));
    table.send(TableCommand::Resume);
    match next_event(&mut rx).await {
        TableEvent::HandCompleted { level, result, .. } => {
            assert_eq!(level, 3);
            // Antes + blinds moved chips: with two call stations the pot is 2*10 antes + 200 blinds
            let total: i64 = result.seats.iter().map(|s| s.stack).sum();
            assert_eq!(total, 20_000);
        }
        other => panic!("unexpected {:?}", other),
    }
    table.send(TableCommand::Close);
    loop {
        if let TableEvent::Closed { players, .. } = next_event(&mut rx).await {
            assert_eq!(players.len(), 2);
            break;
        }
    }
    table.join().await;
}

#[tokio::test]
async fn late_seated_players_join_at_next_hand() {
    let (tx, mut rx) = mpsc::channel(64);
    let mut table = spawn_table(cfg(3), tx);
    let p1: Arc<dyn Player> =
        Arc::new(LocalBot::new(1, CallStrategy).with_latency(Duration::from_millis(20)));
    table.send(TableCommand::Seat {
        player: p1,
        stack: 1000,
    });
    // Only one player: idle.
    assert!(matches!(
        next_event(&mut rx).await,
        TableEvent::Idle { seated: 1, .. }
    ));
    let p2: Arc<dyn Player> =
        Arc::new(LocalBot::new(2, CallStrategy).with_latency(Duration::from_millis(20)));
    table.send(TableCommand::Seat {
        player: p2,
        stack: 1000,
    });
    match next_event(&mut rx).await {
        TableEvent::HandCompleted { result, .. } => {
            assert_eq!(result.participants.len(), 2);
        }
        other => panic!("unexpected {:?}", other),
    }
    let p3: Arc<dyn Player> =
        Arc::new(LocalBot::new(3, CallStrategy).with_latency(Duration::from_millis(20)));
    table.send(TableCommand::Seat {
        player: p3,
        stack: 500,
    });
    // Within a couple of hands, the third player participates.
    let mut saw_three = false;
    for _ in 0..70 {
        if let TableEvent::HandCompleted { result, .. } = next_event(&mut rx).await {
            if result.participants.len() == 3 {
                saw_three = true;
                break;
            }
        }
    }
    assert!(saw_three);
    table.send(TableCommand::Close);
    table.join().await;
}
