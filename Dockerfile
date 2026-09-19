FROM rust:1.85-slim AS builder

WORKDIR /app
COPY armin-core/ .

RUN apt-get update && apt-get install -y pkg-config libssl-dev && \
    cargo build --release -p armin-engine -p armin-server && \
    rm -rf /var/lib/apt/lists/*

FROM debian:bookworm-slim AS engine

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/armin-engine /usr/local/bin/armin-engine

ENTRYPOINT ["armin-engine"]

FROM debian:bookworm-slim AS server

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/armin-server /usr/local/bin/armin-server

ENTRYPOINT ["armin-server"]
