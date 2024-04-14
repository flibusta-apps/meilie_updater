FROM rust:bullseye AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --bin meili_updater


FROM debian:bullseye-slim

RUN apt-get update \
    && apt-get install -y openssl ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/*

COPY ./scripts/*.sh /
RUN chmod +x /*.sh

RUN update-ca-certificates

WORKDIR /app

COPY --from=builder /app/target/release/meili_updater /usr/local/bin
ENTRYPOINT ["/start.sh"]