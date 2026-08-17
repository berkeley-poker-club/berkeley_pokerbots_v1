# Pokerbots Bot Protocol (version 1)

Your bot is a program that reads **JSON Lines** from standard input and writes JSON Lines to
standard output. One JSON object per line, UTF-8, `\n` terminated. Nothing else may be written to
stdout — use stderr for debugging (the last few KB of stderr are shown to you when a smoke test
fails). No network access is available inside the sandbox.

A reference SDK and four reference bots live in [`bots/python/`](../bots/python/):
`pokerbots_sdk.py`, `fold_bot.py`, `call_bot.py`, `raise_bot.py`, `random_bot.py`, plus a
starter `template_bot.py`.

## Lifecycle

```
engine → bot   {"type":"hello", "protocol_version":"1", "player_id":7, "session":"run_x-3"}
engine → bot   {"type":"notify_event", "event":{...}}          (many, throughout the tournament)
engine → bot   {"type":"request_action", "request_id":"req_7_12", "deadline_ms":1000,
                "context":{...}, "legal":{...}}
bot → engine   {"type":"action", "request_id":"req_7_12", "action":{"kind":"RaiseTo","amount":60}}
...
engine → bot   {"type":"goodbye"}                                (then stdin is closed)
```

* One process is started per bot **per tournament**; it lives until it is eliminated or the
  tournament ends. Exit promptly on `goodbye` or when stdin closes; otherwise you are killed.
* You must answer every `request_action` **before `deadline_ms`** (default 1000 ms) with a
  matching `request_id`. Late, unparseable, or illegal answers are replaced by the engine's auto
  action — *check* if checking is legal, otherwise *fold* — and an `ActionSubstituted` event is
  broadcast. Answers to old requests are ignored. A crashed bot is auto-folded for the rest of the
  tournament (it can still finish in the money by blinding out slower than others).
* Optional messages from the bot: `{"type":"hello_ack","name":"..."}` and
  `{"type":"log","message":"..."}` (appended to your stderr log). Anything else is ignored.
* Lines longer than 1 MiB are discarded.

## Actions

```json
{"kind":"Fold"}  {"kind":"Check"}  {"kind":"Call"}
{"kind":"BetTo","amount":40}  {"kind":"RaiseTo","amount":120}  {"kind":"AllIn"}
```

Amounts are **"to" amounts**: the total number of chips you will have committed on the current
street after the action (not the increment). `BetTo` is only legal when nobody has bet on this
street (post-flop with no bet); pre-flop the blinds count as a bet, so opening is a `RaiseTo`.
`AllIn` commits your whole stack and is normalised by the engine to a call, bet or raise.

The `legal` object tells you exactly what is allowed:

```json
{
  "can_fold": true, "can_check": false, "can_call": true, "can_bet": false, "can_raise": true,
  "can_all_in": true,
  "to_call": 10,          // chips needed to call (0 if you can check)
  "call_amount": 10,      // what you would actually add (less than to_call when calling all-in)
  "min_bet_to": 0, "max_bet_to": 0,           // valid BetTo totals when can_bet
  "min_raise_to": 20, "max_raise_to": 1000,   // valid RaiseTo totals when can_raise
  "all_in_to": 1000                            // the "to" amount of shoving
}
```

Rules worth knowing (full details in `SPEC.md`):

* Minimum raise = the last full raise increment (initially the big blind). An all-in raise that is
  smaller than a full raise does **not** reopen the action for players who already acted since the
  last full raise: they may only call or fold (`can_raise` will be false).
* The big blind (and small blind) get their option even if nobody raises pre-flop.
* Once at most one player can still act, remaining streets are dealt without betting.
* Side pots are built from total contributions (antes included); uncalled chips return to the
  bettor; split pots give odd chips to the first winner clockwise from the button.

## `request_action.context` (DecisionContext)

```json
{
  "hand_id": 4294967302, "table_id": 1, "street": "Flop",
  "my_seat": 3, "my_player_id": 7, "button": 0,
  "small_blind": 5, "big_blind": 10, "ante": 0,
  "my_hole_cards": ["As","Kd"], "board": ["Ah","7c","2d"],
  "pot": 65, "bet_level": 20, "to_call": 20, "min_raise": 20,
  "my_stack": 980, "my_committed_street": 0, "my_committed_total": 10,
  "seats": [ {"seat":0,"player_id":3,"stack":990,"committed_street":20,"committed_total":30,"status":"Active"}, ... ],
  "history": [ {"street":"Preflop","seat":1,"action":{"kind":"Call"},"amount":10,"all_in":false}, ... ]
}
```

* `street`: `Preflop | Flop | Turn | River`. Cards are two-character strings: rank
  `2-9 T J Q K A` + suit `c d h s`.
* `seats` covers every seat at the table (`status`: `Empty | Active | Folded | AllIn`);
  `player_id` is null for empty seats.
* `history` is the public action list of the current hand so far, so a stateless bot is possible.
* `bet_level` is the current bet on this street (max committed by any seat); `to_call` is what
  *you* still owe.

## Events (`notify_event.event`, discriminated by `kind`)

Tournament/table level:

| kind | fields |
|------|--------|
| `TournamentStarted` | `tournament_id`, `player_id` (yours), `num_players`, `starting_stack` |
| `Seated` | `table_id`, `seat` (yours), `seats[]` |
| `BlindLevel` | `level`, `small_blind`, `big_blind`, `ante` (effective from the next hand) |
| `PlayerBusted` | `seat`, `player_id` |
| `PlayerMoved` | `seat`, `player_id` (table is breaking; you will get a new `Seated`) |
| `TournamentEnded` | `tournament_id`, `winner`, `your_placement` |

Hand level:

| kind | fields |
|------|--------|
| `HandStarted` | `hand_id`, `table_id`, `button`, `small_blind`, `big_blind`, `ante`, `seats[]` |
| `AntePosted` | `seat`, `amount`, `all_in` |
| `BlindPosted` | `seat`, `amount`, `big` (true = big blind), `all_in` |
| `HoleCards` | `seat`, `cards` — **private**, you only receive your own |
| `ActionTaken` | `seat`, `street`, `action`, `amount` (chips added), `all_in`, `committed_street` |
| `ActionSubstituted` | `seat`, `reason`, `substituted` (the engine acted for a slow/illegal bot) |
| `BoardDealt` | `street`, `cards` (new), `board` (all), `pot` |
| `ShowdownRevealed` | `seat`, `cards`, `strength`, `description` (e.g. `TwoPair(A9K)`) |
| `PotAwarded` | `pot_index`, `amount`, `eligible_seats[]`, `winners[[seat, amount], ...]` |
| `HandEnded` | `hand_id`, `table_id`, `seats[]` (stacks after the hand) |

`seats[]` entries have the same shape as in the decision context.

## Timing and resources

* Decision deadline: `action_timeout_ms` (default **1000 ms**, shown in `deadline_ms`). Budget
  for slow starts: the very first request of a tournament arrives right after your process starts.
* Sandbox (production): one container per bot, no network, 512 MB RAM, 1 CPU, 64 processes,
  read-only filesystem except `/tmp`. Your artifact is mounted read-only at `/bot` and is the
  working directory. Environment: `POKERBOTS_PLAYER_ID`, `POKERBOTS_SESSION`.
* Keep stdout **unbuffered/flushed** after each answer (the SDK does this for you). Python:
  `PYTHONUNBUFFERED=1` is set for you.

## Submitting

Upload a zip (or a single file) with a manifest via `POST /api/v1/submissions`:

```json
{"language":"python","runtime":"python3","entrypoint":"my_bot.py","protocol_version":"1"}
```

`runtime` ∈ `python3 | node | java | native | auto` (`auto` picks by extension: `.py`, `.js`,
`.jar`, otherwise a native executable). `native` entrypoints must be Linux x86-64 executables when
the server runs Docker (build with `--target x86_64-unknown-linux-musl` for Rust/C++).

The server runs a **smoke test** before accepting: it starts your bot, sends `hello`, a few
events and two `request_action`s from a synthetic hand, and expects legal answers within the smoke
timeout (10 s by default, including interpreter start-up). Failures return `422` with the reason
(`timeout | invalid_json | illegal_action | crashed | setup_failed`) and your stderr tail.

Test locally before uploading:

```
pokerbots smoke --bot python:my_bot.py
pokerbots tournament --bot python:my_bot.py --bot builtin:call --bot builtin:random --players 9
```
