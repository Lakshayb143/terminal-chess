# Terminal Chess

Play chess without leaving the terminal. The board adapts to the window, supports
multiple colour themes, and can use a terminal image protocol for smooth pieces.

## Run it

Install a recent Rust toolchain, then:

```sh
git clone git@github.com:Lakshayb143/terminal-chess.git
cd terminal-chess
cargo run --release
```

Choose a side from the opening screen, or start directly in two-player mode:

```sh
cargo run --release -- --two
```

Install it as a normal command from GitHub:

```sh
cargo install --git https://github.com/Lakshayb143/terminal-chess --locked
chess
```

Click a piece to highlight its legal moves, then click a destination to play.
Click another friendly piece to change the selection; press Escape or click
outside the board to cancel. Promotions open a four-piece chooser directly on
the board.

The game screen includes player cards, chess clocks, captured pieces, material
advantage, last-move and check highlighting, and a move list. Use the mouse
wheel or Page Up / Page Down to scroll longer games. In a wide terminal this
information sits beside the board; in a narrow one it becomes a compact stack
underneath it.

Undo, draw, resign, and restart are clickable. Resign and restart require a
second click so they cannot end a game by accident. When a game finishes, the
final position stays visible with clear rematch and quit actions.

Keyboard input remains available: enter SAN (`Nf3`, `exd5`, `O-O`) or
coordinates (`e2e4`). Type `help` during a game to see every command.

## Sound

Short sounds distinguish ordinary moves, captures, checks, castling,
promotions, and the end of a game. They are embedded in the executable, so an
installed game does not need a separate asset directory. Sound is enabled
automatically for local play and stays quiet when an SSH session is detected.

Inside a game, use `sound on`, `sound off`, or `sound auto`. `sound test` plays
an effect immediately and reports the active audio backend. You can also start
muted with `--mute` or choose a mode with `--sound on|off|auto`.

On macOS, the game uses the built-in system audio player for reliable local
playback. Other platforms use the embedded Rodio backend.

Mouse input works locally and over SSH in terminals that support standard SGR
mouse reporting, including iTerm2 and Kitty. The game captures clicks only, so
ordinary mouse movement does not create extra SSH traffic.

## Best-looking board on macOS

Run the game from a current version of iTerm2. This also works when iTerm2 is
connected to a Linux server over SSH: the program runs on the server, while
iTerm2 renders the board on the Mac.

The default `auto` mode prefers smooth inline images when the terminal supports
them and falls back safely elsewhere. Inside the game, use:

```text
size big
pieces auto
```

The rendering choices are:

- `pieces auto` — use high-resolution terminal images when available.
- `pieces art` — force the portable true-colour Unicode renderer.
- `pieces glyph` — use the terminal font's chess characters.

If `auto` still looks like block art in iTerm2 over SSH, check that the terminal
identity reaches the server:

```sh
printf 'TERM=%s\nLC_TERMINAL=%s\n' "$TERM" "$LC_TERMINAL"
```

`LC_TERMINAL=iTerm2` is the useful signal. Updating iTerm2 and reconnecting the
SSH session is a good first step if it is absent.

## Options

```text
--white              play White against the engine
--black              play Black against the engine
--two                play with two people at one keyboard
--time <seconds>     engine time per move
--depth <number>     maximum search depth
--clock <minutes>    starting time for each player (default: 10)
--increment <secs>   time added after each move
--no-clock           play without chess clocks
--theme <name>       slate, wood, forest, or mono
--pieces <kind>      auto, art, or glyph
--compact            keep the small board
--sound <mode>       auto, on, or off
--mute               disable sound effects
--fen <position>     start from a FEN position
```

Run `cargo run --release -- --help` for the complete list.

## Piece artwork

The bundled RhosGFX SVG chess pieces are by RhosGFX and released under CC0.
See [`assets/pieces/rhosgfx/LICENSE.txt`](assets/pieces/rhosgfx/LICENSE.txt).

The bundled sound effects are selected from Kenney's Impact Sounds and
Interface Sounds packs and released under CC0. See
[`assets/sounds/kenney/LICENSE.txt`](assets/sounds/kenney/LICENSE.txt).
