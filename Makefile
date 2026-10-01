# Berkeley Pokerbots — common tasks.
#
# Native targets need Rust + python3. The docker-* targets need only Docker and run everything
# inside the dev container (same image as .devcontainer / Codespaces).

COMPOSE_DEV := docker compose -f deploy/docker-compose.dev.yml
DEV_RUN     := $(COMPOSE_DEV) run --rm dev

.PHONY: help build test check fmt clippy run-api run-worker bench \
        view-hands docker-image docker-shell docker-test docker-check docker-build docker-run-api docker-clean prod-image

HAND_VIEWER_PORT ?= 8000

help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) | awk -F ':.*## ' '{printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

# ---------------------------------------------------------------- native

build: ## Build the workspace (release)
	cargo build --release --workspace

test: ## Run all tests
	cargo test --workspace

fmt: ## Format all code
	cargo fmt --all

clippy: ## Lint (warnings are errors)
	cargo clippy --workspace --all-targets -- -D warnings

check: fmt clippy test ## fmt + clippy + tests (what CI runs)

run-api: ## Run the competition API locally (pokerbots.toml)
	cargo run --release -p competition-platform --bin competition-api

run-worker: ## Run a tournament worker locally
	cargo run --release -p competition-platform --bin tournament-worker

bench: ## 200-bot load test with subprocess bots
	cargo run --release -p pokerbots-cli --bin pokerbots -- bench --players 200 --process --no-time-levels

view-hands: ## Serve the hand viewer and hands.jsonl on localhost
	@printf 'Open http://localhost:%s/hand-viewer.html?load-default=1\n' "$(HAND_VIEWER_PORT)"
	python3 -m http.server "$(HAND_VIEWER_PORT)" --bind 127.0.0.1

# ---------------------------------------------------------------- docker (development)

docker-image: ## Build the dev container image
	$(COMPOSE_DEV) build dev

docker-shell: ## Interactive shell in the dev container (repo mounted at /workspace)
	$(DEV_RUN) bash

docker-test: ## Run the full test suite inside the dev container
	$(DEV_RUN) cargo test --workspace

docker-check: ## fmt-check + clippy + tests inside the dev container
	$(DEV_RUN) bash -c 'cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace'

docker-build: ## Release build inside the dev container
	$(DEV_RUN) cargo build --release --workspace

docker-run-api: ## Run the competition API inside the dev container on :8080
	$(DEV_RUN) bash -c 'cargo run --release -p competition-platform --bin competition-api -- --bind 0.0.0.0:8080'

docker-clean: ## Remove dev container volumes (cargo caches) and image
	$(COMPOSE_DEV) down -v --rmi local

# ---------------------------------------------------------------- docker (production)

prod-image: ## Build the production image (competition-api + tournament-worker + pokerbots)
	docker build -f deploy/Dockerfile -t pokerbots:latest .
