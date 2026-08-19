//! Exercises the JSON-lines protocol against the reference Python bots. Requires `python3`.

use poker_utils::{Deck, HandParams, HandRules, PublicEvent};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use table_runner::{play_hand, Player, ProcessBot, SpawnOptions};

fn bots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bots/python")
}

fn python() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string())
}

async fn spawn_py(script: &str, id: u32) -> ProcessBot {
    let path = bots_dir().join(script);
    ProcessBot::spawn(
        &python(),
        &[path.to_string_lossy().to_string()],
        SpawnOptions::new(id, script).session("test"),
    )
    .await
    .expect("spawn python bot")
}

fn params(n: usize, seed: u64) -> HandParams {
    HandParams {
        hand_id: 1,
        table_id: 1,
        rules: HandRules {
            small_blind: 5,
            big_blind: 10,
            ante: 0,
        },
        button: 0,
        seats: (0..n).map(|i| Some((i as u32 + 1, 200))).collect(),
        deck: Deck::new(seed),
    }
}

#[tokio::test]
async fn python_reference_bots_play_a_hand() {
    let scripts = [
        "call_bot.py",
        "raise_bot.py",
        "random_bot.py",
        "fold_bot.py",
    ];
    let mut players: Vec<Option<Arc<dyn Player>>> = Vec::new();
    let mut bots = Vec::new();
    for (i, s) in scripts.iter().enumerate() {
        let b = Arc::new(spawn_py(s, i as u32 + 1).await);
        bots.push(Arc::clone(&b));
        players.push(Some(b as Arc<dyn Player>));
    }
    let (result, log) = play_hand(params(4, 11), &players, Duration::from_millis(2000))
        .await
        .unwrap();
    let subs: Vec<&PublicEvent> = log
        .iter()
        .filter(|e| matches!(e, PublicEvent::ActionSubstituted { .. }))
        .collect();
    assert!(subs.is_empty(), "no substitutions expected, got {:?}", subs);
    assert_eq!(result.seats.iter().map(|s| s.stack).sum::<i64>(), 800);
    // Fold bot folded preflop => never reached showdown reveal
    assert!(!log
        .iter()
        .any(|e| matches!(e, PublicEvent::ShowdownRevealed { seat: 3, .. })));
    for b in &bots {
        assert!(b.is_alive());
        b.shutdown().await;
        assert!(!b.is_alive());
    }
}

#[tokio::test]
async fn crashed_bot_fails_fast_and_is_auto_folded() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("crash.py");
    std::fs::write(&script, "import sys\nsys.exit(3)\n").unwrap();
    let crash = Arc::new(
        ProcessBot::spawn(
            &python(),
            &[script.to_string_lossy().to_string()],
            SpawnOptions::new(1, "crash"),
        )
        .await
        .unwrap(),
    );
    // give it a moment to die
    crash.wait_exit(Duration::from_secs(5)).await;
    assert!(!crash.is_alive());
    let ok = Arc::new(spawn_py("call_bot.py", 2).await);
    let players: Vec<Option<Arc<dyn Player>>> = vec![Some(crash.clone()), Some(ok.clone())];
    let start = std::time::Instant::now();
    let (result, log) = play_hand(params(2, 5), &players, Duration::from_millis(1500))
        .await
        .unwrap();
    assert!(
        start.elapsed() < Duration::from_millis(1400),
        "dead bot must not consume timeouts"
    );
    assert!(log
        .iter()
        .any(|e| matches!(e, PublicEvent::ActionSubstituted { seat: 0, .. })));
    assert_eq!(result.seats[1].stack, 205);
    ok.shutdown().await;
}

#[tokio::test]
async fn slow_bot_times_out_and_stale_answers_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("slow.py");
    // Answers every request, but only after 400ms; the engine's deadline is 100ms.
    std::fs::write(
        &script,
        r#"
import sys, json, time
for line in sys.stdin:
    m = json.loads(line)
    if m.get("type") == "request_action":
        time.sleep(0.4)
        sys.stdout.write(json.dumps({"type":"action","request_id":m["request_id"],"action":{"kind":"AllIn"}})+"\n")
        sys.stdout.flush()
    elif m.get("type") == "goodbye":
        break
"#,
    )
    .unwrap();
    let slow = Arc::new(
        ProcessBot::spawn(
            &python(),
            &[script.to_string_lossy().to_string()],
            SpawnOptions::new(1, "slow"),
        )
        .await
        .unwrap(),
    );
    let ok = Arc::new(spawn_py("call_bot.py", 2).await);
    let players: Vec<Option<Arc<dyn Player>>> = vec![Some(slow.clone()), Some(ok.clone())];
    let (result, log) = play_hand(params(2, 6), &players, Duration::from_millis(100))
        .await
        .unwrap();
    assert!(log
        .iter()
        .any(|e| matches!(e, PublicEvent::ActionSubstituted { seat: 0, .. })));
    // The slow bot folded (auto), so it never went all-in.
    assert!(!log.iter().any(|e| matches!(
        e,
        PublicEvent::ActionTaken {
            seat: 0,
            action: poker_utils::Action::RaiseTo { .. },
            ..
        }
    )));
    assert_eq!(result.seats[1].stack, 205);
    // Second hand: the stale AllIn answer from hand 1 must not be taken as the answer now.
    let (result2, log2) = play_hand(params(2, 7), &players, Duration::from_millis(100))
        .await
        .unwrap();
    assert!(log2
        .iter()
        .any(|e| matches!(e, PublicEvent::ActionSubstituted { seat: 0, .. })));
    assert_eq!(result2.seats[1].stack, 205);
    slow.shutdown().await;
    ok.shutdown().await;
}

#[tokio::test]
async fn stderr_is_captured() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("noisy.py");
    std::fs::write(
        &script,
        "import sys\nsys.stderr.write('boom: something broke\\n')\nsys.stderr.flush()\nsys.exit(1)\n",
    )
    .unwrap();
    let bot = ProcessBot::spawn(
        &python(),
        &[script.to_string_lossy().to_string()],
        SpawnOptions::new(1, "noisy"),
    )
    .await
    .unwrap();
    bot.wait_exit(Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(bot.stderr_tail().contains("boom"));
}

#[tokio::test]
async fn smoke_test_passes_reference_bots_and_fails_broken_ones() {
    for script in [
        "call_bot.py",
        "raise_bot.py",
        "random_bot.py",
        "fold_bot.py",
        "template_bot.py",
    ] {
        let bot = spawn_py(script, 1).await;
        let report = table_runner::smoke_test(&bot, Duration::from_secs(10)).await;
        assert!(
            report.passed,
            "{script}: {:?} {}",
            report.reason, report.message
        );
        assert!(!report.actions.is_empty());
        bot.shutdown().await;
    }
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.py");
    std::fs::write(
        &bad,
        "import sys\nfor line in sys.stdin:\n    print('hello world')\n    sys.stdout.flush()\n",
    )
    .unwrap();
    let bot = ProcessBot::spawn(
        &python(),
        &[bad.to_string_lossy().to_string()],
        SpawnOptions::new(1, "bad"),
    )
    .await
    .unwrap();
    let report = table_runner::smoke_test(&bot, Duration::from_millis(1500)).await;
    assert!(!report.passed);
    assert_eq!(report.reason, Some(table_runner::SmokeFailure::Timeout));
    bot.shutdown().await;

    let crash = dir.path().join("crash.py");
    std::fs::write(&crash, "raise SystemExit(2)\n").unwrap();
    let bot = ProcessBot::spawn(
        &python(),
        &[crash.to_string_lossy().to_string()],
        SpawnOptions::new(1, "crash"),
    )
    .await
    .unwrap();
    let report = table_runner::smoke_test(&bot, Duration::from_millis(1500)).await;
    assert!(!report.passed);
    assert_eq!(report.reason, Some(table_runner::SmokeFailure::Crashed));

    let illegal = dir.path().join("illegal.py");
    std::fs::write(&illegal, r#"
import sys, json
for line in sys.stdin:
    m = json.loads(line)
    if m.get("type") == "request_action":
        print(json.dumps({"type":"action","request_id":m["request_id"],"action":{"kind":"BetTo","amount":999999}}))
        sys.stdout.flush()
"#).unwrap();
    let bot = ProcessBot::spawn(
        &python(),
        &[illegal.to_string_lossy().to_string()],
        SpawnOptions::new(1, "illegal"),
    )
    .await
    .unwrap();
    let report = table_runner::smoke_test(&bot, Duration::from_millis(1500)).await;
    assert!(!report.passed);
    assert_eq!(
        report.reason,
        Some(table_runner::SmokeFailure::IllegalAction)
    );
    bot.shutdown().await;
}
