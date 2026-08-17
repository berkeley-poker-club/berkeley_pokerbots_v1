# Operations Guide

## Components

| Binary | Role |
|--------|------|
| `competition-api` | HTTP API, nightly scheduler, optional embedded workers |
| `tournament-worker` | Pulls jobs (smoke tests, tournaments, finalisation) from the shared store |
| `pokerbots` | Local CLI: tournaments, series, smoke tests, reference stdio bots, load tests |

State lives in one directory (`storage.*` in `pokerbots.toml`): a SQLite database (WAL mode),
the artifact store and per-run logs. API and workers on the same host share it directly; several
worker processes can run against the same files. (The store is behind a small trait-like surface in
`competition-platform/src/store.rs`; the schema is Postgres-portable if you outgrow SQLite.)

## Configuration

`competition-api --print-example-config > pokerbots.toml`, then edit. Key settings:

```toml
[server]
bind = "0.0.0.0:8080"
admin_key = "change-me"          # or POKERBOTS_ADMIN_KEY
embedded_workers = 1             # 0 when using separate tournament-worker processes
smoke_wait_ms = 30000

[storage]
database = "data/pokerbots.db"
artifacts_dir = "data/artifacts"
logs_dir = "data/logs"

[worker]
concurrency = 2                  # tournaments per worker process at once

[sandbox]
kind = "process"                 # "docker" in production
memory_mb = 512
cpu_seconds = 1800

[defaults]                       # initial runtime settings; later edit via PATCH /admin/config
nightly_time_utc = "07:00"       # midnight Pacific (PDT)
nightly_series_length = 100
ondemand_runs_per_team_per_day = 2
ondemand_max_series_length = 10
registration_open = true

[defaults.tournament]
table_size = 9
break_threshold = 7
min_table_size = 6
starting_stack = 1000
action_timeout_ms = 1000
```

Environment overrides: `POKERBOTS_BIND`, `POKERBOTS_ADMIN_KEY`, `POKERBOTS_DATABASE`,
`POKERBOTS_ARTIFACTS_DIR`, `POKERBOTS_LOGS_DIR`, `POKERBOTS_SANDBOX`, `POKERBOTS_EMBEDDED_WORKERS`,
`POKERBOTS_WORKER_CONCURRENCY`, `POKERBOTS_PUBLIC_BASE_URL`.

Runtime settings (tournament config, nightly time, rate limits, smoke timeout, upload limit,
registration) are stored in the database and edited live with `PATCH /admin/config` — no restart.

## Single-box deployment (simplest)

```bash
cargo build --release
POKERBOTS_ADMIN_KEY=$(openssl rand -hex 24) ./target/release/competition-api --config pokerbots.toml
```

With `embedded_workers = 1` and `sandbox.kind = "process"` this is a complete, working
competition server for trusted bots. Put nginx/Caddy in front for TLS.

## Docker deployment (recommended for untrusted bots)

`deploy/docker-compose.yml` runs the API and workers; workers use the **Docker sandbox**
(one container per bot: `--network=none`, memory/CPU/pids limits, read-only rootfs, dropped
capabilities, unprivileged user). Notes:

* Workers talk to the host Docker daemon through `/var/run/docker.sock`. Because the daemon
  resolves bind-mount paths on the **host**, the data volume must be mounted at the same path
  inside the worker containers as on the host (`/srv/pokerbots/data` in the compose file).
* Pre-pull bot images on the host: `docker pull python:3.12-slim node:22-slim eclipse-temurin:21-jre debian:bookworm-slim`
  (configurable in `[sandbox.docker.images]`).
* Native bots must be Linux executables matching the host architecture (x86-64 on typical
  servers; the sandbox itself is architecture-agnostic and is exercised in development via
  colima on macOS).

```bash
docker compose -f deploy/docker-compose.yml up -d --build
```

## Web UI

The API serves a thin single-page UI at `/` (also `/ui`): team registration / API-key sign-in,
drag-and-drop bot upload with live per-stage validation status (build → smoke → trial), submission
list with one-click activation, the public leaderboard, run progress, and a "quick test run"
button (uses the team's on-demand quota). It is a static page speaking the same public API —
nothing works in the UI that doesn't work with `curl`.

## Validation pipeline

Every upload runs three stages on a worker before it can be activated
(`checks` on the submission records each stage):

1. **build** — the manifest's `build` command (e.g. `["make"]`,
   `["cargo","build","--release","--offline"]`), or a default check for interpreted runtimes
   (`python3 -m py_compile`, `node --check`). Runs inside the sandbox with the artifact mounted
   writable; skipped for `native` uploads without a build command.
2. **smoke** — the protocol handshake + two legal decisions (as before).
3. **trial** — `trial_hands` (default 30) three-handed hands against reference bots. Fails if the
   bot dies or more than `trial_max_substitution_rate` (default 20%) of its decisions time out or
   are illegal; passes-with-warning otherwise. Catches bots that pass a two-decision smoke test
   but fall over in real play — before they hit a 500-player nightly.

Only after all stages pass does the submission become `validated` (and, with `activate=true` on
the upload, active). Stage knobs live under `validation` in `PATCH /admin/config`.

## Autoscaling

The autoscaler sizes the worker fleet so runs finish on time without manual resizing. Every
`poll_secs` it estimates the per-tournament duration for each active run — the run's own completed
tournaments first, then history of similar-sized runs, then `est_secs_per_participant × field
size` — and computes how many tournaments must run in parallel to meet the deadline
(`nightly_deadline_hours` after a nightly starts; `ondemand_deadline_minutes` for on-demand).
Workers are sized by both constraints:

```
workers = clamp(max(slots / worker_concurrency, concurrent_bots / max_bots_per_worker),
                min_workers, max_workers)
```

Scale-up applies immediately; scale-down waits `scale_down_cooldown_secs` after the last change
and stops workers gracefully (SIGINT → the worker finishes its in-flight jobs, then exits; a
tournament is never killed mid-play). Inspect the live plan and the last action at
`GET /admin/autoscale`; tune the knobs under `autoscale` in `PATCH /admin/config`.

Backends (`[autoscaler]` in `pokerbots.toml`):

| backend | behaviour |
|---------|-----------|
| `off` | plan only (still visible in `/admin/autoscale`) |
| `processes` | the API process spawns/stops local `tournament-worker` processes — autoscaling on one box |
| `command` | runs `scale_command` with `{n}` — docker compose `--scale worker={n}`, a k8s `kubectl scale`, or a cloud ASG script |

Jobs are **weighted by field size**: a 500-bot tournament only lands on a worker with 500 bots of
spare capacity (`[worker] max_bots_in_flight`), so two half-loaded workers never both grab jobs
they can't host.

## Scaling to 200–500 bots

Measured on a laptop with the reference Python bots: a 200-player tournament takes ~3–5 s and a
**500-player tournament ~5 s** of engine/protocol time (500 bot processes, ~56 tables, verified
end-to-end through the API: 500 registered teams → uploads → nightly run → 500-row leaderboard).
Real bots that use most of their 1 s decision budget dominate. Rules of thumb:

* A tournament of 200 bots is ~6 000 hands ≈ 50 000 decisions. At an average 100 ms per decision
  and 23 tables in parallel that is ~4 minutes; at 300 ms ≈ 12 minutes. A 500-bot tournament is
  ~15 000 hands across ~56 tables — similar wall-clock per table, more RAM (500 × bot footprint;
  Python bots ≈ 30 MB each ⇒ ~15 GB per concurrent 500-bot tournament — size `max_bots_in_flight`
  and `max_bots_per_worker` to what the box actually has).
* With the autoscaler on you normally don't plan this by hand: set the deadline
  (`nightly_deadline_hours`) and the fleet bounds and let it size the pool per run. Reduce
  `defaults.tournament.action_timeout_ms` or `nightly_series_length` only if `max_workers` alone
  cannot meet the deadline.
* Load-test the exact machine: `pokerbots bench --players 200 --process` (compiled bots) or
  `pokerbots tournament --bot python:bots/python/call_bot.py --players 200`.

## Failure behaviour

| Failure | Behaviour |
|---------|-----------|
| Bot times out / sends garbage / illegal action | Engine substitutes check/fold, broadcasts `ActionSubstituted`, play continues |
| Bot crashes | Further requests fail immediately (no timeouts wasted); it blinds out |
| Bot cannot be started (bad artifact) | An auto-fold stand-in plays its seat; noted in the tournament detail (`spawn_failures`) |
| Worker dies mid-tournament | Its jobs lose their heartbeat and are re-queued (`worker.stale_job_secs`); a tournament restarts from scratch (max 2 attempts) |
| Run cancelled (`POST /runs/{id}/cancel`) | Queued tournaments/jobs are dropped; running ones abort within seconds |
| Nightly missed (server down) | Started on the next scheduler tick within 12 h of the slot; one nightly per UTC date |

Failure drills you can run locally: `pokerbots smoke` against `tests` bots (see
`table-runner/tests/process_bot_tests.rs`: crash, hang, garbage, illegal, overlong lines).

## Admin cheat sheet

```bash
A="-H X-Admin-Key:$POKERBOTS_ADMIN_KEY"
curl -s $A $PB/admin/metrics
curl -s $A $PB/admin/workers
curl -s $A $PB/admin/config
curl -s $A -X PATCH $PB/admin/config -H 'content-type: application/json' -d '{"nightly_series_length":50}'
curl -s $A -X POST $PB/admin/runs/nightly -H 'content-type: application/json' -d '{"series_length":10}'
curl -s $A -X POST $PB/runs/run_.../cancel
curl -s $A -X POST $PB/admin/teams -H 'content-type: application/json' -d '{"team_name":"staff","is_admin":true}'
curl -s $A -X POST $PB/admin/teams/tm_.../suspend
curl -s $A -X POST $PB/admin/revalidate/sub_...
curl -s $A $PB/runs/run_.../tournaments/0/hands > hands.jsonl   # when record_hands is on
```

Logs: bot stderr per tournament under `data/logs/<run_id>/<index>/<team_id>.stderr.log`,
smoke-test stderr under `data/logs/smoke/`. Server logs go to stderr (`RUST_LOG=debug` for more).
