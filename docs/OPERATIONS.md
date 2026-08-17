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

`deploy/docker-compose.yml` runs the API and two workers; workers use the **Docker sandbox**
(one container per bot: `--network=none`, memory/CPU/pids limits, read-only rootfs, dropped
capabilities, unprivileged user). Notes:

* Workers talk to the host Docker daemon through `/var/run/docker.sock`. Because the daemon
  resolves bind-mount paths on the **host**, the data volume must be mounted at the same path
  inside the worker containers as on the host (`/srv/pokerbots/data` in the compose file).
* Pre-pull bot images on the host: `docker pull python:3.12-slim node:22-slim eclipse-temurin:21-jre debian:bookworm-slim`
  (configurable in `[sandbox.docker.images]`).
* Native bots must be Linux x86-64 static executables.

```bash
docker compose -f deploy/docker-compose.yml up -d --build
```

## Scaling to 200 bots

Measured on a laptop with the reference Python bots (`pokerbots tournament --players 200`):
a full 200-player tournament takes ~3–5 s of engine/protocol time; real bots that use most of
their 1 s decision budget dominate. Rules of thumb:

* A tournament of 200 bots is ~6 000 hands ≈ 50 000 decisions. At an average 100 ms per decision
  and 23 tables in parallel that is ~4 minutes; at 300 ms ≈ 12 minutes.
* A nightly series of 100 tournaments should therefore run with `worker.concurrency × workers`
  ≈ 4–8 tournaments in parallel to finish well inside 6 hours. Each concurrent tournament means
  another 200 bot processes/containers — size RAM accordingly (512 MB limit each; Python bots use
  ~30 MB).
* Reduce `defaults.tournament.action_timeout_ms` or `nightly_series_length` if needed;
  `PATCH /admin/config` takes effect for the next run.
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
