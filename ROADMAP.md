# Roadmap

Terminal Chess is being built in small, shippable stages. The immediate goal is
to make local play feel as stable and natural as a graphical chess client before
expanding into networked multiplayer.

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
- [ ] Password change, account deletion, and unlinking SSH keys.
- [ ] Account recovery by email, once there is a public deployment to send it.
- [ ] Public profiles and full game replays from history.
- [ ] Blocking, reporting, and basic abuse controls.

## 6. Matchmaking and rated play — planned

- [ ] Public challenges and time-control queues.
- [ ] Ratings and rated/unrated games.
- [ ] Rematches, friends, and direct challenges.
- [ ] Spectators, leaderboards, and moderation tools.
