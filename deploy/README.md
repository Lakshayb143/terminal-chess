# Deploying private online games

The production layout keeps the game server's WebSocket port private behind
Caddy. Caddy obtains and renews the TLS certificate, serves the health route,
and upgrades secure WebSocket connections at `/ws`. The one other public port
is the SSH gateway, so `ssh <your hostname>` opens the game directly.

## Prerequisites

- A Linux host with Docker Engine and the Compose plugin.
- A DNS `A`/`AAAA` record pointing your game hostname at that host.
- Inbound TCP ports 80 and 443, plus UDP 443 for HTTP/3.
- Inbound TCP port 22 for players, which means moving the host's own SSH
  daemon to another port first (next section).

## Free port 22 for players

Players should be able to type `ssh chess.example.com` with no `-p` flag, so
the gateway takes port 22 and your administrative SSH moves, for example to
2200. Do this from an open session and keep it open until the new port works:

```sh
sudo sed -i 's/^#\?Port .*/Port 2200/' /etc/ssh/sshd_config
sudo ufw allow 2200/tcp        # or your cloud firewall's equivalent
sudo systemctl restart ssh     # `sshd` on some distributions
# From another terminal, confirm before closing this one:
ssh -p 2200 you@chess.example.com
```

On Ubuntu 22.10 and later, socket activation owns the port instead: run
`sudo systemctl edit ssh.socket`, set `ListenStream=` then `ListenStream=2200`,
and restart `ssh.socket`. To keep port 22 for administration instead, set
`SSH_PORT` in `.env` to another port; players then connect with
`ssh -p <port> chess.example.com`.

## Start the service

From the repository root:

```sh
cp deploy/example.env .env
# Edit .env and replace chess.example.com with your real hostname.
docker compose up --build -d
docker compose logs -f
```

Confirm the public endpoints:

```sh
curl https://chess.example.com/health
ssh chess.example.com
CHESS_SERVER_URL=wss://chess.example.com/ws chess online create --name Lakshay
```

The first `ssh` asks visitors to trust the server's host key, as any new SSH
server does. The key is created on first start and kept in the data volume, so
it never changes unless the volume is lost; a new key would make every
returning player's `ssh` warn about an impostor.

The server writes JSON operational logs without player reconnect tokens. It
limits each WebSocket connection to `CHESS_RATE_LIMIT_PER_10S` protocol
requests per ten seconds and rejects messages larger than 16 KiB.

## Data and recovery

Three files live in the `chess-data` Docker volume:

- `server-state.json` holds active rooms and their reconnect credentials. The
  server writes it atomically and flushes it during graceful shutdown.
- `chess.db` is a SQLite database of accounts, sessions, linked SSH keys, and
  finished games. Passwords are stored as Argon2id hashes and session tokens
  as SHA-256 digests, but treat the file as sensitive all the same.
- `ssh_host_ed25519_key` is the SSH gateway's identity. Keep it with the
  backups: restoring it keeps players' `known_hosts` entries valid.

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
- Keep the WebSocket port unexposed; Caddy publishes it with TLS. The SSH
  gateway is the only port the chess server publishes itself.
- Each SSH visitor runs one `chess` process, a few megabytes of memory.
  `CHESS_SSH_MAX_SESSIONS` caps how many run at once; later arrivals are asked
  to try again in a few minutes.
