# Developing Pokerbots

Two equally supported setups. Pick one.

## Option A — dev container (no local toolchain)

Everything (Rust, clippy/rustfmt, Python 3 for the bot tests, sqlite3) lives in one image built
from [`.devcontainer/Dockerfile`](../.devcontainer/Dockerfile). You only need Docker.

**VS Code / GitHub Codespaces.** Open the repo and choose *Reopen in Container* (or create a
Codespace) — [`.devcontainer/devcontainer.json`](../.devcontainer/devcontainer.json) builds the
image, mounts the repo at `/workspace`, installs rust-analyzer, and forwards port 8080. This is
the "develop from anywhere" path: a browser and a Codespace are enough.

**Plain Docker (any editor).**

```bash
make docker-shell     # interactive shell in the container, repo mounted at /workspace
make docker-test      # full test suite in the container
make docker-check     # fmt --check + clippy -D warnings + tests (same as CI)
make docker-run-api   # competition API on http://localhost:8080 (web UI at /)
```

The cargo registry and build artifacts live in named volumes (`cargo-registry`,
`cargo-target`), so incremental builds survive container restarts and never touch the host
checkout. `make docker-clean` resets them.

On macOS without Docker Desktop, [colima](https://github.com/abiosoft/colima) works:
`brew install colima docker docker-compose && colima start --cpu 4 --memory 6`.

## Option B — native toolchain

```bash
curl https://sh.rustup.rs -sSf | sh   # Rust (stable) with rustfmt + clippy
# python3 ≥ 3.9 on PATH (reference bots / protocol tests)
make check                            # fmt + clippy + tests
```

## Everyday commands

| Task | Command |
|------|---------|
| Full check (what CI runs) | `make check` / `make docker-check` |
| One crate's tests | `cargo test -p poker-utils` |
| Run the platform locally | `make run-api` / `make docker-run-api`, then open `http://localhost:8080` |
| Local tournament between bots | `cargo run -p pokerbots-cli -- tournament --bot python:bots/python/call_bot.py --bot builtin:random --players 9` |
| 200-bot load test | `make bench` |
| Production image | `make prod-image` (see [OPERATIONS.md](OPERATIONS.md) for deployment) |

## Repo conventions

* `cargo fmt` + `cargo clippy -D warnings` must be clean; CI enforces both.
* Tests live next to the code (`#[cfg(test)]`) or in each crate's `tests/`; the platform's
  end-to-end test (`competition-platform/tests/api_e2e.rs`) drives the real router + a worker and
  needs `python3`.
* The engine (`poker-utils`) stays pure and synchronous — no I/O, no async, no clocks; anything
  that talks to processes or the network belongs in `table-runner` or above.
* `SPEC.md` is the rules source of truth; deviations are documented in its
  "Implementation Notes" section.
