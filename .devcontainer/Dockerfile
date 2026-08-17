# Development container for Berkeley Pokerbots.
# Everything needed to build, test and run the whole workspace:
#   Rust (rustfmt + clippy), Python 3 (reference bots / protocol tests), sqlite3, zip.
# Used by .devcontainer/devcontainer.json (VS Code / Codespaces), deploy/docker-compose.dev.yml
# and the `make docker-*` targets.

FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        python3 \
        sqlite3 \
        zip unzip \
        git \
        curl \
        jq \
        procps \
        less \
        vim-tiny \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add rustfmt clippy

# Non-root user (uid 1000 matches the default devcontainer/Codespaces user).
ARG USERNAME=dev
ARG UID=1000
ARG GID=1000
RUN groupadd --gid $GID $USERNAME \
    && useradd --uid $UID --gid $GID --create-home --shell /bin/bash $USERNAME \
    # Pre-create the named-volume mount points so freshly created volumes inherit dev ownership.
    && mkdir -p /workspace /usr/local/cargo/registry /home/$USERNAME/target \
    && chown -R $USERNAME:$USERNAME /workspace /usr/local/rustup /usr/local/cargo /home/$USERNAME

USER $USERNAME
WORKDIR /workspace

# Keep the build cache out of the (possibly slow) bind-mounted workspace; compose/devcontainer
# mount a named volume here so incremental builds survive container restarts.
ENV CARGO_TARGET_DIR=/home/dev/target \
    CARGO_TERM_COLOR=always \
    PYTHONUNBUFFERED=1

CMD ["/bin/bash"]
