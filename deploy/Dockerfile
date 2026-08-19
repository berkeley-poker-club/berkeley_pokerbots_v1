# Multi-stage build for competition-api / tournament-worker / pokerbots.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --workspace

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates python3 docker.io && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/competition-api /usr/local/bin/
COPY --from=build /src/target/release/tournament-worker /usr/local/bin/
COPY --from=build /src/target/release/pokerbots /usr/local/bin/
COPY bots /opt/pokerbots/bots
COPY docs /opt/pokerbots/docs
WORKDIR /srv/pokerbots
EXPOSE 8080
CMD ["competition-api", "--config", "/srv/pokerbots/pokerbots.toml"]
