#!/usr/bin/env python3
"""Reference bot: min-bets / min-raises whenever it can, otherwise calls."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pokerbots_sdk import Bot, run, bet_to, raise_to, check_or_call  # noqa: E402


class RaiseBot(Bot):
    def act(self, ctx, legal):
        if legal["can_bet"]:
            return bet_to(legal["min_bet_to"])
        if legal["can_raise"]:
            return raise_to(legal["min_raise_to"])
        return check_or_call(legal)


if __name__ == "__main__":
    run(RaiseBot())
