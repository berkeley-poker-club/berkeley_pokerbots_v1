#!/usr/bin/env python3
"""Reference bot: calling station — checks or calls everything, never bets."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pokerbots_sdk import Bot, run, check_or_call  # noqa: E402


class CallBot(Bot):
    def act(self, ctx, legal):
        return check_or_call(legal)


if __name__ == "__main__":
    run(CallBot())
