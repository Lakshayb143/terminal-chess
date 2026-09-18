# Terminal Chess

Play a complete game of chess without leaving the terminal.

Terminal Chess is a mouse-friendly Rust application that brings the familiar
feel of a graphical chess site to a native command-line program. It combines a
responsive board, scalable vector pieces, clocks, move history, sound effects,
and an embedded engine in a single executable.

The goal is a terminal-first place where people can play locally, against an
engine, or against another person over the network.

## Highlights

- Click a piece, inspect its legal moves, and click a destination to move.
- Play against the built-in engine or another person at the same terminal.
- Create a private online game, share its six-character code, and play from two
  terminals with server-owned rules and clocks.
- Use SAN (`Nf3`, `exd5`, `O-O`) or coordinate notation (`e2e4`) at any time.
- Get a responsive layout with player panels, clocks, captured pieces, material
  advantage, move history, and clear game-over states.
- Render crisp vector-derived pieces through iTerm2 and Kitty image protocols,
  with true-colour and Unicode fallbacks for other terminals.
- Hear distinct sounds for moves, captures, checks, castling, promotions, and
  game endings.
- Choose between four board themes and several piece-rendering modes.
- Pause at any time, resume an autosaved game, and import or export PGN files.
- Keep player names and display preferences in a readable local configuration.

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

To play online against a local development server:

```sh
# Terminal 1: start the authoritative server
cargo run --release --bin chess-server

# Terminal 2: create a game and copy the invite code shown on the board
cargo run --release -- online create --name Lakshay

# Terminal 3: join with that code
cargo run --release -- online join ABC123 --name Guest
```

To install Terminal Chess as a normal command:

```sh
cargo install --git https://github.com/Lakshayb143/terminal-chess --locked chess
chess
```

## Playing

Click a piece to select it. Legal destinations appear on the board; click one
to complete the move. Click another friendly piece to change the selection, or
press `Esc` to cancel. Promotions open a four-piece chooser on the board.

The mouse-accessible actions let you undo, offer or accept a draw, resign,
restart, turn the board around, and change the board size or piece style. Potentially destructive actions require confirmation. The move list
can be scrolled with the mouse wheel or `Page Up` and `Page Down`, and so can
any page of text that is longer than the window - `help`, `history`, `pgn` and
the rest show where you are in them and scroll with the wheel, the arrow keys
or `Page Up` and `Page Down`.

Keyboard input remains fully supported. Type anywhere to return focus to the
move box. Use `Tab` or the arrow keys to move through every visible control,
`Shift+Tab` to go backwards, and `Enter` to activate the focused button. The
`Move` button returns the cursor to the move box, so draw, resign, restart, and
all other game actions remain usable without a mouse.

Quiet legal moves use center dots. Captures use a separate highlighted square
and frame, while an invalid click briefly marks only the rejected square and
explains how to recover. Promotion choices accept either a click or `Q`, `R`,
`B`, or `N` from the keyboard.

## Local games and files

Terminal Chess automatically saves the current game after moves and game-state
changes. The next startup offers **Resume saved game**, or you can use:

```text
pause                  stop the clocks and prevent moves
resume                 continue a paused game
save                    update the default autosave
save game.json          save to a chosen file
load                    restore the default autosave
load game.json          restore a chosen file
export game.pgn         write a standard PGN file
import game.pgn         import and replay a PGN main line
```

PGN import supports standard headers, comments, and annotations. The main line
becomes the playable game, while parenthesized side variations are ignored.

Preferences are stored at `~/.config/terminal-chess/config.toml` by default.
Set `TERMINAL_CHESS_CONFIG` to choose another location. Player names, theme,
piece style, board size, sound mode, orientation, clock, and increment persist
between launches. Run `setup` in the game to see the active config and autosave
paths, or set names with `name white Lakshay` and `name black Guest`.

## Private online games

The server owns the position, legal-move validation, clocks, draw offers, and
game result. The terminal client displays server snapshots and never advances
the board optimistically, so reconnecting cannot leave the players with two
different positions.

Create or join a game with:

```sh
chess online create --name Lakshay --server wss://chess.example.com/ws
chess online join ABC123 --name Guest --server wss://chess.example.com/ws
```

If the connection drops, the client reconnects automatically and restores the
same seat using a local reconnect token. To return after closing the program,
run `chess online resume`. The token is stored beside the config as
`online-session.json`; treat that file as private because it grants control of
the seat.

Online games support mouse and keyboard moves, server clocks, draw offers,
resignation, PGN export, sounds, both board orientations, and all rendering
modes. The status line distinguishes waiting for an opponent, reconnecting,
and an opponent who is temporarily offline.

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
--truecolor          force 24-bit colour
--256                limit colours to the 256-colour palette
--compact            keep the small board
--sound <mode>       auto, on, or off
--mute               disable sound effects
--fen <position>     start from a FEN position
--resume             continue the automatically saved game
--load <file>        open a specific saved game
online create        create a private online game
online join <code>   join a private online game
online resume        restore the last online seat
--server <ws-url>    online endpoint (or CHESS_SERVER_URL)
--name <name>        name shown in an online game
```

Run `chess --help` for the complete reference.

## Engineering notes

The project is intentionally built as a terminal application rather than a web
view wrapped in a desktop shell. A few implementation details:

- The UI negotiates terminal capabilities and keeps a portable ANSI fallback.
- Themes are designed in 24-bit colour, with highlights blended over each
  square. 24-bit output is used when `COLORTERM` or the terminal's own markers
  say it is supported; other terminals get hand-tuned 256-colour squares.
- A cached row-diff renderer and synchronized terminal updates prevent partial
  frames without repainting the whole screen after every move.
- SVG piece assets are rasterized in-process and embedded in the release binary.
- The chess engine uses iterative deepening, alpha-beta search, and a tapered
  positional evaluation.
- The board uses a compact 0x88 representation with incremental Zobrist hashing.
- Mouse events, keyboard commands, clocks, audio, and responsive rendering share
  one interactive game loop.

The current product priorities are tracked in [ROADMAP.md](ROADMAP.md), and
planned interface work in [UI_ROADMAP.md](UI_ROADMAP.md).

## Running the multiplayer server

The repository includes the authoritative WebSocket server used by online
clients. It has durable active rooms, reconnect tokens, per-connection request
limits, structured logs, and graceful shutdown.

Run the server locally with:

```sh
cargo run --bin chess-server
```

It listens on `127.0.0.1:3000` by default and exposes `/health` and `/ws`.
Active games are atomically saved to `data/server-state.json` whenever a move,
join, or result changes them. Finished games are kept for 10 minutes so both
players can see the result, and rooms nobody has been connected to for 30
minutes are discarded. The following environment variables configure it:

| Variable | Default | Purpose |
| --- | --- | --- |
| `CHESS_SERVER_ADDR` | `127.0.0.1:3000` | TCP bind address |
| `CHESS_SERVER_STATE` | `data/server-state.json` | Durable room state |
| `CHESS_RATE_LIMIT_PER_10S` | `60` | Requests allowed per connection window |
| `CHESS_LOG_FORMAT` | text | Set to `json` for structured logs |
| `RUST_LOG` | `chess_server=info` | Log filter |

For a public host, [deploy/README.md](deploy/README.md) provides Docker Compose
and Caddy instructions. Caddy terminates TLS and upgrades `wss://` connections;
the game server stays on the private container network.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --all --check
cargo build --release
```

The repository is a Cargo workspace. The root package is the terminal client;
the other crates are shared by the client and the server:

| Crate | Contents |
| --- | --- |
| `chess` (root) | Terminal UI, input, sound, local storage, and the network client |
| `crates/chess-core` | Rules, notation, evaluation, engine search, and game state; no I/O |
| `crates/chess-protocol` | Versioned wire messages and their conversions to the game model |
| `crates/chess-server` | The authoritative room hub and its WebSocket transport |

Contributions and issue reports are welcome. UI reports are most useful when
they include the terminal application, operating system, window dimensions,
and selected piece mode.

## Credits

The bundled [RhosGFX chess pieces](assets/pieces/rhosgfx/LICENSE.txt) and the
selected [Kenney sound effects](assets/sounds/kenney/LICENSE.txt) are released
under CC0.
