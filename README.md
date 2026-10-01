# Berkeley Pokerbots

A complete No-Limit Hold'em tournament platform for bot competitions: a rules-correct engine,
multi-table tournament director, JSON-lines bot protocol, and a competition service — web UI +
API submissions with a build/smoke/trial validation pipeline, sandboxed autoscaling workers,
nightly series and a geometric-mean leaderboard — verified end-to-end with **500 concurrent
bots**.

```
poker-utils/           pure NLHE engine: cards, hand evaluation, Hand state machine, config
table-runner/          Player trait, ProcessBot (JSON-lines protocol), in-process bots, table task, smoke test
tournament-core/       TournamentDirector (table breaking, blind levels, SPEC placements), series + scoring
pokerbots-cli/         `pokerbots`: local tournaments/series, smoke tests, stdio reference bots, load tests
competition-platform/  `competition-api` + `tournament-worker`: SQLite store, artifacts, sandbox, jobs, API, web UI, scheduler, autoscaler
bots/python/           SDK, reference bots (fold / call / raise / random) and a student template
docs/                  BOT_PROTOCOL.md, openapi.yaml, QUICKSTART.md, OPERATIONS.md, DEVELOPMENT.md
.devcontainer/         dev container (VS Code / Codespaces / `make docker-shell`)
deploy/                production Dockerfile, docker-compose.yml, docker-compose.dev.yml
SPEC.md                game + tournament rules (source of truth)
```

## Quick start

**No local toolchain?** Open the repo in a GitHub Codespace or VS Code *Reopen in Container*
(`.devcontainer/`), or with plain Docker:

```bash
make docker-shell     # dev shell with Rust + Python, repo at /workspace
make docker-check     # fmt + clippy + full test suite in the container
make docker-run-api   # platform on http://localhost:8080
```

**With Rust installed** (`make help` lists all targets):

```bash
cargo build --release
P=./target/release/pokerbots

# 9-handed tournament: three Python reference bots + built-ins, printed placements
$P tournament --bot python:bots/python/call_bot.py --bot python:bots/python/raise_bot.py \
   --bot python:bots/python/template_bot.py --bot builtin:random --players 9

# 10-tournament series with the geometric-mean leaderboard
$P series --length 10 --bot python:bots/python/template_bot.py --bot builtin:call --players 9

# protocol smoke test for your bot
$P smoke --bot python:my_bot.py

# 200-player load test (compiled subprocess bots)
$P bench --players 200 --process
```

Run the competition server (single box, embedded worker):

```bash
./target/release/competition-api --print-example-config > pokerbots.toml   # edit admin_key
POKERBOTS_ADMIN_KEY=secret ./target/release/competition-api
open http://localhost:8080/          # thin web UI: register, upload, activate, leaderboard
curl -s -X POST localhost:8080/api/v1/auth/register -H 'content-type: application/json' -d '{"team_name":"ace-high"}'
```

Uploads pass a three-stage pipeline before they can play — **build** (compile/syntax check),
**smoke** (protocol handshake), **trial** (~30 hands vs reference bots; rejects bots that crash
or time out) — then join the next run. With `[autoscaler]` configured, the platform sizes its own
worker fleet from queued work and run deadlines (single-box `processes` backend, or a `command`
backend for docker compose / k8s / cloud fleets); see `GET /admin/autoscale` and
[docs/OPERATIONS.md](docs/OPERATIONS.md).

Then follow [docs/QUICKSTART.md](docs/QUICKSTART.md) (students) and
[docs/OPERATIONS.md](docs/OPERATIONS.md) (staff). The API is described in
[docs/openapi.yaml](docs/openapi.yaml); the bot protocol in [docs/BOT_PROTOCOL.md](docs/BOT_PROTOCOL.md).

## Hand histories

Run `make view-hands` and open the printed localhost URL to load `hands.jsonl` and
replay cards, actions, chip stacks, and pot awards. Multiple files and tournament
series are supported. Save local histories with `--hand-log hands.jsonl`; see
[docs/HAND_VIEWER.md](docs/HAND_VIEWER.md) for log locations, format, and examples.

## Engine guarantees (tested)

* Blinds/antes incl. heads-up rules, short blinds all-in for less; BB/SB option.
* "Raise-to" semantics; min raise = last full raise; short all-ins do not reopen action.
* Side pots from total contributions; uncalled bets returned; odd chips left of the button.
* Chip conservation is fuzz-tested over thousands of random hands.
* Timeouts / crashes / illegal actions → auto check/fold with an `ActionSubstituted` event; a dead
  bot never costs a timeout.
* Tournament: ⌈T/N⌉ balanced tables, breaking at `break_threshold`, blind levels synchronised at
  hand boundaries (hands / time / average-stack triggers), placements by hands played with shared
  ranks, geometric-mean series scoring.

Two SPEC clarifications made explicit in code: the average-stack trigger fires when the average
stack *reaches* the threshold (it only grows as players bust), and the hands trigger uses the
average hands per table in the current level.

## Development

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). Short version: `make check` (or
`make docker-check` for the containerised toolchain) runs fmt + clippy `-D warnings` + the full
test suite — the same gate as CI, which additionally smoke-tests the reference bots and builds
the Docker images (`.github/workflows/ci.yml`).
