#!/usr/bin/env python3
"""Reference bot: always folds (checks when free)."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pokerbots_sdk import Bot, run, check_or_fold  # noqa: E402


class FoldBot(Bot):
    def act(self, ctx, legal):
        return check_or_fold(legal)


if __name__ == "__main__":
    run(FoldBot())
