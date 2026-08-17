#!/usr/bin/env python3
"""
Starter template for a Pokerbots entry. Copy this file, rename the class, and implement `act`.

Submit a zip containing your files with a manifest like:
    {"language": "python", "runtime": "python3", "entrypoint": "template_bot.py", "protocol_version": "1"}
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pokerbots_sdk import Bot, run, check_or_call, bet_or_raise_to, fold, log  # noqa: E402

RANKS = "23456789TJQKA"


def rank_value(card):
    """'As' -> 12, '2c' -> 0."""
    return RANKS.index(card[0])


class TemplateBot(Bot):
    def on_hello(self, player_id, session):
        log("hello! I am player %s in session %s" % (player_id, session))

    def on_event(self, event):
        # Every public event at your table arrives here, e.g. HandStarted, ActionTaken, BoardDealt,
        # ShowdownRevealed, PotAwarded, HandEnded, BlindLevel, PlayerBusted, ...
        pass

    def act(self, ctx, legal):
        # ctx: hand_id, street, my_seat, button, my_hole_cards, board, pot, to_call, min_raise,
        #      my_stack, seats (stacks/status of everyone), history (actions so far this hand) ...
        # legal: can_fold/check/call/bet/raise/all_in, call_amount, min/max bet & raise totals.
        c1, c2 = ctx["my_hole_cards"]
        pair = c1[0] == c2[0]
        high = max(rank_value(c1), rank_value(c2))

        if pair or high >= rank_value("Q"):
            # Strong-ish: bet/raise about 3x the current bet level or 3 big blinds.
            target = max(3 * ctx["bet_level"], 3 * ctx["big_blind"])
            return bet_or_raise_to(legal, target)
        if legal["to_call"] <= ctx["big_blind"]:
            return check_or_call(legal)
        return fold() if not legal["can_check"] else check_or_call(legal)


if __name__ == "__main__":
    run(TemplateBot())
