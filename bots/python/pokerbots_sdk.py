"""
Minimal Python SDK for the Pokerbots bot protocol (JSON Lines over stdin/stdout).

Usage:

    from pokerbots_sdk import Bot, run, check_or_call

    class MyBot(Bot):
        def act(self, ctx, legal):
            return check_or_call(legal)

    if __name__ == "__main__":
        run(MyBot())

Protocol summary (see docs/BOT_PROTOCOL.md):
  engine -> bot : {"type":"hello", ...}
                  {"type":"notify_event", "event": {...}}
                  {"type":"request_action", "request_id": "...", "deadline_ms": 1000,
                   "context": {...}, "legal": {...}}
                  {"type":"goodbye"}
  bot -> engine : {"type":"action", "request_id": "...", "action": {"kind": "Call"}}

Never print anything else to stdout. Use stderr (or `log()`) for debugging.
"""
import json
import sys


# ---- action constructors -------------------------------------------------------------------

def fold():
    return {"kind": "Fold"}


def check():
    return {"kind": "Check"}


def call():
    return {"kind": "Call"}


def bet_to(amount):
    return {"kind": "BetTo", "amount": int(amount)}


def raise_to(amount):
    return {"kind": "RaiseTo", "amount": int(amount)}


def all_in():
    return {"kind": "AllIn"}


def check_or_fold(legal):
    return check() if legal.get("can_check") else fold()


def check_or_call(legal):
    if legal.get("can_check"):
        return check()
    if legal.get("can_call"):
        return call()
    return fold()


def bet_or_raise_to(legal, amount, fallback=None):
    """Bet or raise *to* `amount` (clamped to the legal range); falls back to check/call."""
    if legal.get("can_bet"):
        amount = max(legal["min_bet_to"], min(int(amount), legal["max_bet_to"]))
        return bet_to(amount)
    if legal.get("can_raise"):
        amount = max(legal["min_raise_to"], min(int(amount), legal["max_raise_to"]))
        return raise_to(amount)
    return fallback if fallback is not None else check_or_call(legal)


def log(message):
    """Write a diagnostic line to stderr (captured by the engine for the smoke-test report)."""
    sys.stderr.write(str(message) + "\n")
    sys.stderr.flush()


# ---- bot base class and main loop ----------------------------------------------------------

class Bot(object):
    player_id = None
    session = None

    def on_hello(self, player_id, session):
        pass

    def on_event(self, event):
        pass

    def act(self, ctx, legal):
        raise NotImplementedError

    def on_goodbye(self):
        pass


def _emit(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def run(bot):
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            log("pokerbots_sdk: could not parse line: %r" % line[:200])
            continue
        t = msg.get("type")
        if t == "request_action":
            try:
                action = bot.act(msg["context"], msg["legal"])
            except Exception as e:  # noqa: BLE001 - never crash the protocol loop
                log("pokerbots_sdk: act() raised %r; folding" % (e,))
                action = check_or_fold(msg["legal"])
            _emit({"type": "action", "request_id": msg["request_id"], "action": action})
        elif t == "notify_event":
            try:
                bot.on_event(msg["event"])
            except Exception as e:  # noqa: BLE001
                log("pokerbots_sdk: on_event() raised %r" % (e,))
        elif t == "hello":
            bot.player_id = msg.get("player_id")
            bot.session = msg.get("session")
            bot.on_hello(bot.player_id, bot.session)
        elif t == "goodbye":
            bot.on_goodbye()
            break
