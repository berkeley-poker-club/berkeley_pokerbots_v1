# Berkeley Pokerbots

A complete No-Limit Hold'em tournament platform for bot competitions: a rules-correct engine,
multi-table tournament director, JSON-lines bot protocol, and a competition service (submissions,
sandboxed workers, nightly series, geometric-mean leaderboard) for ~200 concurrent bots.

```
poker-utils/           pure NLHE engine: cards, hand evaluation, Hand state machine, config
table-runner/          Player trait, ProcessBot (JSON-lines protocol), in-process bots, table task, smoke test
tournament-core/       TournamentDirector (table breaking, blind levels, SPEC placements), series + scoring
pokerbots-cli/         `pokerbots`: local tournaments/series, smoke tests, stdio reference bots, load tests
competition-platform/  `competition-api` + `tournament-worker`: SQLite store, artifacts, sandbox, jobs, API, scheduler
bots/python/           SDK, reference bots (fold / call / raise / random) and a student template
docs/                  BOT_PROTOCOL.md, openapi.yaml, QUICKSTART.md, OPERATIONS.md
deploy/                Dockerfile, docker-compose.yml
SPEC.md                game + tournament rules (source of truth)
```

## Quick start

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
curl -s -X POST localhost:8080/api/v1/auth/register -H 'content-type: application/json' -d '{"team_name":"ace-high"}'
```

Then follow [docs/QUICKSTART.md](docs/QUICKSTART.md) (students) and
[docs/OPERATIONS.md](docs/OPERATIONS.md) (staff). The API is described in
[docs/openapi.yaml](docs/openapi.yaml); the bot protocol in [docs/BOT_PROTOCOL.md](docs/BOT_PROTOCOL.md).

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

```bash
cargo test --workspace          # engine, protocol (needs python3), tournaments, platform e2e
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

CI runs the same plus smoke tests of the reference bots (`.github/workflows/ci.yml`).
