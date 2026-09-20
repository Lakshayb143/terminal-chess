FROM rust:1.90-bookworm AS builder

# The server, plus the terminal client it runs for each SSH visitor. The
# client links ALSA for sound; hosted games are always silent, but the
# library must still be present to build and start it.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libasound2-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY crates ./crates
RUN cargo build --locked --release --package chess-server --package chess --bin chess-server --bin chess

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libasound2 sqlite3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system chess \
    && useradd --system --gid chess --home-dir /var/lib/terminal-chess chess \
    && install --directory --owner chess --group chess /var/lib/terminal-chess

COPY --from=builder /build/target/release/chess-server /usr/local/bin/chess-server
COPY --from=builder /build/target/release/chess /usr/local/bin/chess

USER chess
# 3000 is the WebSocket server behind Caddy; 2222 is the SSH gateway, which
# the host publishes on port 22.
EXPOSE 3000 2222
VOLUME ["/var/lib/terminal-chess"]
ENV CHESS_SERVER_ADDR=0.0.0.0:3000 \
    CHESS_SERVER_STATE=/var/lib/terminal-chess/server-state.json \
    CHESS_SERVER_DB=/var/lib/terminal-chess/chess.db \
    CHESS_SSH_ADDR=0.0.0.0:2222 \
    CHESS_SSH_HOST_KEY=/var/lib/terminal-chess/ssh_host_ed25519_key \
    CHESS_CLIENT_BIN=/usr/local/bin/chess \
    CHESS_LOG_FORMAT=json \
    RUST_LOG=chess_server=info

ENTRYPOINT ["chess-server"]
