# Viewing hand histories

Run `make view-hands` and open the printed localhost URL. It serves the viewer and
loads the neighboring `hands.jsonl` automatically. The viewer runs locally with no
dependencies, and files stay in the browser. Use the file picker to select other
logs or several files together to compare runs.

Choose a tournament/source and **Tables shown** (1–4, limited to the tables in
that source). Each display has its own table selector. With one display,
**All tables** follows file order, or choose a table to browse only its hands.
With multiple displays, each **Hand round** pairs the next recorded hand from
each selected table. Selecting an already displayed table swaps the two slots.
If a table has fewer hands, its slot shows an empty state in later rounds.

Play, step buttons, and the slider advance every displayed table together. A
shorter hand stays at its final state until the other hands finish. Enable
**Continue to next hand / round** to play consecutive rounds automatically.
**Focus timeline** chooses which table appears in the compact bottom-right
action timeline and the expandable hand result. Clicking a timeline event seeks
all tables to that step. **Focus view** hides the heading and file-loading panel;
on desktop it scales the tables to fit the available window height. Exit Focus
view to change files. Small screens stack tables vertically.

**Timing limitation:** these logs do not include hand/event timestamps. Parallel
playback aligns per-table record order and event steps, not actual elapsed time.
It cannot establish that the paired hands happened at the same instant; table
breaks, moves, different hand lengths, and missing records can change alignment.

Tables show cards, stacks, street commitments, folds, all-ins, and pot awards.
Uncheck **Hole cards** to hide private cards until showdown. A dealing burst is
one replay step. The persistent **BTN** marks the dealer; bets reset on a new
street. Hover a seat for its contributions; click a seat to inspect that player.
The expandable hand result shows final chip changes, independent of playback.
Player strategy names are used when recorded; otherwise labels use player IDs.
Seats are zero-based.

## Player rankings and statistics

The right sidebar ranks players by last recorded stack, net chips, hands, or a
statistic. Select a player in the ranking or click their seat for detailed stats.
Search by name or player ID. Expand **All player stats** for a sortable report,
or **Export CSV** for the currently searched players, scope, and order. Player
IDs carry stats across seat/table moves; source files and tournaments stay
separate.

**All loaded hands** includes the complete selected source/tournament, including
future hands relative to playback. **Before current hand / round** excludes the
current hands and everything after them. In single-display All tables mode it
uses the preceding file records; otherwise it uses the preceding records at each
table, including tables not currently displayed. Both scopes cover all tables
in the selected source/tournament. Last stack is the latest recorded **final**
stack in that scope, not a live count of tournament placements.

Every percentage shows its numerator/denominator; hover for definitions.
**—** means no eligible opportunity, **0%** means opportunities with no success,
and **∞** AF means aggression without a call. These are observed decisions,
including actions substituted by the engine, not estimates from hole cards.

| Stat | Calculation used by this viewer |
| --- | --- |
| VPIP | Hands with a voluntary preflop call/bet/raise / hands dealt; forced posts excluded |
| PFR | Hands with any preflop raise / hands dealt |
| 3B | Preflop re-raises / eligible decisions facing the first raise; short raises must reopen action for prior actors |
| F3B | Folds to a preflop 3-bet after voluntarily investing / such decisions, before a 4-bet |
| CB | Flop continuation bets / unopened-flop decisions by the last preflop raiser |
| FCB | Folds facing a flop c-bet / decisions facing it, before an intervening raise |
| Turn CB / River CB | Continuing bets on that street / eligible decisions after c-betting previous streets, without an intervening raise or lead |
| WTSD | Showdowns reached / hands where the player saw the flop, including all-in runouts |
| W$SD | Showdowns with any pot award / showdowns reached; split and side-pot shares count |
| WWSF | Hands with any pot award after seeing a flop / flops seen |
| ATS | Open raises when folded to in cutoff/button/small blind / eligible steal decisions |
| AF | Postflop bets and raises / calls; checks and folds excluded |
| AFq | Postflop bets and raises / bets, raises, calls, and folds; checks excluded |
| bb/100 | Sum of net chips divided by each hand's big blind, normalized per 100 hands |

These definitions follow common [PokerTracker statistical conventions](https://www.pokertracker.com/guides/PT3/general/statistical-reference-guide),
with the specific opportunity rules above. This viewer's lowercase **bb** means
big blinds, not the older tracker convention of big bets. Tournament bb/100 is
a chip rate, not cash profit, tournament ROI, or all-in-adjusted equity. Small
samples and partial histories can be misleading; the samples are shown so you
can assess them directly.

Run the dependency-free stats/parser/replay checks with:

```bash
node tests/hand-viewer.test.cjs
```

To start the viewer without Make, serve the repository and open the URL with
`?load-default=1` to load the neighboring history automatically:

```bash
python3 -m http.server 8000 --bind 127.0.0.1
# Visit http://localhost:8000/hand-viewer.html?load-default=1
```

The file picker works without a server. Selecting files again replaces the
loaded selection; it also lets you reload a log after a run has finished.

## Saving histories

The local CLI only saves histories when `--hand-log` is provided. Relative paths
are resolved from the directory where you run the command. Each invocation
creates or **overwrites** the specified file, so use separate filenames to keep
separate runs:

```bash
./target/release/pokerbots tournament \
  --bot builtin:random --bot builtin:call --players 9 \
  --hand-log hands.jsonl

./target/release/pokerbots series --length 10 --parallel 2 \
  --bot builtin:random --bot builtin:call --players 9 \
  --hand-log series-hands.jsonl
```

For the competition platform, set `[defaults] record_hands = true` in the server
configuration before creating a run. The default is false. Histories are stored
under `<storage.logs_dir>/<run_id>/<tournament_index>/hands.jsonl`; the default
logs directory is `data/logs`. Administrators can download them through
`GET /api/v1/runs/{run_id}/tournaments/{index}/hands`.

`pokerbots.example.toml` configures the platform. The local CLI's `--config`
expects a tournament configuration instead; generate one with
`./target/release/pokerbots default-config > tournament.toml`.

## Format and multiple games

The actual format is **JSON Lines** (`.jsonl` / NDJSON): each line is a complete
JSON object describing one completed hand. It is not a single JSON array and
is not one event per line. Every record has:

| Field | Meaning |
| --- | --- |
| `tournament_id` | Tournament the hand belongs to |
| `table_id` | Table within that tournament |
| `hand_id` | Engine hand identifier; not a simple hand number |
| `level` | Zero-based blind level |
| `events` | Ordered events, including starting seats, private cards, actions, board cards, awards, and final seats |
| `result` | Final seats, board, participants, busted players, and pot awards |

Single tournaments, multi-table tournaments, and series use the same record
format. A local tournament uses `tournament_id: "local"`; a local series uses
`"local#1"`, `"local#2"`, etc. A CLI series puts all hands into the one requested
log. With concurrent tournaments or tables, records can interleave: group by
tournament and table rather than treating the whole file as one table's sequence.
The viewer preserves the recorded order within each selection.

The platform keeps one file per tournament, using a tournament ID formed from
the run ID and its zero-based tournament index. The viewer groups by **source
file and tournament ID**, so independent files whose IDs repeat remain separate.
Keep independent local runs in separate files: concatenating logs with identical
tournament IDs loses the information needed to distinguish those runs.

The viewer also accepts a single hand object or an array of hand objects in a
`.json` file. Renaming JSON Lines to `.json` does not turn it into a JSON array.
Invalid records are reported with their line/record number, while valid hands
still load. This can help when inspecting a partially written file; completed
logs are preferable because the writer buffers records.

## Files to start with

| File or directory | Why it matters |
| --- | --- |
| [`README.md`](../README.md), [`QUICKSTART.md`](QUICKSTART.md), [`Makefile`](../Makefile) | Project map, local commands, and common build/test tasks |
| [`bots/python/template_bot.py`](../bots/python/template_bot.py), [`pokerbots_sdk.py`](../bots/python/pokerbots_sdk.py) | Starting point and protocol helpers for your bot |
| [`BOT_PROTOCOL.md`](BOT_PROTOCOL.md) | What your bot receives and how it responds |
| [`SPEC.md`](../SPEC.md) | Game and tournament rules |
| [`pokerbots-cli/src/main.rs`](../pokerbots-cli/src/main.rs) | CLI options, tournament/series commands, and local history writer |
| [`poker-utils/src/hand.rs`](../poker-utils/src/hand.rs), [`events.rs`](../poker-utils/src/events.rs) | Hand engine and event schema used by the viewer |
| [`table-runner/`](../table-runner/) | Bot processes, decisions, timeouts, and table execution |
| [`tournament-core/`](../tournament-core/) | Multi-table orchestration, series, and scoring |
| [`pokerbots.example.toml`](../pokerbots.example.toml), [`competition-platform/`](../competition-platform/) | Server configuration, API, jobs, and platform log storage |
