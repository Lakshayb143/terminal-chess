# Deploying private online games

The production layout keeps the Rust game server private and exposes only
Caddy. Caddy obtains and renews the TLS certificate, serves the health route,
and upgrades secure WebSocket connections at `/ws`.

## Prerequisites

- A Linux host with Docker Engine and the Compose plugin.
- A DNS `A`/`AAAA` record pointing your game hostname at that host.
- Inbound TCP ports 80 and 443, plus UDP 443 for HTTP/3.

## Start the service

From the repository root:

```sh
cp deploy/example.env .env
# Edit .env and replace chess.example.com with your real hostname.
docker compose up --build -d
docker compose logs -f
```

Confirm the public endpoint:

```sh
curl https://chess.example.com/health
CHESS_SERVER_URL=wss://chess.example.com/ws chess online create --name Lakshay
```

The server writes JSON operational logs without player reconnect tokens. It
limits each WebSocket connection to `CHESS_RATE_LIMIT_PER_10S` protocol
requests per ten seconds and rejects messages larger than 16 KiB.

## Data and recovery

Active rooms and their reconnect credentials live in the `chess-data` Docker
volume. The server writes this state atomically and flushes it during graceful
shutdown. Back up that volume as sensitive data; anyone holding a reconnect
token can claim the associated seat.

Before a planned deployment, let Compose send its normal `SIGTERM` and wait for
the process to exit:

```sh
docker compose down
docker compose up --build -d
```

Clients reconnect automatically after the service returns. The server accounts
for clock time elapsed while it was offline.

## Operations

- Health check: `GET /health` returns `ok`.
- Application logs: `docker compose logs chess-server`.
- TLS/access logs: `docker compose logs caddy`.
- Raise or lower the request limit in `.env`, then run
  `docker compose up -d` to apply it.
- Keep the Rust server port unexposed. Only Caddy should publish host ports.
