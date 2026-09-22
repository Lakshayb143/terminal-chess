#!/usr/bin/env bash
# Back up the chess server's data volume into one archive: the account
# database, the SSH host key, and the games in progress. Run it on the host
# that runs the server, while the server runs:
#
#   deploy/backup.sh                     # into ./backups
#   deploy/backup.sh /srv/backups/chess  # or anywhere else
#
# The archive holds password hashes and the host's private key, so it is
# created readable only by you. Copy it off this host as well: a backup on
# the same disk does not survive losing the disk. deploy/README.md explains
# how to restore one.
set -euo pipefail

cd "$(dirname "$0")/.."
destination=${1:-backups}
umask 077
mkdir -p "$destination"
archive="$destination/chess-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"

# SQLite's online backup gives a consistent copy while games are being
# recorded; copying chess.db itself may not. The archive holds the files
# under their names in the volume, so restoring is unpacking it there.
docker compose exec -T chess-server bash -euo pipefail -c '
  cd /var/lib/terminal-chess
  umask 077
  rm -f chess-backup.db
  trap "rm -f chess-backup.db" EXIT
  sqlite3 chess.db ".backup chess-backup.db"
  files=(chess-backup.db ssh_host_ed25519_key)
  if [ -f server-state.json ]; then files+=(server-state.json); fi
  tar --transform "s/^chess-backup.db\$/chess.db/" -cf - "${files[@]}"
' | gzip >"$archive.partial"
mv "$archive.partial" "$archive"
echo "$archive"
