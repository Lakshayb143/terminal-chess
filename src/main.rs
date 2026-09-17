//! A chess game for the terminal: draw a board, read a move, answer with one.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::time::Instant;

use chess::board::{self, Color, Move, MoveKind, Piece, PieceKind, Position};
use chess::client::{OnlineClient, TransportEvent};
#[cfg(test)]
use chess::game::GameClock;
use chess::game::{describe, outcome, outcome_detail, score_tag, Game, Outcome};
use chess::input::{Action as InputAction, TerminalInput};
use chess::movegen::{generate_legal, in_check};
use chess::protocol::{
    ClientCommand, ErrorCode, FinishReason, GameResult, GameSnapshot, GameStatus, MoveRejection,
    ServerEvent, Side, TimeControl,
};
use chess::san::{parse_move, to_san, to_san_with, ParseError};
use chess::search::{self, Limits, Search, SearchResult};
use chess::ui::{self, BoardView, Theme};
use chess::{eval, sound, storage};

fn main() {
    let loaded = storage::load_preferences();
    let options = match Options::parse(std::env::args().skip(1), &loaded.preferences) {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(message) => {
            eprintln!("chess: {}", message);
            eprintln!("Try `chess --help`.");
            std::process::exit(2);
        }
    };
    if let Err(message) = play(options, loaded) {
        eprintln!("chess: {}", message);
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// The human has White, the engine answers as Black.
    HumanWhite,
    HumanBlack,
    /// Two people sharing the keyboard; the engine only gives hints.
    TwoPlayer,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::HumanWhite => "white",
            Mode::HumanBlack => "black",
            Mode::TwoPlayer => "two",
        }
    }

    fn named(name: &str) -> Option<Mode> {
        match name {
            "white" => Some(Mode::HumanWhite),
            "black" => Some(Mode::HumanBlack),
            "two" => Some(Mode::TwoPlayer),
            _ => None,
        }
    }
}

enum StartChoice {
    Mode(Mode),
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OnlineIntent {
    Create,
    Join(String),
    Resume,
}

struct Options {
    /// `None` until the opening menu asks which side to take.
    mode: Option<Mode>,
    /// `None` means the ordinary starting position.
    fen: Option<String>,
    limits: Limits,
    ascii: bool,
    /// `None` means "colour if this is a terminal".
    color: Option<bool>,
    palette: ui::Palette,
    pieces: ui::Pieces,
    /// Keep the old small board rather than filling the window.
    compact: bool,
    sound: sound::Mode,
    player_names: [String; 2],
    flipped: bool,
    /// Each player's starting time. `None` is an untimed game.
    clock: Option<Duration>,
    increment: Duration,
    /// Restore the default autosave, or a specifically named saved game.
    resume: bool,
    load: Option<PathBuf>,
    online: Option<OnlineIntent>,
    server_url: String,
    online_name: String,
}

const HELP: &str = "\
chess - play chess in your terminal

USAGE:
    chess [OPTIONS]
    chess online create [OPTIONS]
    chess online join <CODE> [OPTIONS]
    chess online resume [OPTIONS]

GAME:
    -w, --white          play White against the engine (asked for if omitted)
    -b, --black          play Black against the engine
    -2, --two            two players at one keyboard, no engine
    -t, --time <SECS>    seconds the engine may think per move (default: 3)
    -d, --depth <N>      cap the search depth; without --time, search to
                         exactly this depth however long it takes
        --clock <MINS>   starting time for each player (default: 10)
        --increment <S>  seconds added after each move (default: 0)
        --no-clock       play without chess clocks
        --fen <FEN>      start from a position rather than the initial one
                         (quote it: it contains spaces)
        --resume         continue the automatically saved local game
        --load <FILE>    open a saved Terminal Chess game

ONLINE:
        online create    create a private game and show its invite code
        online join CODE join a private game as Black
        online resume    reconnect to the last active online game
        --server <URL>   WebSocket endpoint (or CHESS_SERVER_URL)
        --name <NAME>    name shown to the other player

LOOK:
        --theme <NAME>   board colours: slate, wood, forest, mono
        --pieces <KIND>  art (vector pieces), glyph (figurines), or auto
        --compact        keep the small board whatever the window can take
        --sound <MODE>   sound effects: auto, on, or off (default: auto)
        --mute           start with sound effects off
        --ascii          letters instead of figurines, plain rules
        --no-colour      no escape codes at all (also honours NO_COLOR)
        --colour         colour even when the output is not a terminal
    -h, --help           print this help

IN THE GAME:
    Click a piece, then click a highlighted square to move it.
    Type a move as SAN (Nf3, exd5, O-O, e8=Q) or as coordinates (e2e4, e7e8q).
    Type `help` at the prompt for the list of commands.

    The board is drawn as big as the window allows, and grows when the window
    does. Give it room and the pieces are drawn rather than lettered.";

impl Options {
    /// `Ok(None)` means help was printed and there is nothing left to do.
    fn parse(
        args: impl Iterator<Item = String>,
        preferences: &storage::Preferences,
    ) -> Result<Option<Options>, String> {
        let clock = preferences
            .clock_enabled
            .then(|| Duration::from_secs_f64(preferences.clock_minutes * 60.0));
        let mut options = Options {
            mode: None,
            fen: None,
            limits: Limits::default(),
            ascii: false,
            color: None,
            palette: ui::palette(&preferences.theme).unwrap_or(ui::THEMES[0].1),
            pieces: ui::pieces_named(&preferences.pieces).unwrap_or(ui::Pieces::Auto),
            compact: preferences.compact,
            sound: sound::Mode::named(&preferences.sound).unwrap_or(sound::Mode::Auto),
            player_names: [
                preferences.white_name.clone(),
                preferences.black_name.clone(),
            ],
            flipped: preferences.flipped,
            clock,
            increment: Duration::from_secs_f64(preferences.increment_seconds),
            resume: false,
            load: None,
            online: None,
            server_url: std::env::var("CHESS_SERVER_URL")
                .unwrap_or_else(|_| "ws://127.0.0.1:3000/ws".to_string()),
            online_name: preferences.white_name.clone(),
        };
        // Tracked so that `--depth` alone means "this depth, no clock", while
        // `--depth` with `--time` means "this depth, but stop when time runs out".
        let mut timed = false;
        let mut args = args.peekable();

        while let Some(arg) = args.next() {
            let mut value = |flag: &str| -> Result<String, String> {
                args.next().ok_or(format!("{} needs a value", flag))
            };
            match arg.as_str() {
                "-h" | "--help" => {
                    println!("{}", HELP);
                    return Ok(None);
                }
                "-w" | "--white" => options.mode = Some(Mode::HumanWhite),
                "-b" | "--black" => options.mode = Some(Mode::HumanBlack),
                "-2" | "--two" => options.mode = Some(Mode::TwoPlayer),
                "--ascii" => options.ascii = true,
                "--compact" | "--small" => options.compact = true,
                "--mute" | "--silent" => options.sound = sound::Mode::Off,
                "--sound" => {
                    let name = value("--sound")?;
                    options.sound = sound::Mode::named(&name)
                        .ok_or(format!("--sound wants auto, on or off, not '{}'", name))?;
                }
                "--pieces" => {
                    let name = value("--pieces")?;
                    options.pieces = ui::pieces_named(&name)
                        .ok_or(format!("--pieces wants art, glyph or auto, not '{}'", name))?;
                }
                "--no-color" | "--no-colour" | "--plain" => options.color = Some(false),
                "--color" | "--colour" => options.color = Some(true),
                "--theme" => {
                    let name = value("--theme")?;
                    options.palette = ui::palette(&name).ok_or(format!(
                        "no theme called '{}' - try {}",
                        name,
                        ui::theme_names()
                    ))?;
                }
                "-t" | "--time" => {
                    let text = value("--time")?;
                    let secs: f64 = text
                        .parse()
                        .map_err(|_| format!("--time wants a number of seconds, not '{}'", text))?;
                    if !(secs.is_finite() && secs > 0.0) {
                        return Err(format!("--time must be positive, got '{}'", text));
                    }
                    options.limits.movetime = Some(Duration::from_secs_f64(secs));
                    timed = true;
                }
                "-d" | "--depth" => {
                    let text = value("--depth")?;
                    let depth: u32 = text
                        .parse()
                        .map_err(|_| format!("--depth wants a whole number, not '{}'", text))?;
                    if depth == 0 || depth > search::MAX_DEPTH {
                        return Err(format!("--depth must be 1..={}", search::MAX_DEPTH));
                    }
                    options.limits.depth = depth;
                    if !timed {
                        options.limits.movetime = None;
                    }
                }
                "--clock" => {
                    let text = value("--clock")?;
                    let minutes: f64 = text
                        .parse()
                        .map_err(|_| format!("--clock wants minutes, not '{}'", text))?;
                    if !(minutes.is_finite() && minutes > 0.0) {
                        return Err(format!("--clock must be positive, got '{}'", text));
                    }
                    options.clock = Some(Duration::from_secs_f64(minutes * 60.0));
                }
                "--increment" => {
                    let text = value("--increment")?;
                    let seconds: f64 = text
                        .parse()
                        .map_err(|_| format!("--increment wants seconds, not '{}'", text))?;
                    if !(seconds.is_finite() && seconds >= 0.0) {
                        return Err(format!("--increment cannot be negative, got '{}'", text));
                    }
                    options.increment = Duration::from_secs_f64(seconds);
                }
                "--no-clock" => options.clock = None,
                "--fen" => options.fen = Some(value("--fen")?),
                "--resume" => options.resume = true,
                "--load" => options.load = Some(PathBuf::from(value("--load")?)),
                "--server" => options.server_url = value("--server")?,
                "--name" => options.online_name = value("--name")?,
                "online" => {
                    if options.online.is_some() {
                        return Err("online mode was specified more than once".to_string());
                    }
                    let action = value("online")?;
                    options.online = Some(match action.as_str() {
                        "create" => OnlineIntent::Create,
                        "join" => OnlineIntent::Join(value("online join")?.to_ascii_uppercase()),
                        "resume" => OnlineIntent::Resume,
                        _ => {
                            return Err(format!(
                                "online wants create, join <CODE>, or resume, not '{action}'"
                            ))
                        }
                    });
                }
                other => return Err(format!("unknown option '{}'", other)),
            }
        }
        // `--time` after `--depth` has to put the clock back.
        if timed && options.limits.movetime.is_none() {
            options.limits.movetime = Some(Limits::default().movetime.unwrap());
        }
        if options.online.is_some()
            && !(options.server_url.starts_with("ws://")
                || options.server_url.starts_with("wss://"))
        {
            return Err("--server must begin with ws:// or wss://".to_string());
        }
        options.online_name = options
            .online_name
            .trim()
            .chars()
            .filter(|character| !character.is_control())
            .take(32)
            .collect();
        if options.online.is_some() && options.online_name.is_empty() {
            return Err("--name cannot be empty".to_string());
        }
        Ok(Some(options))
    }
}

// Local saves adapt the shared game model to the existing on-disk format.

fn saved_game(game: &mut Game, mode: Mode) -> storage::SavedGame {
    let mut saved = storage::SavedGame::new();
    saved.start_fen = game.start.to_fen();
    saved.moves = game.undos.iter().map(|undo| undo.mv.to_uci()).collect();
    saved.mode = mode.name().to_string();
    saved.initial_clock_ms = game
        .clock
        .initial
        .map(|time| time.as_millis().min(u64::MAX as u128) as u64);
    saved.increment_ms = game.clock.increment.as_millis().min(u64::MAX as u128) as u64;
    saved.remaining_ms = game.clock.snapshot();
    saved.paused = game.paused;
    saved.resigned = game.resigned.map(|color| color.name().to_ascii_lowercase());
    saved.draw_offer = game
        .draw_offer
        .map(|color| color.name().to_ascii_lowercase());
    saved.agreed_draw = game.agreed_draw;
    saved
}

fn restore_game(saved: storage::SavedGame) -> Result<(Game, Mode), String> {
    let mode = Mode::named(&saved.mode)
        .ok_or_else(|| format!("saved game has unknown mode `{}`", saved.mode))?;
    let start = Position::from_fen(&saved.start_fen)
        .map_err(|error| format!("saved starting position is invalid: {error}"))?;
    let initial = saved.initial_clock_ms.map(Duration::from_millis);
    let increment = Duration::from_millis(saved.increment_ms);
    let mut game = Game::with_clock(start, initial, increment);
    for (index, notation) in saved.moves.iter().enumerate() {
        let movement = parse_move(&game.pos, notation).map_err(|_| {
            format!(
                "saved move {} (`{}`) is not legal in its position",
                index + 1,
                notation
            )
        })?;
        game.play(movement);
    }
    game.resigned = saved.resigned.as_deref().and_then(color_named);
    game.draw_offer = saved.draw_offer.as_deref().and_then(color_named);
    game.agreed_draw = saved.agreed_draw;
    game.paused = saved.paused;
    let running = !game.paused && outcome(&game).is_none();
    game.clock
        .restore(saved.remaining_ms, game.pos.side, running);
    game.revision = 0;
    Ok((game, mode))
}

fn color_named(name: &str) -> Option<Color> {
    match name.to_ascii_lowercase().as_str() {
        "white" => Some(Color::White),
        "black" => Some(Color::Black),
        _ => None,
    }
}

fn runtime_preferences(screen: &Screen, game: &Game) -> storage::Preferences {
    storage::Preferences {
        version: 1,
        white_name: screen.player_names[Color::White.index()].clone(),
        black_name: screen.player_names[Color::Black.index()].clone(),
        theme: ui::palette_name(screen.theme.palette).to_string(),
        pieces: screen.pieces.name().to_string(),
        compact: screen.compact,
        sound: screen.sound.mode().name().to_string(),
        flipped: screen.flipped,
        clock_enabled: game.clock.initial.is_some(),
        clock_minutes: game
            .clock
            .initial
            .map(|time| time.as_secs_f64() / 60.0)
            .unwrap_or(10.0),
        increment_seconds: game.clock.increment.as_secs_f64(),
        onboarding_complete: true,
    }
}

fn save_current_game(path: &Path, game: &mut Game, mode: Mode) -> Result<(), String> {
    storage::save_game(path, &saved_game(game, mode))
}

fn kind_name(kind: PieceKind) -> &'static str {
    match kind {
        PieceKind::Pawn => "pawn",
        PieceKind::Knight => "knight",
        PieceKind::Bishop => "bishop",
        PieceKind::Rook => "rook",
        PieceKind::Queen => "queen",
        PieceKind::King => "king",
    }
}

/// Piece worth in pawns, for the material count beside the board.
fn pawns_worth(kind: PieceKind) -> i32 {
    [1, 3, 3, 5, 9, 0][kind.index()]
}

fn material(pos: &Position, color: Color) -> i32 {
    board::all_squares()
        .filter_map(|s| pos.at(s))
        .filter(|piece| piece.color == color)
        .map(|piece| pawns_worth(piece.kind))
        .sum()
}

/// Make one real game move and emit exactly one matching audio cue. Keeping
/// this at the mutation boundary means mouse, keyboard, and engine moves can
/// never drift into different feedback behavior.
fn play_move(game: &mut Game, mv: Move, screen: &mut Screen) -> String {
    let text = game.play(mv);
    if outcome(game).is_some() {
        game.clock.pause();
    }
    screen.sound.play(sound_after_move(game));
    text
}

fn sound_after_move(game: &Game) -> sound::Cue {
    if outcome(game).is_some() {
        return sound::Cue::GameEnd;
    }
    if in_check(&game.pos, game.pos.side) {
        return sound::Cue::Check;
    }
    let Some(undo) = game.undos.last() else {
        return sound::Cue::Move;
    };
    if undo.mv.promo.is_some() {
        sound::Cue::Promotion
    } else if matches!(undo.mv.kind, MoveKind::CastleKing | MoveKind::CastleQueen) {
        sound::Cue::Castle
    } else if undo.captured.is_some() {
        sound::Cue::Capture
    } else {
        sound::Cue::Move
    }
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

struct Screen {
    theme: Theme,
    sound: sound::Player,
    /// A negotiated Kitty/iTerm-family image protocol. The Unicode board is
    /// always rendered underneath as a zero-risk fallback.
    inline_images: bool,
    flipped: bool,
    /// The terminal as it was when the last frame was drawn. It is measured
    /// again every frame, so resizing the window and pressing return is all it
    /// takes for the board to grow into it.
    cols: usize,
    rows: usize,
    metrics: ui::Metrics,
    /// Wide layouts put game information beside the board; narrow layouts
    /// keep the board large and stack compact information underneath.
    wide_panel: bool,
    pieces: ui::Pieces,
    player_names: [String; 2],
    /// Hold the board at its old small size whatever the window could take.
    compact: bool,
    /// The left edge of the last frame, so the prompt and the engine's
    /// thinking line start where the board starts.
    indent: String,
    /// The playable 8x8 rectangle from the last frame, in terminal cells.
    board_hitbox: Option<BoardHitbox>,
    body_top: usize,
    action_hitboxes: Vec<ActionHitbox>,
    /// The control currently selected for keyboard activation. Typing always
    /// returns this to the move prompt; Tab and arrows traverse the buttons.
    focused: UiAction,
    history_offset: usize,
    history_capacity: usize,
    confirming: Option<UiAction>,
    /// What the engine last said, kept because the frame is redrawn often.
    analysis: Vec<String>,
    /// Feedback under the board: a complaint, a note, a list of moves.
    message: Vec<String>,
    /// The piece currently chosen for click-to-move.
    selected: Option<board::Square>,
    /// Squares the board should point at until the next move.
    targets: Vec<board::Square>,
    /// Legal destinations that capture, styled separately from quiet moves.
    captures: Vec<board::Square>,
    /// A rejected click, shown briefly as local feedback on the board.
    invalid: Option<board::Square>,
    /// Clickable pieces shown when a pawn reaches the back rank.
    promotions: Vec<ui::PromotionOption>,
    /// A page of text - the help, the move list, the score - shown in place of
    /// the board until the next thing is typed.
    page: Option<Page>,
    /// The terminal rows from the last completed paint. Keeping the styled
    /// strings lets a redraw touch only rows whose visible contents changed.
    last_frame: Vec<String>,
    last_size: Option<(usize, usize)>,
    /// The board image currently covering the Unicode fallback, if any.
    inline_drawn: bool,
    last_inline_board: Option<InlineBoardKey>,
    /// The independently edited command row, cached so idle clock polls do
    /// not keep sending the same cursor movement and text.
    last_prompt: Option<String>,
    redraw: bool,
    online: Option<OnlineDisplay>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionDisplay {
    Connecting,
    Connected,
    Reconnecting,
    Stopped,
}

struct OnlineDisplay {
    connection: ConnectionDisplay,
    invite_code: Option<String>,
    your_side: Option<Color>,
    white_connected: bool,
    black_connected: bool,
    reconnect_deadline_ms: Option<u64>,
    move_pending: bool,
}

impl OnlineDisplay {
    fn connected(&self, color: Color) -> bool {
        match color {
            Color::White => self.white_connected,
            Color::Black => self.black_connected,
        }
    }

    fn can_move(&self, game: &Game) -> bool {
        self.connection == ConnectionDisplay::Connected
            && self.your_side == Some(game.pos.side)
            && self.white_connected
            && self.black_connected
            && !self.move_pending
            && outcome(game).is_none()
    }
}

/// A page shown instead of the board, with a heading over it.
struct Page {
    title: String,
    lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAction {
    MoveInput,
    Pause,
    Undo,
    Draw,
    Resign,
    Restart,
    ToggleSize,
    CyclePieces,
    Rematch,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActionHitbox {
    left: usize,
    top: usize,
    width: usize,
    action: UiAction,
}

impl ActionHitbox {
    fn contains(self, column: u16, row: u16) -> bool {
        usize::from(row) == self.top
            && usize::from(column) >= self.left
            && usize::from(column) < self.left + self.width
    }
}

struct RelativeAction {
    left: usize,
    row: usize,
    width: usize,
    action: UiAction,
}

struct RenderedBody {
    lines: Vec<String>,
    actions: Vec<RelativeAction>,
    history_capacity: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct InlineBoardKey {
    position: u64,
    flipped: bool,
    last: Option<Move>,
    check: Option<board::Square>,
    selected: Option<board::Square>,
    targets: Vec<board::Square>,
    captures: Vec<board::Square>,
    invalid: Option<board::Square>,
    promotions: Vec<(board::Square, Piece)>,
    palette: ui::Palette,
    metrics: ui::Metrics,
    left: usize,
    top: usize,
}

struct ButtonSpec {
    action: UiAction,
    label: &'static str,
    enabled: bool,
}

/// Geometry needed to turn a terminal-cell click into a chess square.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BoardHitbox {
    left: usize,
    top: usize,
    cell_w: usize,
    cell_h: usize,
    flipped: bool,
}

impl BoardHitbox {
    fn square_at(self, column: u16, row: u16) -> Option<board::Square> {
        let x = usize::from(column).checked_sub(self.left)?;
        let y = usize::from(row).checked_sub(self.top)?;
        if x >= self.cell_w * 8 || y >= self.cell_h * 8 {
            return None;
        }

        let display_file = (x / self.cell_w) as u8;
        let display_rank = (y / self.cell_h) as u8;
        let file = if self.flipped {
            7 - display_file
        } else {
            display_file
        };
        let rank = if self.flipped {
            display_rank
        } else {
            7 - display_rank
        };
        Some(board::sq(file, rank))
    }
}

impl Screen {
    /// Ask the terminal how big it is and work out the biggest board that
    /// still leaves room for everything drawn around it.
    fn measure(&mut self) {
        let live = self.theme.live && self.theme.color;
        let (cols, rows) = match (live, ui::terminal_size()) {
            (true, Some(size)) => size,
            _ => (80, 24),
        };
        self.cols = cols.max(30);
        self.rows = rows.max(12);
        let preferred = if live && !self.compact {
            // Drawn pieces are made of Unicode block elements, exactly the kind
            // of character `--ascii` is there to say the terminal has not got.
            let pieces = if self.theme.ascii {
                ui::Pieces::Glyph
            } else {
                self.pieces
            };
            ui::Metrics::fit(self.cols, self.rows, pieces)
        } else {
            ui::Metrics::COMPACT
        };
        self.wide_panel = self
            .cols
            .saturating_sub(preferred.board_width() + self.gap_for(preferred) + 2)
            >= 24;
        self.metrics = if live && !self.compact && !self.wide_panel {
            let pieces = if self.theme.ascii {
                ui::Pieces::Glyph
            } else {
                self.pieces
            };
            ui::Metrics::fit_with_reserve(self.cols, self.rows, pieces, 5)
        } else {
            preferred
        };
        // Centre the board and its panel as one rectangle of a fixed width,
        // rather than on what happens to be written in them, so that nothing
        // slides sideways as the move list fills up.
        self.indent = if live {
            let panel = self.panel_width();
            let block = self.metrics.board_width() + if panel > 0 { self.gap() + panel } else { 0 };
            " ".repeat(self.cols.saturating_sub(block) / 2)
        } else {
            "  ".to_string()
        };
    }

    /// The space between the board and the panel: wider when the squares are.
    fn gap(&self) -> usize {
        self.gap_for(self.metrics)
    }

    fn gap_for(&self, metrics: ui::Metrics) -> usize {
        if metrics.cell_w >= 6 {
            5
        } else {
            3
        }
    }

    /// How much room is left beside the board. Zero means the window is too
    /// narrow to put anything there at all.
    fn panel_width(&self) -> usize {
        if !self.wide_panel {
            return 0;
        }
        let left = self
            .cols
            .saturating_sub(self.metrics.board_width() + self.gap() + 2);
        if left >= 20 {
            left.min(36)
        } else {
            0
        }
    }

    /// Feedback belongs in the frame when we own the screen, and inline when
    /// the output is a transcript.
    fn note(&mut self, text: String) {
        if self.theme.live {
            self.page = None;
            self.message = vec![text];
            self.redraw = true;
        } else {
            println!("  {}", text);
        }
    }

    /// Feedback that only means anything beside a freshly drawn board, so the
    /// board is drawn again whether or not we are holding the screen.
    fn show(&mut self, lines: Vec<String>) {
        self.page = None;
        self.message = lines;
        self.redraw = true;
    }

    /// A page of text. It takes the screen over when there is a screen to take
    /// over, and is simply printed when the session is a transcript.
    fn open(&mut self, title: &str, lines: Vec<String>) {
        if self.theme.live {
            self.page = Some(Page {
                title: title.to_string(),
                lines,
            });
            self.redraw = true;
        } else {
            println!();
            for line in &lines {
                println!("  {}", line);
            }
            println!();
        }
    }

    /// A move has been played: the pointing, the complaints and whatever the
    /// engine last said about the position are all stale.
    fn clear_marks(&mut self) {
        self.message.clear();
        self.selected = None;
        self.targets.clear();
        self.captures.clear();
        self.invalid = None;
        self.promotions.clear();
        self.analysis.clear();
        self.history_offset = 0;
        self.confirming = None;
    }

    fn draw(&mut self, game: &Game, mode: Mode, limits: &Limits) {
        self.measure();
        self.board_hitbox = None;
        self.action_hitboxes.clear();
        let wants_inline_board = self.inline_images
            && self.pieces == ui::Pieces::Auto
            && self.page.is_none()
            && self.metrics.art;
        let rendered = match &self.page {
            Some(page) => RenderedBody {
                lines: self.page_body(page),
                actions: Vec::new(),
                history_capacity: 0,
            },
            None => self.board_body(game, mode, limits),
        };
        let RenderedBody {
            lines: body,
            actions,
            history_capacity,
        } = rendered;
        self.history_capacity = history_capacity;

        if !self.theme.live || !self.theme.color {
            println!();
            for line in &body {
                println!("{}", line);
            }
            println!();
            return;
        }

        let hint = if self.page.is_some() {
            "return goes back"
        } else if outcome(game).is_some() {
            if self.cols < 55 {
                "Tab controls  ·  Enter selects"
            } else {
                "Tab moves focus  ·  Enter selects  ·  type a command"
            }
        } else if game.paused {
            "game paused  ·  choose Resume or type a command"
        } else if self.confirming.is_some() {
            if self.cols < 55 {
                "Enter confirms  ·  esc cancels"
            } else {
                "choose Confirm  ·  Escape cancels"
            }
        } else if self.selected.is_some() {
            if self.cols < 55 {
                "choose target  ·  esc"
            } else {
                "choose a highlighted square  ·  Escape cancels"
            }
        } else if self.cols < 55 {
            "type a move  ·  Tab controls"
        } else {
            "type a move  ·  click a piece  ·  Tab moves focus"
        };
        let mut frame = vec![self.theme.bar("C H E S S", hint, self.cols)];
        if !ui::tight(self.rows) {
            frame.push(String::new());
        }
        // Rows the board could not use are split above and below it, so the
        // board sits in the middle of the window rather than riding up it. A
        // page is read from the top, so it starts at the top.
        let spare = self.rows.saturating_sub(frame.len() + body.len() + 1);
        let above = if self.page.is_some() {
            1.min(spare)
        } else {
            spare / 2
        };
        for _ in 0..above {
            frame.push(String::new());
        }
        let body_start = frame.len();
        self.body_top = body_start;
        if self.page.is_none() {
            self.board_hitbox = Some(BoardHitbox {
                left: self.indent.len() + ui::GUTTER,
                top: body_start + 1,
                cell_w: self.metrics.cell_w,
                cell_h: self.metrics.cell_h,
                flipped: self.flipped,
            });
            self.action_hitboxes = actions
                .into_iter()
                .map(|target| ActionHitbox {
                    left: target.left,
                    top: body_start + target.row,
                    width: target.width,
                    action: target.action,
                })
                .collect();
        }
        frame.extend(body);
        // Pad down the window so the prompt always sits on the bottom row.
        while frame.len() + 1 < self.rows {
            frame.push(String::new());
        }
        frame.truncate(self.rows.saturating_sub(1));

        let inline_key = wants_inline_board.then(|| InlineBoardKey {
            position: game.pos.hash,
            flipped: self.flipped,
            last: game.last_move(),
            check: in_check(&game.pos, game.pos.side)
                .then_some(game.pos.king[game.pos.side.index()]),
            selected: self.selected,
            targets: self.targets.clone(),
            captures: self.captures.clone(),
            invalid: self.invalid,
            promotions: self
                .promotions
                .iter()
                .map(|choice| (choice.square, choice.piece))
                .collect(),
            palette: self.theme.palette,
            metrics: self.metrics,
            left: self.indent.len() + ui::GUTTER,
            top: body_start + 1,
        });
        let image_changed = inline_key != self.last_inline_board;
        let inline_board = (wants_inline_board && image_changed)
            .then(|| self.theme.board_image(&self.board_view(game)));
        let next_frame: Vec<String> = frame.iter().map(|line| ui::clip(line, self.cols)).collect();
        let size_changed = self.last_size != Some((self.cols, self.rows));
        let inline_transition = self.inline_drawn != wants_inline_board;
        let full_redraw = self.last_frame.is_empty() || size_changed || inline_transition;

        // Synchronized output lets supporting terminals present text and the
        // replacement board image as one completed frame. Terminals that do
        // not implement mode 2026 safely ignore it. Ordinary frames are row
        // diffs; a full clear is reserved for geometry and image-mode changes.
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        if self.inline_drawn
            && !ui::inline_images_replace_in_place()
            && (full_redraw || image_changed)
        {
            out.push_str("\x1b_Ga=d,d=A\x1b\\");
        }
        if full_redraw {
            out.push_str("\x1b[2J");
        }
        for row in changed_frame_rows(&self.last_frame, &next_frame, full_redraw) {
            out.push_str(&format!("\x1b[{};1H{}\x1b[K", row + 1, next_frame[row]));
        }
        // The command line is edited outside the frame cache and therefore
        // must be reset on every structural redraw.
        out.push_str(&format!("\x1b[{};1H\x1b[K", self.rows));
        self.last_prompt = None;
        print!("{}", out);
        let _ = io::stdout().flush();

        if let Some(image) = inline_board {
            ui::draw_inline_image(
                &image,
                self.metrics,
                self.indent.len() + ui::GUTTER,
                body_start + 1,
            );
        }
        print!("\x1b[?25l\x1b[?2026l");
        let _ = io::stdout().flush();

        self.last_frame = next_frame;
        self.last_size = Some((self.cols, self.rows));
        self.inline_drawn = wants_inline_board;
        self.last_inline_board = inline_key;
    }

    /// Update only the two player rows when a displayed second changes. This
    /// avoids retransmitting an inline board image once a second.
    fn draw_clock_tick(&self, game: &Game, mode: Mode, limits: &Limits) {
        if !self.theme.live || !self.theme.color || self.page.is_some() {
            return;
        }
        let mut updates: Vec<(usize, usize, usize, Color)> = Vec::new();
        if self.wide_panel {
            let left = self.indent.len() + self.metrics.board_width() + self.gap();
            let width = self.panel_width();
            let top = if self.flipped {
                Color::White
            } else {
                Color::Black
            };
            updates.push((left, self.body_top, width, top));
            updates.push((
                left,
                self.body_top + self.metrics.board_height() - 1,
                width,
                top.flip(),
            ));
        } else {
            let first =
                self.body_top + self.metrics.board_height() + usize::from(!ui::tight(self.rows));
            updates.push((
                self.indent.len(),
                first,
                self.metrics.board_width(),
                Color::White,
            ));
            updates.push((
                self.indent.len(),
                first + 1,
                self.metrics.board_width(),
                Color::Black,
            ));
        }

        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        for (left, row, width, color) in updates {
            let line = self.player_line(game, mode, limits, color, width, !self.wide_panel);
            out.push_str(&format!(
                "\x1b[{};{}H{}\x1b[K",
                row + 1,
                left + 1,
                ui::clip(&line, width)
            ));
        }
        out.push_str(&self.prompt_cursor_escape());
        out.push_str("\x1b[?2026l");
        print!("{}", out);
        let _ = io::stdout().flush();
    }

    fn board_view<'a>(&'a self, game: &'a Game) -> BoardView<'a> {
        let pos = &game.pos;
        BoardView {
            pos,
            flipped: self.flipped,
            last: game.last_move(),
            check: if in_check(pos, pos.side) {
                Some(pos.king[pos.side.index()])
            } else {
                None
            },
            selected: self.selected,
            targets: &self.targets,
            captures: &self.captures,
            invalid: self.invalid,
            promotions: &self.promotions,
        }
    }

    fn square_at(&self, column: u16, row: u16) -> Option<board::Square> {
        self.board_hitbox?.square_at(column, row)
    }

    fn action_at(&self, column: u16, row: u16) -> Option<UiAction> {
        self.action_hitboxes
            .iter()
            .find(|target| target.contains(column, row))
            .map(|target| target.action)
    }

    fn focus(&mut self, action: UiAction) -> bool {
        if self.focused == action {
            return false;
        }
        self.focused = action;
        self.redraw = true;
        true
    }

    fn focus_move_input(&mut self) -> bool {
        self.focus(UiAction::MoveInput)
    }

    /// Traverse only controls that are enabled in the current frame. The
    /// hitboxes already follow visual reading order, so keyboard and mouse
    /// share one source of truth.
    fn move_focus(&mut self, reverse: bool) {
        let controls: Vec<UiAction> = self
            .action_hitboxes
            .iter()
            .map(|target| target.action)
            .collect();
        if controls.is_empty() {
            return;
        }
        let current = controls.iter().position(|&action| action == self.focused);
        let next = match (current, reverse) {
            (Some(0), true) | (None, true) => controls.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % controls.len(),
            (None, false) => 0,
        };
        self.focus(controls[next]);
    }

    fn scroll_history(&mut self, game: &Game, older: bool) {
        if self.history_capacity == 0 {
            return;
        }
        let len = history_lines(game).len();
        let most = len.saturating_sub(self.history_capacity.min(len));
        let step = self.history_capacity.clamp(1, 3);
        let next = if older {
            (self.history_offset + step).min(most)
        } else {
            self.history_offset.saturating_sub(step)
        };
        if next != self.history_offset {
            self.history_offset = next;
            self.redraw = true;
        }
    }

    /// The board, its responsive game information, and the few lines below.
    fn board_body(&self, game: &Game, mode: Mode, limits: &Limits) -> RenderedBody {
        let m = self.metrics;
        let view = self.board_view(game);
        let board = self.theme.board_lines(&view, m);
        let tight = ui::tight(self.rows);
        let mut actions = Vec::new();
        let history_capacity;

        let mut lines = if self.wide_panel {
            let width = self.panel_width();
            let panel = self.panel(game, mode, limits, m.board_height(), width);
            let panel_left = self.indent.len() + m.board_width() + self.gap();
            actions.extend(panel.actions.into_iter().map(|target| RelativeAction {
                left: panel_left + target.left,
                row: target.row,
                width: target.width,
                action: target.action,
            }));
            history_capacity = panel.history_capacity;
            ui::beside(&board, &panel.lines, m.board_width(), self.gap())
                .iter()
                .map(|line| format!("{}{}", self.indent, line))
                .collect::<Vec<_>>()
        } else {
            let mut stacked: Vec<String> = board
                .iter()
                .map(|line| format!("{}{}", self.indent, line))
                .collect();
            let compact = self.compact_panel(game, mode, limits, m.board_width());
            if !tight {
                stacked.push(String::new());
            }
            let compact_start = stacked.len();
            stacked.extend(
                compact
                    .lines
                    .iter()
                    .map(|line| format!("{}{}", self.indent, line)),
            );
            actions.extend(compact.actions.into_iter().map(|target| RelativeAction {
                left: self.indent.len() + target.left,
                row: compact_start + target.row,
                width: target.width,
                action: target.action,
            }));
            history_capacity = compact.history_capacity;
            stacked
        };

        if !tight && self.wide_panel {
            lines.push(String::new());
        }
        lines.push(format!("{}{}", self.indent, self.state_line(game)));
        if !tight {
            lines.push(String::new());
        }

        let room = if self.wide_panel && !tight { 3 } else { 1 };
        let mut notes: Vec<String> = self
            .analysis
            .iter()
            .chain(self.message.iter())
            .take(room)
            .map(|line| format!("{}{}", self.indent, line))
            .collect();
        if self.theme.live {
            while notes.len() < room {
                notes.push(String::new());
            }
        }
        lines.extend(notes);

        RenderedBody {
            lines,
            actions,
            history_capacity,
        }
    }

    fn page_body(&self, page: &Page) -> Vec<String> {
        let theme = &self.theme;
        // An underline for the heading rather than a rule across the page,
        // which at this width would read as a wall.
        let longest = page
            .lines
            .iter()
            .map(|line| ui::width(line))
            .max()
            .unwrap_or(0)
            .clamp(24, 52);
        let mut block = vec![
            theme.strong(theme.palette.accent, &page.title),
            theme.rule(longest.min(self.cols.saturating_sub(8))),
            String::new(),
        ];
        block.extend(page.lines.iter().cloned());
        let widest = block.iter().map(|line| ui::width(line)).max().unwrap_or(0);
        let left = " ".repeat(self.cols.saturating_sub(widest) / 2);
        block
            .iter()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{}{}", left, line)
                }
            })
            .collect()
    }

    /// Player cards, scrollable history, and controls beside the board.
    fn panel(
        &self,
        game: &Game,
        mode: Mode,
        limits: &Limits,
        height: usize,
        width: usize,
    ) -> RenderedBody {
        let mut rows = vec![String::new(); height];
        if height < 6 {
            return RenderedBody {
                lines: rows,
                actions: Vec::new(),
                history_capacity: 0,
            };
        }
        let top = if self.flipped {
            Color::White
        } else {
            Color::Black
        };
        let bottom = top.flip();
        rows[0] = self.player_line(game, mode, limits, top, width, false);
        rows[1] = self.capture_line(game, top);
        rows[height - 2] = self.capture_line(game, bottom);
        rows[height - 1] = self.player_line(game, mode, limits, bottom, width, false);

        if let Some(result) = outcome(game) {
            let title = match result {
                Outcome::Checkmate(_) => "CHECKMATE",
                Outcome::Resignation(_) => "RESIGNED",
                Outcome::Timeout(_) => "TIME",
                Outcome::Abandonment(_) => "ABANDONED",
                _ => "DRAW",
            };
            rows[3] = self.theme.strong(self.theme.palette.accent, "GAME OVER");
            if height > 4 {
                rows[4] = self.theme.bold(title);
            }
            let detail = outcome_detail(&result);
            let score = score_tag(game);
            if height > 5 {
                rows[5] = self.theme.dim(&detail);
            }
            let detail_width = ui::width(&detail);
            let mut result_rows = 1;
            if detail_width + ui::width(score) + 2 <= width {
                rows[5] = format!("{}  {}", rows[5], self.theme.accent(score));
            } else if height > 6 {
                rows[6] = self.theme.accent(score);
                result_rows = 2;
            }
            let buttons = self.render_buttons(&self.game_over_buttons(), width);
            let desired_start = 5 + result_rows + 1;
            let start = desired_start.min(height.saturating_sub(buttons.lines.len() + 2));
            let actions = buttons
                .actions
                .into_iter()
                .map(|target| RelativeAction {
                    row: start + target.row,
                    ..target
                })
                .collect();
            for (offset, line) in buttons.lines.into_iter().enumerate() {
                if start + offset < height.saturating_sub(2) {
                    rows[start + offset] = line;
                }
            }
            return RenderedBody {
                lines: rows.iter().map(|row| ui::clip(row, width)).collect(),
                actions,
                history_capacity: 0,
            };
        }

        let buttons = self.render_buttons(&self.game_buttons(game, mode), width);
        let button_start = height.saturating_sub(buttons.lines.len() + 3);
        let actions = buttons
            .actions
            .into_iter()
            .map(|target| RelativeAction {
                row: button_start + target.row,
                ..target
            })
            .collect();
        for (offset, line) in buttons.lines.into_iter().enumerate() {
            rows[button_start + offset] = line;
        }

        let first = 3;
        let history_capacity = button_start.saturating_sub(first + 1);
        if history_capacity > 0 {
            let played = history_lines(game);
            rows[first] = self.history_heading(played.len(), history_capacity);
            let (start, end) = self.history_bounds(played.len(), history_capacity);
            for (i, line) in played[start..end].iter().enumerate() {
                rows[first + 1 + i] = if self.history_offset == 0 && i + 1 == end - start {
                    self.theme.bold(line)
                } else {
                    self.theme.dim(line)
                };
            }
        }

        RenderedBody {
            lines: rows.iter().map(|row| ui::clip(row, width)).collect(),
            actions,
            history_capacity,
        }
    }

    fn compact_panel(
        &self,
        game: &Game,
        mode: Mode,
        limits: &Limits,
        width: usize,
    ) -> RenderedBody {
        let mut lines = vec![
            self.player_line(game, mode, limits, Color::White, width, true),
            self.player_line(game, mode, limits, Color::Black, width, true),
        ];
        let history_capacity;

        if let Some(result) = outcome(game) {
            lines.push(self.theme.strong(
                self.theme.palette.accent,
                &format!("GAME OVER  {}", describe(&result)),
            ));
            history_capacity = 0;
        } else {
            let played = history_lines(game);
            let (start, end) = self.history_bounds(played.len(), 1);
            let history = played
                .get(start..end)
                .and_then(|slice| slice.first())
                .map(String::as_str)
                .unwrap_or("No moves yet");
            let heading = if played.len() > 1 {
                "MOVES ↑↓"
            } else {
                "MOVES"
            };
            lines.push(format!(
                "{}  {}",
                self.theme.label(heading),
                self.theme.dim(history)
            ));
            history_capacity = 1;
        }

        let buttons = if outcome(game).is_some() {
            self.render_buttons(&self.game_over_buttons(), width)
        } else {
            self.render_buttons(&self.game_buttons(game, mode), width)
        };
        let button_start = lines.len();
        lines.extend(buttons.lines);
        let actions = buttons
            .actions
            .into_iter()
            .map(|target| RelativeAction {
                row: button_start + target.row,
                ..target
            })
            .collect();

        RenderedBody {
            lines: lines
                .into_iter()
                .map(|line| ui::clip(&line, width))
                .collect(),
            actions,
            history_capacity,
        }
    }

    fn player_line(
        &self,
        game: &Game,
        mode: Mode,
        limits: &Limits,
        color: Color,
        width: usize,
        compact: bool,
    ) -> String {
        let theme = &self.theme;
        let to_move = game.pos.side == color && outcome(game).is_none() && !game.paused;
        let marker = match (to_move, theme.ascii) {
            (false, _) => " ",
            (true, true) => ">",
            (true, false) => "\u{25B8}",
        };
        let player = match (mode, color) {
            (Mode::HumanWhite, Color::Black) | (Mode::HumanBlack, Color::White) => "Engine",
            _ if self.player_names[color.index()].is_empty() => "Waiting…",
            _ => &self.player_names[color.index()],
        };
        let name = if to_move {
            theme.bold(player)
        } else {
            theme.label(player)
        };
        let mut role = match player {
            _ if width < 30 => String::new(),
            "Engine" if width >= 34 => {
                format!(
                    "{} · {}",
                    color.name().to_ascii_uppercase(),
                    budget_text(limits)
                )
            }
            _ => color.name().to_ascii_uppercase(),
        };
        if self
            .online
            .as_ref()
            .is_some_and(|online| !online.connected(color) && player != "Waiting…")
        {
            role = if role.is_empty() {
                "OFFLINE".to_string()
            } else {
                format!("{role} · OFFLINE")
            };
        }
        let marker = if to_move {
            theme.accent(marker)
        } else {
            marker.to_string()
        };
        let icon = theme.piece(Piece::new(color, PieceKind::King));
        let mut left = if role.is_empty() {
            format!("{} {} {}", marker, icon, name)
        } else {
            format!("{} {} {}  {}", marker, icon, name, theme.dim(&role))
        };
        if compact {
            let captures = self.capture_summary(game, color);
            if !captures.is_empty() {
                left.push_str("  ");
                left.push_str(&captures);
            }
        }
        let clock = game.clock.format(color);
        let clock = if game.clock.low(color) {
            theme.warn(&clock)
        } else if to_move {
            theme.strong(theme.palette.accent, &clock)
        } else {
            theme.label(&clock)
        };
        let left = ui::clip(&left, width.saturating_sub(ui::width(&clock) + 1));
        let gap = width
            .saturating_sub(ui::width(&left) + ui::width(&clock))
            .max(1);
        format!("{}{}{}", left, " ".repeat(gap), clock)
    }

    fn capture_line(&self, game: &Game, color: Color) -> String {
        let summary = self.capture_summary(game, color);
        if summary.is_empty() {
            return self.theme.dim("  no captures");
        }
        format!("  {}", summary)
    }

    fn capture_summary(&self, game: &Game, color: Color) -> String {
        let taken: String = game
            .captured_by(color)
            .iter()
            .map(|&piece| self.theme.piece(piece))
            .collect();
        let edge = material(&game.pos, color) - material(&game.pos, color.flip());
        let lead = if edge > 0 {
            self.theme.good(&format!(" +{}", edge))
        } else {
            String::new()
        };
        format!("{}{}", taken, lead)
    }

    fn history_bounds(&self, len: usize, capacity: usize) -> (usize, usize) {
        let offset = self
            .history_offset
            .min(len.saturating_sub(capacity.min(len)));
        let end = len.saturating_sub(offset);
        (end.saturating_sub(capacity), end)
    }

    fn history_heading(&self, len: usize, capacity: usize) -> String {
        if len == 0 {
            return self.theme.label("NO MOVES YET");
        }
        let (start, end) = self.history_bounds(len, capacity);
        let range = if len > capacity {
            format!("  {}-{} / {}  ↑↓", start + 1, end, len)
        } else {
            String::new()
        };
        format!("{}{}", self.theme.label("MOVES"), self.theme.dim(&range))
    }

    fn game_buttons(&self, game: &Game, mode: Mode) -> Vec<ButtonSpec> {
        let draw_label = match game.draw_offer {
            Some(color) if color != game.pos.side => "Accept",
            Some(_) => "Offered",
            None => "Draw",
        };
        if let Some(online) = &self.online {
            return vec![
                ButtonSpec {
                    action: UiAction::MoveInput,
                    label: "Move",
                    enabled: online.can_move(game),
                },
                ButtonSpec {
                    action: UiAction::Draw,
                    label: draw_label,
                    enabled: online.connection == ConnectionDisplay::Connected
                        && online.white_connected
                        && online.black_connected
                        && game.draw_offer != online.your_side
                        && outcome(game).is_none(),
                },
                ButtonSpec {
                    action: UiAction::Resign,
                    label: if self.confirming == Some(UiAction::Resign) {
                        "Confirm resign"
                    } else {
                        "Resign"
                    },
                    enabled: online.connection == ConnectionDisplay::Connected
                        && outcome(game).is_none()
                        && online.white_connected
                        && online.black_connected,
                },
                self.size_button(),
                self.pieces_button(),
            ];
        }
        vec![
            ButtonSpec {
                action: UiAction::MoveInput,
                label: "Move",
                enabled: true,
            },
            ButtonSpec {
                action: UiAction::Pause,
                label: if game.paused { "Resume" } else { "Pause" },
                enabled: outcome(game).is_none(),
            },
            ButtonSpec {
                action: UiAction::Undo,
                label: "Undo",
                enabled: !game.sans.is_empty(),
            },
            ButtonSpec {
                action: UiAction::Draw,
                label: draw_label,
                enabled: mode == Mode::TwoPlayer && game.draw_offer != Some(game.pos.side),
            },
            ButtonSpec {
                action: UiAction::Resign,
                label: if self.confirming == Some(UiAction::Resign) {
                    "Confirm resign"
                } else {
                    "Resign"
                },
                enabled: true,
            },
            ButtonSpec {
                action: UiAction::Restart,
                label: if self.confirming == Some(UiAction::Restart) {
                    "Confirm restart"
                } else {
                    "Restart"
                },
                enabled: true,
            },
            self.size_button(),
            self.pieces_button(),
        ]
    }

    fn game_over_buttons(&self) -> Vec<ButtonSpec> {
        if self.online.is_some() {
            return vec![
                ButtonSpec {
                    action: UiAction::Quit,
                    label: "Quit",
                    enabled: true,
                },
                self.size_button(),
                self.pieces_button(),
            ];
        }
        vec![
            ButtonSpec {
                action: UiAction::MoveInput,
                label: "Move",
                enabled: true,
            },
            ButtonSpec {
                action: UiAction::Rematch,
                label: "Rematch",
                enabled: true,
            },
            ButtonSpec {
                action: UiAction::Quit,
                label: "Quit",
                enabled: true,
            },
            self.size_button(),
            self.pieces_button(),
        ]
    }

    fn size_button(&self) -> ButtonSpec {
        ButtonSpec {
            action: UiAction::ToggleSize,
            label: if self.compact {
                "Size:Small"
            } else {
                "Size:Big"
            },
            enabled: true,
        }
    }

    fn pieces_button(&self) -> ButtonSpec {
        ButtonSpec {
            action: UiAction::CyclePieces,
            label: match self.pieces {
                ui::Pieces::Auto => "Piece:Auto",
                ui::Pieces::Art => "Piece:Art",
                ui::Pieces::Glyph => "Piece:Glyph",
            },
            enabled: !self.theme.ascii,
        }
    }

    fn render_buttons(&self, specs: &[ButtonSpec], width: usize) -> RenderedBody {
        let mut lines = vec![String::new()];
        let mut actions = Vec::new();
        let mut row = 0;
        let mut column = 0;
        for spec in specs {
            let plain = format!("[ {} ]", spec.label);
            let button_width = ui::width(&plain);
            let gap = usize::from(column > 0) * 2;
            if column > 0 && column + gap + button_width > width {
                lines.push(String::new());
                row += 1;
                column = 0;
            }
            let gap = usize::from(column > 0) * 2;
            lines[row].push_str(&" ".repeat(gap));
            column += gap;
            let danger = self.confirming == Some(spec.action)
                || matches!(spec.action, UiAction::Resign | UiAction::Restart);
            let styled = if !spec.enabled {
                self.theme.dim(&plain)
            } else if self.focused == spec.action {
                let color = if danger {
                    self.theme.palette.warn
                } else {
                    self.theme.palette.accent
                };
                self.theme.focused(color, &plain)
            } else if danger {
                self.theme.warn(&plain)
            } else {
                self.theme.accent(&plain)
            };
            lines[row].push_str(&styled);
            if spec.enabled {
                actions.push(RelativeAction {
                    left: column,
                    row,
                    width: button_width,
                    action: spec.action,
                });
            }
            column += button_width;
        }
        RenderedBody {
            lines,
            actions,
            history_capacity: 0,
        }
    }

    fn state_line(&self, game: &Game) -> String {
        let theme = &self.theme;
        if let Some(online) = &self.online {
            match online.connection {
                ConnectionDisplay::Connecting => {
                    return format!(
                        "{}  {}",
                        theme.strong(theme.palette.accent, "CONNECTING"),
                        theme.dim("opening a secure game connection")
                    );
                }
                ConnectionDisplay::Reconnecting => {
                    return format!(
                        "{}  {}",
                        theme.strong(theme.palette.warn, "RECONNECTING"),
                        theme.dim("your seat is reserved; moves are paused here")
                    );
                }
                ConnectionDisplay::Stopped => {
                    return format!(
                        "{}  {}",
                        theme.strong(theme.palette.warn, "OFFLINE"),
                        theme.dim("type quit, then run online resume to return")
                    );
                }
                ConnectionDisplay::Connected => {}
            }
            if online.reconnect_deadline_ms.is_some() {
                return format!(
                    "{}  {}",
                    theme.strong(theme.palette.warn, "OPPONENT OFFLINE"),
                    theme.dim("their seat is held for 60 seconds")
                );
            }
            if let Some(code) = &online.invite_code {
                if !online.black_connected {
                    return format!(
                        "{}  {}",
                        theme.strong(theme.palette.accent, &format!("INVITE {code}")),
                        theme.dim("share this code; waiting for your opponent")
                    );
                }
            }
        }
        if let Some(result) = outcome(game) {
            return format!(
                "{}  {}",
                theme.strong(theme.palette.accent, &describe(&result)),
                theme.dim("choose Rematch or Quit")
            );
        }
        if game.paused {
            return format!(
                "{}  {}",
                theme.strong(theme.palette.accent, "PAUSED"),
                theme.dim("clocks and moves are stopped")
            );
        }
        let separator = theme.dim("  \u{b7}  ");
        let mut parts = vec![
            self.online
                .as_ref()
                .map(|_| theme.accent("online"))
                .unwrap_or_default(),
            theme.dim(&format!("move {}", game.pos.fullmove)),
            format!("{} to move", theme.bold(game.pos.side.name())),
        ];
        parts.retain(|part| !part.is_empty());
        if in_check(&game.pos, game.pos.side) {
            parts.push(theme.warn("check!"));
        }
        if let Some(color) = game.draw_offer {
            parts.push(if color == game.pos.side {
                theme.dim("draw offered")
            } else {
                theme.accent("draw offer")
            });
        }
        parts.join(&separator)
    }

    fn prompt(&self, game: &Game) -> String {
        let arrow = if self.theme.ascii { ">" } else { "\u{203a}" };
        let label = if outcome(game).is_some() {
            "Game over"
        } else if self
            .online
            .as_ref()
            .is_some_and(|online| !online.can_move(game))
        {
            "Online"
        } else {
            game.pos.side.name()
        };
        format!(
            "{}{} {} ",
            self.indent,
            self.theme.bold(label),
            self.theme.dim(arrow)
        )
    }

    /// Repaint the bottom-row command line without redrawing the board.
    fn draw_prompt(&mut self, game: &Game, input: &str) {
        let line = format!("{}{}", self.prompt(game), input);
        if self.last_prompt.as_ref() == Some(&line) {
            return;
        }
        self.last_prompt = Some(line.clone());
        print!(
            "\x1b[{};1H\x1b[K{}{}",
            self.rows,
            ui::clip(&line, self.cols),
            self.prompt_cursor_escape()
        );
        let _ = io::stdout().flush();
    }

    /// Return the cursor to the end of the command line after a partial
    /// repaint. Clock updates touch rows above the prompt, so leaving their
    /// final cursor position at column one makes the caret appear to jump
    /// away from text that is still correctly cached on the bottom row.
    fn prompt_cursor_escape(&self) -> String {
        if self.focused != UiAction::MoveInput {
            return "\x1b[?25l".to_string();
        }
        let column = self
            .last_prompt
            .as_deref()
            .map(ui::width)
            .unwrap_or(0)
            .saturating_add(1)
            .clamp(1, self.cols);
        format!("\x1b[{};{}H\x1b[?25h", self.rows, column)
    }
}

fn changed_frame_rows(previous: &[String], next: &[String], force_all: bool) -> Vec<usize> {
    next.iter()
        .enumerate()
        .filter_map(|(row, line)| (force_all || previous.get(row) != Some(line)).then_some(row))
        .collect()
}

/// `1. e4 e5` lines, one per move pair, from whatever side started.
fn history_lines(game: &Game) -> Vec<String> {
    let mut lines = Vec::new();
    let mut number = game.start.fullmove;
    let mut side = game.start.side;
    let mut line = String::new();
    for text in &game.sans {
        if side == Color::White {
            line = format!("{:>3}. {:<7}", number, text);
        } else {
            if line.is_empty() {
                line = format!("{:>3}. {:<7}", number, "...");
            }
            line.push_str(text);
            number += 1;
            lines.push(line.trim_end().to_string());
            line.clear();
        }
        side = side.flip();
    }
    if !line.is_empty() {
        lines.push(line.trim_end().to_string());
    }
    lines
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// How long the engine gets, in the words used by the flags that set it.
fn budget_text(limits: &Limits) -> String {
    match limits.movetime {
        Some(budget) => {
            let secs = budget.as_secs_f64();
            if secs >= 1.0 {
                format!("{}s a move", trim_number(secs))
            } else {
                format!("{}ms a move", (secs * 1000.0).round() as u64)
            }
        }
        None => format!("depth {}", limits.depth),
    }
}

fn trim_number(value: f64) -> String {
    let text = format!("{:.1}", value);
    text.trim_end_matches(".0").to_string()
}

/// Centipawns as pawns, or the distance to mate, always from White's side of
/// the board - the one point of view that does not move around during a game.
fn format_score(white_pov: i32) -> String {
    if white_pov.abs() >= search::MATE_THRESHOLD {
        let plies = search::MATE - white_pov.abs();
        let moves = (plies + 1) / 2;
        let winner = if white_pov > 0 { "White" } else { "Black" };
        format!("{} mates in {}", winner, moves.max(1))
    } else {
        format!("{:+.2}", white_pov as f64 / 100.0)
    }
}

/// The engine scores for the side it is moving for; the board is White's.
fn white_pov(pos: &Position, score: i32) -> i32 {
    if pos.side == Color::White {
        score
    } else {
        -score
    }
}

fn node_count(nodes: u64) -> String {
    match nodes {
        0..=9_999 => nodes.to_string(),
        10_000..=999_999 => format!("{:.0}k", nodes as f64 / 1_000.0),
        _ => format!("{:.2}M", nodes as f64 / 1_000_000.0),
    }
}

/// Render a line of play as SAN, stopping if it runs past what is legal.
fn pv_text(pos: &Position, pv: &[Move], most: usize) -> String {
    let mut work = pos.clone();
    let mut parts = Vec::new();
    let mut undos = Vec::new();
    for &mv in pv.iter().take(most) {
        // A truncated line can outlive its position; stop rather than guess.
        if !generate_legal(&work).contains(&mv) {
            break;
        }
        parts.push(to_san(&work, mv));
        undos.push(work.make_move(mv));
    }
    while let Some(undo) = undos.pop() {
        work.unmake_move(undo);
    }
    parts.join(" ")
}

/// Fold a long list of words into indented lines.
fn wrap(words: &[String], width: usize, indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in words {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(format!("{}{}", indent, line));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(format!("{}{}", indent, line));
    }
    lines
}

// ---------------------------------------------------------------------------
// The game loop
// ---------------------------------------------------------------------------

struct OnlineSession {
    client: OnlineClient,
    intent: OnlineIntent,
    server_url: String,
    seat_path: PathBuf,
    game_id: Option<String>,
    reconnect_token: Option<String>,
    side: Option<Color>,
    player_name: String,
    has_snapshot: bool,
}

impl OnlineSession {
    fn send(&self, command: ClientCommand, screen: &mut Screen) -> bool {
        match self.client.send(command) {
            Ok(()) => true,
            Err(error) => {
                if let Some(online) = &mut screen.online {
                    online.connection = ConnectionDisplay::Stopped;
                    online.move_pending = false;
                }
                screen.note(screen.theme.warn(&error));
                false
            }
        }
    }

    fn reconnect_command(&self) -> Option<ClientCommand> {
        Some(ClientCommand::Reconnect {
            game_id: self.game_id.clone()?,
            reconnect_token: self.reconnect_token.clone()?,
        })
    }

    fn save_seat(&self) -> Result<(), String> {
        let side = self
            .side
            .ok_or_else(|| "online seat has no assigned side".to_string())?;
        let game_id = self
            .game_id
            .clone()
            .ok_or_else(|| "online seat has no game id".to_string())?;
        let token = self
            .reconnect_token
            .clone()
            .ok_or_else(|| "online seat has no reconnect token".to_string())?;
        storage::save_online_seat(
            &self.seat_path,
            &storage::SavedOnlineSeat::new(
                self.server_url.clone(),
                game_id,
                token,
                side.name().to_ascii_lowercase(),
            ),
        )
    }
}

fn play_online(mut options: Options, loaded: storage::LoadedPreferences) -> Result<(), String> {
    let config_path = loaded.path;
    let seat_path = storage::default_online_session_path(&config_path);
    let intent = options
        .online
        .take()
        .expect("online mode checked by caller");
    let restored_seat = if intent == OnlineIntent::Resume {
        Some(
            storage::load_online_seat(&seat_path)
                .map_err(|error| format!("could not resume the last online game: {error}"))?,
        )
    } else {
        None
    };
    if let Some(seat) = &restored_seat {
        options.server_url = seat.server_url.clone();
    }

    let initial_side = restored_seat
        .as_ref()
        .and_then(|seat| color_named(&seat.side))
        .or(match intent {
            OnlineIntent::Create => Some(Color::White),
            OnlineIntent::Join(_) => Some(Color::Black),
            OnlineIntent::Resume => None,
        });
    let color = options.color.unwrap_or_else(Theme::detect_color);
    let mut player_names = [String::new(), String::new()];
    if let Some(side) = initial_side {
        player_names[side.index()] = options.online_name.clone();
    }
    let mut screen = Screen {
        theme: Theme::new(
            color,
            options.ascii,
            color && Theme::detect_live(),
            options.palette,
        ),
        sound: sound::Player::new(options.sound),
        inline_images: false,
        flipped: options.flipped || initial_side == Some(Color::Black),
        cols: 80,
        rows: 24,
        metrics: ui::Metrics::COMPACT,
        wide_panel: false,
        pieces: options.pieces,
        player_names,
        compact: options.compact,
        indent: "  ".to_string(),
        board_hitbox: None,
        body_top: 0,
        action_hitboxes: Vec::new(),
        focused: UiAction::MoveInput,
        history_offset: 0,
        history_capacity: 0,
        confirming: None,
        analysis: Vec::new(),
        message: vec![Theme::new(color, options.ascii, color, options.palette)
            .dim("Connecting to the game server…")],
        selected: None,
        targets: Vec::new(),
        captures: Vec::new(),
        invalid: None,
        promotions: Vec::new(),
        page: None,
        last_frame: Vec::new(),
        last_size: None,
        inline_drawn: false,
        last_inline_board: None,
        last_prompt: None,
        redraw: true,
        online: Some(OnlineDisplay {
            connection: ConnectionDisplay::Connecting,
            invite_code: None,
            your_side: initial_side,
            white_connected: false,
            black_connected: false,
            reconnect_deadline_ms: None,
            move_pending: true,
        }),
    };
    let _fullscreen = ui::Fullscreen::enter(&screen.theme);
    if screen.theme.live && !screen.theme.ascii {
        screen.inline_images = ui::detect_inline_images();
    }

    let mut game = Game::with_clock(Position::startpos(), options.clock, options.increment);
    game.clock.pause();
    let client = OnlineClient::connect(options.server_url.clone());
    let mut session = OnlineSession {
        client,
        intent,
        server_url: options.server_url,
        seat_path,
        game_id: restored_seat.as_ref().map(|seat| seat.game_id.clone()),
        reconnect_token: restored_seat
            .as_ref()
            .map(|seat| seat.reconnect_token.clone()),
        side: initial_side,
        player_name: options.online_name,
        has_snapshot: false,
    };
    let mut terminal_input = TerminalInput::enter(screen.theme.live && screen.theme.color)?;
    let limits = Limits::default();
    let mut last_preferences = loaded.preferences;
    let mut persistence_error_reported = false;

    loop {
        while let Some(event) = session.client.try_recv() {
            handle_transport_event(event, &mut session, &mut game, &mut screen)?;
        }

        let preferences = online_preferences(&last_preferences, &screen, &session, &game);
        if preferences != last_preferences {
            if let Err(error) = storage::save_preferences(&config_path, &preferences) {
                if !persistence_error_reported {
                    screen.note(
                        screen
                            .theme
                            .warn(&format!("Preferences were not saved: {error}")),
                    );
                    persistence_error_reported = true;
                }
            } else {
                last_preferences = preferences;
            }
        }

        if game.clock.tick() {
            screen.draw_clock_tick(&game, Mode::TwoPlayer, &limits);
        }
        if screen.redraw {
            screen.redraw = false;
            screen.draw(&game, Mode::TwoPlayer, &limits);
        }

        let action = if terminal_input.is_active() {
            screen.draw_prompt(&game, terminal_input.buffer());
            terminal_input.read_for(Duration::from_millis(100))?
        } else {
            let mut stdin = io::stdin().lock();
            match read_line(&mut stdin, &screen.prompt(&game))? {
                Some(line) => InputAction::Submit(line),
                None => InputAction::Quit,
            }
        };

        let line = match action {
            InputAction::Submit(line) => {
                if line.is_empty() && screen.focused != UiAction::MoveInput {
                    if handle_online_action(screen.focused, &game, &session, &mut screen) {
                        return Ok(());
                    }
                    continue;
                }
                line
            }
            InputAction::Prompt => {
                let cleared_invalid = screen.invalid.take().is_some();
                if screen.focus_move_input() || cleared_invalid {
                    screen.redraw = true;
                } else {
                    screen.draw_prompt(&game, terminal_input.buffer());
                }
                continue;
            }
            InputAction::Focus { reverse } => {
                screen.move_focus(reverse);
                continue;
            }
            InputAction::Resize => {
                screen.redraw = true;
                continue;
            }
            InputAction::Tick => continue,
            InputAction::History { older } => {
                screen.scroll_history(&game, older);
                continue;
            }
            InputAction::Cancel => {
                let changed = screen.focus_move_input()
                    || screen.page.take().is_some()
                    || screen.selected.take().is_some()
                    || screen.confirming.take().is_some()
                    || !screen.promotions.is_empty();
                screen.targets.clear();
                screen.captures.clear();
                screen.promotions.clear();
                if changed {
                    screen.redraw = true;
                }
                continue;
            }
            InputAction::Click { column, row } => {
                if screen.page.take().is_some() {
                    screen.redraw = true;
                    continue;
                }
                if let Some(action) = screen.action_at(column, row) {
                    screen.focus(action);
                    if handle_online_action(action, &game, &session, &mut screen) {
                        return Ok(());
                    }
                    continue;
                }
                screen.confirming = None;
                let square = screen.square_at(column, row);
                if online_can_move(&screen, &game) {
                    if let Some(movement) = online_board_click(&game, &mut screen, square) {
                        send_online_move(&session, &game, movement, &mut screen);
                    }
                } else {
                    explain_online_wait(&game, &mut screen);
                }
                continue;
            }
            InputAction::Quit => return Ok(()),
        };

        let input = line.trim();
        if screen.page.take().is_some() {
            screen.redraw = true;
        }
        if input.is_empty() {
            screen.redraw = screen.theme.live;
            continue;
        }
        if !screen.promotions.is_empty() && input.len() == 1 {
            let kind = input.chars().next().and_then(PieceKind::from_char);
            if let Some(
                kind @ (PieceKind::Queen | PieceKind::Rook | PieceKind::Bishop | PieceKind::Knight),
            ) = kind
            {
                if let Some(movement) = screen
                    .promotions
                    .iter()
                    .find(|choice| choice.piece.kind == kind)
                    .map(|choice| choice.movement)
                {
                    screen.clear_marks();
                    send_online_move(&session, &game, movement, &mut screen);
                }
                continue;
            }
        }

        let (word, rest) = split_command(input);
        match word.as_str() {
            "quit" | "exit" | "q" => return Ok(()),
            "help" | "h" | "?" => {
                screen.open("ONLINE GAME", online_help_lines(&screen.theme));
                continue;
            }
            "history" | "moveslist" => {
                screen.open("THE GAME SO FAR", history_page(&screen.theme, &game));
                continue;
            }
            "pgn" => {
                screen.open("PGN", pgn_lines(&game, &screen.player_names));
                continue;
            }
            "export" => {
                let path = if rest.is_empty() {
                    PathBuf::from("game.pgn")
                } else {
                    PathBuf::from(rest)
                };
                match export_pgn(&path, &game, &screen.player_names) {
                    Ok(()) => screen.note(
                        screen
                            .theme
                            .good(&format!("Exported PGN to {}.", path.display())),
                    ),
                    Err(error) => screen.note(screen.theme.warn(&error)),
                }
                continue;
            }
            "fen" => {
                screen.note(screen.theme.accent(&game.pos.to_fen()));
                continue;
            }
            "flip" => {
                screen.flipped = !screen.flipped;
                screen.redraw = true;
                continue;
            }
            "theme" => {
                set_theme(&mut screen, rest);
                continue;
            }
            "pieces" => {
                set_pieces(&mut screen, rest);
                continue;
            }
            "sound" | "sounds" => {
                set_sound(&mut screen, rest);
                continue;
            }
            "size" => {
                set_size(&mut screen, rest);
                continue;
            }
            "draw" => {
                if rest.eq_ignore_ascii_case("decline") {
                    respond_to_draw(&session, &game, false, &mut screen);
                } else {
                    offer_or_accept_draw(&session, &game, &mut screen);
                }
                continue;
            }
            "resign" => {
                send_resignation(&session, &game, &mut screen);
                continue;
            }
            _ => {}
        }

        if !online_can_move(&screen, &game) {
            explain_online_wait(&game, &mut screen);
            continue;
        }
        request_online_move(&session, &game, input, &mut screen);
    }
}

fn handle_transport_event(
    event: TransportEvent,
    session: &mut OnlineSession,
    game: &mut Game,
    screen: &mut Screen,
) -> Result<(), String> {
    match event {
        TransportEvent::Connecting { attempt } => {
            if let Some(online) = &mut screen.online {
                online.connection = if attempt == 1 && !session.has_snapshot {
                    ConnectionDisplay::Connecting
                } else {
                    ConnectionDisplay::Reconnecting
                };
                online.move_pending = true;
            }
            screen.redraw = true;
        }
        TransportEvent::Connected => {
            if let Some(online) = &mut screen.online {
                online.connection = ConnectionDisplay::Connected;
                online.move_pending = true;
            }
            session.send(
                ClientCommand::Hello {
                    client_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                screen,
            );
            let command = session
                .reconnect_command()
                .unwrap_or_else(|| match &session.intent {
                    OnlineIntent::Create => ClientCommand::CreateGame {
                        player_name: session.player_name.clone(),
                        time_control: TimeControl {
                            initial_ms: game
                                .clock
                                .initial
                                .map(|time| time.as_millis().min(u64::MAX as u128) as u64)
                                .unwrap_or(0),
                            increment_ms: game.clock.increment.as_millis().min(u64::MAX as u128)
                                as u64,
                        },
                    },
                    OnlineIntent::Join(code) => ClientCommand::JoinGame {
                        invite_code: code.clone(),
                        player_name: session.player_name.clone(),
                    },
                    OnlineIntent::Resume => unreachable!("resume has reconnect credentials"),
                });
            session.send(command, screen);
            screen.redraw = true;
        }
        TransportEvent::Disconnected { reason, retry_in } => {
            if let Some(online) = &mut screen.online {
                online.connection = ConnectionDisplay::Reconnecting;
                online.move_pending = true;
            }
            screen.show(vec![
                screen
                    .theme
                    .warn("Connection lost — reconnecting automatically."),
                screen.theme.dim(&format!(
                    "Retrying in {:.1}s · {reason}",
                    retry_in.as_secs_f32()
                )),
            ]);
        }
        TransportEvent::Stopped(reason) => {
            if let Some(online) = &mut screen.online {
                online.connection = ConnectionDisplay::Stopped;
                online.move_pending = false;
            }
            screen.note(screen.theme.warn(&reason));
        }
        TransportEvent::Message(envelope) => match envelope.event {
            ServerEvent::Welcome { .. } | ServerEvent::Pong => {}
            ServerEvent::GameCreated {
                invite_code,
                reconnect_token,
                game: snapshot,
            } => {
                session.game_id = Some(snapshot.game_id.clone());
                session.reconnect_token = Some(reconnect_token);
                session.side = Some(Color::White);
                if let Some(online) = &mut screen.online {
                    online.invite_code = Some(invite_code);
                    online.your_side = session.side;
                }
                session.save_seat()?;
                apply_online_snapshot(snapshot, session, game, screen)?;
            }
            ServerEvent::GameJoined {
                reconnect_token,
                game: snapshot,
            } => {
                session.game_id = Some(snapshot.game_id.clone());
                session.reconnect_token = Some(reconnect_token);
                if session.side.is_none() {
                    session.side = Some(Color::Black);
                }
                if let Some(online) = &mut screen.online {
                    online.your_side = session.side;
                }
                session.save_seat()?;
                apply_online_snapshot(snapshot, session, game, screen)?;
                screen.note(screen.theme.good("Connected to the game."));
            }
            ServerEvent::GameUpdated { game: snapshot } => {
                apply_online_snapshot(snapshot, session, game, screen)?;
            }
            ServerEvent::MoveRejected {
                reason,
                game: snapshot,
                ..
            } => {
                apply_online_snapshot(snapshot, session, game, screen)?;
                let reason = match reason {
                    MoveRejection::NotYourTurn => "It is not your turn.",
                    MoveRejection::IllegalMove => "That move is not legal.",
                    MoveRejection::StalePosition => {
                        "The position changed before that move arrived. Try again."
                    }
                    MoveRejection::GameNotActive => "The game is not active.",
                };
                screen.note(screen.theme.warn(reason));
            }
            ServerEvent::OpponentDisconnected {
                reconnect_deadline_ms,
                ..
            } => {
                if let Some(online) = &mut screen.online {
                    online.reconnect_deadline_ms = Some(reconnect_deadline_ms);
                    let opponent = session.side.map(Color::flip);
                    if opponent == Some(Color::White) {
                        online.white_connected = false;
                    } else if opponent == Some(Color::Black) {
                        online.black_connected = false;
                    }
                }
                screen.note(
                    screen
                        .theme
                        .warn("Your opponent disconnected. Their seat is held for 60 seconds."),
                );
            }
            ServerEvent::OpponentReconnected { .. } => {
                if let Some(online) = &mut screen.online {
                    online.reconnect_deadline_ms = None;
                    if session.side == Some(Color::White) {
                        online.black_connected = true;
                    } else {
                        online.white_connected = true;
                    }
                }
                screen.note(screen.theme.good("Your opponent reconnected."));
            }
            ServerEvent::Error { code, message } => {
                if matches!(
                    code,
                    ErrorCode::UnsupportedProtocol
                        | ErrorCode::GameNotFound
                        | ErrorCode::InvalidReconnectToken
                ) && session.reconnect_token.is_some()
                {
                    if let Some(online) = &mut screen.online {
                        online.connection = ConnectionDisplay::Stopped;
                        online.move_pending = false;
                    }
                }
                screen.note(screen.theme.warn(&message));
            }
        },
    }
    Ok(())
}

fn apply_online_snapshot(
    snapshot: GameSnapshot,
    session: &mut OnlineSession,
    game: &mut Game,
    screen: &mut Screen,
) -> Result<(), String> {
    let previous_ply = game.sans.len();
    let previous_finished = outcome(game).is_some();
    let mut updated = Game::with_clock(
        Position::startpos(),
        (snapshot.time_control.initial_ms > 0)
            .then(|| Duration::from_millis(snapshot.time_control.initial_ms)),
        Duration::from_millis(snapshot.time_control.increment_ms),
    );
    for (index, notation) in snapshot.moves.iter().enumerate() {
        let movement = parse_move(&updated.pos, notation).map_err(|_| {
            format!(
                "server snapshot contains an illegal move {} (`{notation}`)",
                index + 1
            )
        })?;
        updated.play(movement);
    }
    if updated.pos.to_fen() != snapshot.fen {
        return Err("server snapshot position does not match its move history".to_string());
    }
    let mut remaining = [snapshot.clock.white_ms, snapshot.clock.black_ms];
    if let Some(running) = snapshot.clock.running {
        let elapsed = unix_time_ms().saturating_sub(snapshot.clock.server_time_ms);
        let index = side_color(running).index();
        remaining[index] = remaining[index].saturating_sub(elapsed);
    }
    let active = matches!(snapshot.status, GameStatus::Active);
    let clock_side = snapshot
        .clock
        .running
        .map(side_color)
        .unwrap_or(updated.pos.side);
    updated.clock.restore(
        remaining,
        clock_side,
        active && snapshot.clock.running.is_some(),
    );
    updated.draw_offer = snapshot.draw_offer.map(side_color);
    apply_finished_status(&snapshot.status, &mut updated);
    updated.revision = snapshot.revision;

    screen.player_names = [snapshot.white.name.clone(), snapshot.black.name.clone()];
    if let Some(online) = &mut screen.online {
        online.connection = ConnectionDisplay::Connected;
        online.your_side = session.side;
        online.white_connected = snapshot.white.connected;
        online.black_connected = snapshot.black.connected;
        online.reconnect_deadline_ms = None;
        online.move_pending = false;
    }
    let play_move_sound = session.has_snapshot && updated.sans.len() > previous_ply;
    let play_end_sound = session.has_snapshot
        && !previous_finished
        && outcome(&updated).is_some()
        && !play_move_sound;
    *game = updated;
    session.has_snapshot = true;
    screen.clear_marks();
    if play_move_sound {
        screen.sound.play(sound_after_move(game));
    } else if play_end_sound {
        screen.sound.play(sound::Cue::GameEnd);
    }
    screen.redraw = true;
    Ok(())
}

fn apply_finished_status(status: &GameStatus, game: &mut Game) {
    let GameStatus::Finished { result, reason } = status else {
        return;
    };
    let loser = match result {
        GameResult::WhiteWins => Some(Color::Black),
        GameResult::BlackWins => Some(Color::White),
        GameResult::Draw => None,
    };
    match reason {
        FinishReason::Resignation => game.resigned = loser,
        FinishReason::Timeout => game.clock.flagged = loser,
        FinishReason::Abandonment => game.abandoned = loser,
        FinishReason::DrawAgreement => game.agreed_draw = true,
        FinishReason::Checkmate
        | FinishReason::Stalemate
        | FinishReason::FiftyMove
        | FinishReason::Threefold
        | FinishReason::InsufficientMaterial => {}
    }
    game.clock.pause();
}

fn handle_online_action(
    action: UiAction,
    game: &Game,
    session: &OnlineSession,
    screen: &mut Screen,
) -> bool {
    match action {
        UiAction::MoveInput => {
            screen.focus_move_input();
        }
        UiAction::Draw => offer_or_accept_draw(session, game, screen),
        UiAction::Resign => {
            if screen.confirming == Some(UiAction::Resign) {
                send_resignation(session, game, screen);
            } else {
                screen.confirming = Some(UiAction::Resign);
                screen.note(
                    screen
                        .theme
                        .warn("Choose Confirm to resign, or press Escape."),
                );
            }
        }
        UiAction::ToggleSize => toggle_size(screen),
        UiAction::CyclePieces => cycle_pieces(screen),
        UiAction::Quit => return true,
        UiAction::Pause | UiAction::Undo | UiAction::Restart | UiAction::Rematch => {}
    }
    false
}

fn offer_or_accept_draw(session: &OnlineSession, game: &Game, screen: &mut Screen) {
    let Some(game_id) = session.game_id.clone() else {
        explain_online_wait(game, screen);
        return;
    };
    let Some(side) = session.side else {
        explain_online_wait(game, screen);
        return;
    };
    let command = if game.draw_offer == Some(side.flip()) {
        ClientCommand::RespondDraw {
            game_id,
            accept: true,
        }
    } else if game.draw_offer == Some(side) {
        screen.note(
            screen
                .theme
                .dim("Your draw offer is waiting for your opponent."),
        );
        return;
    } else {
        ClientCommand::OfferDraw { game_id }
    };
    session.send(command, screen);
}

fn respond_to_draw(session: &OnlineSession, game: &Game, accept: bool, screen: &mut Screen) {
    let Some(side) = session.side else {
        explain_online_wait(game, screen);
        return;
    };
    if game.draw_offer != Some(side.flip()) {
        screen.note(screen.theme.dim("There is no draw offer to answer."));
        return;
    }
    if let Some(game_id) = session.game_id.clone() {
        session.send(ClientCommand::RespondDraw { game_id, accept }, screen);
    }
}

fn send_resignation(session: &OnlineSession, game: &Game, screen: &mut Screen) {
    if outcome(game).is_some() {
        screen.note(screen.theme.dim("The game is already over."));
        return;
    }
    if let Some(game_id) = session.game_id.clone() {
        screen.confirming = None;
        session.send(ClientCommand::Resign { game_id }, screen);
    } else {
        explain_online_wait(game, screen);
    }
}

fn online_can_move(screen: &Screen, game: &Game) -> bool {
    screen
        .online
        .as_ref()
        .is_some_and(|online| online.can_move(game))
}

fn explain_online_wait(game: &Game, screen: &mut Screen) {
    let message = if outcome(game).is_some() {
        "The game is over. Export the PGN or quit when you are ready."
    } else if screen
        .online
        .as_ref()
        .is_some_and(|online| online.connection != ConnectionDisplay::Connected)
    {
        "Reconnecting — your seat is reserved and moves will resume automatically."
    } else if screen
        .online
        .as_ref()
        .is_some_and(|online| !online.white_connected || !online.black_connected)
    {
        "Waiting for both players to be connected."
    } else {
        "It is your opponent's turn."
    };
    screen.note(screen.theme.dim(message));
}

fn request_online_move(session: &OnlineSession, game: &Game, input: &str, screen: &mut Screen) {
    match parse_move(&game.pos, input) {
        Ok(movement) => send_online_move(session, game, movement, screen),
        Err(ParseError::Illegal(text)) => {
            if let Some(movement) = promotion_default(&game.pos, input) {
                send_online_move(session, game, movement, screen);
                return;
            }
            let near = nearby_moves(&game.pos, input);
            let hint = if near.is_empty() {
                "type `moves` to list legal moves".to_string()
            } else {
                format!("did you mean {}?", near.join(" or "))
            };
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("`{text}` is not a legal move")),
                screen.theme.dim(&format!("— {hint}"))
            ));
        }
        Err(ParseError::Ambiguous(text, candidates)) => screen.note(format!(
            "{} {}",
            screen.theme.warn(&format!("`{text}` could mean")),
            screen.theme.bold(&candidates.join(" or "))
        )),
    }
}

fn send_online_move(session: &OnlineSession, game: &Game, movement: Move, screen: &mut Screen) {
    let Some(game_id) = session.game_id.clone() else {
        explain_online_wait(game, screen);
        return;
    };
    if let Some(online) = &mut screen.online {
        online.move_pending = true;
    }
    screen.clear_marks();
    screen.redraw = true;
    session.send(
        ClientCommand::PlayMove {
            game_id,
            expected_ply: game.sans.len() as u32,
            uci: movement.to_uci(),
        },
        screen,
    );
}

fn online_board_click(
    game: &Game,
    screen: &mut Screen,
    square: Option<board::Square>,
) -> Option<Move> {
    let square = match square {
        Some(square) => square,
        None => {
            if screen.selected.take().is_some() {
                screen.targets.clear();
                screen.captures.clear();
                screen.promotions.clear();
                screen.invalid = None;
                screen.redraw = true;
            }
            return None;
        }
    };
    screen.invalid = None;
    if let Some(choice) = screen
        .promotions
        .iter()
        .find(|choice| choice.square == square)
        .copied()
    {
        return Some(choice.movement);
    }
    screen.promotions.clear();

    let legal = generate_legal(&game.pos);
    if let Some(from) = screen.selected {
        if square == from {
            screen.selected = None;
            screen.targets.clear();
            screen.captures.clear();
            screen.redraw = true;
            return None;
        }
        let choices: Vec<Move> = legal
            .iter()
            .copied()
            .filter(|movement| movement.from == from && movement.to == square)
            .collect();
        if let Some(&movement) = choices.first() {
            if choices.iter().any(|movement| movement.promo.is_some()) {
                open_promotion_menu(game, screen, &choices);
                return None;
            }
            return Some(movement);
        }
    }

    match game.pos.at(square) {
        Some(piece) if piece.color == game.pos.side => {
            screen.selected = Some(square);
            let moves: Vec<Move> = legal
                .iter()
                .filter(|movement| movement.from == square)
                .copied()
                .collect();
            screen.targets = moves.iter().map(|movement| movement.to).collect();
            screen.captures = moves
                .iter()
                .filter(|movement| {
                    movement.kind == MoveKind::EnPassant || game.pos.at(movement.to).is_some()
                })
                .map(|movement| movement.to)
                .collect();
            screen.message = if moves.is_empty() {
                vec![screen.theme.dim(&format!(
                    "The {} on {} has no legal moves.",
                    kind_name(piece.kind),
                    board::square_name(square)
                ))]
            } else {
                Vec::new()
            };
            screen.redraw = true;
        }
        _ if screen.selected.is_none() => {
            screen.invalid = Some(square);
            screen.message = vec![screen
                .theme
                .dim(&format!("Choose a {} piece.", game.pos.side.name()))];
            screen.redraw = true;
        }
        _ => {
            screen.invalid = Some(square);
            screen.message = vec![screen.theme.warn(&format!(
                "{} is not a legal destination for the selected piece.",
                board::square_name(square)
            ))];
            screen.redraw = true;
        }
    }
    None
}

fn online_preferences(
    previous: &storage::Preferences,
    screen: &Screen,
    session: &OnlineSession,
    game: &Game,
) -> storage::Preferences {
    let mut preferences = runtime_preferences(screen, game);
    preferences.white_name = session.player_name.clone();
    preferences.black_name = previous.black_name.clone();
    preferences
}

fn online_help_lines(theme: &Theme) -> Vec<String> {
    vec![
        theme.bold("PLAY"),
        "  Click a piece and a highlighted square, or type e4 / Nf3 / e2e4.".to_string(),
        "  The server validates every move and owns both clocks.".to_string(),
        String::new(),
        theme.bold("GAME"),
        "  draw             offer or accept a draw".to_string(),
        "  draw decline     decline the current draw offer".to_string(),
        "  resign           resign the game".to_string(),
        "  history · pgn · export [FILE] · fen".to_string(),
        String::new(),
        theme.bold("VIEW"),
        "  flip · size [small|big] · pieces [auto|art|glyph]".to_string(),
        "  theme [slate|wood|forest|mono] · sound [auto|on|off]".to_string(),
        String::new(),
        theme.dim("If the connection drops, this client reconnects and restores your seat."),
    ]
}

fn side_color(side: Side) -> Color {
    match side {
        Side::White => Color::White,
        Side::Black => Color::Black,
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn play(options: Options, loaded: storage::LoadedPreferences) -> Result<(), String> {
    if options.online.is_some() {
        return play_online(options, loaded);
    }
    let config_path = loaded.path;
    let session_path = storage::default_session_path(&config_path);
    let mut last_preferences = loaded.preferences;
    let mut backup_config_before_save = loaded.backup_before_save;
    let first_run = loaded.first_run;
    let config_warning = loaded.warning;
    let requested_load = options
        .load
        .clone()
        .or_else(|| options.resume.then(|| session_path.clone()));
    let mut resuming = requested_load.is_some();
    let mut restored = match requested_load {
        Some(path) => Some(restore_game(storage::load_game(&path)?)?),
        None => None,
    };
    let start = match (&restored, &options.fen) {
        (Some(_), _) => None,
        (None, Some(fen)) => {
            Some(Position::from_fen(fen).map_err(|why| format!("bad --fen: {}", why))?)
        }
        (None, None) => Some(Position::startpos()),
    };

    let color = options.color.unwrap_or_else(Theme::detect_color);
    let mut screen = Screen {
        theme: Theme::new(
            color,
            options.ascii,
            color && Theme::detect_live(),
            options.palette,
        ),
        sound: sound::Player::new(options.sound),
        inline_images: false,
        flipped: false,
        cols: 80,
        rows: 24,
        metrics: ui::Metrics::COMPACT,
        wide_panel: false,
        pieces: options.pieces,
        player_names: options.player_names,
        compact: options.compact,
        indent: "  ".to_string(),
        board_hitbox: None,
        body_top: 0,
        action_hitboxes: Vec::new(),
        focused: UiAction::MoveInput,
        history_offset: 0,
        history_capacity: 0,
        confirming: None,
        analysis: Vec::new(),
        message: Vec::new(),
        selected: None,
        targets: Vec::new(),
        captures: Vec::new(),
        invalid: None,
        promotions: Vec::new(),
        page: None,
        last_frame: Vec::new(),
        last_size: None,
        inline_drawn: false,
        last_inline_board: None,
        last_prompt: None,
        redraw: true,
        online: None,
    };
    // Held for as long as the game lasts. Whatever was on the terminal before
    // comes back when this is dropped, however the program ends.
    let _fullscreen = ui::Fullscreen::enter(&screen.theme);
    // Probe once up front even when another piece style was requested. The
    // in-game Piece control can switch to Auto later without querying the
    // terminal in the middle of raw event input.
    if screen.theme.live && !screen.theme.ascii {
        screen.inline_images = ui::detect_inline_images();
    }

    let (mut game, mut mode) = match restored.take() {
        Some((game, mode)) => (game, mode),
        None => {
            let mut stdin = io::stdin().lock();
            let choice = match options.mode {
                Some(mode) => StartChoice::Mode(mode),
                None => match ask_mode(&mut stdin, &mut screen, session_path.exists())? {
                    Some(choice) => choice,
                    None => return Ok(()),
                },
            };
            drop(stdin);
            match choice {
                StartChoice::Mode(mode) => (
                    Game::with_clock(
                        start.expect("new games have a starting position"),
                        options.clock,
                        options.increment,
                    ),
                    mode,
                ),
                StartChoice::Resume => {
                    resuming = true;
                    restore_game(storage::load_game(&session_path)?)?
                }
            }
        }
    };

    // Raw events are only appropriate while we own an interactive colour
    // terminal. Piped input retains the original line-oriented interface.
    let mut terminal_input = TerminalInput::enter(screen.theme.live && screen.theme.color)?;

    let mut engine = Search::new();
    let mut limits = options.limits;
    screen.flipped = options.flipped || mode == Mode::HumanBlack;
    screen.message = if let Some(warning) = config_warning {
        vec![screen
            .theme
            .warn(&format!("{} — using safe defaults.", warning))]
    } else if resuming {
        vec![screen.theme.good("Saved game restored.")]
    } else if first_run {
        vec![screen.theme.accent(
            "Welcome — type a move, click a piece, or press Tab. Preferences save automatically.",
        )]
    } else {
        vec![screen
            .theme
            .dim("Type a move, click a piece, or press Tab for controls.")]
    };
    let mut saved_revision = game.revision;
    let mut persistence_error_reported = false;

    loop {
        let preferences = runtime_preferences(&screen, &game);
        if preferences != last_preferences {
            let backup = if backup_config_before_save {
                storage::backup_invalid_preferences(&config_path).map(|_| ())
            } else {
                Ok(())
            };
            if backup.is_ok() {
                backup_config_before_save = false;
            }
            let saved = backup.and_then(|_| storage::save_preferences(&config_path, &preferences));
            if let Err(error) = saved {
                if !persistence_error_reported {
                    screen.note(
                        screen
                            .theme
                            .warn(&format!("Preferences were not saved: {error}")),
                    );
                    persistence_error_reported = true;
                }
            }
            last_preferences = preferences;
        }
        if game.revision != saved_revision {
            if let Err(error) = save_current_game(&session_path, &mut game, mode) {
                if !persistence_error_reported {
                    screen.note(screen.theme.warn(&format!("Autosave failed: {error}")));
                    persistence_error_reported = true;
                }
            }
            saved_revision = game.revision;
        }
        let flag_before = game.clock.flagged;
        if game.clock.tick() {
            if flag_before.is_none() && game.clock.flagged.is_some() {
                game.changed();
                screen.sound.play(sound::Cue::GameEnd);
                screen.clear_marks();
                screen.page = None;
                screen.redraw = true;
            } else {
                screen.draw_clock_tick(&game, mode, &limits);
            }
        }
        if screen.redraw {
            screen.redraw = false;
            screen.draw(&game, mode, &limits);
        }

        let finished = outcome(&game).is_some();
        let engine_to_move = match mode {
            Mode::HumanWhite => game.pos.side == Color::Black,
            Mode::HumanBlack => game.pos.side == Color::White,
            Mode::TwoPlayer => false,
        };
        if !finished && !game.paused && engine_to_move {
            engine_move(&mut game, &mut engine, &limits, &mut screen);
            screen.redraw = true;
            continue;
        }

        let action = if terminal_input.is_active() {
            screen.draw_prompt(&game, terminal_input.buffer());
            terminal_input.read_for(Duration::from_millis(200))?
        } else {
            let mut stdin = io::stdin().lock();
            match read_line(&mut stdin, &screen.prompt(&game))? {
                Some(line) => InputAction::Submit(line),
                None => InputAction::Quit,
            }
        };

        let line = match action {
            InputAction::Submit(line) => {
                if line.is_empty() && screen.focused != UiAction::MoveInput {
                    if handle_ui_action(screen.focused, &mut game, mode, &mut screen) {
                        let _ = save_current_game(&session_path, &mut game, mode);
                        return Ok(());
                    }
                    continue;
                }
                line
            }
            InputAction::Prompt => {
                let cleared_invalid = screen.invalid.take().is_some();
                if screen.focus_move_input() || cleared_invalid {
                    screen.redraw = true;
                    continue;
                }
                screen.draw_prompt(&game, terminal_input.buffer());
                continue;
            }
            InputAction::Focus { reverse } => {
                screen.move_focus(reverse);
                continue;
            }
            InputAction::Resize => {
                screen.redraw = true;
                continue;
            }
            InputAction::Tick => continue,
            InputAction::History { older } => {
                screen.scroll_history(&game, older);
                continue;
            }
            InputAction::Cancel => {
                let focus_changed = screen.focus_move_input();
                if screen.page.take().is_some() {
                    screen.redraw = true;
                } else {
                    let cancelled = screen.selected.take().is_some()
                        || !screen.targets.is_empty()
                        || !screen.promotions.is_empty();
                    let confirmation = screen.confirming.take().is_some();
                    if cancelled || confirmation || focus_changed {
                        screen.targets.clear();
                        screen.promotions.clear();
                        screen.redraw = true;
                    } else {
                        screen.draw_prompt(&game, terminal_input.buffer());
                    }
                }
                continue;
            }
            InputAction::Click { column, row } => {
                let flag_before = game.clock.flagged;
                game.clock.tick();
                if flag_before.is_none() && game.clock.flagged.is_some() {
                    game.changed();
                    screen.sound.play(sound::Cue::GameEnd);
                    screen.clear_marks();
                    screen.page = None;
                    screen.redraw = true;
                    continue;
                }
                if screen.page.take().is_some() {
                    screen.redraw = true;
                    continue;
                }
                if let Some(action) = screen.action_at(column, row) {
                    screen.focus(action);
                    if handle_ui_action(action, &mut game, mode, &mut screen) {
                        let _ = save_current_game(&session_path, &mut game, mode);
                        return Ok(());
                    }
                    continue;
                }
                screen.confirming = None;
                let square = screen.square_at(column, row);
                let finished = outcome(&game).is_some();
                handle_board_click(&mut game, &mut screen, square, finished);
                continue;
            }
            // End of input, Ctrl-C or Ctrl-D.
            InputAction::Quit => {
                let _ = save_current_game(&session_path, &mut game, mode);
                println!();
                return Ok(());
            }
        };

        let flag_before = game.clock.flagged;
        game.clock.tick();
        if flag_before.is_none() && game.clock.flagged.is_some() {
            game.changed();
            screen.sound.play(sound::Cue::GameEnd);
            screen.clear_marks();
            screen.page = None;
            screen.redraw = true;
        }
        let finished = outcome(&game).is_some();
        let input = line.trim();
        // Anything at all puts an open page away and brings the board back.
        if screen.page.take().is_some() {
            screen.redraw = true;
        }
        if input.is_empty() {
            // Return on its own redraws, which is how the frame catches up
            // with a window that has been resized under it.
            screen.redraw = screen.theme.live;
            continue;
        }
        if !screen.promotions.is_empty() && input.len() == 1 {
            let kind = input.chars().next().and_then(PieceKind::from_char);
            if let Some(
                kind @ (PieceKind::Queen | PieceKind::Rook | PieceKind::Bishop | PieceKind::Knight),
            ) = kind
            {
                play_promotion(&mut game, &mut screen, kind);
                continue;
            }
        }
        let (word, rest) = split_command(input);

        match word.as_str() {
            "quit" | "exit" | "q" => {
                // In a transcript this is the last line; on a screen we own,
                // the screen is about to be handed back, so it would flash past.
                if !screen.theme.live {
                    println!("  {}", screen.theme.dim("Goodbye."));
                }
                let _ = save_current_game(&session_path, &mut game, mode);
                return Ok(());
            }
            "help" | "h" | "?" => {
                let lines = help_lines(&screen.theme);
                screen.open("HELP", lines);
                continue;
            }
            "board" | "b" => {
                screen.redraw = true;
                continue;
            }
            "moves" | "l" | "m" => {
                if rest.is_empty() {
                    let lines = moves_lines(&screen.theme, &game.pos);
                    screen.open("LEGAL MOVES", lines);
                } else {
                    show_piece_moves(&mut screen, &game.pos, rest);
                }
                continue;
            }
            "history" | "moveslist" => {
                let lines = history_page(&screen.theme, &game);
                screen.open("THE GAME SO FAR", lines);
                continue;
            }
            "pgn" => {
                let lines = pgn_lines(&game, &screen.player_names);
                screen.open("PGN", lines);
                continue;
            }
            "export" => {
                let path = if rest.is_empty() {
                    PathBuf::from("game.pgn")
                } else {
                    PathBuf::from(rest)
                };
                match export_pgn(&path, &game, &screen.player_names) {
                    Ok(()) => screen.note(
                        screen
                            .theme
                            .good(&format!("Exported PGN to {}.", path.display())),
                    ),
                    Err(error) => screen.note(screen.theme.warn(&error)),
                }
                continue;
            }
            "import" => {
                if rest.is_empty() {
                    screen.note(screen.theme.dim("Use `import game.pgn`."));
                } else if let Err(error) = import_pgn(Path::new(rest), &mut game, &mut screen) {
                    screen.note(screen.theme.warn(&error));
                }
                continue;
            }
            "save" => {
                let path = if rest.is_empty() {
                    session_path.clone()
                } else {
                    PathBuf::from(rest)
                };
                match save_current_game(&path, &mut game, mode) {
                    Ok(()) => screen.note(
                        screen
                            .theme
                            .good(&format!("Saved game to {}.", path.display())),
                    ),
                    Err(error) => screen.note(screen.theme.warn(&error)),
                }
                continue;
            }
            "load" => {
                let path = if rest.is_empty() {
                    session_path.clone()
                } else {
                    PathBuf::from(rest)
                };
                if let Err(error) = load_saved_game(&path, &mut game, &mut mode, &mut screen) {
                    screen.note(screen.theme.warn(&error));
                }
                continue;
            }
            "setup" | "config" => {
                screen.open(
                    "LOCAL SETUP",
                    setup_lines(&screen.theme, &config_path, &session_path),
                );
                continue;
            }
            "name" => {
                set_player_name(&mut screen, rest);
                continue;
            }
            "fen" => {
                let text = screen.theme.accent(&game.pos.to_fen());
                screen.note(text);
                continue;
            }
            "eval" => {
                let score = white_pov(&game.pos, eval::evaluate(&game.pos));
                let text = format!(
                    "{}  {}",
                    screen.theme.bold(&format_score(score)),
                    screen.theme.dim("(static, from White's point of view)")
                );
                screen.note(text);
                continue;
            }
            "flip" => {
                screen.flipped = !screen.flipped;
                screen.redraw = true;
                continue;
            }
            "theme" => {
                set_theme(&mut screen, rest);
                continue;
            }
            "pieces" => {
                set_pieces(&mut screen, rest);
                continue;
            }
            "sound" | "sounds" => {
                set_sound(&mut screen, rest);
                continue;
            }
            "size" => {
                set_size(&mut screen, rest);
                continue;
            }
            "hint" => {
                hint(&game, &mut engine, &limits, &mut screen);
                continue;
            }
            "time" => {
                set_time(&mut limits, &mut screen, rest);
                continue;
            }
            "depth" => {
                set_depth(&mut limits, &mut screen, rest);
                continue;
            }
            "undo" | "u" | "back" | "takeback" => {
                undo(&mut game, mode, &mut screen);
                continue;
            }
            "pause" | "resume" => {
                let wants_pause = word == "pause";
                if game.paused == wants_pause {
                    let state = if game.paused {
                        "already paused"
                    } else {
                        "already running"
                    };
                    screen.note(screen.theme.dim(&format!("The game is {}.", state)));
                } else {
                    handle_ui_action(UiAction::Pause, &mut game, mode, &mut screen);
                }
                continue;
            }
            "draw" => {
                handle_ui_action(UiAction::Draw, &mut game, mode, &mut screen);
                continue;
            }
            "new" | "restart" => {
                if confirm_new(&mut terminal_input, &game, &screen)? {
                    game.restart();
                    screen.clear_marks();
                    screen.note(screen.theme.good("New game."));
                    screen.redraw = true;
                }
                continue;
            }
            "resign" => {
                if finished {
                    screen.note(screen.theme.dim("The game is already over."));
                } else {
                    game.resigned = Some(game.pos.side);
                    game.changed();
                    game.clock.pause();
                    screen.analysis.clear();
                    screen.sound.play(sound::Cue::GameEnd);
                    screen.redraw = true;
                }
                continue;
            }
            _ => {}
        }

        if finished {
            screen.note(
                screen
                    .theme
                    .dim("The game is over. Try `new`, `undo` or `quit`."),
            );
            continue;
        }
        if game.paused {
            screen.note(
                screen
                    .theme
                    .dim("The game is paused. Choose Resume before moving."),
            );
            continue;
        }
        if looks_like_command(&word) {
            let hint = match command_guess(&word) {
                Some(command) => format!("Did you mean `{}`?", command),
                None => "Type `help` for the commands.".to_string(),
            };
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("I do not know `{}`.", word)),
                screen.theme.dim(&hint)
            ));
            continue;
        }

        make_move(&mut game, input, &mut screen);
    }
}

/// The first word, lower-cased, and whatever followed it.
fn split_command(input: &str) -> (String, &str) {
    match input.split_once(char::is_whitespace) {
        Some((word, rest)) => (word.to_ascii_lowercase(), rest.trim()),
        None => (input.to_ascii_lowercase(), ""),
    }
}

/// Returns true when the application should close.
fn handle_ui_action(action: UiAction, game: &mut Game, mode: Mode, screen: &mut Screen) -> bool {
    match action {
        UiAction::MoveInput => {
            screen.focus_move_input();
        }
        UiAction::Pause => {
            if outcome(game).is_none() {
                game.toggle_pause();
                screen.clear_marks();
                let message = if game.paused {
                    screen
                        .theme
                        .accent("Game paused. Choose Resume when you are ready.")
                } else {
                    screen.theme.good("Game resumed.")
                };
                screen.note(message);
            }
        }
        UiAction::Undo => {
            screen.confirming = None;
            undo(game, mode, screen);
        }
        UiAction::Draw => {
            screen.confirming = None;
            if mode != Mode::TwoPlayer {
                screen.note(
                    screen
                        .theme
                        .dim("Draw offers are available in two-player games."),
                );
            } else if game.draw_offer == Some(game.pos.side.flip()) {
                game.agreed_draw = true;
                game.draw_offer = None;
                game.changed();
                game.clock.pause();
                screen.clear_marks();
                screen.sound.play(sound::Cue::GameEnd);
                screen.redraw = true;
            } else if game.draw_offer == Some(game.pos.side) {
                screen.note(
                    screen
                        .theme
                        .dim("Your draw offer is waiting for the next player."),
                );
            } else {
                game.draw_offer = Some(game.pos.side);
                game.changed();
                screen.note(screen.theme.accent(&format!(
                    "{} offers a draw. Play your move; the opponent can then accept.",
                    game.pos.side.name()
                )));
            }
        }
        UiAction::Resign => {
            if screen.confirming == Some(UiAction::Resign) {
                game.resigned = Some(game.pos.side);
                game.changed();
                game.clock.pause();
                screen.clear_marks();
                screen.sound.play(sound::Cue::GameEnd);
                screen.redraw = true;
            } else {
                screen.confirming = Some(UiAction::Resign);
                screen.note(
                    screen
                        .theme
                        .warn("Choose Confirm to resign, or press Escape."),
                );
            }
        }
        UiAction::Restart => {
            if screen.confirming == Some(UiAction::Restart) {
                game.restart();
                screen.clear_marks();
                screen.note(screen.theme.good("New game."));
            } else {
                screen.confirming = Some(UiAction::Restart);
                screen.note(
                    screen
                        .theme
                        .warn("Choose Confirm to restart, or press Escape."),
                );
            }
        }
        UiAction::ToggleSize => toggle_size(screen),
        UiAction::CyclePieces => cycle_pieces(screen),
        UiAction::Rematch => {
            game.restart();
            screen.clear_marks();
            screen.note(screen.theme.good("Rematch started."));
        }
        UiAction::Quit => return true,
    }
    false
}

fn make_move(game: &mut Game, input: &str, screen: &mut Screen) {
    match parse_move(&game.pos, input) {
        Ok(mv) => {
            let mover = game.pos.side;
            let text = play_move(game, mv, screen);
            screen.clear_marks();
            screen.redraw = true;
            // Confirm sloppy input by echoing how it was read, but do not
            // repeat a move back that was already typed as it is written.
            if !text.eq_ignore_ascii_case(input) {
                screen.note(format!(
                    "{} {}",
                    screen.theme.dim(&format!("{} plays", mover.name())),
                    screen.theme.bold(&text)
                ));
            }
        }
        Err(ParseError::Illegal(text)) => {
            // A pawn reaching the last rank without being told what to become.
            if let Some(mv) = promotion_default(&game.pos, input) {
                let square = board::square_name(mv.to);
                let name = play_move(game, mv, screen);
                screen.clear_marks();
                screen.redraw = true;
                screen.note(format!(
                    "{} {}",
                    screen.theme.bold(&name),
                    screen.theme.dim(&format!(
                        "- say {0}=N, {0}=R or {0}=B for anything but a queen",
                        square
                    ))
                ));
                return;
            }
            let choices = under_specified(&game.pos, input);
            if choices.len() > 1 {
                screen.note(format!(
                    "{} {}",
                    screen.theme.warn(&format!("`{}` could be", text)),
                    screen.theme.bold(&choices.join(" or "))
                ));
                return;
            }
            let near = nearby_moves(&game.pos, input);
            let hint = if near.is_empty() {
                screen.theme.dim("- `moves` lists what is legal")
            } else {
                screen
                    .theme
                    .dim(&format!("- did you mean {}?", near.join(" or ")))
            };
            screen.note(format!(
                "{} {}",
                screen
                    .theme
                    .warn(&format!("`{}` is not a legal move", text)),
                hint
            ));
        }
        Err(ParseError::Ambiguous(text, candidates)) => {
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("`{}` could mean", text)),
                screen.theme.bold(&candidates.join(" or "))
            ));
        }
    }
}

/// Chess.com-style two-click movement: choose a friendly piece, then one of
/// its legal destinations. Clicking another friendly piece simply reselects.
fn handle_board_click(
    game: &mut Game,
    screen: &mut Screen,
    square: Option<board::Square>,
    finished: bool,
) {
    let square = match square {
        Some(square) => square,
        None => {
            if screen.selected.take().is_some() {
                screen.targets.clear();
                screen.captures.clear();
                screen.promotions.clear();
                screen.invalid = None;
                screen.redraw = true;
            }
            return;
        }
    };

    if finished {
        screen.note(screen.theme.dim("The game is over. Try `new` or `undo`."));
        return;
    }
    if game.paused {
        screen.note(
            screen
                .theme
                .dim("The game is paused. Choose Resume before moving."),
        );
        return;
    }
    screen.invalid = None;

    if let Some(choice) = screen
        .promotions
        .iter()
        .find(|choice| choice.square == square)
        .copied()
    {
        play_move(game, choice.movement, screen);
        screen.clear_marks();
        screen.redraw = true;
        return;
    }
    screen.promotions.clear();

    let legal = generate_legal(&game.pos);
    if let Some(from) = screen.selected {
        if square == from {
            screen.selected = None;
            screen.targets.clear();
            screen.captures.clear();
            screen.redraw = true;
            return;
        }

        let mut choices = legal
            .iter()
            .copied()
            .filter(|mv| mv.from == from && mv.to == square);
        if let Some(first) = choices.next() {
            let choices: Vec<Move> = std::iter::once(first).chain(choices).collect();
            if choices.iter().any(|mv| mv.promo.is_some()) {
                open_promotion_menu(game, screen, &choices);
                return;
            }
            play_move(game, first, screen);
            screen.clear_marks();
            screen.redraw = true;
            return;
        }
    }

    match game.pos.at(square) {
        Some(piece) if piece.color == game.pos.side => {
            screen.selected = Some(square);
            let moves: Vec<Move> = legal
                .iter()
                .filter(|mv| mv.from == square)
                .copied()
                .collect();
            screen.targets = moves.iter().map(|mv| mv.to).collect();
            screen.captures = moves
                .iter()
                .filter(|mv| mv.kind == MoveKind::EnPassant || game.pos.at(mv.to).is_some())
                .map(|mv| mv.to)
                .collect();
            screen.message = if moves.is_empty() {
                vec![screen.theme.dim(&format!(
                    "The {} on {} has no legal moves.",
                    kind_name(piece.kind),
                    board::square_name(square)
                ))]
            } else {
                Vec::new()
            };
            screen.redraw = true;
        }
        _ if screen.selected.is_none() => {
            screen.invalid = Some(square);
            let guidance = match game.pos.at(square) {
                Some(piece) => format!(
                    "That {} belongs to {}. Choose a {} piece.",
                    kind_name(piece.kind),
                    piece.color.name(),
                    game.pos.side.name()
                ),
                None => format!(
                    "Choose a {} piece before choosing a destination.",
                    game.pos.side.name()
                ),
            };
            screen.message = vec![screen.theme.dim(&guidance)];
            screen.redraw = true;
        }
        _ => {
            screen.invalid = Some(square);
            screen.message = vec![screen.theme.warn(&format!(
                "{} is not a legal destination for the selected piece.",
                board::square_name(square)
            ))];
            screen.redraw = true;
        }
    }
}

fn open_promotion_menu(game: &Game, screen: &mut Screen, moves: &[Move]) {
    let destination = moves[0].to;
    let file = board::file_of(destination);
    let rank = board::rank_of(destination);
    let step: i8 = if rank == 7 { -1 } else { 1 };
    let kinds = [
        PieceKind::Queen,
        PieceKind::Rook,
        PieceKind::Bishop,
        PieceKind::Knight,
    ];

    screen.promotions = kinds
        .iter()
        .enumerate()
        .filter_map(|(offset, &kind)| {
            let movement = moves.iter().find(|mv| mv.promo == Some(kind)).copied()?;
            let display_rank = (rank as i8 + step * offset as i8) as u8;
            Some(ui::PromotionOption {
                square: board::sq(file, display_rank),
                piece: Piece::new(game.pos.side, kind),
                movement,
            })
        })
        .collect();
    screen.analysis.clear();
    screen.message = vec![format!(
        "{}  {}",
        screen.theme.accent("Promotion"),
        screen.theme.dim("choose a piece, or type Q, R, B or N")
    )];
    screen.redraw = true;
}

fn play_promotion(game: &mut Game, screen: &mut Screen, kind: PieceKind) {
    if let Some(choice) = screen
        .promotions
        .iter()
        .find(|choice| choice.piece.kind == kind)
        .copied()
    {
        play_move(game, choice.movement, screen);
        screen.clear_marks();
        screen.redraw = true;
    }
}

fn engine_move(game: &mut Game, engine: &mut Search, limits: &Limits, screen: &mut Screen) {
    engine.set_history(game.prior_positions());
    let indent = screen.indent.clone();
    let result = {
        let theme = &screen.theme;
        let pos = &game.pos;
        let mut report = |snapshot: &SearchResult| {
            if theme.live {
                print!("\r\x1b[K{}{}", indent, thinking_line(theme, pos, snapshot));
                let _ = io::stdout().flush();
            }
        };
        engine.think(pos, limits, &mut report)
    };
    screen.theme.erase_line();

    let flag_before = game.clock.flagged;
    game.clock.tick();
    if flag_before.is_none() && game.clock.flagged.is_some() {
        game.changed();
        screen.sound.play(sound::Cue::GameEnd);
        screen.clear_marks();
        screen.page = None;
        screen.redraw = true;
        return;
    }

    let mv = match result.best {
        Some(mv) => mv,
        None => {
            screen.note(screen.theme.warn("The engine has no legal move."));
            return;
        }
    };
    let score = white_pov(&game.pos, result.score);
    let text = play_move(game, mv, screen);
    screen.clear_marks();

    let theme = &screen.theme;
    let mut lines = vec![format!(
        "{} {}   {}",
        theme.dim("Engine plays"),
        theme.strong(theme.palette.accent, &text),
        theme.dim(&format!(
            "depth {}  \u{b7}  {}  \u{b7}  {} nodes  \u{b7}  {:.1}s",
            result.depth,
            format_score(score),
            node_count(result.nodes),
            result.elapsed.as_secs_f64()
        ))
    )];
    // The rest of the line it expects, from the position it has just reached.
    let expected = pv_text(&game.pos, result.pv.get(1..).unwrap_or(&[]), 6);
    if !expected.is_empty() {
        lines.push(format!(
            "{} {}",
            theme.dim("expects"),
            theme.label(&expected)
        ));
    }
    screen.analysis = lines;
}

fn thinking_line(theme: &Theme, pos: &Position, snapshot: &SearchResult) -> String {
    format!(
        "{}  {}  {}  {}",
        theme.dim("thinking"),
        theme.label(&format!("depth {}", snapshot.depth)),
        theme.accent(&format_score(white_pov(pos, snapshot.score))),
        theme.dim(&pv_text(pos, &snapshot.pv, 4))
    )
}

fn hint(game: &Game, engine: &mut Search, limits: &Limits, screen: &mut Screen) {
    if outcome(game).is_some() {
        screen.note(screen.theme.dim("The game is over."));
        return;
    }
    // A hint should come back quickly even when the engine has a long budget.
    let budget = Limits {
        depth: limits.depth,
        movetime: Some(
            limits
                .movetime
                .unwrap_or(Duration::from_secs(1))
                .min(Duration::from_secs(1)),
        ),
    };
    engine.set_history(game.prior_positions());
    let indent = screen.indent.clone();
    let result = {
        let theme = &screen.theme;
        let pos = &game.pos;
        let mut report = |snapshot: &SearchResult| {
            if theme.live {
                print!("\r\x1b[K{}{}", indent, thinking_line(theme, pos, snapshot));
                let _ = io::stdout().flush();
            }
        };
        engine.think(pos, &budget, &mut report)
    };
    screen.theme.erase_line();

    let theme = &screen.theme;
    match result.best {
        Some(mv) => {
            let line = pv_text(&game.pos, &result.pv, 6);
            let mut lines = vec![format!(
                "{} {}   {}",
                theme.dim("Try"),
                theme.strong(theme.palette.accent, &to_san(&game.pos, mv)),
                theme.dim(&format!(
                    "depth {}  \u{b7}  {}",
                    result.depth,
                    format_score(white_pov(&game.pos, result.score))
                ))
            )];
            if !line.is_empty() {
                lines.push(format!("{} {}", theme.dim("expects"), theme.label(&line)));
            }
            screen.analysis = lines;
            screen.targets = vec![mv.from, mv.to];
            screen.captures = if mv.kind == MoveKind::EnPassant || game.pos.at(mv.to).is_some() {
                vec![mv.to]
            } else {
                Vec::new()
            };
            screen.redraw = true;
        }
        None => {
            let text = theme.dim("There is nothing to play.");
            screen.note(text);
        }
    }
}

fn undo(game: &mut Game, mode: Mode, screen: &mut Screen) {
    // Take back the engine's reply as well, so the human lands back on their
    // own turn rather than watching it answer again.
    let plies = if mode == Mode::TwoPlayer { 1 } else { 2 };
    let mut taken = Vec::new();
    while taken.len() < plies {
        match game.take_back() {
            Some(text) => taken.push(text),
            None => break,
        }
    }
    if taken.is_empty() {
        screen.note(screen.theme.dim("Nothing to take back."));
        return;
    }
    taken.reverse();
    screen.clear_marks();
    screen.note(format!(
        "{} {}",
        screen.theme.dim("Took back"),
        screen.theme.bold(&taken.join(" and "))
    ));
    screen.redraw = true;
}

fn set_time(limits: &mut Limits, screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        screen.note(
            screen
                .theme
                .dim(&format!("The engine gets {}.", budget_text(limits))),
        );
        return;
    }
    match rest.parse::<f64>() {
        Ok(secs) if secs.is_finite() && secs > 0.0 => {
            limits.movetime = Some(Duration::from_secs_f64(secs));
            screen.note(
                screen
                    .theme
                    .good(&format!("The engine now gets {}.", budget_text(limits))),
            );
        }
        _ => screen.note(
            screen
                .theme
                .warn(&format!("`{}` is not a number of seconds.", rest)),
        ),
    }
}

fn set_depth(limits: &mut Limits, screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        screen.note(
            screen
                .theme
                .dim(&format!("Depth is capped at {}.", limits.depth)),
        );
        return;
    }
    match rest.parse::<u32>() {
        Ok(depth) if depth >= 1 && depth <= search::MAX_DEPTH => {
            limits.depth = depth;
            // A depth asked for by name is a depth to reach, not to give up on.
            limits.movetime = None;
            screen.note(
                screen
                    .theme
                    .good(&format!("The engine now searches to depth {}.", depth)),
            );
        }
        _ => screen.note(screen.theme.warn(&format!(
            "Depth must be a number from 1 to {}.",
            search::MAX_DEPTH
        ))),
    }
}

fn set_theme(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        screen.note(screen.theme.dim(&format!("Themes: {}.", ui::theme_names())));
        return;
    }
    match ui::palette(rest) {
        Some(palette) => {
            screen.theme.palette = palette;
            screen.redraw = true;
        }
        None => screen.note(screen.theme.warn(&format!(
            "No theme called `{}`. Try {}.",
            rest,
            ui::theme_names()
        ))),
    }
}

/// Drawn pieces or figurines. Drawn pieces need a square at least three rows
/// tall, so on a small window this is a preference rather than an order.
fn set_pieces(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        let text = screen.theme.dim("Pieces are `art`, `glyph` or `auto`.");
        screen.note(text);
        return;
    }
    match ui::pieces_named(rest) {
        Some(pieces) => apply_pieces(screen, pieces),
        None => {
            let text = screen
                .theme
                .warn(&format!("`{}` is not art, glyph or auto.", rest));
            screen.note(text);
        }
    }
}

fn apply_pieces(screen: &mut Screen, pieces: ui::Pieces) {
    screen.pieces = pieces;
    screen.redraw = true;
}

/// The UI cycles from automatic images directly to font glyphs first. That
/// gives terminals with soft image scaling a crisp escape hatch in one key or
/// click, while portable block art remains the third choice.
fn cycle_pieces(screen: &mut Screen) {
    let pieces = match screen.pieces {
        ui::Pieces::Auto => ui::Pieces::Glyph,
        ui::Pieces::Glyph => ui::Pieces::Art,
        ui::Pieces::Art => ui::Pieces::Auto,
    };
    apply_pieces(screen, pieces);
    let detail = match pieces {
        ui::Pieces::Auto if screen.inline_images => "terminal images",
        ui::Pieces::Auto => "the best available fallback",
        ui::Pieces::Glyph => "your terminal font for maximum sharpness",
        ui::Pieces::Art => "portable block artwork",
    };
    screen.note(screen.theme.good(&format!(
        "Piece style: {} — using {}.",
        pieces.name(),
        detail
    )));
}

fn set_sound(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        let mode = match screen.sound.mode() {
            sound::Mode::Auto => "auto",
            sound::Mode::On => "on",
            sound::Mode::Off => "off",
        };
        let state = if let Some(backend) = screen.sound.backend() {
            format!("{} is ready", backend)
        } else if screen.sound.mode() == sound::Mode::Off {
            "muted".to_string()
        } else if let Some(error) = screen.sound.last_error() {
            format!("audio could not start: {}", error)
        } else {
            "no local audio output was found".to_string()
        };
        screen.note(
            screen
                .theme
                .dim(&format!("Sound is `{}` - {}.", mode, state)),
        );
        return;
    }

    if rest.eq_ignore_ascii_case("test") {
        let message = if let Some(backend) = screen.sound.backend() {
            if screen.sound.play(sound::Cue::Move) {
                screen
                    .theme
                    .good(&format!("Playing a test move through {}.", backend))
            } else {
                screen.theme.warn("The test sound could not be queued.")
            }
        } else if screen.sound.mode() == sound::Mode::Off {
            screen.theme.dim("Sound is off. Use `sound on` first.")
        } else {
            screen.theme.warn(&format!(
                "Audio is unavailable: {}.",
                screen
                    .sound
                    .last_error()
                    .unwrap_or("no output device was found")
            ))
        };
        screen.note(message);
        return;
    }

    let Some(mode) = sound::Mode::named(rest) else {
        screen.note(
            screen
                .theme
                .warn(&format!("`{}` is not `auto`, `on` or `off`.", rest)),
        );
        return;
    };
    let active = screen.sound.set_mode(mode);
    let message = match (mode, active) {
        (sound::Mode::Off, _) => screen.theme.dim("Sound effects are off."),
        (_, true) => {
            let backend = screen.sound.backend().unwrap_or("the audio device");
            let _ = screen.sound.play(sound::Cue::Move);
            screen.theme.good(&format!(
                "Sound effects are on through {}. Playing a test move.",
                backend
            ))
        }
        _ => screen.theme.warn(&format!(
            "No local audio output is available: {}.",
            screen
                .sound
                .last_error()
                .unwrap_or("no output device was found")
        )),
    };
    screen.note(message);
}

/// Fill the window, or hold the board at the size it used to be.
fn set_size(screen: &mut Screen, rest: &str) {
    let compact = match rest.to_ascii_lowercase().as_str() {
        "" => !screen.compact,
        "big" | "large" | "full" | "fill" => false,
        "small" | "compact" | "tiny" => true,
        _ => {
            let text = screen
                .theme
                .warn(&format!("`{}` is not `big` or `small`.", rest));
            screen.note(text);
            return;
        }
    };
    screen.compact = compact;
    screen.redraw = true;
}

fn toggle_size(screen: &mut Screen) {
    screen.compact = !screen.compact;
    let size = if screen.compact { "small" } else { "big" };
    screen.note(screen.theme.good(&format!("Board size: {}.", size)));
}

/// Throwing a game away is the one thing `undo` cannot rescue, so ask first.
fn confirm_new(input: &mut TerminalInput, game: &Game, screen: &Screen) -> Result<bool, String> {
    if game.sans.is_empty() || !screen.theme.live {
        return Ok(true);
    }
    let prompt = format!(
        "  {} ",
        screen
            .theme
            .warn("Start a new game and lose this one? [y/N]")
    );
    input.suspend()?;
    let answer = {
        let mut stdin = io::stdin().lock();
        read_line(&mut stdin, &prompt)
    };
    let resumed = input.resume();
    let line = answer?;
    resumed?;
    match line {
        Some(line) => Ok(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        )),
        None => Ok(false),
    }
}

// ---------------------------------------------------------------------------
// Understanding what was typed
// ---------------------------------------------------------------------------

/// Legal moves that `input` names apart from the file or rank hint SAN adds
/// when two pieces can reach the same square. `parse_move` turns an
/// under-specified move down flat, and "not legal" is the wrong thing to say
/// about a move that is legal twice over; these are the moves to offer instead.
fn under_specified(pos: &Position, input: &str) -> Vec<String> {
    let legal = generate_legal(pos);
    let want = bare_form(input);
    legal
        .iter()
        .filter(|&&mv| {
            let piece = match pos.at(mv.from) {
                // Pawn moves name their file rather than the piece, so they
                // are never ambiguous in this way.
                Some(piece) if piece.kind != PieceKind::Pawn => piece,
                _ => return false,
            };
            let mut bare = String::new();
            bare.push(piece.kind.to_char());
            bare.push_str(&board::square_name(mv.to));
            bare_form(&bare) == want
        })
        .map(|&mv| to_san_with(pos, mv, &legal))
        .collect()
}

/// A pawn move onto the last rank that did not say what to promote to. Nearly
/// everyone means a queen, which is also what the coordinate form already does.
fn promotion_default(pos: &Position, input: &str) -> Option<Move> {
    let legal = generate_legal(pos);
    let want = bare_form(input);
    legal.iter().copied().find(|&mv| {
        mv.promo == Some(PieceKind::Queen) && {
            let san = to_san_with(pos, mv, &legal);
            // `e8=Q` named as `e8`, `dxe8=Q` named as `dxe8`.
            bare_form(san.split('=').next().unwrap_or("")) == want
        }
    })
}

/// Legal moves within a typo of what was typed.
fn nearby_moves(pos: &Position, input: &str) -> Vec<String> {
    let legal = generate_legal(pos);
    let want = bare_form(input);
    let mut scored: Vec<(usize, String)> = legal
        .iter()
        .map(|&mv| to_san_with(pos, mv, &legal))
        .filter_map(|san| {
            let distance = edit_distance(&bare_form(&san), &want);
            (distance <= 1).then_some((distance, san))
        })
        .collect();
    scored.sort();
    scored.truncate(3);
    scored.into_iter().map(|(_, san)| san).collect()
}

/// Lower case, with the capture and check marks that carry no meaning dropped.
fn bare_form(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() && !matches!(c, 'x' | 'X'))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Every command and what it does, in the order the help lists them.
const COMMANDS: [(&str, &str); 27] = [
    ("help", "this list"),
    ("board", "redraw the board"),
    ("flip", "turn the board around"),
    ("moves", "list the legal moves"),
    ("history", "the moves so far"),
    ("pgn", "the game as PGN"),
    ("export", "write a PGN file"),
    ("import", "open a PGN file"),
    ("save", "save this game"),
    ("load", "restore a game"),
    ("fen", "the position as FEN"),
    ("eval", "the engine's opinion"),
    ("hint", "ask for a suggestion"),
    ("undo", "take back a move"),
    ("pause", "pause or resume"),
    ("draw", "offer or accept a draw"),
    ("time", "seconds per move"),
    ("depth", "search depth instead"),
    ("theme", "board colours"),
    ("pieces", "drawn, or figurines"),
    ("sound", "auto, on, off, or test"),
    ("size", "fill the window, or not"),
    ("name", "set a player name"),
    ("setup", "local files and setup"),
    ("new", "start again"),
    ("resign", "concede the game"),
    ("quit", "leave"),
];

/// Every move names the rank it ends on, so a word with no digit in it was
/// meant as a command - castling, which names no square, aside.
fn looks_like_command(word: &str) -> bool {
    let castling = matches!(
        word.replace('0', "o").as_str(),
        "o-o" | "oo" | "o-o-o" | "ooo"
    );
    !castling && !word.chars().any(|c| c.is_ascii_digit())
}

/// The command `word` was probably a misspelling of.
fn command_guess(word: &str) -> Option<&'static str> {
    let names = COMMANDS.iter().map(|(name, _)| *name).chain(["exit"]);
    let mut best: Option<(usize, &'static str)> = None;
    for name in names {
        let distance = edit_distance(word, name);
        if distance <= 2 && best.map_or(true, |(d, _)| distance < d) {
            best = Some((distance, name));
        }
    }
    best.map(|(_, name)| name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

// ---------------------------------------------------------------------------
// Pages printed on request
// ---------------------------------------------------------------------------

fn help_lines(theme: &Theme) -> Vec<String> {
    let mut lines = vec![
        theme.bold("MOUSE"),
        format!(
            "  {}",
            theme.dim("Click a piece, then its highlighted target. Escape cancels.")
        ),
        format!(
            "  {}",
            theme.dim("Use action buttons directly; scroll the move list with the mouse wheel.")
        ),
        format!(
            "  {}",
            theme.dim("Page Up / Page Down also scroll the move list.")
        ),
        String::new(),
        theme.bold("KEYBOARD"),
        format!(
            "  {}",
            theme.dim("Tab or arrows move focus; Shift+Tab goes back; Enter selects.")
        ),
        format!(
            "  {}",
            theme.dim("Start typing at any time to focus the move box.")
        ),
        String::new(),
        theme.bold("MOVES"),
        format!(
            "  {}   {}",
            theme.accent("e4  Nf3  exd5  O-O  e8=Q"),
            theme.dim("standard notation")
        ),
        format!(
            "  {}                {}",
            theme.accent("e2e4  e7e8q"),
            theme.dim("coordinates; a promotion defaults to a queen")
        ),
        String::new(),
        theme.bold("COMMANDS"),
    ];
    // Two columns, so the list stays one glance rather than one scroll.
    let half = (COMMANDS.len() + 1) / 2;
    for row in 0..half {
        let mut line = String::new();
        for column in [row, row + half] {
            if let Some((name, what)) = COMMANDS.get(column) {
                let cell = format!(
                    "{}  {}",
                    theme.accent(&format!("{:<8}", name)),
                    theme.dim(what)
                );
                line.push_str(&ui::pad(&cell, 36));
            }
        }
        lines.push(format!("  {}", line.trim_end()));
    }
    lines.push(String::new());
    lines.push(theme.dim("  `moves e2` points at one piece on the board"));
    lines.push(
        theme.dim(
            "  `time 5`, `depth 8`, `theme wood`, `pieces art`, `size small` all take a value",
        ),
    );
    lines.push(
        theme.dim("  `save`, `load`, `import game.pgn` and `export game.pgn` keep games local"),
    );
    lines.push(theme.dim("  Scores are in pawns, always from White's point of view."));
    lines
}

fn moves_lines(theme: &Theme, pos: &Position) -> Vec<String> {
    let legal = generate_legal(pos);
    let groups = [
        (PieceKind::Pawn, "Pawns"),
        (PieceKind::Knight, "Knights"),
        (PieceKind::Bishop, "Bishops"),
        (PieceKind::Rook, "Rooks"),
        (PieceKind::Queen, "Queen"),
        (PieceKind::King, "King"),
    ];
    let mut lines = Vec::new();
    for (kind, label) in groups {
        let mut moves: Vec<String> = legal
            .iter()
            .filter(|mv| pos.at(mv.from).map(|piece| piece.kind) == Some(kind))
            .map(|&mv| to_san_with(pos, mv, &legal))
            .collect();
        moves.sort();
        if moves.is_empty() {
            continue;
        }
        for (i, line) in wrap(&moves, 56, "").into_iter().enumerate() {
            let head = if i == 0 { label } else { "" };
            lines.push(format!(
                "{}{}",
                theme.label(&format!("{:<9}", head)),
                theme.accent(&line)
            ));
        }
    }
    lines.push(String::new());
    lines.push(theme.dim(&format!(
        "{} legal moves  \u{b7}  `moves e2` points at the board instead",
        legal.len()
    )));
    lines
}

/// Mark where one piece can go, on the board, where the question was asked.
fn show_piece_moves(screen: &mut Screen, pos: &Position, filter: &str) {
    let from = match board::parse_square(&filter.to_ascii_lowercase()) {
        Some(from) => from,
        None => {
            let text = screen.theme.warn(&format!("`{}` is not a square.", filter));
            screen.note(text);
            return;
        }
    };
    let legal = generate_legal(pos);
    let moves: Vec<Move> = legal.iter().copied().filter(|mv| mv.from == from).collect();
    let square = board::square_name(from);
    if moves.is_empty() {
        let why = match pos.at(from) {
            None => format!("There is nothing on {}.", square),
            Some(piece) if piece.color != pos.side => format!(
                "The {} on {} is {}'s, and it is {} to move.",
                kind_name(piece.kind),
                square,
                piece.color.name(),
                pos.side.name()
            ),
            Some(piece) => format!(
                "The {} on {} has nowhere to go.",
                kind_name(piece.kind),
                square
            ),
        };
        let text = screen.theme.dim(&why);
        screen.note(text);
        return;
    }

    let mut sans: Vec<String> = moves
        .iter()
        .map(|&mv| to_san_with(pos, mv, &legal))
        .collect();
    sans.sort();
    let name = pos.at(from).map_or("piece", |piece| kind_name(piece.kind));
    let mut message = vec![screen.theme.dim(&format!(
        "The {} on {} has {} move{}:",
        name,
        square,
        sans.len(),
        if sans.len() == 1 { "" } else { "s" }
    ))];
    message.extend(
        wrap(&sans, 60, "")
            .iter()
            .map(|line| screen.theme.accent(line)),
    );
    screen.targets = moves.iter().map(|mv| mv.to).collect();
    screen.captures = moves
        .iter()
        .filter(|mv| mv.kind == MoveKind::EnPassant || pos.at(mv.to).is_some())
        .map(|mv| mv.to)
        .collect();
    screen.selected = Some(from);
    screen.show(message);
}

fn history_page(theme: &Theme, game: &Game) -> Vec<String> {
    let played = history_lines(game);
    if played.is_empty() {
        return vec![theme.dim("No moves played yet.")];
    }
    // Two columns of move pairs, oldest first.
    let half = (played.len() + 1) / 2;
    (0..half)
        .map(|row| {
            let mut line = String::new();
            for column in [row, row + half] {
                if let Some(text) = played.get(column) {
                    line.push_str(&format!("{:<24}", text));
                }
            }
            theme.accent(line.trim_end())
        })
        .collect()
}

/// Left plain on purpose: PGN is for pasting somewhere else, and escape codes
/// would go with it.
fn pgn_lines(game: &Game, names: &[String; 2]) -> Vec<String> {
    let mut lines = vec![
        "[Event \"Casual game\"]".to_string(),
        "[Site \"Terminal\"]".to_string(),
        "[Date \"????.??.??\"]".to_string(),
        format!("[White \"{}\"]", pgn_escape(&names[Color::White.index()])),
        format!("[Black \"{}\"]", pgn_escape(&names[Color::Black.index()])),
        format!("[Result \"{}\"]", score_tag(game)),
    ];
    if game.start.to_fen() != board::START_FEN {
        lines.push("[SetUp \"1\"]".to_string());
        lines.push(format!("[FEN \"{}\"]", game.start.to_fen()));
    }
    lines.push(String::new());

    let mut words: Vec<String> = Vec::new();
    let mut number = game.start.fullmove;
    let mut side = game.start.side;
    for text in &game.sans {
        if side == Color::White {
            words.push(format!("{}.", number));
        } else if words.is_empty() {
            words.push(format!("{}...", number));
        }
        words.push(text.clone());
        if side == Color::Black {
            number += 1;
        }
        side = side.flip();
    }
    words.push(score_tag(game).to_string());
    lines.extend(wrap(&words, 66, ""));
    lines
}

fn pgn_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn load_saved_game(
    path: &Path,
    game: &mut Game,
    mode: &mut Mode,
    screen: &mut Screen,
) -> Result<(), String> {
    let (loaded, loaded_mode) = restore_game(storage::load_game(path)?)?;
    *game = loaded;
    *mode = loaded_mode;
    screen.clear_marks();
    screen.page = None;
    screen.note(screen.theme.good(&format!("Restored {}.", path.display())));
    Ok(())
}

fn import_pgn(path: &Path, game: &mut Game, screen: &mut Screen) -> Result<(), String> {
    let imported = storage::read_pgn(path)?;
    let start = match imported.start_fen {
        Some(fen) => Position::from_fen(&fen)
            .map_err(|error| format!("PGN starting position is invalid: {error}"))?,
        None => Position::startpos(),
    };
    let mut candidate = Game::with_clock(start, game.clock.initial, game.clock.increment);
    for (index, notation) in imported.moves.iter().enumerate() {
        let movement = parse_move(&candidate.pos, notation).map_err(|_| {
            format!(
                "PGN move {} (`{}`) is not legal in its position",
                index + 1,
                notation
            )
        })?;
        candidate.play(movement);
    }
    candidate.revision = game.revision.wrapping_add(1);
    *game = candidate;
    screen.clear_marks();
    screen.page = None;
    screen.note(screen.theme.good(&format!(
        "Imported {} moves from {}.",
        game.sans.len(),
        path.display()
    )));
    Ok(())
}

fn export_pgn(path: &Path, game: &Game, names: &[String; 2]) -> Result<(), String> {
    let mut text = pgn_lines(game, names).join("\n");
    text.push('\n');
    storage::write_text(path, &text)
}

fn set_player_name(screen: &mut Screen, rest: &str) {
    let Some((side, name)) = rest.split_once(char::is_whitespace) else {
        screen.note(
            screen
                .theme
                .dim("Use `name white Lakshay` or `name black Guest`."),
        );
        return;
    };
    let Some(color) = color_named(side) else {
        screen.note(
            screen
                .theme
                .warn("Choose `white` or `black` before the name."),
        );
        return;
    };
    let name: String = name
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(32)
        .collect();
    if name.is_empty() {
        screen.note(screen.theme.warn("A player name cannot be empty."));
        return;
    }
    screen.player_names[color.index()] = name.clone();
    screen.note(
        screen
            .theme
            .good(&format!("{} is now {}.", color.name(), name)),
    );
}

fn setup_lines(theme: &Theme, config: &Path, session: &Path) -> Vec<String> {
    vec![
        theme.bold("YOUR LOCAL GAME"),
        theme.dim("Moves, clicks, clocks and sounds work without a browser."),
        String::new(),
        format!("{}  {}", theme.label("Preferences"), config.display()),
        format!("{}  {}", theme.label("Autosave"), session.display()),
        String::new(),
        theme.bold("QUICK SETUP"),
        format!("  {}", theme.accent("name white Lakshay")),
        format!(
            "  {}",
            theme.accent("theme forest  ·  pieces glyph  ·  sound on")
        ),
        format!("  {}", theme.accent("pause  ·  save  ·  load")),
        String::new(),
        theme.dim("Preferences are saved automatically. Edit config.toml when the game is closed."),
        theme.dim("Press Escape or Return to go back to the board."),
    ]
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// `Ok(None)` at end of input.
fn read_line(stdin: &mut io::StdinLock, prompt: &str) -> Result<Option<String>, String> {
    print!("{}", prompt);
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    match stdin.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(line)),
        Err(e) => Err(format!("could not read input: {}", e)),
    }
}

fn ask_mode(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    can_resume: bool,
) -> Result<Option<StartChoice>, String> {
    screen.measure();
    let mut complaint = String::new();
    loop {
        let theme = &screen.theme;
        let mut block = vec![
            theme.strong(theme.palette.accent, "C H E S S"),
            theme.rule(32),
            String::new(),
            format!(
                "  {}   play as White   {}",
                theme.bold("1"),
                theme.dim("(default)")
            ),
            format!("  {}   play as Black", theme.bold("2")),
            format!("  {}   two players at one keyboard", theme.bold("3")),
        ];
        if can_resume {
            block.push(format!("  {}   resume saved game", theme.bold("4")));
        }
        block.push(String::new());
        block.push(theme.dim("  q   leave"));
        if !complaint.is_empty() {
            block.push(String::new());
            block.push(complaint.clone());
        }
        // Sitting the menu in the middle of the window says, before a single
        // move is played, that the whole window is what the game is drawn on.
        let widest = block.iter().map(|line| ui::width(line)).max().unwrap_or(0);
        let left = " ".repeat(screen.cols.saturating_sub(widest) / 2);
        theme.clear();
        let above = if theme.live {
            screen.rows.saturating_sub(block.len() + 4) / 2
        } else {
            1
        };
        for _ in 0..above {
            println!();
        }
        for line in &block {
            if line.is_empty() {
                println!();
            } else {
                println!("{}{}", left, line);
            }
        }
        println!();
        let arrow = if theme.ascii { ">" } else { "\u{203a}" };
        let prompt = format!("{}  {} ", left, theme.dim(arrow));
        let line = match read_line(stdin, &prompt)? {
            Some(line) => line,
            None => {
                println!();
                return Ok(None);
            }
        };
        match line.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "w" | "white" => return Ok(Some(StartChoice::Mode(Mode::HumanWhite))),
            "2" | "b" | "black" => return Ok(Some(StartChoice::Mode(Mode::HumanBlack))),
            "3" | "t" | "two" => return Ok(Some(StartChoice::Mode(Mode::TwoPlayer))),
            "4" | "r" | "resume" if can_resume => return Ok(Some(StartChoice::Resume)),
            "q" | "quit" | "exit" => return Ok(None),
            _ => {
                complaint = theme.warn(if can_resume {
                    "  Choose 1, 2, 3 or 4."
                } else {
                    "  Choose 1, 2 or 3."
                })
            }
        }
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;

    fn hitbox(flipped: bool) -> BoardHitbox {
        BoardHitbox {
            left: 10,
            top: 4,
            cell_w: 8,
            cell_h: 4,
            flipped,
        }
    }

    fn screen() -> Screen {
        Screen {
            theme: Theme::new(true, false, false, ui::THEMES[0].1),
            sound: sound::Player::new(sound::Mode::Off),
            inline_images: false,
            flipped: false,
            cols: 100,
            rows: 40,
            metrics: ui::Metrics::COMPACT,
            wide_panel: true,
            pieces: ui::Pieces::Glyph,
            player_names: ["Player 1".to_string(), "Player 2".to_string()],
            compact: false,
            indent: String::new(),
            board_hitbox: None,
            body_top: 0,
            action_hitboxes: Vec::new(),
            focused: UiAction::MoveInput,
            history_offset: 0,
            history_capacity: 0,
            confirming: None,
            analysis: Vec::new(),
            message: Vec::new(),
            selected: None,
            targets: Vec::new(),
            captures: Vec::new(),
            invalid: None,
            promotions: Vec::new(),
            page: None,
            last_frame: Vec::new(),
            last_size: None,
            inline_drawn: false,
            last_inline_board: None,
            last_prompt: None,
            redraw: false,
            online: None,
        }
    }

    #[test]
    fn board_hitbox_maps_white_orientation() {
        let board = hitbox(false);
        assert_eq!(board.square_at(10, 4), Some(board::H8 - 7)); // a8
        assert_eq!(board.square_at(73, 4), Some(board::H8));
        assert_eq!(board.square_at(10, 35), Some(board::A1));
        assert_eq!(board.square_at(73, 35), Some(board::H1));
    }

    #[test]
    fn board_hitbox_maps_flipped_orientation() {
        let board = hitbox(true);
        assert_eq!(board.square_at(10, 4), Some(board::H1));
        assert_eq!(board.square_at(73, 4), Some(board::A1));
        assert_eq!(board.square_at(10, 35), Some(board::H8));
        assert_eq!(board.square_at(73, 35), Some(board::H8 - 7)); // a8
    }

    #[test]
    fn board_hitbox_rejects_labels_and_outside_cells() {
        let board = hitbox(false);
        assert_eq!(board.square_at(9, 4), None);
        assert_eq!(board.square_at(10, 3), None);
        assert_eq!(board.square_at(74, 4), None);
        assert_eq!(board.square_at(10, 36), None);
    }

    #[test]
    fn clicking_a_piece_then_a_target_plays_the_move() {
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();
        let e2 = board::parse_square("e2").unwrap();
        let e3 = board::parse_square("e3").unwrap();
        let e4 = board::parse_square("e4").unwrap();

        handle_board_click(&mut game, &mut screen, Some(e2), false);
        assert_eq!(screen.selected, Some(e2));
        assert!(screen.targets.contains(&e3));
        assert!(screen.targets.contains(&e4));

        handle_board_click(&mut game, &mut screen, Some(e4), false);
        assert_eq!(game.pos.at(e2), None);
        assert_eq!(
            game.pos.at(e4),
            Some(Piece::new(Color::White, PieceKind::Pawn))
        );
        assert_eq!(game.pos.side, Color::Black);
        assert_eq!(screen.selected, None);
        assert!(screen.targets.is_empty());
    }

    #[test]
    fn clicking_another_friendly_piece_reselects() {
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();
        let e2 = board::parse_square("e2").unwrap();
        let g1 = board::parse_square("g1").unwrap();
        let f3 = board::parse_square("f3").unwrap();

        handle_board_click(&mut game, &mut screen, Some(e2), false);
        handle_board_click(&mut game, &mut screen, Some(g1), false);
        assert_eq!(screen.selected, Some(g1));
        assert_eq!(screen.targets.len(), 2);
        assert!(screen.targets.contains(&f3));
        assert!(screen.targets.contains(&board::parse_square("h3").unwrap()));
    }

    #[test]
    fn capture_targets_are_distinct_from_quiet_moves() {
        let position = Position::from_fen("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1").unwrap();
        let mut game = Game::new(position);
        let mut screen = screen();
        let e4 = board::parse_square("e4").unwrap();
        let d5 = board::parse_square("d5").unwrap();
        let e5 = board::parse_square("e5").unwrap();

        handle_board_click(&mut game, &mut screen, Some(e4), false);

        assert!(screen.targets.contains(&d5));
        assert!(screen.targets.contains(&e5));
        assert_eq!(screen.captures, vec![d5]);
    }

    #[test]
    fn invalid_destination_gets_local_feedback() {
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();
        let e2 = board::parse_square("e2").unwrap();
        let e5 = board::parse_square("e5").unwrap();

        handle_board_click(&mut game, &mut screen, Some(e2), false);
        handle_board_click(&mut game, &mut screen, Some(e5), false);

        assert_eq!(screen.invalid, Some(e5));
        assert_eq!(screen.selected, Some(e2));
        assert!(!screen.message.is_empty());
    }

    #[test]
    fn promotion_menu_accepts_a_piece_click() {
        let position = Position::from_fen("4k3/P7/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        let mut game = Game::new(position);
        let mut screen = screen();
        let a7 = board::parse_square("a7").unwrap();
        let a8 = board::parse_square("a8").unwrap();
        let bishop_choice = board::parse_square("a6").unwrap();

        handle_board_click(&mut game, &mut screen, Some(a7), false);
        handle_board_click(&mut game, &mut screen, Some(a8), false);
        assert_eq!(screen.promotions.len(), 4);

        handle_board_click(&mut game, &mut screen, Some(bishop_choice), false);
        assert_eq!(
            game.pos.at(a8),
            Some(Piece::new(Color::White, PieceKind::Bishop))
        );
        assert!(screen.promotions.is_empty());
    }

    fn cue_after(fen: &str, notation: &str) -> sound::Cue {
        let mut game = Game::new(Position::from_fen(fen).unwrap());
        let movement = match parse_move(&game.pos, notation) {
            Ok(movement) => movement,
            Err(_) => panic!("test move {notation} did not parse"),
        };
        game.play(movement);
        sound_after_move(&game)
    }

    #[test]
    fn moves_choose_their_most_meaningful_sound() {
        assert_eq!(cue_after(board::START_FEN, "e4"), sound::Cue::Move);
        assert_eq!(
            cue_after("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "exd5"),
            sound::Cue::Capture
        );
        assert_eq!(
            cue_after("4k3/8/8/8/8/8/4R3/4K3 w - - 0 1", "Re7+"),
            sound::Cue::Check
        );
        assert_eq!(
            cue_after("4k3/8/8/8/8/8/8/4K2R w K - 0 1", "O-O"),
            sound::Cue::Castle
        );
        assert_eq!(
            cue_after("8/P6k/8/8/8/8/8/4K3 w - - 0 1", "a8=Q"),
            sound::Cue::Promotion
        );
    }

    #[test]
    fn clock_flags_the_side_that_runs_out_of_time() {
        let mut clock = GameClock::new(Some(Duration::from_secs(60)), Duration::ZERO, Color::White);
        clock.remaining[Color::White.index()] = Duration::from_millis(10);
        clock.running = Some((Color::White, Instant::now() - Duration::from_millis(20)));

        assert!(clock.tick());
        assert_eq!(clock.flagged, Some(Color::White));
        assert_eq!(clock.format(Color::White), "0:00");
    }

    #[test]
    fn clock_adds_increment_and_switches_sides() {
        let mut clock = GameClock::new(
            Some(Duration::from_secs(60)),
            Duration::from_secs(2),
            Color::White,
        );
        clock.complete_move(Color::White, Color::Black);

        assert!(clock.remaining[Color::White.index()] > Duration::from_secs(61));
        assert_eq!(clock.running.map(|(color, _)| color), Some(Color::Black));
    }

    #[test]
    fn pausing_stops_and_resumes_the_active_clock() {
        let mut game = Game::with_clock(
            Position::startpos(),
            Some(Duration::from_secs(60)),
            Duration::ZERO,
        );

        game.toggle_pause();
        assert!(game.paused);
        assert!(game.clock.running.is_none());

        game.toggle_pause();
        assert!(!game.paused);
        assert_eq!(
            game.clock.running.map(|(color, _)| color),
            Some(Color::White)
        );
    }

    #[test]
    fn saved_game_round_trips_moves_clock_mode_and_pause() {
        let mut game = Game::with_clock(
            Position::startpos(),
            Some(Duration::from_secs(300)),
            Duration::from_secs(2),
        );
        for notation in ["e4", "e5", "Nf3"] {
            let movement = parse_move(&game.pos, notation).ok().unwrap();
            game.play(movement);
        }
        game.toggle_pause();

        let saved = saved_game(&mut game, Mode::TwoPlayer);
        let (restored, mode) = restore_game(saved).unwrap();

        assert_eq!(mode, Mode::TwoPlayer);
        assert_eq!(restored.pos.to_fen(), game.pos.to_fen());
        assert_eq!(restored.sans, game.sans);
        assert!(restored.paused);
        assert_eq!(restored.clock.increment, Duration::from_secs(2));
        assert!(restored.clock.running.is_none());
    }

    #[test]
    fn pgn_import_replays_the_main_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.pgn");
        std::fs::write(
            &path,
            "[Event \"Test\"]\n\n1. e4 e5 2. Nf3 (2. Bc4) Nc6 *\n",
        )
        .unwrap();
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();

        import_pgn(&path, &mut game, &mut screen).unwrap();

        assert_eq!(game.sans, ["e4", "e5", "Nf3", "Nc6"]);
        assert_eq!(game.pos.side, Color::White);
    }

    #[test]
    fn draw_offer_can_be_accepted_after_the_offering_move() {
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();
        screen.theme.live = true;

        assert!(!handle_ui_action(
            UiAction::Draw,
            &mut game,
            Mode::TwoPlayer,
            &mut screen,
        ));
        assert_eq!(game.draw_offer, Some(Color::White));

        let movement = parse_move(&game.pos, "e4").ok().unwrap();
        game.play(movement);
        handle_ui_action(UiAction::Draw, &mut game, Mode::TwoPlayer, &mut screen);
        assert!(game.agreed_draw);
        assert!(matches!(outcome(&game), Some(Outcome::DrawAgreement)));
    }

    #[test]
    fn destructive_mouse_action_requires_confirmation() {
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();
        screen.theme.live = true;

        handle_ui_action(UiAction::Resign, &mut game, Mode::TwoPlayer, &mut screen);
        assert_eq!(screen.confirming, Some(UiAction::Resign));
        assert_eq!(game.resigned, None);

        handle_ui_action(UiAction::Resign, &mut game, Mode::TwoPlayer, &mut screen);
        assert_eq!(game.resigned, Some(Color::White));
    }

    #[test]
    fn compact_buttons_wrap_without_losing_hit_targets() {
        let game = Game::new(Position::startpos());
        let screen = screen();
        let buttons = screen.render_buttons(&screen.game_buttons(&game, Mode::TwoPlayer), 24);

        assert!(buttons.lines.len() >= 2);
        assert_eq!(buttons.actions.len(), 7); // Undo starts disabled.
        assert!(buttons
            .actions
            .iter()
            .all(|target| target.left + target.width <= 24));
    }

    #[test]
    fn short_side_panel_gives_controls_priority_over_history() {
        let game = Game::new(Position::startpos());
        let screen = screen();
        let panel = screen.panel(&game, Mode::TwoPlayer, &Limits::default(), 10, 24);

        assert_eq!(panel.lines.len(), 10);
        assert_eq!(panel.history_capacity, 0);
        assert!(panel.actions.iter().all(|target| target.row < 8));
    }

    #[test]
    fn keyboard_focus_follows_the_visible_control_order() {
        let mut screen = screen();
        screen.action_hitboxes = vec![
            ActionHitbox {
                left: 0,
                top: 0,
                width: 8,
                action: UiAction::MoveInput,
            },
            ActionHitbox {
                left: 10,
                top: 0,
                width: 8,
                action: UiAction::Draw,
            },
            ActionHitbox {
                left: 20,
                top: 0,
                width: 12,
                action: UiAction::ToggleSize,
            },
        ];

        screen.move_focus(false);
        assert_eq!(screen.focused, UiAction::Draw);
        screen.move_focus(false);
        assert_eq!(screen.focused, UiAction::ToggleSize);
        screen.move_focus(false);
        assert_eq!(screen.focused, UiAction::MoveInput);
        screen.move_focus(true);
        assert_eq!(screen.focused, UiAction::ToggleSize);
    }

    #[test]
    fn view_controls_cycle_without_a_typed_command() {
        let mut screen = screen();
        assert!(!screen.compact);
        assert_eq!(screen.pieces, ui::Pieces::Glyph);

        toggle_size(&mut screen);
        cycle_pieces(&mut screen);

        assert!(screen.compact);
        assert_eq!(screen.pieces, ui::Pieces::Art);
        assert!(screen.redraw);
    }

    #[test]
    fn move_history_scrolls_away_from_and_back_to_the_latest_move() {
        let mut game = Game::new(Position::startpos());
        for notation in ["e4", "e5", "Nf3", "Nc6"] {
            let movement = parse_move(&game.pos, notation).ok().unwrap();
            game.play(movement);
        }
        let mut screen = screen();
        screen.history_capacity = 1;

        screen.scroll_history(&game, true);
        assert_eq!(screen.history_offset, 1);
        screen.scroll_history(&game, false);
        assert_eq!(screen.history_offset, 0);
    }

    #[test]
    fn frame_diff_only_repaints_changed_rows() {
        let previous = vec![
            "title".to_string(),
            "board".to_string(),
            "status".to_string(),
        ];
        let next = vec![
            "title".to_string(),
            "board".to_string(),
            "new status".to_string(),
        ];

        assert_eq!(changed_frame_rows(&previous, &next, false), vec![2]);
        assert_eq!(changed_frame_rows(&previous, &next, true), vec![0, 1, 2]);
    }

    #[test]
    fn clock_repaint_restores_the_prompt_cursor() {
        let mut screen = screen();
        screen.cols = 80;
        screen.rows = 32;
        screen.last_prompt = Some("    White › e4".to_string());

        assert_eq!(screen.prompt_cursor_escape(), "\x1b[32;15H\x1b[?25h");

        screen.focused = UiAction::Draw;
        assert_eq!(screen.prompt_cursor_escape(), "\x1b[?25l");
    }

    #[test]
    fn online_cli_parses_create_join_and_server_url() {
        let preferences = storage::Preferences::default();
        let create = Options::parse(
            [
                "online",
                "create",
                "--server",
                "wss://play.example/ws",
                "--name",
                "Ada",
            ]
            .into_iter()
            .map(str::to_string),
            &preferences,
        )
        .unwrap()
        .unwrap();
        assert_eq!(create.online, Some(OnlineIntent::Create));
        assert_eq!(create.server_url, "wss://play.example/ws");
        assert_eq!(create.online_name, "Ada");

        let join = Options::parse(
            ["online", "join", "ab12cd"].into_iter().map(str::to_string),
            &preferences,
        )
        .unwrap()
        .unwrap();
        assert_eq!(join.online, Some(OnlineIntent::Join("AB12CD".to_string())));
    }

    #[test]
    fn online_click_builds_a_request_without_advancing_locally() {
        let game = Game::new(Position::startpos());
        let mut screen = screen();
        let e2 = board::parse_square("e2").unwrap();
        let e4 = board::parse_square("e4").unwrap();

        assert!(online_board_click(&game, &mut screen, Some(e2)).is_none());
        let movement = online_board_click(&game, &mut screen, Some(e4)).unwrap();

        assert_eq!(movement.to_uci(), "e2e4");
        assert_eq!(
            game.pos.at(e2),
            Some(Piece::new(Color::White, PieceKind::Pawn))
        );
        assert_eq!(game.pos.at(e4), None);
    }
}
