# Terminal Chess

Play a complete game of chess without leaving the terminal.

Terminal Chess is a mouse-friendly Rust application that brings the familiar
feel of a graphical chess site to a native command-line program. It combines a
responsive board, scalable vector pieces, clocks, move history, sound effects,
and an embedded engine in a single executable.

The goal is a terminal-first place where people can play locally, against an
engine, or against another person over the network.

## Highlights

- Click a piece, inspect its legal moves, and click a destination to move, or
  do the same with the arrow keys and `Enter`.
- Play against the built-in engine at four levels, from Beginner to Strong, or
  another person at the same terminal.
- Create a private online game, share its six-character code, and play from two
  terminals with server-owned rules and clocks.
- Play a random opponent: the server pairs you with the next person looking,
  and the home page shows how many people are online.
- Use SAN (`Nf3`, `exd5`, `O-O`) or coordinate notation (`e2e4`) at any time.
- Get a responsive layout with player panels, clocks, captured pieces, material
  advantage, move history, and a game-over summary with Rematch, Review, and
  PGN.
- Step back through any game, during play or after it, and return to it.
- Render crisp vector-derived pieces through the iTerm2, Kitty, and sixel image
  protocols, with true-colour and Unicode fallbacks for other terminals.
- Hear distinct sounds for moves, captures, checks, castling, promotions, and
  game endings.
- Choose between four board themes and several piece-rendering modes.
- Pause at any time, resume an autosaved game, and import or export PGN files.
- Keep player names and display preferences in a readable local configuration.

## Quick start

To play on the public server, there is nothing to install:

```sh
ssh -p 2222 chess.lakshaybhatia.com
```

To run it yourself, you need a recent [Rust toolchain](https://www.rust-lang.org/tools/install).

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
move box. The arrow keys move a cursor over the board: `Enter` picks up the
piece under it and `Enter` again puts it down, exactly as two clicks would.
`Tab` moves through every visible control, `Shift+Tab` goes backwards, and
`Enter` activates the focused button, so draw, resign, restart, and all other
game actions remain usable without a mouse.

`Page Up` steps back through the game one move at a time, and `Page Down`
steps forward; while you look back, the arrow keys step too. `Home` jumps to
the starting position and `End` returns to the game. Clicking a move in the
list shows the position after it. Nothing can be played until you return, and
in an online game your opponent's move brings you back.

`Esc` always goes one step back: it closes a page, drops a selected piece,
leaves a review, hides the cursor, and finally moves the focus to `Menu`, from
which `Enter` goes back to the home page. It never leaves a game on its own.
Every game ends on the home page, whether through `Menu`, `menu`, or `quit`;
local games are saved, so **Continue** picks them up. Only `q` on the home
page, or `Ctrl+C`, ends the program.

When a game ends, the panel shows the result, how it was reached, and the
number of moves, with **Rematch** (colours swapped), **Review**, **PGN**, and
**Menu**.

Quiet legal moves use center dots. Captures use a separate highlighted square
and frame, while an invalid click briefly marks only the rejected square and
explains how to recover. Promotion choices accept either a click or `Q`, `R`,
`B`, or `N` from the keyboard.

## Engine levels and clocks

Choose **Settings** (`s`) on the home page to set how the engine plays, the
clock, the board colours, the pieces, and the sound. The rows that start a game
against the engine say which level and clock it will use.

| Level | How it plays |
| --- | --- |
| Beginner | Looks one move ahead and chooses loosely, so it gives pieces away |
| Casual | Looks two moves ahead; its mistakes can be punished |
| Club | Looks four moves ahead and seldom blunders |
| Strong | Full strength, thinking up to 3 seconds a move |

Each level won all 24 games of a match against the level below it, so each is
a clear step up. Casual is the default. During a game, `level` shows the level
and `level club` changes it; `time` and `depth` still set the engine by hand,
at full strength.

The clock offers Bullet 1+0, Blitz 3+2 and 5+0, Rapid 10+0 and 15+10, and
Untimed; type your own on the Clock row, such as `7+3`. The same clock is used
for private online games you create. The engine budgets its own clock, so it
never loses on time in a short game. **Random side** (`r`) plays the engine as
White or Black, drawn at random.

## Local games and files

Terminal Chess automatically saves the current game after moves and game-state
changes. The home page then offers **Continue saved game**, or you can use:

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

When the game ends, either player can offer a **Rematch**; once the other
accepts, both move to a new game on the same clock with the colours swapped.
Leaving a game in progress for the menu asks first, because the seat is only
held for 60 seconds.

The terminal window's title says whose move it is, with your clock when it is
yours, so a game in a background window can still be followed. In terminals
that report focus, the bell rings when your opponent moves, or a rematch or a
random opponent turns up, while the window is in the background.

## Playing a random opponent

With no friend at hand, choose **Play a random opponent** on the home page, or
run:

```sh
chess online find --name Lakshay --server wss://chess.example.com/ws
```

The server pairs you with the next person looking for a game. Every such game
is 10+5 (ten minutes each, five seconds added per move), colours are drawn at
random, and an account is never paired with itself.

The home page shows a coloured badge beside the ONLINE heading: green with the
number of people online, red when you are the only one, and grey when the
server cannot be reached. The random-opponent row says when someone is already
waiting. If you are alone while searching, the game says so; press `q` to go
back to the menu and choose another mode.

## Accounts

Anyone can play online as a guest. An account adds a name nobody else can use
and keeps every finished online game in your history.

Press `a` on the start menu to sign in or create one: choose a username, then
type your password twice. The password never appears on screen. This computer
stays signed in until you sign out from the same page or leave it unused for 90
days; the sign-in lives beside the config as `account.json`, which, like the
online seat, should be kept private.

While signed in, online games are played under your username and the account
page lists your recent results. Guests' names are marked as guests there, so
nobody can pass as a registered player.

The account page also lets you:

- change your password, which signs out every other computer;
- see the SSH keys that sign you in, and unlink any of them;
- delete the account, after typing your password and your username. Its
  sign-ins and keys go with it. Your finished games stay in your opponents'
  histories, shown as played by a deleted player, and the username becomes
  free again.

## Playing over SSH

The server can also let people in over plain `ssh`, which every macOS, Linux,
and Windows 10 or later computer already has. Each visitor gets the full game
in their own terminal: the local modes, random opponents, online games with
invite codes, and the account page.

- No SSH password is asked for. Anyone may play as a guest.
- Signing in or creating an account from the menu links the SSH key the
  visitor connected with, so from then on `ssh` signs them in by itself.
- Everything else matches the installed client, except that sound stays off
  and commands that read or write files are disabled, since the files would
  be on the server. Use `pgn` to show a game and copy it from the screen.
- The engine thinks for at most 5 seconds a move, since it runs on the
  server's processors.
- One address may have a few games open at once (3 by default), so a single
  visitor cannot take every seat.
- The gateway only runs the game: commands, `sftp`, `scp` and port forwarding
  are refused.

Images are as sharp as the visitor's terminal allows: iTerm2 and Kitty show
real images through SSH; other terminals get the portable block pieces.

## Board rendering

The board grows with the terminal window. Wide terminals place game information
beside it; narrow terminals arrange a compact information panel underneath.

The default `auto` mode selects the best renderer available:

| Command | Rendering mode |
| --- | --- |
| `pieces auto` | High-resolution inline images (iTerm2, Kitty, or sixel) when supported, with a safe fallback |
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

At startup the game asks the terminal which image protocols it supports and how
large its character cells are. The question and the answer travel through SSH,
so detection works on a remote machine too.

| Terminal | Large board with `pieces auto` |
| --- | --- |
| iTerm2, WezTerm | iTerm2 images |
| Kitty, Ghostty, Konsole | Kitty images |
| Windows Terminal 1.22+ (cmd, PowerShell, WSL, SSH), foot, xterm (`-ti vt340`), VTE builds with sixel | Sixel images |
| VS Code with `terminal.integrated.enableImages` | iTerm2 images |
| GNOME Terminal, Ptyxis, Alacritty, the classic Windows console, and others | Unicode block art |

Terminals without image support draw the board with block art. VS Code
recolours text it considers low-contrast, which spoils the art's shading; set
`terminal.integrated.minimumContrastRatio` to `1`, or turn on
`terminal.integrated.enableImages` to get the image board instead. The game
shows this tip when it starts in VS Code without images.

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
--random             play a side drawn at random against the engine
--two                play with two people at one keyboard
--level <name>       beginner, casual, club, or strong
--time <seconds>     engine time per move, at full strength
--depth <number>     maximum search depth, at full strength
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
online find          play whoever else is looking for a game
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
  positional evaluation. The weaker levels search less deeply and add a
  bounded random bonus to each move's score before choosing, so they make
  mistakes of a known size rather than random ones.
- The board uses a compact 0x88 representation with incremental Zobrist hashing.
- Mouse events, keyboard commands, clocks, audio, and responsive rendering share
  one interactive game loop.

The current product priorities are tracked in [ROADMAP.md](ROADMAP.md), and
planned interface work in [UI_ROADMAP.md](UI_ROADMAP.md).

## Running the multiplayer server

The repository includes the authoritative WebSocket server used by online
clients. It has durable active rooms, reconnect tokens, accounts and game
history in SQLite, per-connection request limits, structured logs, and graceful
shutdown.

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
| `CHESS_SERVER_DB` | `data/chess.db` | Accounts and finished games (SQLite); empty for guests only |
| `CHESS_RATE_LIMIT_PER_10S` | `60` | Requests allowed per connection window |
| `CHESS_SSH_ADDR` | unset | Address for the SSH gateway, such as `0.0.0.0:2222`; unset turns it off |
| `CHESS_SSH_HOST_KEY` | `data/ssh_host_ed25519_key` | The gateway's host key, created on first start |
| `CHESS_CLIENT_BIN` | `chess` beside the server | The terminal client started for each visitor |
| `CHESS_SSH_MAX_SESSIONS` | `100` | Visitors allowed at once |
| `CHESS_SSH_MAX_PER_IP` | `3` | Games one address may have open at once |
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
| `crates/chess-server` | The authoritative room hub, random-opponent pairing, accounts, and the WebSocket and SSH gateways |

Contributions and issue reports are welcome. UI reports are most useful when
they include the terminal application, operating system, window dimensions,
and selected piece mode.

## Credits

The bundled [RhosGFX chess pieces](assets/pieces/rhosgfx/LICENSE.txt) and the
selected [Kenney sound effects](assets/sounds/kenney/LICENSE.txt) are released
under CC0.
