//! A chess game for the terminal: draw a board, read a move, answer with one.

mod board;
mod eval;
mod input;
mod movegen;
mod san;
mod search;
mod sound;
mod ui;

use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use board::{Color, Move, MoveKind, Piece, PieceKind, Position, Undo};
use input::{Action as InputAction, TerminalInput};
use movegen::{generate_legal, in_check};
use san::{parse_move, to_san, to_san_with, ParseError};
use search::{Limits, Search, SearchResult};
use ui::{BoardView, Theme};

fn main() {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(message) => {
            eprintln!("chess: {}", message);
            eprintln!("Try `chess --help`.");
            std::process::exit(2);
        }
    };
    if let Err(message) = play(options) {
        eprintln!("chess: {}", message);
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The human has White, the engine answers as Black.
    HumanWhite,
    HumanBlack,
    /// Two people sharing the keyboard; the engine only gives hints.
    TwoPlayer,
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
    /// Each player's starting time. `None` is an untimed game.
    clock: Option<Duration>,
    increment: Duration,
}

const HELP: &str = "\
chess - play chess in your terminal

USAGE:
    chess [OPTIONS]

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
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
        let mut options = Options {
            mode: None,
            fen: None,
            limits: Limits::default(),
            ascii: false,
            color: None,
            palette: ui::THEMES[0].1,
            pieces: ui::Pieces::Auto,
            compact: false,
            sound: sound::Mode::Auto,
            clock: Some(Duration::from_secs(10 * 60)),
            increment: Duration::ZERO,
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
                other => return Err(format!("unknown option '{}'", other)),
            }
        }
        // `--time` after `--depth` has to put the clock back.
        if timed && options.limits.movetime.is_none() {
            options.limits.movetime = Some(Limits::default().movetime.unwrap());
        }
        Ok(Some(options))
    }
}

// ---------------------------------------------------------------------------
// Game state
// ---------------------------------------------------------------------------

struct GameClock {
    initial: Option<Duration>,
    increment: Duration,
    remaining: [Duration; 2],
    running: Option<(Color, Instant)>,
    flagged: Option<Color>,
    shown_seconds: [u64; 2],
}

impl GameClock {
    fn new(initial: Option<Duration>, increment: Duration, side: Color) -> GameClock {
        let time = initial.unwrap_or(Duration::ZERO);
        let seconds = shown_seconds(time);
        GameClock {
            initial,
            increment,
            remaining: [time; 2],
            running: initial.map(|_| (side, Instant::now())),
            flagged: None,
            shown_seconds: [seconds; 2],
        }
    }

    fn tick(&mut self) -> bool {
        let flagged_before = self.flagged;
        self.sync();
        let shown = [
            shown_seconds(self.remaining[Color::White.index()]),
            shown_seconds(self.remaining[Color::Black.index()]),
        ];
        let changed = shown != self.shown_seconds || self.flagged != flagged_before;
        self.shown_seconds = shown;
        changed
    }

    fn complete_move(&mut self, mover: Color, next: Color) {
        self.sync();
        if self.flagged.is_some() || self.initial.is_none() {
            return;
        }
        self.remaining[mover.index()] += self.increment;
        self.running = Some((next, Instant::now()));
        self.remember_shown();
    }

    fn pause(&mut self) {
        self.sync();
        self.running = None;
        self.remember_shown();
    }

    fn resume(&mut self, side: Color) {
        if self.initial.is_none() {
            return;
        }
        self.flagged = None;
        self.running = Some((side, Instant::now()));
        self.remember_shown();
    }

    fn reset(&mut self, side: Color) {
        let time = self.initial.unwrap_or(Duration::ZERO);
        self.remaining = [time; 2];
        self.flagged = None;
        self.running = self.initial.map(|_| (side, Instant::now()));
        self.remember_shown();
    }

    fn remaining(&self, color: Color) -> Option<Duration> {
        self.initial?;
        let stored = self.remaining[color.index()];
        match self.running {
            Some((running, since)) if running == color => {
                Some(stored.saturating_sub(since.elapsed()))
            }
            _ => Some(stored),
        }
    }

    fn format(&self, color: Color) -> String {
        let Some(remaining) = self.remaining(color) else {
            return "UNTIMED".to_string();
        };
        let total = shown_seconds(remaining);
        if total >= 60 {
            format!("{}:{:02}", total / 60, total % 60)
        } else {
            format!("0:{:02}", total)
        }
    }

    fn low(&self, color: Color) -> bool {
        self.remaining(color)
            .is_some_and(|time| time <= Duration::from_secs(30))
    }

    fn sync(&mut self) {
        let Some((color, since)) = self.running else {
            return;
        };
        let remaining = &mut self.remaining[color.index()];
        *remaining = remaining.saturating_sub(since.elapsed());
        if remaining.is_zero() {
            self.flagged = Some(color);
            self.running = None;
        } else {
            self.running = Some((color, Instant::now()));
        }
    }

    fn remember_shown(&mut self) {
        self.shown_seconds = [
            shown_seconds(self.remaining[Color::White.index()]),
            shown_seconds(self.remaining[Color::Black.index()]),
        ];
    }
}

fn shown_seconds(time: Duration) -> u64 {
    time.as_millis().div_ceil(1000).min(u64::MAX as u128) as u64
}

struct Game {
    pos: Position,
    /// The position every `new` returns to.
    start: Position,
    undos: Vec<Undo>,
    /// SAN of each move played, for the move list.
    sans: Vec<String>,
    /// Zobrist key of every position reached, starting with `start`. The last
    /// entry is always the current position.
    hashes: Vec<u64>,
    /// Set when someone gives up. Nothing else ends a game early.
    resigned: Option<Color>,
    /// A draw offer remains live until the opponent accepts or plays a move.
    draw_offer: Option<Color>,
    agreed_draw: bool,
    clock: GameClock,
}

impl Game {
    #[cfg(test)]
    fn new(start: Position) -> Game {
        Game::with_clock(start, None, Duration::ZERO)
    }

    fn with_clock(start: Position, initial: Option<Duration>, increment: Duration) -> Game {
        let side = start.side;
        Game {
            pos: start.clone(),
            hashes: vec![start.hash],
            start,
            undos: Vec::new(),
            sans: Vec::new(),
            resigned: None,
            draw_offer: None,
            agreed_draw: false,
            clock: GameClock::new(initial, increment, side),
        }
    }

    fn play(&mut self, mv: Move) -> String {
        let mover = self.pos.side;
        if self.draw_offer == Some(mover.flip()) {
            self.draw_offer = None;
        }
        let text = to_san(&self.pos, mv);
        let undo = self.pos.make_move(mv);
        self.undos.push(undo);
        self.sans.push(text.clone());
        self.hashes.push(self.pos.hash);
        self.clock.complete_move(mover, self.pos.side);
        text
    }

    fn take_back(&mut self) -> Option<String> {
        let undo = self.undos.pop()?;
        self.pos.unmake_move(undo);
        self.hashes.pop();
        self.resigned = None;
        self.draw_offer = None;
        self.agreed_draw = false;
        self.clock.resume(self.pos.side);
        self.sans.pop()
    }

    fn restart(&mut self) {
        self.pos = self.start.clone();
        self.undos.clear();
        self.sans.clear();
        self.hashes = vec![self.start.hash];
        self.resigned = None;
        self.draw_offer = None;
        self.agreed_draw = false;
        self.clock.reset(self.pos.side);
    }

    fn last_move(&self) -> Option<Move> {
        self.undos.last().map(|undo| undo.mv)
    }

    /// The pieces `color` has taken, which are the enemy pieces in the undo log.
    fn captured_by(&self, color: Color) -> Vec<Piece> {
        let mut taken: Vec<Piece> = self
            .undos
            .iter()
            .filter_map(|undo| undo.captured)
            .filter(|piece| piece.color != color)
            .collect();
        taken.sort_by_key(|piece| std::cmp::Reverse(pawns_worth(piece.kind)));
        taken
    }

    /// Everything the search needs for repetition detection: the positions
    /// before the current one.
    fn prior_positions(&self) -> &[u64] {
        &self.hashes[..self.hashes.len() - 1]
    }
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

enum Outcome {
    /// Carries the side that delivered mate.
    Checkmate(Color),
    /// Carries the side that gave up.
    Resignation(Color),
    Timeout(Color),
    DrawAgreement,
    Stalemate,
    FiftyMove,
    Threefold,
    Insufficient,
}

fn outcome(game: &Game) -> Option<Outcome> {
    if let Some(color) = game.resigned {
        return Some(Outcome::Resignation(color));
    }
    if game.agreed_draw {
        return Some(Outcome::DrawAgreement);
    }
    if let Some(color) = game.clock.flagged {
        return Some(Outcome::Timeout(color));
    }
    let pos = &game.pos;
    if generate_legal(pos).is_empty() {
        return Some(if in_check(pos, pos.side) {
            Outcome::Checkmate(pos.side.flip())
        } else {
            Outcome::Stalemate
        });
    }
    if pos.halfmove >= 100 {
        return Some(Outcome::FiftyMove);
    }
    if game.hashes.iter().filter(|&&h| h == pos.hash).count() >= 3 {
        return Some(Outcome::Threefold);
    }
    if search::is_insufficient_material(pos) {
        return Some(Outcome::Insufficient);
    }
    None
}

fn describe(result: &Outcome) -> String {
    match result {
        Outcome::Checkmate(winner) => format!("Checkmate - {} wins", winner.name()),
        Outcome::Resignation(loser) => {
            format!("{} resigns - {} wins", loser.name(), loser.flip().name())
        }
        Outcome::Timeout(loser) => {
            format!("{} runs out of time - {} wins", loser.name(), loser.flip().name())
        }
        Outcome::DrawAgreement => "Draw by agreement".to_string(),
        Outcome::Stalemate => "Stalemate - the game is drawn".to_string(),
        Outcome::FiftyMove => "Drawn by the fifty-move rule".to_string(),
        Outcome::Threefold => "Drawn by threefold repetition".to_string(),
        Outcome::Insufficient => "Drawn - neither side has enough material to mate".to_string(),
    }
}

/// A compact result explanation for the fixed-width side panel.
fn outcome_detail(result: &Outcome) -> String {
    match result {
        Outcome::Checkmate(winner) => format!("{} wins", winner.name()),
        Outcome::Resignation(loser) => format!("{} resigned", loser.name()),
        Outcome::Timeout(loser) => format!("{} lost on time", loser.name()),
        Outcome::DrawAgreement => "By agreement".to_string(),
        Outcome::Stalemate => "Stalemate".to_string(),
        Outcome::FiftyMove => "Fifty-move rule".to_string(),
        Outcome::Threefold => "Threefold repetition".to_string(),
        Outcome::Insufficient => "Insufficient material".to_string(),
    }
}

/// The PGN result tag.
fn score_tag(game: &Game) -> &'static str {
    match outcome(game) {
        Some(Outcome::Checkmate(Color::White))
        | Some(Outcome::Resignation(Color::Black))
        | Some(Outcome::Timeout(Color::Black)) => "1-0",
        Some(Outcome::Checkmate(Color::Black))
        | Some(Outcome::Resignation(Color::White))
        | Some(Outcome::Timeout(Color::White)) => "0-1",
        Some(_) => "1/2-1/2",
        None => "*",
    }
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
    /// Hold the board at its old small size whatever the window could take.
    compact: bool,
    /// The left edge of the last frame, so the prompt and the engine's
    /// thinking line start where the board starts.
    indent: String,
    /// The playable 8x8 rectangle from the last frame, in terminal cells.
    board_hitbox: Option<BoardHitbox>,
    body_top: usize,
    action_hitboxes: Vec<ActionHitbox>,
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
    /// Clickable pieces shown when a pawn reaches the back rank.
    promotions: Vec<ui::PromotionOption>,
    /// A page of text - the help, the move list, the score - shown in place of
    /// the board until the next thing is typed.
    page: Option<Page>,
    redraw: bool,
}

/// A page shown instead of the board, with a heading over it.
struct Page {
    title: String,
    lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAction {
    Undo,
    Draw,
    Resign,
    Restart,
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
        let file = if self.flipped { 7 - display_file } else { display_file };
        let rank = if self.flipped { display_rank } else { 7 - display_rank };
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
            let pieces = if self.theme.ascii { ui::Pieces::Glyph } else { self.pieces };
            ui::Metrics::fit(self.cols, self.rows, pieces)
        } else {
            ui::Metrics::COMPACT
        };
        self.wide_panel = self
            .cols
            .saturating_sub(preferred.board_width() + self.gap_for(preferred) + 2)
            >= 24;
        self.metrics = if live && !self.compact && !self.wide_panel {
            let pieces = if self.theme.ascii { ui::Pieces::Glyph } else { self.pieces };
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
            self.page = Some(Page { title: title.to_string(), lines });
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
        self.promotions.clear();
        self.analysis.clear();
        self.history_offset = 0;
        self.confirming = None;
    }

    fn draw(&mut self, game: &Game, mode: Mode, limits: &Limits) {
        self.measure();
        self.board_hitbox = None;
        self.action_hitboxes.clear();
        let inline_board = if self.inline_images
            && self.pieces == ui::Pieces::Auto
            && self.page.is_none()
            && self.metrics.art
        {
            Some(self.theme.board_image(&self.board_view(game)))
        } else {
            None
        };
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
            if self.cols < 55 { "rematch or quit" } else { "choose Rematch or Quit" }
        } else if self.confirming.is_some() {
            if self.cols < 55 { "confirm  ·  esc" } else { "click Confirm  ·  esc cancels" }
        } else if self.selected.is_some() {
            if self.cols < 55 { "choose target  \u{b7}  esc" } else { "click a highlighted square  \u{b7}  esc cancels" }
        } else if self.cols < 55 {
            "click  \u{b7}  help"
        } else {
            "click a piece  \u{b7}  help  \u{b7}  undo  \u{b7}  quit"
        };
        let mut frame = vec![self.theme.bar("C H E S S", hint, self.cols)];
        if !ui::tight(self.rows) {
            frame.push(String::new());
        }
        // Rows the board could not use are split above and below it, so the
        // board sits in the middle of the window rather than riding up it. A
        // page is read from the top, so it starts at the top.
        let spare = self.rows.saturating_sub(frame.len() + body.len() + 1);
        let above = if self.page.is_some() { 1.min(spare) } else { spare / 2 };
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

        // Home the cursor and write over what is there rather than blanking
        // the screen first, which is what keeps a redraw from flickering, and
        // send the whole frame in one write for the same reason.
        let mut out = if self.inline_images {
            // Kitty placements are stateful; clearing both placements and the
            // cell grid also gives iTerm-family implementations a clean frame.
            String::from("\x1b[?25l\x1b_Ga=d\x1b\\\x1b[2J\x1b[H")
        } else {
            String::from("\x1b[?25l\x1b[H")
        };
        for line in &frame {
            out.push_str(&ui::clip(line, self.cols));
            out.push_str("\x1b[K\r\n");
        }
        out.push_str("\x1b[J\x1b[?25h");
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
            let top = if self.flipped { Color::White } else { Color::Black };
            updates.push((left, self.body_top, width, top));
            updates.push((
                left,
                self.body_top + self.metrics.board_height() - 1,
                width,
                top.flip(),
            ));
        } else {
            let first = self.body_top
                + self.metrics.board_height()
                + usize::from(!ui::tight(self.rows));
            updates.push((self.indent.len(), first, self.metrics.board_width(), Color::White));
            updates.push((
                self.indent.len(),
                first + 1,
                self.metrics.board_width(),
                Color::Black,
            ));
        }

        let mut out = String::from("\x1b[?25l");
        for (left, row, width, color) in updates {
            let line = self.player_line(game, mode, limits, color, width, !self.wide_panel);
            out.push_str(&format!(
                "\x1b[{};{}H{}\x1b[K",
                row + 1,
                left + 1,
                ui::clip(&line, width)
            ));
        }
        out.push_str(&format!("\x1b[{};1H\x1b[?25h", self.rows));
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
        let top = if self.flipped { Color::White } else { Color::Black };
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
            let heading = if played.len() > 1 { "MOVES ↑↓" } else { "MOVES" };
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
            lines: lines.into_iter().map(|line| ui::clip(&line, width)).collect(),
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
        let to_move = game.pos.side == color && outcome(game).is_none();
        let marker = match (to_move, theme.ascii) {
            (false, _) => " ",
            (true, true) => ">",
            (true, false) => "\u{25B8}",
        };
        let player = match (mode, color) {
            (Mode::TwoPlayer, Color::White) => "Player 1",
            (Mode::TwoPlayer, Color::Black) => "Player 2",
            (Mode::HumanWhite, Color::White) | (Mode::HumanBlack, Color::Black) => "You",
            _ => "Engine",
        };
        let name = if to_move {
            theme.bold(player)
        } else {
            theme.label(player)
        };
        let role = match player {
            _ if width < 30 => String::new(),
            "Engine" if width >= 34 => {
                format!("{} · {}", color.name().to_ascii_uppercase(), budget_text(limits))
            }
            _ => color.name().to_ascii_uppercase(),
        };
        let marker = if to_move { theme.accent(marker) } else { marker.to_string() };
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
        let left = ui::clip(
            &left,
            width.saturating_sub(ui::width(&clock) + 1),
        );
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
        let offset = self.history_offset.min(len.saturating_sub(capacity.min(len)));
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
        vec![
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
                    "Confirm"
                } else {
                    "Resign"
                },
                enabled: true,
            },
            ButtonSpec {
                action: UiAction::Restart,
                label: if self.confirming == Some(UiAction::Restart) {
                    "Confirm"
                } else {
                    "Restart"
                },
                enabled: true,
            },
        ]
    }

    fn game_over_buttons(&self) -> Vec<ButtonSpec> {
        vec![
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
        ]
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
            let styled = if !spec.enabled {
                self.theme.dim(&plain)
            } else if self.confirming == Some(spec.action)
                || matches!(spec.action, UiAction::Resign | UiAction::Restart)
            {
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
        if let Some(result) = outcome(game) {
            return format!(
                "{}  {}",
                theme.strong(theme.palette.accent, &describe(&result)),
                theme.dim("choose Rematch or Quit")
            );
        }
        let separator = theme.dim("  \u{b7}  ");
        let mut parts = vec![
            theme.dim(&format!("move {}", game.pos.fullmove)),
            format!("{} to move", theme.bold(game.pos.side.name())),
        ];
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
    fn draw_prompt(&self, game: &Game, input: &str) {
        print!("\r\x1b[K{}{}", self.prompt(game), input);
        let _ = io::stdout().flush();
    }
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

fn play(options: Options) -> Result<(), String> {
    let start = match &options.fen {
        Some(fen) => Position::from_fen(fen).map_err(|why| format!("bad --fen: {}", why))?,
        None => Position::startpos(),
    };

    let color = options.color.unwrap_or_else(Theme::detect_color);
    let mut screen = Screen {
        theme: Theme::new(color, options.ascii, color && Theme::detect_live(), options.palette),
        sound: sound::Player::new(options.sound),
        inline_images: false,
        flipped: false,
        cols: 80,
        rows: 24,
        metrics: ui::Metrics::COMPACT,
        wide_panel: false,
        pieces: options.pieces,
        compact: options.compact,
        indent: "  ".to_string(),
        board_hitbox: None,
        body_top: 0,
        action_hitboxes: Vec::new(),
        history_offset: 0,
        history_capacity: 0,
        confirming: None,
        analysis: Vec::new(),
        message: Vec::new(),
        selected: None,
        targets: Vec::new(),
        promotions: Vec::new(),
        page: None,
        redraw: true,
    };
    // Held for as long as the game lasts. Whatever was on the terminal before
    // comes back when this is dropped, however the program ends.
    let _fullscreen = ui::Fullscreen::enter(&screen.theme);
    if screen.theme.live && !screen.theme.ascii && screen.pieces == ui::Pieces::Auto {
        screen.inline_images = ui::detect_inline_images();
    }

    let mut stdin = io::stdin().lock();
    let mode = match options.mode {
        Some(mode) => mode,
        None => match ask_mode(&mut stdin, &mut screen)? {
            Some(mode) => mode,
            None => return Ok(()),
        },
    };
    drop(stdin);

    // Raw events are only appropriate while we own an interactive colour
    // terminal. Piped input retains the original line-oriented interface.
    let mut terminal_input = TerminalInput::enter(screen.theme.live && screen.theme.color)?;

    let mut game = Game::with_clock(start, options.clock, options.increment);
    let mut engine = Search::new();
    let mut limits = options.limits;
    screen.flipped = mode == Mode::HumanBlack;
    screen.message = vec![screen.theme.dim("Click a piece, then its target.")];

    loop {
        let flag_before = game.clock.flagged;
        if game.clock.tick() {
            if flag_before.is_none() && game.clock.flagged.is_some() {
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
        if !finished && engine_to_move {
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
            InputAction::Submit(line) => line,
            InputAction::Prompt => {
                screen.draw_prompt(&game, terminal_input.buffer());
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
                if screen.page.take().is_some() {
                    screen.redraw = true;
                } else {
                    let cancelled = screen.selected.take().is_some()
                        || !screen.targets.is_empty()
                        || !screen.promotions.is_empty();
                    let confirmation = screen.confirming.take().is_some();
                    if cancelled || confirmation {
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
                    if handle_ui_action(action, &mut game, mode, &mut screen) {
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
                println!();
                return Ok(());
            }
        };

        let flag_before = game.clock.flagged;
        game.clock.tick();
        if flag_before.is_none() && game.clock.flagged.is_some() {
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
                kind @ (PieceKind::Queen
                | PieceKind::Rook
                | PieceKind::Bishop
                | PieceKind::Knight),
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
                let lines = pgn_lines(&game);
                screen.open("PGN", lines);
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
            screen.note(screen.theme.dim("The game is over. Try `new`, `undo` or `quit`."));
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
        UiAction::Undo => {
            screen.confirming = None;
            undo(game, mode, screen);
        }
        UiAction::Draw => {
            screen.confirming = None;
            if mode != Mode::TwoPlayer {
                screen.note(screen.theme.dim("Draw offers are available in two-player games."));
            } else if game.draw_offer == Some(game.pos.side.flip()) {
                game.agreed_draw = true;
                game.draw_offer = None;
                game.clock.pause();
                screen.clear_marks();
                screen.sound.play(sound::Cue::GameEnd);
                screen.redraw = true;
            } else if game.draw_offer == Some(game.pos.side) {
                screen.note(screen.theme.dim("Your draw offer is waiting for the next player."));
            } else {
                game.draw_offer = Some(game.pos.side);
                screen.note(screen.theme.accent(&format!(
                    "{} offers a draw. Play your move; the opponent can then accept.",
                    game.pos.side.name()
                )));
            }
        }
        UiAction::Resign => {
            if screen.confirming == Some(UiAction::Resign) {
                game.resigned = Some(game.pos.side);
                game.clock.pause();
                screen.clear_marks();
                screen.sound.play(sound::Cue::GameEnd);
                screen.redraw = true;
            } else {
                screen.confirming = Some(UiAction::Resign);
                screen.note(screen.theme.warn("Click Confirm to resign, or press Escape."));
            }
        }
        UiAction::Restart => {
            if screen.confirming == Some(UiAction::Restart) {
                game.restart();
                screen.clear_marks();
                screen.note(screen.theme.good("New game."));
            } else {
                screen.confirming = Some(UiAction::Restart);
                screen.note(screen.theme.warn("Click Confirm to restart, or press Escape."));
            }
        }
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
                screen.theme.dim(&format!("- did you mean {}?", near.join(" or ")))
            };
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("`{}` is not a legal move", text)),
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
                screen.promotions.clear();
                screen.redraw = true;
            }
            return;
        }
    };

    if finished {
        screen.note(screen.theme.dim("The game is over. Try `new` or `undo`."));
        return;
    }

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
            screen.targets = legal
                .iter()
                .filter(|mv| mv.from == square)
                .map(|mv| mv.to)
                .collect();
            screen.message.clear();
            screen.redraw = true;
        }
        _ if screen.selected.is_none() => {
            if !screen.targets.is_empty() {
                screen.targets.clear();
                screen.redraw = true;
            }
        }
        _ => {}
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
    screen.message = vec![screen
        .theme
        .dim("Choose promotion: click a piece, or type Q, R, B or N.")];
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
        lines.push(format!("{} {}", theme.dim("expects"), theme.label(&expected)));
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
        screen.note(screen.theme.dim(&format!("The engine gets {}.", budget_text(limits))));
        return;
    }
    match rest.parse::<f64>() {
        Ok(secs) if secs.is_finite() && secs > 0.0 => {
            limits.movetime = Some(Duration::from_secs_f64(secs));
            screen.note(screen.theme.good(&format!("The engine now gets {}.", budget_text(limits))));
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
        screen.note(screen.theme.dim(&format!("Depth is capped at {}.", limits.depth)));
        return;
    }
    match rest.parse::<u32>() {
        Ok(depth) if depth >= 1 && depth <= search::MAX_DEPTH => {
            limits.depth = depth;
            // A depth asked for by name is a depth to reach, not to give up on.
            limits.movetime = None;
            screen.note(screen.theme.good(&format!("The engine now searches to depth {}.", depth)));
        }
        _ => screen.note(
            screen
                .theme
                .warn(&format!("Depth must be a number from 1 to {}.", search::MAX_DEPTH)),
        ),
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
        None => screen.note(
            screen
                .theme
                .warn(&format!("No theme called `{}`. Try {}.", rest, ui::theme_names())),
        ),
    }
}

/// Drawn pieces or figurines. Drawn pieces need a square at least three rows
/// tall, so on a small window this is a preference rather than an order.
fn set_pieces(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        let text = screen
            .theme
            .dim("Pieces are `art`, `glyph` or `auto`.");
        screen.note(text);
        return;
    }
    match ui::pieces_named(rest) {
        Some(pieces) => {
            screen.pieces = pieces;
            screen.redraw = true;
        }
        None => {
            let text = screen
                .theme
                .warn(&format!("`{}` is not art, glyph or auto.", rest));
            screen.note(text);
        }
    }
}

fn set_sound(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        let mode = match screen.sound.mode() {
            sound::Mode::Auto => "auto",
            sound::Mode::On => "on",
            sound::Mode::Off => "off",
        };
        let state = if screen.sound.is_playing() {
            "audio output is ready"
        } else if screen.sound.mode() == sound::Mode::Off {
            "muted"
        } else {
            "no local audio output was found"
        };
        screen.note(screen.theme.dim(&format!("Sound is `{}` - {}.", mode, state)));
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
        (_, true) => screen.theme.good("Sound effects are on."),
        _ => screen
            .theme
            .warn("No local audio output is available; the game will stay silent."),
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
        Some(line) => Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")),
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
const COMMANDS: [(&str, &str); 20] = [
    ("help", "this list"),
    ("board", "redraw the board"),
    ("flip", "turn the board around"),
    ("moves", "list the legal moves"),
    ("history", "the moves so far"),
    ("pgn", "the game as PGN"),
    ("fen", "the position as FEN"),
    ("eval", "the engine's opinion"),
    ("hint", "ask for a suggestion"),
    ("undo", "take back a move"),
    ("draw", "offer or accept a draw"),
    ("time", "seconds per move"),
    ("depth", "search depth instead"),
    ("theme", "board colours"),
    ("pieces", "drawn, or figurines"),
    ("sound", "auto, on, or off"),
    ("size", "fill the window, or not"),
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
                let cell = format!("{}  {}", theme.accent(&format!("{:<8}", name)), theme.dim(what));
                line.push_str(&ui::pad(&cell, 36));
            }
        }
        lines.push(format!("  {}", line.trim_end()));
    }
    lines.push(String::new());
    lines.push(theme.dim("  `moves e2` points at one piece on the board"));
    lines.push(theme.dim(
        "  `time 5`, `depth 8`, `theme wood`, `pieces art`, `size small` all take a value",
    ));
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
            let text = screen
                .theme
                .warn(&format!("`{}` is not a square.", filter));
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
fn pgn_lines(game: &Game) -> Vec<String> {
    let mut lines = vec![
        "[Event \"Casual game\"]".to_string(),
        "[Site \"Terminal\"]".to_string(),
        "[Date \"????.??.??\"]".to_string(),
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

fn ask_mode(stdin: &mut io::StdinLock, screen: &mut Screen) -> Result<Option<Mode>, String> {
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
            String::new(),
            theme.dim("  q   leave"),
        ];
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
            "" | "1" | "w" | "white" => return Ok(Some(Mode::HumanWhite)),
            "2" | "b" | "black" => return Ok(Some(Mode::HumanBlack)),
            "3" | "t" | "two" => return Ok(Some(Mode::TwoPlayer)),
            "q" | "quit" | "exit" => return Ok(None),
            _ => complaint = theme.warn("  Choose 1, 2 or 3."),
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
            sound: sound::Player::off(),
            inline_images: false,
            flipped: false,
            cols: 100,
            rows: 40,
            metrics: ui::Metrics::COMPACT,
            wide_panel: true,
            pieces: ui::Pieces::Glyph,
            compact: false,
            indent: String::new(),
            board_hitbox: None,
            body_top: 0,
            action_hitboxes: Vec::new(),
            history_offset: 0,
            history_capacity: 0,
            confirming: None,
            analysis: Vec::new(),
            message: Vec::new(),
            selected: None,
            targets: Vec::new(),
            promotions: Vec::new(),
            page: None,
            redraw: false,
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
        assert_eq!(game.pos.at(e4), Some(Piece::new(Color::White, PieceKind::Pawn)));
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
        assert_eq!(
            cue_after(board::START_FEN, "e4"),
            sound::Cue::Move
        );
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
        let mut clock = GameClock::new(
            Some(Duration::from_secs(60)),
            Duration::ZERO,
            Color::White,
        );
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
        handle_ui_action(
            UiAction::Draw,
            &mut game,
            Mode::TwoPlayer,
            &mut screen,
        );
        assert!(game.agreed_draw);
        assert!(matches!(outcome(&game), Some(Outcome::DrawAgreement)));
    }

    #[test]
    fn destructive_mouse_action_requires_confirmation() {
        let mut game = Game::new(Position::startpos());
        let mut screen = screen();
        screen.theme.live = true;

        handle_ui_action(
            UiAction::Resign,
            &mut game,
            Mode::TwoPlayer,
            &mut screen,
        );
        assert_eq!(screen.confirming, Some(UiAction::Resign));
        assert_eq!(game.resigned, None);

        handle_ui_action(
            UiAction::Resign,
            &mut game,
            Mode::TwoPlayer,
            &mut screen,
        );
        assert_eq!(game.resigned, Some(Color::White));
    }

    #[test]
    fn compact_buttons_wrap_without_losing_hit_targets() {
        let game = Game::new(Position::startpos());
        let screen = screen();
        let buttons = screen.render_buttons(&screen.game_buttons(&game, Mode::TwoPlayer), 24);

        assert!(buttons.lines.len() >= 2);
        assert_eq!(buttons.actions.len(), 3); // Undo starts disabled.
        assert!(buttons
            .actions
            .iter()
            .all(|target| target.left + target.width <= 24));
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
}
