# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

A terminal chess program in Rust (`Cargo.toml` describes it as "Play chess in your terminal"). It has an interactive game loop, human-vs-engine and local two-player modes, SAN/coordinate move entry, search, and a responsive terminal UI. Large boards use bundled SVG pieces: supported terminals receive inline images, while other terminals receive a true-colour Unicode block rendering. Compact boards use chess glyphs from the terminal font.

## Commands

```sh
cargo build --release          # optimized binary
cargo run --release            # opening menu
cargo run --release -- --two   # local two-player game
cargo run --release -- --help  # complete CLI reference
cargo test                     # every crate, including perft
cargo clippy --all-targets
cargo fmt --all
cargo run --bin chess-server   # local multiplayer server
```

- Use `--pieces auto` for protocol images where supported, `--pieces art` for the portable vector-derived renderer, and `--pieces glyph` for font glyphs.
- The runtime commands `size small` and `size big` switch between compact and responsive boards.
- The UI is terminal-dependent. Verify both a generic ANSI terminal and an image-capable terminal such as iTerm2 when changing rendering code.

## Architecture

A Cargo workspace. Dependencies only point downwards: `chess-core` has no I/O, `chess-protocol` depends on core, `chess-server` on both, and the root `chess` package (the terminal client) on core and protocol.

- `crates/chess-core`: `board` is the base, `movegen` depends on it, `san` and `eval` depend on both, `search` owns engine search, and `game` holds UI-independent game state and outcomes. `tests/rules.rs` has perft, hashing, and notation round-trip tests; run them after touching move generation.
- `crates/chess-protocol`: wire messages plus `From` conversions (`Color`↔`Side`, `Outcome`→`GameStatus`, `TimeControl`↔`Duration`). Add conversions here rather than in the client or server.
- `crates/chess-server`: `hub` owns rooms, seats, clocks, pairing strangers and the lobby count, eviction, and persistence, and never touches a socket; `store` is the SQLite database (accounts, sessions, SSH keys, finished games; schema changes are appended to `MIGRATIONS`, never edited); `accounts` adds validation, Argon2id hashing, and session tokens on top and is async so hashing never runs under a lock; `server` is the axum transport, which answers account commands itself and passes the rest to the hub; `ssh` is the SSH gateway, which runs the terminal client in a pseudo-terminal per visitor with `CHESS_HOSTED=1` (file commands off) and hands it a session or an SSH-key link ticket through the environment; `main.rs` only reads the environment.
- Root package: `ui` owns terminal rendering and layout, `client` the WebSocket transport, `storage` local files, and `main.rs` the application loop. `tests/online.rs` runs a real server in-process against two clients.

**`src/ui.rs`: terminal presentation**
- `Metrics` fits the board to the current terminal dimensions.
- Large portable pieces are rasterized from the bundled RhosGFX SVGs and represented with true-colour Unicode quadrant blocks.
- `detect_inline_images` negotiates terminal graphics support. `board_image` and `draw_inline_image` render a higher-resolution board when supported.
- Always retain a portable fallback: the application is intended to run over SSH on many terminal emulators.

**`crates/chess-core/src/search.rs`: engine**
- Iterative deepening search with time and depth limits.
- Search output feeds the evaluation panel and the `hint` command.

**`crates/chess-core/src/board.rs`: position representation**
- **0x88 board.** `Square` is a `u8` with value `rank * 16 + file`. One rank is ±16 and one file is ±1. A square is off the board when `s & 0x88 != 0`. Do step arithmetic in `i16` and check it with `on_board(i16)` before indexing. `squares` is a 128-entry array. Use `all_squares()` to iterate over the real board squares, and `sq64()` to convert a square to a 0..64 table index.
- **`make_move` updates the Zobrist hash incrementally**, using `put`/`take`, which XOR piece keys. `unmake_move` does not re-hash. It writes `squares` directly and restores `hash` and the other state from `Undo`. After any change, `pos.hash` must still equal `compute_hash()`.
- **En passant has two rules.** `pos.ep` is set after *every* double push, so FEN output matches other tools. The ep-file hash key is only XORed in when a pawn of the side to move can actually capture (`ep_is_capturable`). The order inside `make_move` matters: remove the old ep key before changing state, and add the new one after `side` flips.
- **Castling rights** are cleared through `castle_mask(from) & castle_mask(to)`. Moving from or capturing onto a king or rook home square removes the matching rights.
- **King cache.** `king: [Square; 2]` is updated in make/unmake. `from_fen` rejects any position that doesn't have exactly one king per side, because the rest of the engine relies on this cache.

**`crates/chess-core/src/movegen.rs`: move generation**
- `generate_pseudo_legal(pos, captures_only)`, then `filter_legal`: make each move on a cloned position and discard it if our king is in check. There are no pin or check-evasion tricks.
- Queens reuse `KING_STEPS` as their ray directions (bishop and rook directions combined).
- Castling generation checks the real board (king on e1/e8, rook in the corner, empty squares in between, no attacked transit squares), not only the rights flags.
- `perft` lives here as well.

**`crates/chess-core/src/san.rs`: notation**
- `to_san_with` renders a move, disambiguating by file first, then rank, then the full square. It adds `+`/`#` by making the move. Pass in the legal move list you already have to avoid regenerating it.
- `parse_move` builds the SAN of every legal move and matches the input in stages: exact SAN, then coordinate/UCI, then case-insensitive, then ignoring punctuation (`x`, `=`, `-`, `0`). It returns `ParseError::Ambiguous` with the candidate moves when several match. A coordinate move to the last rank with no promotion letter defaults to a queen. `0-0` spellings are normalised to `O-O`.

**`crates/chess-core/src/eval.rs`: evaluation**
- Tapered eval: separate midgame and endgame scores, blended by phase (24 = opening, 0 = only kings and pawns left). It uses material plus piece-square tables, with bishop pair, doubled/isolated/passed pawns, rook on open or semi-open file, rook on the 7th, and king pawn shield.
- Piece-square tables are written from White's point of view with **rank 8 in the first row**. `table_index` mirrors them for Black. Keep that orientation when you edit a table.
- `evaluate` returns centipawns from the **side to move's** point of view, including a `TEMPO` bonus. That is the negamax convention.
