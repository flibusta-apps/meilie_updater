FROM rust:bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies only - this layer is cached as long as
# Cargo.toml/Cargo.lock don't change.
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release --bin meili_updater


FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y openssl ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --no-create-home appuser

COPY ./scripts/start.sh /
RUN chmod +x /start.sh

RUN update-ca-certificates

WORKDIR /app

COPY --from=builder --chown=appuser:appuser /app/target/release/meili_updater /usr/local/bin/meili_updater

USER appuser

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -f http://localhost:8080/health || exit 1

CMD ["/start.sh"]
