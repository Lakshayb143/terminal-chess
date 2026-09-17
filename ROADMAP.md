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

## 2. Piece interaction polish — next

Make mouse and keyboard play feel deliberate, clear, and forgiving.

- [ ] Refine selected-square and legal-move markers.
- [ ] Distinguish quiet moves from captures at a glance.
- [ ] Add restrained feedback for invalid clicks.
- [x] Improve keyboard navigation and focus states.
- [x] Add in-game controls for board size and piece style.
- [ ] Review promotion and confirmation interactions.

## 3. Complete the local-game experience — later

Turn the current game into a session players can leave, share, and customize.

- [ ] Pause and resume a game.
- [ ] Save and restore unfinished games.
- [ ] Import and export PGN.
- [ ] Persist player names, themes, clocks, sound, and board orientation.
- [ ] Add a first-run setup and straightforward configuration file.

Multiplayer, matchmaking, spectators, and SSH-hosted games will follow after
these local foundations are solid.
