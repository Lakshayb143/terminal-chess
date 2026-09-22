# Roadmap

Terminal Chess is being built in small, shippable stages. The immediate goal is
to make local play feel as stable and natural as a graphical chess client before
expanding into networked multiplayer.

## Where things stand — 22 September 2026

**Live.** Anyone can play with `ssh -p 2222 chess.lakshaybhatia.com`; there is
nothing to install. The server runs the `fix/home-board-rendering` branch at
`f837e4b` in Docker, on a VPS it shares with other work. Only the chess server
is started: Caddy is not, and ports 80 and 443 are closed, so the public server
is reachable over SSH only. An installed `chess` client cannot connect to it
over `wss://` yet.

Limits in force for hosted play:

- 60 visitors at once, and at most 3 games open from one address.
- The engine thinks for at most 5 seconds a move, whatever a visitor asks for.
- The chess container is held to 2 CPUs and 2 GB of memory.
- File commands are off, and sound stays silent.

**Done on the branch, not yet merged into `main`.** Everything below is
deployed.

| Commit | Change |
| --- | --- |
| `e3d10d5` | The home page board is a real picture in image-capable terminals, and figurines rather than coarse block art elsewhere. |
| `5d12638` | Over SSH the engine is capped at 5 seconds a move; `time 1e300` no longer crashes the client. |
| `cb12107` | One address may have at most `CHESS_SSH_MAX_PER_IP` SSH games open (3 by default). |
| `b96e927` | The chess server gets 2 CPUs and 2 GB by default, and Caddy half a core and 256 MB. |
| `ee747b2` | **Play a random opponent**: a 10+5 queue on the server with random colours, plus a live count of who is online and who is waiting (protocol 4). |
| `f837e4b` | The online count is a coloured badge beside the ONLINE heading: green with the count, red when you are alone, grey when the server is offline. |

**On `feat/launch-readiness`, not yet committed or deployed.** Built on the
branch above.

- Hosting: both containers drop every Linux capability (Caddy keeps the one
  that binds 80 and 443), set `no-new-privileges`, and run read-only; the
  gateway refuses `sftp` and `scp` at once instead of hanging; and
  `deploy/backup.sh` backs the data volume up, with the restore written down
  and tested.
- Engine levels Beginner, Casual, Club and Strong, chosen on a new Settings
  page with clock presets and a custom clock. Each level won all 24 games of
  a match against the one below it.
- **Random side** on the home page, and `--random` and `--level` flags.
- Every game ends on the home page instead of closing the program. `Esc`
  steps back one thing at a time and never leaves a game by itself.
- A game-over summary with Rematch, Review, PGN and Menu; online rematches
  swap colours once both players ask (protocol 5).
- Looking back through a game with `Page Up`, `Page Down`, `Home` and `End`,
  or by clicking a move; a board cursor on the arrow keys.
- The window title says whose move it is, and the bell rings for an opponent's
  move while the window is in the background.
- The account page changes the password, lists and unlinks SSH keys, and
  deletes the account.

**Next.**

- Commit the work above, open pull requests, and merge into `main`.
- Deploy it: rebuild the chess server with the hardened `compose.yaml`, and
  schedule `deploy/backup.sh`.
- Give Ghostty the sharp Kitty-image board; it still shows blurry pieces.
  macOS Terminal has no image protocol, so it keeps figurines and block art.
- Start Caddy with ports 80 and 443 open, when installed clients should be
  able to play on the public server.
- Publish the web page at lakshaybhatia.com/chess, which waits on the
  portfolio's `chess-page` branch for a screen recording.

## 1. Zero-flicker rendering — complete

Keep the board and surrounding interface visually stationary during play.

- [x] Stop clearing the whole terminal when the position changes.
- [x] Update only screen regions that actually changed.
- [x] Avoid blank frames while replacing an inline board image.
- [x] Preserve the existing low-cost clock refresh path.
- [x] Verify resizing and every piece-rendering mode.

## 2. Piece interaction polish — complete

Make mouse and keyboard play feel deliberate, clear, and forgiving.

- [x] Refine selected-square and legal-move markers.
- [x] Distinguish quiet moves from captures at a glance.
- [x] Add restrained feedback for invalid clicks.
- [x] Improve keyboard navigation and focus states.
- [x] Add in-game controls for board size and piece style.
- [x] Review promotion and confirmation interactions.

## 3. Complete the local-game experience — complete

Turn the current game into a session players can leave, share, and customize.

- [x] Pause and resume a game.
- [x] Save and restore unfinished games.
- [x] Import and export PGN.
- [x] Persist player names, themes, clocks, sound, and board orientation.
- [x] Add a first-run setup and straightforward configuration file.

## 4. Private online games — complete

Prove that two terminal clients can finish a reliable server-authoritative
game before adding public accounts or ratings.

- [x] Extract the game model from terminal rendering and input.
- [x] Define a versioned JSON protocol shared by clients and the server.
- [x] Add an authoritative WebSocket server for legal moves and clocks.
- [x] Add private guest rooms with invite codes.
- [x] Add draw, resignation, disconnect, and reconnect behavior on the server.
- [x] Connect the terminal UI to create and join online games.
- [x] Show waiting, connection, reconnecting, and opponent-offline states.
- [x] Persist active server games across restarts.
- [x] Add deployment configuration, TLS, rate limits, and operational logging.
- [x] Test complete games between two clients under latency and disconnects.

## 5. Accounts, game history, and SSH play — in progress

Add identity only after guest games are stable, so authentication does not hide
problems in the core multiplayer loop. Guests can always play without one.

- [x] Registration and login with Argon2id passwords and revocable session tokens.
- [x] Completed online games recorded, with recent games on the account page.
- [x] Play over plain `ssh`, with SSH keys linked to accounts for password-free sign-in.
- [x] Sign out from the terminal.
- [x] Password change, account deletion, and unlinking SSH keys.
- [ ] Account recovery by email, once there is a public deployment to send it.
- [ ] Public profiles and full game replays from history.
- [x] Protect a shared host from hosted play: a cap on visitors overall and
      per address, a five-second engine cap, and a CPU and memory budget.
- [x] Host the game publicly over SSH.
- [ ] Blocking and reporting players.

## 6. Matchmaking and rated play — started

- [x] Play a random opponent: one 10+5 queue with random colours, never
      pairing an account with itself.
- [x] Show on the home page how many people are online and whether anyone is
      waiting for a game.
- [ ] More time-control queues and public challenges.
- [ ] Ratings and rated/unrated games.
- [x] Rematches: colours swapped, once both players ask.
- [ ] Friends and direct challenges.
- [ ] Spectators, leaderboards, and moderation tools.
