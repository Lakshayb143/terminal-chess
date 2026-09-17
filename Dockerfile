FROM rust:1.90-bookworm AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        clang cmake libasound2-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN cargo build --locked --release --bin chess-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libasound2 \
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
    CHESS_LOG_FORMAT=json \
    RUST_LOG=chess_server=info

ENTRYPOINT ["chess-server"]
