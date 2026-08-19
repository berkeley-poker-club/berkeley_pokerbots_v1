# Overview

## Core Components

1.  **Game State**: Maintains all rules, players, cards, pots, and
    betting status

2.  **Rules Engine**: Validates and applies moves, enforces blinds/antes

3.  **Table Runner / Event Loop**: Orchestrates dealing, betting rounds,
    street advancement, and showdown

4.  **Hand Evaluator**: Compares hands at showdown given game rules
    (default NLH)

5.  **Pot Manager**: Calculates pots and side pots, distributes winnings

6.  **Serialization**: Provides language-neutral protocol for bot
    communication

7.  **Public API**: Provides standardized information sets to players
    and accepts their decisions

# Table-level Data Structures

## Game State

    GameState:
        rules: Rules
        deck: list of Card
        board: list of Card
        seats: list of SeatState
        pot: PotManager
        betting: BettingState
        actor: Seat
        button: Seat
        hand_id: int

## Seat State

    SeatState:
        player_id: PlayerId
        stack: int
        committed: int       # current street
        total_committed: int # across all streets
        hole_cards: list of Card
        status: {Active, Folded, AllIn, Empty}

## Betting State

    BettingState:
        to_call: int
        min_raise: int
        last_raiser: Seat or None
        can_act: list of Seat

## Pot Manager

    PotManager:
        main_pot: int
        side_pots: list of (eligible_seats, amount)

# Core Functions

## Move Validation

Input: `state, seat, proposed_action`\
Output: `ValidAction` or `Error`

-   Only current actor may act (extraneous if event loop makes requests
    to player)

-   Fold/Check/Call/Bet/Raise/All-in constrained by `to_call`,
    `min_raise`, and stack

-   Raise semantics are "raise-to", i.e., specify total amount

-   Short all-ins below minimum raise do not reopen action

-   etc.

## Apply Action (pseudocode)

    function apply_action(state, action):
        if action.kind == Fold:
            mark seat as Folded
        if action.kind == Check:
            update can_act list
        if action.kind == Call or AllIn:
            deduct from stack
            add to committed
        if action.kind == Bet or Raise:
            update to_call and min_raise
            update committed and last_raiser
            reset can_act for others
        advance actor to next seat

## Betting Round Loop

    while can_act is not empty:
        ctx, legal = build_infoset(state, actor)
        action = player.act(ctx, legal, deadline)
        if validate(action):
            apply_action(state, action)
        else:
            reject and request again (until deadline)
    if multiple active seats remain:
        move committed chips to pots

## Side Pot Construction

    function build_side_pots(seats):
        sort seats by committed amount
        for each unique level:
            create pot = delta * num_eligible
            record eligible seats

# Player Abstraction Layer

## Observation Flow

All players receive a stream of `PublicEvent`s:

-   Blinds/antes posted

-   Cards dealt (others only see counts)

-   Actions taken with amounts

-   Board cards revealed

-   Pots awarded

## Decision Flow

    DecisionContext:
        hand_id, me, button, street
        my_hole: list of Card
        board: list of Card
        stacks, committed, total_committed (public)
        pot, to_call, min_open, min_raise, max_bet
        active/folded/all-in seats
        action history (public) # remove this and maybe let players parse?

    LegalActions:
        allowed: {Fold, Check, Call, Bet, Raise, AllIn}
        to_call, min_open, min_raise, max_bet, max_raise_to

## Player API

-   `on_event(event)`: observe public information

-   `act(context, legal, deadline)`: given infoset and legal moves,
    return one `Action`

## Action Format

    Action:
        kind: {Fold, Check, Call, BetTo, RaiseTo, AllIn}
        amount: int (relevant for BetTo/RaiseTo/AllIn)

# Main Event Loop & Player Management

## Player Storage Model

    Player:
        id: PlayerId
        display_name: string
        bankroll / stack: int
        seat: Seat
        send_event(event): void
        request_action(ctx, legal, deadline_ms) -> Action or Timeout

## Request/Response Contract

1.  Engine broadcasts `PublicEvent`s to all endpoints via `send_event`

2.  When `actor` must act, engine builds `DecisionContext` and
    `LegalActions` (infoset), then calls
    `request_action(ctx, legal, deadline_ms)` on the actor's endpoint

3.  The endpoint returns exactly one `Action` before the deadline, or
    signals `Timeout`

4.  Engine validates the action; on error it may re-request (if time
    remains) or apply auto-check/fold on timeout

## Main Table Runner Loop (Pseudocode)

    run_table(table):
        # end loop to reorganize tables or if there is a winner of some kind?
        loop forever: 
            state = new_hand_state(table)
            broadcast(PublicEvent.HandStarted(hand_id, button, seats_summary(state)))

            post_forced_bets(state) # emits events
            deal_hole_cards(state) # emits events

            for street in [Preflop, Flop, Turn, River]:
                if street != Preflop:
                    deal_board_for(street, state) # emits BoardDealt event
                start_betting_round(state) # reset to_call, min_raise, can_act, actor

                # Betting Round 
                while not betting_round_complete(state):
                    if only_one_contender_left(state):
                        award_uncontested(state) # emit PotAwarded
                        goto hand_cleanup

                    actor = state.actor
                    (ctx, legal) = build_infoset(state, actor)

                    deadline_ms = compute_deadline(state, actor)
                    action = request_action_with_timeout(actor.endpoint, ctx, legal, deadline_ms)

                    if action == Timeout:
                        action = auto_action(legal) # Check if to_call==0 else Fold
                        broadcast(PublicEvent.DecisionTimeout(actor))

                    if not validate(state, actor, action):
                        if time_remaining(deadline_ms):
                            broadcast(PublicEvent.ActionRejected(actor, reason="illegal"))
                            continue # ask again within time?
                        else:
                            action = auto_action(legal)

                    apply_action(state, actor, action)
                    broadcast(PublicEvent.ActionTaken(actor, action))
                    advance_actor(state)

                move_committed_to_pots(state) # side-pot

                if showdown_ready_early(state):
                    break

            # Showdown
            showdown_and_payout(state) # emits ShowdownReveal, PotAwarded, HandEnded

            hand_cleanup:
                emit_hand_summary(state)
                persist_hand_log_if_needed(state)
                reset_per_hand_buffers(table)

## Suboutines

#### Betting Round Control

    start_betting_round(state):
        betting.to_call = 0
        betting.min_raise = rules.big_blind
        for seat in seats: seat.committed = 0
        betting.can_act = compute_can_act(state)
        state.actor = first_actor_for_street(state)

    betting_round_complete(state):
        return (betting.can_act is empty)

    advance_actor(state):
        if betting.can_act is empty: return
        state.actor = next_seat_in_rotation_after(state.actor) that is in betting.can_act

#### Infoset Projection

    build_infoset(state, me):
        ctx = DecisionContext(
            hand_id=state.hand_id, me=me, button=state.button, street=state.street,
            my_hole = state.seats[me].hole_cards,
            board   = state.board,
            stacks  = [s.stack for s in state.seats],
            committed = [s.committed for s in state.seats],
            total_committed = [s.total_committed for s in state.seats],
            pot = current_pot_total(state),
            active/folded/allin masks or lists,
            to_act_order = rotation_from_actor(state),
            small_blind = rules.small_blind, big_blind = rules.big_blind,
            )
        legal = compute_legal_actions(state, me)
        return (ctx, legal)

#### Validation (Pure) and Application (Stateful)

    validate(state, seat, action): ValidAction or Error
    apply_action(state, seat, ValidAction): void

#### Side Pots

    move_committed_to_pots(state):
        levels = unique_sorted([s.total_committed for s in contenders(state)])
        for i in 0..len(levels)-1:
            delta = levels[i] - levels[i-1] (or levels[i] if i==0)
            eligible = seats with total_committed >= levels[i]
            amount = delta * count(eligible)
            add_pot(eligible, amount)

# Top-Level Tournament Orchestration *Tournament Style Variant*

## Tournament Spec and Rules

-   Players can submit bots on a rolling basis. **Exactly one active bot
    per player/team** at any time.

-   Each night at midnight, a **series of N tournaments** is hosted with
    all currently active bots

-   For each tournament with $T$ players and table size $N$, start with
    $\lceil T/N \rceil$ tables. No starting table may have fewer than
    $N-k$ players; rebalance quasi-randomly so all table sizes differ by
    at most $1$.

-   During play, if any table drops to $N-k$ players at a hand boundary,
    that table **breaks** and its remaining players are redistributed
    quasi-randomly to other tables (sizes kept within $\pm 1$).
    Re-seated players wait until the next hand at their destination
    tables and are placed in random open seats.

-   Blind levels increase based on **average stack per active player**
    crossing configured thresholds *or* a level duration cap. Level
    changes occur **only at hand boundaries** and are synchronized
    across all tables.

-   Tournament continues until **one player remains**. **Placement
    ranking** is determined by *number of hands played* (not
    timestamps). Larger hands-played $\Rightarrow$ *better* placement
    (i.e., lower numeric place). Players eliminated in the same hand
    share the same rank.

-   After the nightly series of 100 tournaments, each player's
    **leaderboard score** is the *geometric mean* of their placements
    across those 100 tournaments. Lower scores are better; the public
    leaderboard is sorted ascending.

## Data Structures

    BotRegistry:
        active_bots: map PlayerId -> Player # at most one active per player/team
        pending_submissions: queue of (PlayerId, BotRef, submitted_at)

    TournamentConfig:
        table_size: int
        min_table_size: int
        start_balance_tolerance: int = 1
        break_threshold: int  # break a table at or below this size
        blind_levels: list of LevelSpec
        avg_stack_thresholds: list of (level_id, min_avg_stack)
        max_level_duration_seconds: int
        series_length: int = 100
        rng_seed: int // keep track of random seed maybe?

    LevelSpec:
        level_id: int
        small_blind: int
        big_blind: int
        ante: int

    TournamentState:
        tournament_id: TournamentId  # identifies series number and number within the series
        players: map PlayerId -> Player
        tables: list of TableHandle
        waiting_lists: map TableId -> queue PlayerId   # join at next hand
        level_index: int
        level_started_at: time
        hands_played: map PlayerId -> int
        eliminated: list of (PlayerId, hands_played)   # in elimination order
        active_players: set PlayerId

    TableHandle:
        table_id: TableId
        seats: list of (SeatIndex, PlayerId or Empty)
        status: {Running, Paused}
        runner_endpoint: Endpoint # control plane to table runner

    Leaderboard:
        # geometric mean of placements, persistent across single tournament series
        last_score: map PlayerId -> float
        # submission id identifies both the player id and their active bot submission
        active_bots: map PlayerId -> SubmissionId

## Control Plane APIs (TD $\leftrightarrow$ Tables)

-   **TD $\to$ Table**: `CreateTable(N)`, `SeatPlayer(player, seat)`,
    `Start()`, `PauseAfterHand()`, `ApplyBlinds(level)`,
    `CloseAfterHand()`.

-   **Table $\to$ TD (events)**: `HandEnded(summary)`,
    `PlayerBusted(player)`, `TableSizes(active_count)`,
    `ReadyForReseat()`.

## Nightly Series Orchestrator

    run_nightly_series(config, registry, clock):
        wait_until_midnight(clock)

        players = snapshot_active_players(registry)   # one active bot per PlayerId
        placements_over_series = map PlayerId -> list()

        for series_idx in 1..config.series_length:
            placement_map = run_single_tournament(config, players)
            for (p, place) in placement_map:
                placements_over_series[p].append(place)

        scores = compute_geometric_means(placements_over_series)   # lower is better
        update_public_leaderboard(scores)

## Single Tournament Orchestration

#### Construction, Start Balancing, and Synchronization

    run_single_tournament(config: TournamentConfig, players: List of PlayerId):
        tables = build_tables(players, N)
        if any table.size < config.min_table_size:
            tables = rebalance_start(tables, tolerance=config.start_balance_tolerance)

        state = TournamentState(
            players=players, tables=tables, waiting_lists=empty_queues(),
            level_index=0, level_started_at=now(), hands_played=zero_map(players),
            eliminated=[], active_players=set(players)
        )

        broadcast ApplyBlinds(config.blind_levels[0]) at all tables (next-hand boundary)
        start_all_tables(state.tables)

        while |state.active_players| > 1:
            process_table_events(state)         # consumes HandEnded, PlayerBusted, sizes
            maybe_break_tables(state, config)   # at hand boundaries only
            integrate_waiting_players(state)    # seat to random open seats for next hand

            if should_advance_level(state, config):
                synchronize_level_change(state, config)  # wait hand boundary, then apply

            sleep(small_interval)

        # Winner is the last active player; assign placements and return
        placements = finalize_placements(state)
        return placements

## Initial Table Building and Rebalancing

    build_tables(players, N, tolerance=1):
        shuffled = shuffle(players)
        tables = chunk_into_tables(shuffled, N) # ceil(len/N) tables
        
        # Ensure all table sizes differ by at most 'tolerance' and >= min_table_size
        # if not, pull from largest tables to smallest until balanced
        
        while ((max_size(tables) - min_size(tables) > tolerance) or 
                    any(table.size < min_table_size)):
            donor = argmax_size(tables)
            receiver = argmin_size(tables)
            p = donor.pop_random_player()
            receiver.add_player(p)
        return tables

## Runtime Table Breaking and Redistribution

#### Trigger and Distribution (Hand Boundaries Only).

    maybe_break_tables(state, config):
        for table in state.tables:
            if table.active_count <= config.break_threshold and
               table.active_count > 0:
                # Mark table to close after current hand
                table_close_queue.add(table)

        for table in drain(table_close_queue):
            players_to_move = table.list_active_players()
            issue CloseAfterHand(table)
            # Quasi-random distribution:
            pool = shuffle(players_to_move)
            while not empty(pool):
                p = pop_front(pool)
                dst = table_with_min_active_count(state.tables, excluding=table)
                enqueue_waiting(state.waiting_lists[dst.id], p)   # join next hand

#### Waiting-List Integration and Random Seating.

    integrate_waiting_players(state):
        for table in state.tables:
            if table.signaled(ReadyForReseat) and has_open_seats(table):
                while has_open_seats(table) and not empty(state.waiting_lists[table.id]):
                    p = dequeue(state.waiting_lists[table.id])
                    seat = pick_random_open_seat(table)
                    SeatPlayer(table, p, seat)       # effective next hand

## Blind-Level Advancement

#### Average-Stack Trigger and Time Trigger

    should_advance_level(state, config):
        level = config.blind_levels[state.level_index]
        avg_stack = total_chips_of_active_players(state) / |state.active_players|
        stack_trigger = exists (lvl_id, min_avg) in config.avg_stack_thresholds
                         where lvl_id == state.level_index + 1 and avg_stack <= min_avg
        time_trigger = (now() - state.level_started_at) >= config.max_level_duration_seconds
        return stack_trigger or time_trigger

#### Synchronized Change at Hand Boundary.

    synchronize_level_change(state, config):
        # Pause at hand boundary, then apply next level everywhere, then resume
        for t in state.tables: issue PauseAfterHand(t)
        wait until all t report ReadyForReseat()

        state.level_index += 1
        next_level = config.blind_levels[state.level_index]
        for t in state.tables: issue ApplyBlinds(next_level)

        state.level_started_at = now()
        resume_all_tables(state.tables)

## Event Processing and Bust Handling

    process_table_events(state):
        for ev in drain_all_table_events():
            match ev:
              HandEnded{table_id, hand_count_delta_by_player}:
                  for (p, delta) in hand_count_delta_by_player:
                      state.hands_played[p] += delta
              PlayerBusted{player_id}:
                  state.active_players.remove(player_id)
                  state.eliminated.append((player_id, state.hands_played[player_id]))
              TableSizes{table_id, active_count}:
                  update_cached_size(table_id, active_count)

## Placements, Ties, and Finalization

    finalize_placements(state):
        # Winner is the last remaining active player
        if |state.active_players| == 1:
            winner = only_element(state.active_players)
            state.eliminated.append((winner, state.hands_played[winner]))

        # Rank by hands_played (descending)
        # Players eliminated in the same hand share rank
        sorted = sort_desc(state.eliminated, key = hands_played)
        placements = map PlayerId -> place_number

        place = 1
        i = 0
        while i < len(sorted):
            j = i
            while j < len(sorted) and sorted[j].hands_played == sorted[i].hands_played:
                j += 1
            # Assign same place to [i .. j-1]
            for k in i..j-1: placements[sorted[k].player_id] = place
            # Next place advances by cohort size (competition ranking)
            place += (j - i)
            i = j

        return placements

**Note:** Higher number of hands played $\Rightarrow$ better finish
(lower numeric place). Elimination time-of-day is ignored.

## Leaderboard Update

    compute_geometric_means(series_placements):
        scores = map PlayerId -> float # default all values to 1 so geometric mean works
        for p in placements_over_series.keys():
            places = series_placements[p]
            g = exp( average( map(log, places) ) )
            scores[p] = g
        return scores

    update_public_leaderboard(scores, stats):
        leaderboard = sort_by_value(scores, ascending=true)  # lower is better
        publish(leaderboard)  # public leaderboard
        publish(stats)  # raw scores, placement on individual tournaments, etc.

# Implementation Notes (engine as built)

The Rust implementation follows this document; the following details are made explicit:

- **Dealing:** no burn cards; two hole cards per seat dealt in seat order starting left of the
  button, then flop/turn/river. Decks are seeded per (tournament seed, table, hand) so a
  tournament is reproducible given the same bot answers.
- **Blinds:** heads-up the button posts the small blind and acts first pre-flop; a short blind is
  posted all-in for less and the big blind still sets the price to call; antes are posted before
  the blinds and count toward side pots.
- **Button:** moves to the next occupied seat clockwise each hand (no dead-button rule); newly
  seated players take a random open seat and are dealt in from the next hand.
- **Betting:** amounts are "to" amounts; the minimum raise is the last full raise increment; a
  short all-in raise does not reopen the action for players who already acted since the last full
  raise; once at most one player can act, remaining streets are dealt without betting.
- **Pots:** built from total contributions at hand end (uncalled chips return through a
  single-eligible pot layer); odd chips go to the first winner clockwise from the button.
- **Placements:** the winner is placed 1st outright; everyone else is ranked by hands played
  (descending) with shared ranks (competition ranking).
- **Level advancement:** `hands_per_level` compares the *average* hands per table played in the
  current level; the average-stack trigger fires when the average stack per active player *reaches*
  the configured threshold (it only grows as players bust); the wall-clock cap is unchanged. All
  tables are paused at a hand boundary before a level is applied.
- **Table breaking:** one table breaks at a time, only when the remaining tables have room; its
  players go to the emptiest tables (capacity permitting) and join at the next hand.
- **Illegal / late / crashed:** the engine substitutes check-if-possible-else-fold and broadcasts
  `ActionSubstituted`; a dead bot fails fast (no timeout is waited).
- **Safety valve:** `max_hands_per_table` stops a tournament that cannot converge; survivors are
  then ranked by stack ahead of everyone eliminated.
