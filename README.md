# Terminal Chess

Play a complete game of chess without leaving the terminal.

Terminal Chess is a mouse-friendly Rust application that brings the familiar
feel of a graphical chess site to a native command-line program. It combines a
responsive board, scalable vector pieces, clocks, move history, sound effects,
and an embedded engine in a single executable.

The long-term goal is a terminal-first place where people can play locally,
against an engine, or eventually against another person over the network.

## Highlights

- Click a piece, inspect its legal moves, and click a destination to move.
- Play against the built-in engine or another person at the same terminal.
- Use SAN (`Nf3`, `exd5`, `O-O`) or coordinate notation (`e2e4`) at any time.
- Get a responsive layout with player panels, clocks, captured pieces, material
  advantage, move history, and clear game-over states.
- Render crisp vector-derived pieces through iTerm2 and Kitty image protocols,
  with true-colour and Unicode fallbacks for other terminals.
- Hear distinct sounds for moves, captures, checks, castling, promotions, and
  game endings.
- Choose between four board themes and several piece-rendering modes.

## Quick start

You need a recent [Rust toolchain](https://www.rust-lang.org/tools/install).

```sh
git clone https://github.com/Lakshayb143/terminal-chess.git
cd terminal-chess
cargo run --release
```

To start a local two-player game immediately:

```sh
cargo run --release -- --two
```

To install Terminal Chess as a normal command:

```sh
cargo install --git https://github.com/Lakshayb143/terminal-chess --locked
chess
```

## Playing

Click a piece to select it. Legal destinations appear on the board; click one
to complete the move. Click another friendly piece to change the selection, or
press `Esc` to cancel. Promotions open a four-piece chooser on the board.

The mouse-accessible actions let you undo, offer or accept a draw, resign, and
restart. Potentially destructive actions require confirmation. The move list
can be scrolled with the mouse wheel or `Page Up` and `Page Down`.

Keyboard input remains fully supported. Type anywhere to return focus to the
move box. Use `Tab` or the arrow keys to move through every visible control,
`Shift+Tab` to go backwards, and `Enter` to activate the focused button. The
`Move` button returns the cursor to the move box, so draw, resign, restart, and
all other game actions remain usable without a mouse.

## Board rendering

The board grows with the terminal window. Wide terminals place game information
beside it; narrow terminals arrange a compact information panel underneath.

The default `auto` mode selects the best renderer available:

| Command | Rendering mode |
| --- | --- |
| `pieces auto` | High-resolution inline images when supported, with a safe fallback |
| `pieces art` | Portable true-colour artwork made from Unicode block elements |
| `pieces glyph` | Chess characters supplied by the terminal font |

The `Size` and `Piece` buttons change both settings while the game is running;
no command is required. Each press on `Piece` cycles from `Auto` to `Glyph` to
`Art`. If terminal images look soft or blurry, choose `Glyph` for the sharpest
font-rendered pieces. `Art` is the portable drawn fallback for terminals that
do not render inline images cleanly.

For the sharpest large board on macOS, use a current version of iTerm2 and run:

```text
size big
pieces auto
```

Image rendering also works when iTerm2 is connected to a Linux machine over
SSH. If detection fails, confirm that `LC_TERMINAL=iTerm2` reaches the server.

## Sound

Sound effects are embedded in the executable, so no separate asset directory
is required. Sound is enabled for local games and automatically stays quiet in
an SSH session.

Use `sound on`, `sound off`, or `sound auto` during a game. Run `sound test` to
play an effect immediately and report the selected audio backend. On macOS,
Terminal Chess uses the system audio player; other platforms use Rodio.

## Command-line options

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

Run `chess --help` for the complete reference.

## Engineering notes

The project is intentionally built as a terminal application rather than a web
view wrapped in a desktop shell. A few implementation details:

- The UI negotiates terminal capabilities and keeps a portable ANSI fallback.
- A cached row-diff renderer and synchronized terminal updates prevent partial
  frames without repainting the whole screen after every move.
- SVG piece assets are rasterized in-process and embedded in the release binary.
- The chess engine uses iterative deepening, alpha-beta search, and a tapered
  positional evaluation.
- The board uses a compact 0x88 representation with incremental Zobrist hashing.
- Mouse events, keyboard commands, clocks, audio, and responsive rendering share
  one interactive game loop.

The current product priorities are tracked in [ROADMAP.md](ROADMAP.md).

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --check
cargo build --release
```

Contributions and issue reports are welcome. UI reports are most useful when
they include the terminal application, operating system, window dimensions,
and selected piece mode.

## Credits

The bundled [RhosGFX chess pieces](assets/pieces/rhosgfx/LICENSE.txt) and the
selected [Kenney sound effects](assets/sounds/kenney/LICENSE.txt) are released
under CC0.
