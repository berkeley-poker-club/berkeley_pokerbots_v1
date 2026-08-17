# Student Quickstart

Everything you need to go from zero to a bot on the leaderboard.

## 1. Write a bot

Start from [`bots/python/template_bot.py`](../bots/python/template_bot.py) (Python 3.9+, no
dependencies) or read [`BOT_PROTOCOL.md`](BOT_PROTOCOL.md) to implement the JSON-lines protocol in
any language. Keep `pokerbots_sdk.py` next to your bot if you use the SDK.

## 2. Test locally

Build the tools once (`cargo build --release`), then:

```bash
# protocol smoke test (what the server runs on upload)
pokerbots smoke --bot python:my_bot.py

# a full 9-player tournament against reference bots, hand histories to a file
pokerbots tournament --bot python:my_bot.py --bot builtin:call --bot builtin:raise \
    --bot builtin:random --players 9 --hand-log hands.jsonl

# a 10-tournament series with the geometric-mean leaderboard
pokerbots series --length 10 --bot python:my_bot.py --bot python:bots/python/call_bot.py --players 9
```

`--bot` accepts `python:<file.py>`, `builtin:<fold|call|raise|random>`, `exec:<cmd args>` or a
path to an executable. `--players N` cycles through the specs to fill N seats.

## 3. Register your team (once)

```bash
export PB=https://pokerbots.example.edu/api/v1
curl -s -X POST $PB/auth/register -H 'content-type: application/json' \
  -d '{"team_name":"ace-high","members":[{"name":"Ada","email":"ada@berkeley.edu","student_id":"12345678"}]}'
# → {"team_id":"tm_...","team_name":"ace-high","api_key":"pb_live_...","created_at":"..."}
export PB_KEY=pb_live_...      # shown once — store it safely
```

(If registration is closed, the course staff will give you a key.)

## 4. Upload

```bash
zip -r bot.zip my_bot.py pokerbots_sdk.py
curl -s -X POST $PB/submissions -H "X-Api-Key: $PB_KEY" \
  -F artifact=@bot.zip \
  -F 'manifest={"language":"python","runtime":"python3","entrypoint":"my_bot.py","protocol_version":"1"}'
```

* `201` → `{"submission_id":"sub_tm_..._0001","status":"validated","smoke_test":{"passed":true,...}}`
* `422` → the smoke test failed; `error.details.reason` and `error.details.stderr_tail` tell you why.
* `202` → the smoke test is still running (rare); poll `GET /submissions/{id}`.

## 5. Activate

Uploads are never active by themselves. Exactly one submission per team is active at a time:

```bash
curl -s -X POST $PB/submissions/sub_tm_..._0001/activate -H "X-Api-Key: $PB_KEY"
curl -s $PB/me -H "X-Api-Key: $PB_KEY"          # shows active_submission_id
```

The bots that are active when a run starts are the ones that play in it.

## 6. Runs and results

* Every night a series of tournaments runs with all active bots; the public board is at
  `GET /leaderboard` (score = geometric mean of your placements; lower is better).
* You can also start a small on-demand run yourself (rate limited, e.g. 2/day):

```bash
curl -s -X POST $PB/runs -H "X-Api-Key: $PB_KEY" -H 'content-type: application/json' \
  -d '{"mode":"series","series_length":5}'
curl -s $PB/runs/run_...                        # progress
curl -s $PB/runs/run_.../tournaments/0          # placements of one tournament
curl -s "$PB/leaderboard?run_id=run_..."        # board for that run
curl -s $PB/teams/tm_.../stats -H "X-Api-Key: $PB_KEY"
```

## Tips

* Answer within the deadline (1 s by default) — do heavy precomputation at start-up, not per hand.
* Never print to stdout except protocol messages. Log to stderr.
* If your bot crashes it auto-folds for the rest of the tournament; check the `smoke_test.stderr_tail`
  and test locally with `pokerbots tournament`.
* The full API is described in [`openapi.yaml`](openapi.yaml) (also served at `/api/v1/openapi.yaml`).
