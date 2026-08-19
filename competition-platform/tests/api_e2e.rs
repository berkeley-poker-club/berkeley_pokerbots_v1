//! End-to-end: HTTP API (in-process router) + a worker + the reference Python bots.
//! Requires `python3`.

use axum::body::Body;
use axum::Router;
use competition_platform::config::PlatformConfig;
use competition_platform::Services;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct TestEnv {
    _dir: tempfile::TempDir,
    app: Router,
    services: Arc<Services>,
    shutdown: tokio::sync::watch::Sender<bool>,
    admin_key: String,
}

async fn env() -> TestEnv {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = PlatformConfig::default();
    cfg.storage.database = dir.path().join("db.sqlite");
    cfg.storage.artifacts_dir = dir.path().join("artifacts");
    cfg.storage.logs_dir = dir.path().join("logs");
    cfg.server.admin_key = Some("adminsecret".into());
    cfg.server.smoke_wait_ms = 20_000;
    cfg.worker.poll_interval_ms = 50;
    cfg.worker.concurrency = 2;
    cfg.sandbox.kind = "process".into();
    cfg.sandbox.python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    // Fast tournaments for tests.
    cfg.defaults.tournament.starting_stack = 200;
    cfg.defaults.tournament.level_advance.hands_per_level = Some(5);
    cfg.defaults
        .tournament
        .level_advance
        .max_level_duration_secs = None;
    cfg.defaults.tournament.action_timeout_ms = 2000;
    cfg.defaults.smoke_timeout_ms = 10_000;
    cfg.defaults.ondemand_runs_per_team_per_day = 2;
    cfg.defaults.record_hands = true;
    let services = Arc::new(Services::open(cfg).unwrap());
    let (tx, rx) = tokio::sync::watch::channel(false);
    let worker = services.worker();
    tokio::spawn(worker.run(rx));
    let app = competition_platform::api::router(services.app_state());
    TestEnv {
        _dir: dir,
        app,
        services,
        shutdown: tx,
        admin_key: "adminsecret".into(),
    }
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    (status, v)
}

fn json_req(
    method: &str,
    path: &str,
    key: Option<&str>,
    admin: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    if let Some(k) = key {
        b = b.header("x-api-key", k);
    }
    if let Some(a) = admin {
        b = b.header("x-admin-key", a);
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

fn bots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bots/python")
}

fn zip_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, content) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(content).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn python_bot_zip(script: &str) -> Vec<u8> {
    let sdk = std::fs::read(bots_dir().join("pokerbots_sdk.py")).unwrap();
    let bot = std::fs::read(bots_dir().join(script)).unwrap();
    zip_of(&[("pokerbots_sdk.py", sdk), (script, bot)])
}

fn multipart_with_activate(
    key: &str,
    artifact: &[u8],
    manifest: &Value,
    activate: bool,
) -> Request<Body> {
    let boundary = "----pokerbotsboundary42";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\nContent-Type: application/json\r\n\r\n{}\r\n", manifest).as_bytes());
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"activate\"\r\n\r\n{}\r\n",
            activate
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"artifact\"; filename=\"bot.zip\"\r\nContent-Type: application/zip\r\n\r\n").as_bytes());
    body.extend_from_slice(artifact);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/api/v1/submissions")
        .header("x-api-key", key)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

fn multipart(key: &str, artifact: &[u8], manifest: &Value) -> Request<Body> {
    let boundary = "----pokerbotsboundary42";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\nContent-Type: application/json\r\n\r\n{}\r\n", manifest).as_bytes());
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"artifact\"; filename=\"bot.zip\"\r\nContent-Type: application/zip\r\n\r\n").as_bytes());
    body.extend_from_slice(artifact);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/api/v1/submissions")
        .header("x-api-key", key)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

async fn register(app: &Router, name: &str) -> (String, String) {
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/auth/register",
            None,
            None,
            Some(json!({
                "team_name": name,
                "members": [{"name": "Ada", "email": "ada@berkeley.edu", "student_id": "123"}]
            })),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    (
        v["team_id"].as_str().unwrap().to_string(),
        v["api_key"].as_str().unwrap().to_string(),
    )
}

async fn upload(app: &Router, key: &str, script: &str) -> (StatusCode, Value) {
    let manifest = json!({"language": "python", "runtime": "python3", "entrypoint": script, "protocol_version": "1"});
    call(app, multipart(key, &python_bot_zip(script), &manifest)).await
}

async fn wait_run(app: &Router, run_id: &str, timeout: Duration) -> Value {
    let start = Instant::now();
    loop {
        let (st, v) = call(
            app,
            json_req("GET", &format!("/api/v1/runs/{run_id}"), None, None, None),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let status = v["status"].as_str().unwrap_or("");
        if matches!(status, "completed" | "failed" | "cancelled") {
            return v;
        }
        assert!(start.elapsed() < timeout, "run did not finish in time: {v}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_competition_flow() {
    let env = env().await;
    let app = &env.app;

    // ---- health
    let (st, v) = call(app, json_req("GET", "/health", None, None, None)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["status"], "ok");
    let (st, v) = call(app, json_req("GET", "/api/v1/ready", None, None, None)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["ready"], true);

    // ---- auth
    let (st, _) = call(app, json_req("GET", "/api/v1/me", None, None, None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        app,
        json_req("GET", "/api/v1/me", Some("pb_live_nope"), None, None),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (team_a, key_a) = register(app, "ace-high").await;
    let (team_b, key_b) = register(app, "call-station").await;
    let (team_c, key_c) = register(app, "raisers").await;
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/auth/register",
            None,
            None,
            Some(json!({"team_name": "ace-high"})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "conflict");
    let (st, v) = call(app, json_req("GET", "/api/v1/me", Some(&key_a), None, None)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["team_id"], team_a);
    assert_eq!(v["active_submission_id"], Value::Null);
    // Bearer auth works too
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/me")
        .header("authorization", format!("Bearer {key_b}"))
        .body(Body::empty())
        .unwrap();
    let (st, v) = call(app, req).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["team_id"], team_b);

    // ---- submissions: good bots validate synchronously
    let (st, v) = upload(app, &key_a, "template_bot.py").await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "validated");
    assert_eq!(v["smoke_test"]["passed"], true);
    let checks = v["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3, "{v}");
    assert_eq!(checks[0]["stage"], "build");
    assert_eq!(checks[0]["status"], "passed");
    assert_eq!(checks[1]["stage"], "smoke");
    assert_eq!(checks[1]["status"], "passed");
    assert_eq!(checks[2]["stage"], "trial");
    assert!(
        checks[2]["status"] == "passed" || checks[2]["status"] == "warned",
        "{v}"
    );
    assert!(checks[2]["details"]["decisions"].as_u64().unwrap() > 0);
    let sub_a1 = v["submission_id"].as_str().unwrap().to_string();
    assert!(sub_a1.starts_with(&format!("sub_{team_a}_")), "{sub_a1}");
    let (st, v) = upload(app, &key_b, "call_bot.py").await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let sub_b1 = v["submission_id"].as_str().unwrap().to_string();
    let (st, v) = upload(app, &key_c, "raise_bot.py").await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let sub_c1 = v["submission_id"].as_str().unwrap().to_string();

    // ---- a broken bot is rejected with a clear 422
    let broken = zip_of(&[(
        "bot.py",
        b"import sys\nprint('not json')\nsys.stdout.flush()\nimport time\ntime.sleep(30)\n"
            .to_vec(),
    )]);
    let manifest = json!({"language": "python", "runtime": "python3", "entrypoint": "bot.py", "protocol_version": "1"});
    // shorten smoke timeout for this one
    let (st, _) = call(
        app,
        json_req(
            "PATCH",
            "/api/v1/admin/config",
            None,
            Some(&env.admin_key),
            Some(json!({"smoke_timeout_ms": 1500})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = call(app, multipart(&key_a, &broken, &manifest)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "validation_error");
    assert_eq!(v["error"]["details"]["reason"], "timeout");
    assert_eq!(v["error"]["details"]["stage"], "smoke", "{v}");
    let rejected_id = v["error"]["details"]["submission_id"]
        .as_str()
        .unwrap()
        .to_string();

    // ---- a bot that does not compile is rejected at the build stage
    let syntax_err = zip_of(&[("bot.py", b"def act(:\n".to_vec())]);
    let (st, v) = call(
        app,
        multipart(
            &key_a,
            &syntax_err,
            &json!({"entrypoint": "bot.py", "runtime": "python3"}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["details"]["stage"], "build", "{v}");
    let summary = v["error"]["details"]["stage_summary"].as_str().unwrap();
    assert!(
        summary.contains("py_compile") || summary.contains("exited"),
        "{summary}"
    );

    // ---- auto-activate on upload: multipart `activate=true` goes live once validated
    let (st, v) = call(
        app,
        multipart_with_activate(
            &key_a,
            &python_bot_zip("call_bot.py"),
            &json!({"entrypoint": "call_bot.py", "runtime": "python3"}),
            true,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["active"], true, "{v}");
    let auto_id = v["submission_id"].as_str().unwrap().to_string();
    let (_, me) = call(app, json_req("GET", "/api/v1/me", Some(&key_a), None, None)).await;
    assert_eq!(me["active_submission_id"], json!(auto_id));
    // missing entrypoint => setup_failed
    let (st, v) = call(
        app,
        multipart(
            &key_a,
            &broken,
            &json!({"entrypoint": "nope.py", "runtime": "python3"}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["details"]["reason"], "setup_failed");
    // bad manifest
    let (st, v) = call(
        app,
        multipart(&key_a, &broken, &json!({"entrypoint": "../x.py"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");

    // ---- list / get / activate / delete
    let (st, v) = call(
        app,
        json_req("GET", "/api/v1/submissions", Some(&key_a), None, None),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["submissions"].as_array().unwrap().len(), 5);
    let (st, v) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/submissions/{rejected_id}"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["status"], "rejected");
    // cannot activate a rejected submission
    let (st, v) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/submissions/{rejected_id}/activate"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    // cannot see other team's submission
    let (st, _) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/submissions/{sub_b1}"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    for (key, sub) in [(&key_a, &sub_a1), (&key_b, &sub_b1), (&key_c, &sub_c1)] {
        let (st, v) = call(
            app,
            json_req(
                "POST",
                &format!("/api/v1/submissions/{sub}/activate"),
                Some(key),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["active_submission_id"], json!(sub));
    }
    let (st, v) = call(
        app,
        json_req(
            "DELETE",
            &format!("/api/v1/submissions/{sub_a1}"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (st, _) = call(
        app,
        json_req(
            "DELETE",
            &format!("/api/v1/submissions/{rejected_id}"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    // one active bot per team: a second validated upload + activate replaces the first
    let (st, v) = upload(app, &key_a, "random_bot.py").await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let sub_a2 = v["submission_id"].as_str().unwrap().to_string();
    let (st, v) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/submissions/{sub_a2}/activate"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["previous_submission_id"], json!(sub_a1));
    let (_, v) = call(app, json_req("GET", "/api/v1/me", Some(&key_a), None, None)).await;
    assert_eq!(v["active_submission_id"], json!(sub_a2));

    // ---- admin auth
    let (st, _) = call(
        app,
        json_req("GET", "/api/v1/admin/metrics", None, None, None),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        app,
        json_req("GET", "/api/v1/admin/metrics", Some(&key_a), None, None),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        app,
        json_req("GET", "/api/v1/admin/metrics", None, Some("wrong"), None),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        app,
        json_req(
            "GET",
            "/api/v1/admin/metrics",
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["teams"], 3);
    assert_eq!(v["active_bots"], 3);
    let (st, v) = call(
        app,
        json_req(
            "GET",
            "/api/v1/admin/workers",
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["workers"].as_array().unwrap().len(), 1);

    // ---- nightly run (admin) → leaderboard
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/admin/runs/nightly",
            None,
            Some(&env.admin_key),
            Some(json!({"series_length": 3, "seed": 5})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["participant_count"], 3);
    assert_eq!(v["kind"], "nightly");
    let run_id = v["run_id"].as_str().unwrap().to_string();
    let run = wait_run(app, &run_id, Duration::from_secs(120)).await;
    assert_eq!(run["status"], "completed", "{run}");
    assert_eq!(run["progress"]["tournaments_completed"], 3);
    let (st, v) = call(
        app,
        json_req("GET", "/api/v1/leaderboard", None, None, None),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["run_id"], json!(run_id));
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["rank"], 1);
    for e in entries {
        assert_eq!(e["tournaments_counted"], 3);
        let gm = e["geo_mean_placement"].as_f64().unwrap();
        assert!((1.0..=3.0).contains(&gm), "{gm}");
        assert!(e["team_name"].is_string());
    }
    let (st, v) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/runs/{run_id}/tournaments"),
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let ts = v["tournaments"].as_array().unwrap();
    assert_eq!(ts.len(), 3);
    assert!(ts.iter().all(|t| t["status"] == "completed"));
    let (st, v) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/runs/{run_id}/tournaments/0"),
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let placements = v["placements"].as_array().unwrap();
    assert_eq!(placements.len(), 3);
    assert_eq!(placements[0]["place"], 1);
    assert!(v["hand_log_url"].is_string());
    // hand log is admin-only and streams ndjson
    let (st, _) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/runs/{run_id}/tournaments/0/hands"),
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let resp = app
        .clone()
        .oneshot(json_req(
            "GET",
            &format!("/api/v1/runs/{run_id}/tournaments/0/hands"),
            None,
            Some(&env.admin_key),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let first_line = String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let rec: Value = serde_json::from_str(&first_line).expect("ndjson line");
    assert!(rec["events"].is_array());
    // history + team stats
    let (st, v) = call(
        app,
        json_req("GET", "/api/v1/leaderboard/history", None, None, None),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["boards"].as_array().unwrap().len(), 1);
    let (st, v) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/teams/{team_b}/stats"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["summary"]["tournaments_played"], 3);
    assert_eq!(v["recent_scores"].as_array().unwrap().len(), 1);

    // ---- on-demand runs by a team: rate limited (2/day)
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/runs",
            Some(&key_b),
            None,
            Some(json!({"mode": "single"})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let od1 = v["run_id"].as_str().unwrap().to_string();
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/runs",
            Some(&key_b),
            None,
            Some(json!({"mode": "series", "series_length": 2})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let od2 = v["run_id"].as_str().unwrap().to_string();
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/runs",
            Some(&key_b),
            None,
            Some(json!({"mode": "single"})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"]["code"], "rate_limited");
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/runs",
            Some(&key_c),
            None,
            Some(json!({"mode": "series", "series_length": 500})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let r1 = wait_run(app, &od1, Duration::from_secs(120)).await;
    assert_eq!(r1["status"], "completed", "{r1}");
    assert_eq!(r1["progress"]["tournaments_total"], 1);
    let r2 = wait_run(app, &od2, Duration::from_secs(120)).await;
    assert_eq!(r2["status"], "completed", "{r2}");
    // default leaderboard still points at the nightly run
    let (_, v) = call(
        app,
        json_req("GET", "/api/v1/leaderboard", None, None, None),
    )
    .await;
    assert_eq!(v["run_id"], json!(run_id));
    let (st, v) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/leaderboard?run_id={od2}"),
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["entries"].as_array().unwrap().len(), 3);
    // list with filters
    let (st, v) = call(
        app,
        json_req(
            "GET",
            "/api/v1/runs?kind=ondemand&status=completed",
            None,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["runs"].as_array().unwrap().len(), 2);

    // ---- cancel: cancelling a completed run conflicts; cancelling a queued run works
    let (st, _) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/runs/{od1}/cancel"),
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/runs/{od1}/cancel"),
            Some(&key_b),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // Suspend team C so it drops out of the next snapshot; then a nightly with only 2 bots still runs.
    let (st, v) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/admin/teams/{team_c}/suspend"),
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/submissions/x/activate",
            Some(&key_c),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        app,
        json_req(
            "POST",
            "/api/v1/admin/runs/nightly",
            None,
            Some(&env.admin_key),
            Some(json!({"series_length": 5})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["participant_count"], 2);
    let run3 = v["run_id"].as_str().unwrap().to_string();
    let (st, v) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/runs/{run3}/cancel"),
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "cancelled");
    let r3 = wait_run(app, &run3, Duration::from_secs(60)).await;
    assert_eq!(r3["status"], "cancelled");
    let (st, v) = call(
        app,
        json_req(
            "GET",
            &format!("/api/v1/admin/runs/{run3}/jobs"),
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(!v["jobs"].as_array().unwrap().is_empty());

    // ---- admin config roundtrip
    let (st, v) = call(
        app,
        json_req(
            "GET",
            "/api/v1/admin/config",
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["smoke_timeout_ms"], 1500);
    let (st, v) = call(
        app,
        json_req(
            "PATCH",
            "/api/v1/admin/config",
            None,
            Some(&env.admin_key),
            Some(json!({"tournament": {"table_size": 1}})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (st, v) = call(app, json_req("PATCH", "/api/v1/admin/config", None, Some(&env.admin_key), Some(json!({"nightly_time_utc": "08:30", "tournament": {"table_size": 6, "break_threshold": 4}})))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["nightly_time_utc"], "08:30");
    assert_eq!(v["tournament"]["table_size"], 6);
    assert_eq!(
        v["tournament"]["starting_stack"], 200,
        "untouched fields survive a patch"
    );

    // ---- key rotation
    let (st, v) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/teams/{team_a}/rotate-key"),
            Some(&key_a),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let new_key = v["api_key"].as_str().unwrap().to_string();
    let (st, _) = call(app, json_req("GET", "/api/v1/me", Some(&key_a), None, None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        app,
        json_req("GET", "/api/v1/me", Some(&new_key), None, None),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        app,
        json_req(
            "POST",
            &format!("/api/v1/teams/{team_b}/rotate-key"),
            Some(&new_key),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // ---- web UI served at /
    let resp = app
        .clone()
        .oneshot(json_req("GET", "/", None, None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("text/html"));
    let html = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Pokerbots") && html.contains("/api/v1"));

    // ---- autoscale plan endpoint (admin)
    let (st, _) = call(
        app,
        json_req("GET", "/api/v1/admin/autoscale", None, None, None),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, v) = call(
        app,
        json_req(
            "GET",
            "/api/v1/admin/autoscale",
            None,
            Some(&env.admin_key),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["plan"]["desired_workers"].as_u64().unwrap() >= 1);
    assert!(v["settings"]["max_workers"].as_u64().is_some());

    // ---- openapi served
    let resp = app
        .clone()
        .oneshot(json_req("GET", "/api/v1/openapi.yaml", None, None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = env.shutdown.send(true);
    let _ = env.services;
}

#[tokio::test]
async fn scheduler_creates_one_nightly_per_date() {
    use competition_platform::scheduler::due_nightly_date;
    use competition_platform::store::Store;
    let store = Store::open_memory().unwrap();
    let mut s = store.settings().unwrap();
    // scheduled one minute ago (UTC)
    let t = chrono::Utc::now() - chrono::Duration::minutes(1);
    s.nightly_time_utc = t.format("%H:%M").to_string();
    store.save_settings(&s).unwrap();
    let date = due_nightly_date(&store).unwrap().expect("nightly due");
    assert_eq!(date, t.format("%Y-%m-%d").to_string());
    let run = competition_platform::runs::create_run(
        &store,
        &s,
        competition_platform::runs::RunRequest {
            kind: competition_platform::models::RunKind::Nightly,
            series_length: 1,
            created_by: "scheduler".into(),
            nightly_date: Some(date.clone()),
            seed: None,
        },
    )
    .unwrap();
    assert_eq!(run.nightly_date.as_deref(), Some(date.as_str()));
    assert!(
        due_nightly_date(&store).unwrap().is_none(),
        "already created today"
    );
    // disabled => never due
    s.nightly_enabled = false;
    store.save_settings(&s).unwrap();
    assert!(due_nightly_date(&store).unwrap().is_none());
}
