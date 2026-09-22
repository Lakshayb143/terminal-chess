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

### SSH only

To offer play over SSH alone, without ports 80 and 443, start only the chess
server. `DOMAIN` must still be set in `.env`, because Compose checks the whole
file:

```sh
docker compose up -d --build chess-server
ssh -p <SSH_PORT> chess.example.com
```

Visitors get every mode, including random opponents and invite codes, because
their games run inside the server's container. What they lose is the
`wss://` endpoint, so an installed `chess` client cannot reach this server.
This is how the public server at `chess.lakshaybhatia.com` runs, with
`SSH_PORT=2222`.

### Host key

The first `ssh` asks visitors to trust the server's host key, as any new SSH
server does. The key is created on first start and kept in the data volume, so
it never changes unless the volume is lost; a new key would make every
returning player's `ssh` warn about an impostor.

The server writes JSON operational logs without player reconnect tokens. It
limits each WebSocket connection to `CHESS_RATE_LIMIT_PER_10S` protocol
requests per ten seconds and rejects messages larger than 16 KiB.

## Firewall

Docker writes its own packet-filter rules for published ports, and they take
effect before `ufw`'s. A port that any container on the host publishes is
therefore reachable from the internet even when `ufw` denies it. On a host
that also runs other services:

- Publish anything meant to stay private on the loopback address only, for
  example `"127.0.0.1:5173:5173"` in its Compose `ports:` list, and reach it
  through an SSH tunnel.
- Use the cloud provider's firewall as well. It sits in front of the host, so
  Docker cannot open holes in it. Add the same rules for IPv6 as for IPv4.
- Open only the ports this deployment needs: the administrative SSH port, the
  game's SSH port, and 80 and 443 only if Caddy runs.

## Data and recovery

Three files live in the `chess-data` Docker volume:

- `server-state.json` holds active rooms and their reconnect credentials. The
  server writes it atomically and flushes it during graceful shutdown.
- `chess.db` is a SQLite database of accounts, sessions, linked SSH keys, and
  finished games. Passwords are stored as Argon2id hashes and session tokens
  as SHA-256 digests, but treat the file as sensitive all the same.
- `ssh_host_ed25519_key` is the SSH gateway's identity. Keep it with the
  backups: restoring it keeps players' `known_hosts` entries valid.

### Backups

`deploy/backup.sh` writes all three into one archive while the server runs,
using SQLite's online backup so the database copy is consistent:

```sh
deploy/backup.sh                     # writes backups/chess-<time>.tar.gz
deploy/backup.sh /srv/backups/chess  # or into another directory
```

The archive is readable only by you, because it holds password hashes, the
host's private key, and reconnect tokens that can claim a seat. Keep copies
off this host too; a backup on the same disk is lost with the disk. To run it
every night at 04:00, add a line like this with `crontab -e`:

```
0 4 * * * /srv/projects/chess/deploy/backup.sh /srv/backups/chess
```

To restore an archive, stop the server, replace the volume's contents, and
start it again. Removing the old `-wal` and `-shm` files matters: SQLite
would otherwise apply them to the restored database.

```sh
docker compose stop chess-server
docker compose run --rm --no-deps -T --entrypoint bash chess-server -c \
  'cd /var/lib/terminal-chess && rm -f chess.db-wal chess.db-shm && tar -xzf -' \
  < backups/chess-<time>.tar.gz
docker compose start chess-server
```

The server logs its host key fingerprint on start; it should match the one
players already trust.

Before a planned deployment, let Compose send its normal `SIGTERM` and wait for
the process to exit:

```sh
docker compose down
docker compose up --build -d
```

Clients reconnect automatically after the service returns. The server accounts
for clock time elapsed while it was offline.

## Operations

- To deploy new code, pull it and run `docker compose up -d --build
  chess-server`. The restart disconnects everyone playing over SSH, so
  choose a quiet moment. The host key stays the same because it lives in
  the volume.
- Health check: `GET /health` returns `ok`.
- Application logs: `docker compose logs chess-server`.
- TLS/access logs: `docker compose logs caddy`.
- Raise or lower the request limit in `.env`, then run
  `docker compose up -d` to apply it.
- Keep the WebSocket port unexposed; Caddy publishes it with TLS. The SSH
  gateway is the only port the chess server publishes itself.
- Each SSH visitor runs one `chess` process, about 25 MB while playing the
  engine. `CHESS_SSH_MAX_SESSIONS` caps how many run at once, and
  `CHESS_SSH_MAX_PER_IP` how many one address may have open; later arrivals
  are asked to try again.
- The chess server is held to `CHESS_CPUS` and `CHESS_MEMORY`, so it can share
  a host with other work. Keep `CHESS_SSH_MAX_SESSIONS` times 25 MB within the
  memory. Over SSH the engine thinks for at most 5 seconds a move, whatever
  `time` or `depth` a visitor asks for.
- Both containers run with every Linux capability dropped (Caddy keeps only
  the one that binds ports 80 and 443), with `no-new-privileges`, and with a
  read-only filesystem. The volumes and a small `/tmp` are the only writable
  places; each SSH visitor's settings and autosave live in `/tmp` until they
  leave.
- The gateway runs the game and nothing else: commands, `sftp`, `scp`, and
  port forwarding are all refused.
- Docker passes visitors' real addresses to the gateway over IPv4 only. Unless
  Docker's IPv6 support is set up, publish an `A` record for the hostname and
  no `AAAA`, or every IPv6 visitor shares one address and one per-address
  limit.
