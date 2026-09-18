# UI and UX Roadmap

This roadmap tracks presentation and interaction work only: how the game looks,
how it is played, and how it feels in the terminal. Product and multiplayer
stages are tracked in [ROADMAP.md](ROADMAP.md).

The long-term goal is a public launch after accounts (ROADMAP stage 5) and
matchmaking (ROADMAP stage 6) are in place. That changes who the UI is for: not
only the people who built it and know every command, but strangers arriving
from a link, over SSH, in terminals nobody has tested, playing people they have
never met. The stages below are grouped by the point in that journey they must
be ready for.

Each stage should ship on its own, keep the portable ANSI fallback working, and
be checked in both a generic 256-colour terminal and an image-capable terminal
such as iTerm2.

## Principles

These apply to every stage and are the test for any new screen.

- **The board is the product.** New surfaces go around the board, never over
  it, and disappear cleanly when the window is too small.
- **Mouse, keyboard, and typing are equal.** Every action can be reached all
  three ways; no feature ships mouse-only or command-only.
- **Never signal by colour alone.** Every highlight, warning, and state also has
  a shape, symbol, or word, so it survives `NO_COLOR`, ASCII mode, and colour
  blindness.
- **Always say what just happened and what to do next.** Every state (waiting,
  searching, reconnecting, game over) names the next action.
- **Degrade, don't break.** Images fall back to art, art to glyphs, 24-bit to
  256 colours, colour to plain text, wide to narrow.
- **Respect attention.** Sounds, bells, and notifications are restrained,
  predictable, and easy to turn off.

---

## Part A — Polish the game we have

Work that improves every game today, before any new product surface exists.

### 1. Rendering foundations — complete

Make every page and every terminal render cleanly before adding new surfaces.

- [x] Scroll long pages (`help`, `history`, `pgn`) with a position indicator.
- [x] Fit the frame to short windows without clipping controls.
- [x] Design themes in 24-bit colour, with highlights blended over each square.
- [x] Detect colour depth from the environment, with `--truecolor` and `--256`
      overrides.
- [x] Keep hand-tuned 256-colour squares where blended tints would collapse.
- [x] Make the art renderer respect the detected colour depth.
- [x] Draw the image board with sixel where the terminal offers it (foot,
      xterm, Windows Terminal, VTE), sized to the measured cell.
- [x] Recognise VS Code's image support and send it iTerm2 images.
- [x] Sharpen block art: hard piece silhouettes and colours taken from the
      piece, with no grey fringe.
- [x] Explain the VS Code settings that improve the board when it starts
      without images.

### 2. Keyboard board cursor — next

Let keyboard players move pieces the way mouse players do, instead of typing
every move. This also works over SSH where mouse reporting is unavailable.

- [ ] Arrow keys and `hjkl` move a highlighted cursor square over the board.
- [ ] `Enter` picks up a piece and shows its legal moves; `Enter` again drops it.
- [ ] `Esc` cancels the selection and returns the cursor to rest.
- [ ] Reuse the existing selected-square, move-dot, and capture markers.
- [ ] Respect board orientation, so the cursor follows the flipped board.
- [ ] Fit cursor focus into the existing `Tab` order between the board, the
      move box, and the controls.

### 3. Move review — next

Let players look back through the game, during play and after it ends. The
board cursor from stage 2 provides the keyboard controls.

- [ ] Click a move in the list, or press `←` and `→`, to show that position.
- [ ] `Home` and `End` jump to the start and the current position.
- [ ] Show a clear banner such as "Viewing move 14 · End to return".
- [ ] Block input to the live game while viewing an earlier position.
- [ ] Keep the last-move and check highlights accurate for the position shown.
- [ ] Return to the live position automatically when the opponent moves online.

### 4. Clock pressure — planned

Stop players from losing on time without noticing.

- [ ] Turn a clock the theme's warning colour below about 20 seconds.
- [ ] Show tenths of a second in the final seconds.
- [ ] Add an optional single tick sound when the warning starts.
- [ ] Verify the refresh stays on the existing low-cost clock path.

### 5. Evaluation bar — planned

Show who is better at a glance, using scores the engine already produces.

- [ ] Draw a thin vertical bar between the board and the side panel.
- [ ] Scale it from the evaluation panel's score, with mate shown as full.
- [ ] Add `eval on` and `eval off`, persisted in the config.
- [ ] Default to off in two-player and online games, and never show it during
      a rated game.
- [ ] Hide it cleanly when the terminal is too narrow.

### 6. Game-over summary — planned

Replace the status line with a clear ending and obvious next steps.

- [ ] Show a summary card with the result and its reason.
- [ ] Include the move count and the time left on each clock.
- [ ] Offer **Rematch**, **Export PGN**, and **Review** actions.
- [ ] Connect **Review** to move review from stage 3.
- [ ] Keep every action reachable from the keyboard.
- [ ] Leave room for rating change and report actions (stages 14 and 15).

### 7. Setup previews — planned

Let players choose a look by seeing it rather than by name.

- [ ] Show a small live board preview while cycling `theme`.
- [ ] Update the preview while cycling `pieces`.
- [ ] Include a highlighted last move and legal-move dots in the preview.

### 8. Polish — planned

Small improvements that make the game feel finished.

- [ ] Show the opening name from a compact embedded ECO table for the first
      moves of a game.
- [ ] Add premoves in online games, queued while the opponent is thinking and
      shown with their own highlight.
- [ ] Replace the muddy orange capture tint on the slate theme with a clearer
      colour.
- [ ] Animate nothing, but make the opponent's move unmistakable: briefly
      emphasise the last-move highlight when it arrives while you are away.

---

## Part B — Ready for strangers

Work that must land before the game is shared publicly, even for guest-only
play. A first-time player should never need `--help`.

### 9. Home screen — planned

The start menu is currently a typed numbered prompt with three local modes.
Online play, engine strength, and time controls are only reachable through
command-line flags. Replace it with a real home screen.

- [ ] A navigable menu (mouse, arrow keys, and number shortcuts) with **Play
      the computer**, **Play a friend here**, **Play online**, **Resume**, and
      **Settings**.
- [ ] Named engine levels (for example Beginner, Casual, Club, Strong) instead
      of raw `--time` and `--depth`.
- [ ] Time-control presets (Bullet, Blitz, Rapid, Untimed) with a custom
      option.
- [ ] **Play online** offers create, join by code, and resume from the menu,
      without flags.
- [ ] Side choice includes **Random**.
- [ ] Consistent `Esc` behaviour: always one step back, never out of the
      program without confirmation during a game.
- [ ] Keep the current flags as shortcuts that skip the home screen.

### 10. First run and learning — planned

Get a new player from launch to a first move in under a minute.

- [ ] A terminal check on first launch that shows the board in each rendering
      mode and asks "which looks right?", then saves the answer.
- [ ] A skippable 30-second tour: click or type a move, where the controls
      are, how to open help.
- [ ] Contextual hints the first few times a feature appears (first check,
      first promotion, first draw offer), each shown once.
- [ ] Group `help` by task (moving, game actions, display, online) instead
      of one long list.
- [ ] A command palette (`:` or `Ctrl+P`) that searches every command and
      button by name, so nothing has to be memorised.
- [ ] Friendly suggestions for mistyped commands ("Did you mean `resign`?").

### 11. Accessibility — planned

Make the game playable for people who cannot rely on colour, sight, or a
mouse. `NO_COLOR` and ASCII mode already exist and are the starting point.

- [ ] A colour-blind-safe theme, with check, capture, and last move
      distinguishable without red/green contrast.
- [ ] A high-contrast theme and a theme tuned for light terminal backgrounds.
- [ ] Audit every state against "never signal by colour alone".
- [ ] A text mode for screen readers that announces moves in words ("Knight
      takes e5, check") and can read out the position on request.
- [ ] Remappable keys, stored in the config.
- [ ] A reduced-sound option that keeps only essential cues (your turn, low
      time, game over).

### 12. Presence and attention — planned

Online games are played in a terminal that is often behind other windows.

- [ ] Set the terminal title to the game state, for example
      "Your move · 3:12 — Terminal Chess".
- [ ] Use terminal focus events to ring the bell or show a desktop
      notification when it becomes your turn while the window is unfocused.
- [ ] A small connection indicator with latency, and a clear warning when it
      degrades.
- [ ] A visible countdown when the opponent disconnects, with the option to
      claim the win or wait when it expires.
- [ ] Allow aborting a game before the first move without penalty, and say so.

---

## Part C — Accounts (with ROADMAP stage 5)

### 13. Account flows in the terminal — planned

Make sign-in feel safe and simple in a place without a browser.

- [ ] Browser sign-in with a short device code, plus masked password entry
      for terminals without a browser nearby.
- [ ] Recovery flow with clear, non-technical instructions.
- [ ] Show the signed-in name in the header, and guest status just as clearly.
- [ ] A settings page for sessions: list signed-in devices and sign out of
      each or all.
- [ ] Offer to keep a guest game in the new account after signing up.

### 14. Profiles and history — planned

- [ ] A profile page: name, join date, games played, results by time control.
- [ ] A game history browser with filters (result, colour, time control,
      opponent), opening any game in move review (stage 3).
- [ ] Export a single game or a whole history to PGN.
- [ ] Block and report from the player panel and the game-over card, with
      confirmation and a clear note of what happens next.

---

## Part D — Matchmaking and community (with ROADMAP stage 6)

### 15. Lobby and seeking — planned

- [ ] A time-control grid (1+0, 3+2, 5+0, 10+0, 15+10, custom) for one-press
      seeking.
- [ ] A searching state with elapsed time, the pool being searched, and
      Cancel.
- [ ] A match-found transition showing the opponent's name, rating, and your
      colour before the clock starts.
- [ ] Rated and unrated shown on every game, before and during play.
- [ ] Rating on the player panels, provisional ratings marked, and the rating
      change on the game-over card.
- [ ] Rematch with accept and decline, and a timeout that says what happened.

### 16. Friends and challenges — planned

- [ ] A friends list with online and in-game status.
- [ ] Send, accept, and decline direct challenges, with time control and
      colour shown before accepting.
- [ ] A challenge inbox that works while you are on the home screen.

### 17. Watching and talking — planned

- [ ] A read-only spectator view with viewer count and the same review
      controls as stage 3.
- [ ] Watch a friend's game from the friends list.
- [ ] Leaderboards by time control, opened from the home screen.
- [ ] Chat that starts safe: preset messages ("Good luck", "Good game") by
      default, free text opt-in, with mute and report on every message.

### 18. Post-game analysis — planned

Help players improve, using the engine that already ships in the binary.

- [ ] Analyse a finished game and draw an evaluation graph under the move
      list.
- [ ] Mark mistakes and blunders in the move list with symbols as well as
      colour.
- [ ] Show the engine's better move with from and to squares highlighted.
- [ ] Share a game as PGN or, once available, a link.

---

## Part E — Launch quality

Qualities that decide whether strangers stay.

### 19. Reliability of the experience — planned

- [ ] A readable error screen for crashes and lost connections, with the
      next step and a way to report it, instead of a raw error.
- [ ] A clear message when the client and server versions disagree, with the
      update command.
- [ ] A notice when a newer version is available.
- [ ] A `chess doctor` command that reports detected colour depth, image
      support, mouse support, sound, and config paths.
- [ ] A bandwidth budget per frame over SSH, measured and kept.

### 20. SSH gateway experience — planned

The final ROADMAP item, and the easiest possible first game: `ssh` and play.

- [ ] Identify returning players by SSH key, with a first-visit name prompt.
- [ ] Assume the least capable terminal until proven otherwise, and offer the
      terminal check from stage 10.
- [ ] Keep sound silent and explain why.

### 21. Visual language — planned

Keep the product coherent as more people add screens.

- [ ] Document colour tokens, spacing, button styles, and copy tone in a
      short design guide.
- [ ] Keep a terminal compatibility table (terminal, OS, features that work)
      in the README, updated for every release.

---

## Tooling

Support work that makes every stage above safer to change.

- [ ] Add a `--frame WxH` mode that prints one rendered frame and exits.
- [ ] Use it to make layout checks across terminal sizes reproducible.
- [ ] Snapshot-test the rendered UI in CI, per theme and colour depth.
- [ ] A `feedback` command that opens a prefilled report with terminal
      details from `chess doctor`, and never sends anything without asking.
