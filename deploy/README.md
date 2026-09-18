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

Two files live in the `chess-data` Docker volume:

- `server-state.json` holds active rooms and their reconnect credentials. The
  server writes it atomically and flushes it during graceful shutdown.
- `chess.db` is a SQLite database of accounts, sessions, linked SSH keys, and
  finished games. Passwords are stored as Argon2id hashes and session tokens
  as SHA-256 digests, but treat the file as sensitive all the same.

Back up the volume as sensitive data; anyone holding a reconnect token can
claim the associated seat. To copy the database while the server runs, use
SQLite's online backup rather than copying the file:

```sh
docker compose exec chess-server sh -c \
  'sqlite3 /var/lib/terminal-chess/chess.db ".backup /var/lib/terminal-chess/backup.db"'
```

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
