#!/usr/bin/env python3
"""Reference bot: random legal actions with random sizing."""
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pokerbots_sdk import (  # noqa: E402
    Bot, run, fold, check, call, bet_to, raise_to, all_in, check_or_call,
)


class RandomBot(Bot):
    def __init__(self, seed=None):
        self.rng = random.Random(seed)

    def on_hello(self, player_id, session):
        if player_id is not None:
            self.rng.seed("%s-%s" % (session, player_id))

    def act(self, ctx, legal):
        r = self.rng.random()
        if r < 0.10:
            return check() if legal["can_check"] else fold()
        if r < 0.65:
            return check_or_call(legal)
        if r < 0.95:
            if legal["can_bet"]:
                return bet_to(self.rng.randint(legal["min_bet_to"], legal["max_bet_to"]))
            if legal["can_raise"]:
                return raise_to(self.rng.randint(legal["min_raise_to"], legal["max_raise_to"]))
            return check_or_call(legal)
        if legal["can_all_in"]:
            return all_in()
        return check_or_call(legal)


if __name__ == "__main__":
    run(RandomBot())
