FROM rust:1.90-bookworm AS builder

# The server crate is pure Rust; the terminal client's audio and image
# dependencies are workspace members but are never compiled here.
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY crates ./crates
RUN cargo build --locked --release --package chess-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates sqlite3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system chess \
    && useradd --system --gid chess --home-dir /var/lib/terminal-chess chess \
    && install --directory --owner chess --group chess /var/lib/terminal-chess

COPY --from=builder /build/target/release/chess-server /usr/local/bin/chess-server

USER chess
EXPOSE 3000
VOLUME ["/var/lib/terminal-chess"]
ENV CHESS_SERVER_ADDR=0.0.0.0:3000 \
    CHESS_SERVER_STATE=/var/lib/terminal-chess/server-state.json \
    CHESS_SERVER_DB=/var/lib/terminal-chess/chess.db \
    CHESS_LOG_FORMAT=json \
    RUST_LOG=chess_server=info

ENTRYPOINT ["chess-server"]
