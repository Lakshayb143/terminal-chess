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

Moves can be entered as SAN (`Nf3`, `exd5`, `O-O`) or coordinates (`e2e4`).
Type `help` during a game to see the available commands.

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
--theme <name>       slate, wood, forest, or mono
--pieces <kind>      auto, art, or glyph
--compact            keep the small board
--fen <position>     start from a FEN position
```

Run `cargo run --release -- --help` for the complete list.

## Piece artwork

The bundled RhosGFX SVG chess pieces are by RhosGFX and released under CC0.
See [`assets/pieces/rhosgfx/LICENSE.txt`](assets/pieces/rhosgfx/LICENSE.txt).
