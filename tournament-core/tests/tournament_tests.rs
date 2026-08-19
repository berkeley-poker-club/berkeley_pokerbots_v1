use poker_utils::{BlindSchedule, LevelAdvance, TournamentConfig};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use table_runner::{CallStrategy, FoldStrategy, LocalBot, Player, RaiseStrategy, RandomStrategy};
use tournament_core::{PlayerFactory, SeriesRunner, TournamentDirector};

fn fast_cfg() -> TournamentConfig {
    TournamentConfig {
        table_size: 9,
        break_threshold: 7,
        min_table_size: 6,
        starting_stack: 500,
        blinds: BlindSchedule::geometric(5, 10, 2.0),
        level_advance: LevelAdvance {
            max_level_duration_secs: None,
            hands_per_level: Some(10),
            avg_stack_thresholds: vec![],
        },
        action_timeout_ms: 200,
        series_length: 3,
        rng_seed: 7,
        max_hands_per_table: Some(20_000),
    }
}

/// Chips are conserved across every hand, table break and reseat: the winner ends with everything.
fn assert_chips_conserved(out: &tournament_core::TournamentOutcome, cfg: &TournamentConfig) {
    let total: i64 = out.final_stacks.values().sum();
    assert_eq!(
        total,
        out.num_players as i64 * cfg.starting_stack,
        "chips leaked or duplicated"
    );
    if out.aborted.is_none() {
        let w = out.winner.unwrap();
        assert_eq!(out.final_stacks[&w], total, "winner should hold every chip");
        assert!(out
            .final_stacks
            .iter()
            .filter(|(p, _)| **p != w)
            .all(|(_, s)| *s == 0));
    }
}

fn random_players(n: u32, seed: u64) -> Vec<Arc<dyn Player>> {
    (1..=n)
        .map(|i| {
            Arc::new(LocalBot::new(
                i,
                RandomStrategy::new(seed * 1000 + i as u64),
            )) as Arc<dyn Player>
        })
        .collect()
}

/// A mixed field: call stations, min-raisers and random bots.
fn mixed_players(n: u32, seed: u64) -> Vec<Arc<dyn Player>> {
    (1..=n)
        .map(|i| match i % 3 {
            0 => Arc::new(LocalBot::new(i, CallStrategy)) as Arc<dyn Player>,
            1 => Arc::new(LocalBot::new(i, RaiseStrategy)) as Arc<dyn Player>,
            _ => Arc::new(LocalBot::new(
                i,
                RandomStrategy::new(seed * 1000 + i as u64),
            )) as Arc<dyn Player>,
        })
        .collect()
}

#[tokio::test]
async fn heads_up_tournament_produces_winner_and_loser() {
    let players: Vec<Arc<dyn Player>> = vec![
        Arc::new(LocalBot::new(1, RaiseStrategy)),
        Arc::new(LocalBot::new(2, CallStrategy)),
    ];
    let td = TournamentDirector::new(fast_cfg(), "t1", 1);
    let out = td.run(players, None, None).await;
    assert!(out.aborted.is_none(), "{:?}", out.aborted);
    assert_eq!(out.placements.len(), 2);
    let winner = out.winner.unwrap();
    assert_eq!(out.placements[&winner], 1);
    let loser = if winner == 1 { 2 } else { 1 };
    assert_eq!(out.placements[&loser], 2);
    assert!(out.total_hands > 0);
    assert_eq!(out.eliminations.len(), 1);
    assert_chips_conserved(&out, &fast_cfg());
}

#[tokio::test]
async fn fold_bots_blind_off_and_hands_played_orders_placements() {
    // 3 fold bots + 1 call station: folders blind away; the caller wins.
    let players: Vec<Arc<dyn Player>> = vec![
        Arc::new(LocalBot::new(1, FoldStrategy)),
        Arc::new(LocalBot::new(2, FoldStrategy)),
        Arc::new(LocalBot::new(3, FoldStrategy)),
        Arc::new(LocalBot::new(4, CallStrategy)),
    ];
    let mut cfg = fast_cfg();
    cfg.starting_stack = 60;
    let out = TournamentDirector::new(cfg, "t2", 2)
        .run(players, None, None)
        .await;
    assert!(out.aborted.is_none());
    assert_eq!(out.winner, Some(4));
    assert_eq!(out.placements[&4], 1);
    // The eliminated players are ranked by hands played (desc); ties share ranks; every player placed.
    assert_eq!(out.placements.len(), 4);
    let mut places: Vec<usize> = out.placements.values().copied().collect();
    places.sort();
    assert_eq!(places[0], 1);
    for e in &out.eliminations {
        assert_eq!(out.hands_played[&e.player_id], e.hands_played);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_table_tournament_breaks_tables_and_places_everyone() {
    // Call stations only bust when the blinds force them all-in, so this tournament runs through
    // many levels and table breaks before it converges.
    let n = 27;
    let players: Vec<Arc<dyn Player>> = (1..=n)
        .map(|i| Arc::new(LocalBot::new(i, CallStrategy)) as Arc<dyn Player>)
        .collect();
    let td = TournamentDirector::new(fast_cfg(), "t3", 3);
    let out = td.run(players, None, None).await;
    assert!(out.aborted.is_none(), "{:?}", out.aborted);
    assert_eq!(out.placements.len(), n as usize);
    let winner = out.winner.unwrap();
    assert_eq!(out.placements[&winner], 1);
    assert_eq!(out.eliminations.len(), n as usize - 1);
    // Ranks are consistent with hands played: more hands => better (lower) or equal place.
    let mut elims = out.eliminations.clone();
    elims.sort_by_key(|e| std::cmp::Reverse(e.hands_played));
    for w in elims.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(out.placements[&a.player_id] <= out.placements[&b.player_id]);
        if a.hands_played == b.hands_played {
            assert_eq!(out.placements[&a.player_id], out.placements[&b.player_id]);
        }
    }
    assert_chips_conserved(&out, &fast_cfg());
    assert!(out.levels_reached >= 3, "blinds should have advanced");
    assert!(
        out.total_hands >= 3 * 10,
        "each of the 3 tables should play at least a level"
    );
    // Everyone played at least one hand.
    assert!(out.hands_played.values().all(|&h| h >= 1));
}

#[tokio::test]
async fn cancellation_aborts_promptly() {
    let players: Vec<Arc<dyn Player>> = (1..=9)
        .map(|i| {
            Arc::new(LocalBot::new(i, CallStrategy).with_latency(Duration::from_millis(5)))
                as Arc<dyn Player>
        })
        .collect();
    let mut cfg = fast_cfg();
    cfg.starting_stack = 100_000;
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = Arc::clone(&cancel);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.store(true, Ordering::SeqCst);
    });
    let start = std::time::Instant::now();
    let out = TournamentDirector::new(cfg, "t4", 4)
        .run(players, Some(cancel), None)
        .await;
    assert_eq!(out.aborted.as_deref(), Some("cancelled"));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn series_scores_by_geometric_mean() {
    let cfg = fast_cfg();
    let factory: PlayerFactory =
        Arc::new(|i: usize| Box::pin(async move { random_players(12, i as u64 + 100) }));
    let out = SeriesRunner::new(cfg, "s1")
        .parallelism(2)
        .run(Some(4), factory, None, None, None)
        .await;
    assert_eq!(out.tournaments.len(), 4);
    assert_eq!(out.scores.len(), 12);
    for s in &out.scores {
        assert_eq!(s.tournaments_counted, 4);
        assert!(s.geo_mean >= 1.0 && s.geo_mean <= 12.0);
    }
    for w in out.scores.windows(2) {
        assert!(w[0].geo_mean <= w[1].geo_mean);
    }
    let ids: HashSet<u32> = out.scores.iter().map(|s| s.player_id).collect();
    assert_eq!(ids.len(), 12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hundred_player_tournament_completes() {
    let players = mixed_players(200, 9);
    let start = std::time::Instant::now();
    let out = TournamentDirector::new(fast_cfg(), "big", 9)
        .run(players, None, None)
        .await;
    assert!(out.aborted.is_none(), "{:?}", out.aborted);
    assert_eq!(out.placements.len(), 200);
    assert_chips_conserved(&out, &fast_cfg());
    eprintln!(
        "200-player tournament: {} hands, {} levels, {:?}",
        out.total_hands,
        out.levels_reached,
        start.elapsed()
    );
}
