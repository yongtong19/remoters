FROM rust:1.100-bookworm AS builder

WORKDIR /app

# Pre-populate Cargo workspace manifests and vendored patch crate to cache dependency builds.
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/desktop/Cargo.toml crates/desktop/Cargo.toml
COPY vendor/gpui vendor/gpui

RUN mkdir -p crates/core/src crates/server/src crates/desktop/src && \
    printf "pub fn _stub() {}\n" > crates/core/src/lib.rs && \
    printf "pub fn _stub() {}\n" > crates/server/src/lib.rs && \
    printf "fn main() {}\n" > crates/server/src/main.rs && \
    printf "fn main() {}\n" > crates/desktop/src/main.rs && \
    printf "fn main() {}\n" > crates/desktop/build.rs && \
    cargo build --release --locked -p remoters-server

# Build actual core and server sources.
COPY crates/core/src crates/core/src
COPY crates/server/src crates/server/src

RUN touch crates/core/src/lib.rs crates/server/src/lib.rs crates/server/src/main.rs && \
    cargo build --release --locked -p remoters-server && \
    strip target/release/remoters-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --system --create-home --home-dir /home/remoters --shell /usr/sbin/nologin remoters && \
    mkdir -p /data && \
    chown -R remoters:remoters /data

COPY --from=builder /app/target/release/remoters-server /usr/local/bin/remoters-server

ENV REMOTERS_SERVER_BIND=0.0.0.0:8787
ENV REMOTERS_DATABASE_URL=/data/remoters-server.db

VOLUME ["/data"]
EXPOSE 8787

USER remoters
WORKDIR /data

ENTRYPOINT ["/usr/local/bin/remoters-server"]
