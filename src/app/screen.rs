//! The screen: layout, frame diffing, panels, buttons, and hit testing.

use std::io::{self, Write};

use chess::input::Direction;
use chess::ui::{BoardView, Theme};
use chess::{sound, ui};
use chess_core::board::{self, Color, Move, Piece, PieceKind, Position};
use chess_core::game::{describe, outcome, outcome_detail, score_tag, Game, Outcome};
use chess_core::movegen::in_check;
use chess_core::search::Limits;
use chess_protocol::Lobby;

use crate::app::cli::{Mode, Options};
use crate::app::format::{budget_text, material, others_online};
use crate::app::settings::Level;

pub(crate) struct Screen {
    pub(crate) theme: Theme,
    pub(crate) sound: sound::Player,
    /// A negotiated iTerm, Kitty, or sixel image protocol. The Unicode board is
    /// always rendered underneath as a zero-risk fallback.
    pub(crate) image_protocol: Option<ui::ImageProtocol>,
    pub(crate) flipped: bool,
    /// The terminal as it was when the last frame was drawn. It is measured
    /// again every frame, so resizing the window and pressing return is all it
    /// takes for the board to grow into it.
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) metrics: ui::Metrics,
    /// Wide layouts put game information beside the board; narrow layouts
    /// keep the board large and stack compact information underneath.
    pub(crate) wide_panel: bool,
    pub(crate) pieces: ui::Pieces,
    pub(crate) player_names: [String; 2],
    /// Hold the board at its old small size whatever the window could take.
    pub(crate) compact: bool,
    /// The left edge of the last frame, so the prompt and the engine's
    /// thinking line start where the board starts.
    pub(crate) indent: String,
    /// The playable 8x8 rectangle from the last frame, in terminal cells.
    pub(crate) board_hitbox: Option<BoardHitbox>,
    /// Where the two player cards were drawn on the last frame. A ticking
    /// clock repaints exactly these, and nothing at all when the window was
    /// too small to draw them.
    pub(crate) clock_rows: Vec<ClockRow>,
    pub(crate) body_top: usize,
    pub(crate) action_hitboxes: Vec<ActionHitbox>,
    /// The control currently selected for keyboard activation. Typing always
    /// returns this to the move prompt; Tab and arrows traverse the buttons.
    pub(crate) focused: UiAction,
    pub(crate) history_offset: usize,
    pub(crate) history_capacity: usize,
    pub(crate) confirming: Option<UiAction>,
    /// What the engine last said, kept because the frame is redrawn often.
    pub(crate) analysis: Vec<String>,
    /// Feedback under the board: a complaint, a note, a list of moves.
    pub(crate) message: Vec<String>,
    /// The piece currently chosen for click-to-move.
    pub(crate) selected: Option<board::Square>,
    /// Squares the board should point at until the next move.
    pub(crate) targets: Vec<board::Square>,
    /// Legal destinations that capture, styled separately from quiet moves.
    pub(crate) captures: Vec<board::Square>,
    /// A rejected click, shown briefly as local feedback on the board.
    pub(crate) invalid: Option<board::Square>,
    /// Clickable pieces shown when a pawn reaches the back rank.
    pub(crate) promotions: Vec<ui::PromotionOption>,
    /// A page of text - the help, the move list, the score - shown in place of
    /// the board until the next thing is typed.
    pub(crate) page: Option<Page>,
    /// The first line of the open page that is on screen, and how many of its
    /// lines fit. A page longer than the window scrolls rather than being cut.
    pub(crate) page_offset: usize,
    pub(crate) page_capacity: usize,
    /// The terminal rows from the last completed paint. Keeping the styled
    /// strings lets a redraw touch only rows whose visible contents changed.
    pub(crate) last_frame: Vec<String>,
    pub(crate) last_size: Option<(usize, usize)>,
    /// The board image currently covering the Unicode fallback, if any.
    pub(crate) inline_drawn: bool,
    /// The encoded picture of the board last sent, for sending again when
    /// text has been written over it.
    pub(crate) inline_bytes: Option<(InlineBoardKey, Vec<u8>)>,
    pub(crate) last_inline_board: Option<InlineBoardKey>,
    /// The independently edited command row, cached so idle clock polls do
    /// not keep sending the same cursor movement and text.
    pub(crate) last_prompt: Option<String>,
    pub(crate) redraw: bool,
    pub(crate) online: Option<OnlineDisplay>,
    /// The square the arrow keys have put the board cursor on, while it is
    /// shown. Enter on it does what a click there would.
    pub(crate) cursor: Option<board::Square>,
    /// An earlier position of the game, shown instead of the live one.
    pub(crate) reviewing: Option<Reviewing>,
    /// Moves in the list beside the board, which a click shows.
    pub(crate) history_hits: Vec<HistoryHit>,
    /// Whether the terminal window has the focus, as far as it has said.
    /// Terminals that never say are taken to have it.
    pub(crate) window_focused: bool,
    /// The window title last set, so it is sent only when it changes.
    pub(crate) title: Option<String>,
}

/// A position from earlier in the game, while the player looks back.
pub(crate) struct Reviewing {
    /// How many moves had been played: 0 is the starting position.
    pub(crate) ply: usize,
    pub(crate) pos: Position,
    pub(crate) last: Option<Move>,
}

/// A move in the list, and the position after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistoryHit {
    pub(crate) left: usize,
    pub(crate) row: usize,
    pub(crate) width: usize,
    pub(crate) ply: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionDisplay {
    Connecting,
    Connected,
    Reconnecting,
    Stopped,
}

pub(crate) struct OnlineDisplay {
    pub(crate) connection: ConnectionDisplay,
    pub(crate) invite_code: Option<String>,
    pub(crate) your_side: Option<Color>,
    pub(crate) white_connected: bool,
    pub(crate) black_connected: bool,
    pub(crate) reconnect_deadline_ms: Option<u64>,
    pub(crate) move_pending: bool,
    pub(crate) failure_help: Option<String>,
    /// Waiting for the server to pair us with a stranger.
    pub(crate) seeking: bool,
    /// Who else is around, as the server last said.
    pub(crate) lobby: Option<Lobby>,
    /// Who has offered to play again once the game is over.
    pub(crate) rematch_offer: Option<Color>,
}

impl OnlineDisplay {
    pub(crate) fn connected(&self, color: Color) -> bool {
        match color {
            Color::White => self.white_connected,
            Color::Black => self.black_connected,
        }
    }

    pub(crate) fn can_move(&self, game: &Game) -> bool {
        self.connection == ConnectionDisplay::Connected
            && self.your_side == Some(game.pos.side)
            && self.white_connected
            && self.black_connected
            && !self.move_pending
            && outcome(game).is_none()
    }
}

/// A page shown instead of the board, with a heading over it.
pub(crate) struct Page {
    pub(crate) title: String,
    pub(crate) lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UiAction {
    MoveInput,
    Pause,
    Undo,
    Draw,
    DeclineDraw,
    Resign,
    Restart,
    ToggleSize,
    CyclePieces,
    Flip,
    Rematch,
    DeclineRematch,
    /// Look back through the game from its first move.
    Review,
    /// Show the game as PGN, to copy.
    Pgn,
    /// Back to the home page.
    Menu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActionHitbox {
    pub(crate) left: usize,
    pub(crate) top: usize,
    pub(crate) width: usize,
    pub(crate) action: UiAction,
}

impl ActionHitbox {
    pub(crate) fn contains(self, column: u16, row: u16) -> bool {
        usize::from(row) == self.top
            && usize::from(column) >= self.left
            && usize::from(column) < self.left + self.width
    }
}

pub(crate) struct RelativeAction {
    pub(crate) left: usize,
    pub(crate) row: usize,
    pub(crate) width: usize,
    pub(crate) action: UiAction,
}

/// Where a player card sits, so that a ticking clock can be repainted
/// without redrawing - or displacing - anything else on the frame.
#[derive(Clone, Copy)]
pub(crate) struct ClockRow {
    pub(crate) left: usize,
    pub(crate) row: usize,
    pub(crate) width: usize,
    pub(crate) color: Color,
}

pub(crate) struct RenderedBody {
    pub(crate) lines: Vec<String>,
    pub(crate) actions: Vec<RelativeAction>,
    pub(crate) clocks: Vec<ClockRow>,
    pub(crate) history_capacity: usize,
    pub(crate) history: Vec<HistoryHit>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InlineBoardKey {
    pub(crate) position: u64,
    pub(crate) flipped: bool,
    pub(crate) last: Option<Move>,
    pub(crate) check: Option<board::Square>,
    pub(crate) selected: Option<board::Square>,
    pub(crate) targets: Vec<board::Square>,
    pub(crate) captures: Vec<board::Square>,
    pub(crate) invalid: Option<board::Square>,
    pub(crate) promotions: Vec<(board::Square, Piece)>,
    pub(crate) cursor: Option<board::Square>,
    pub(crate) palette: ui::Palette,
    pub(crate) metrics: ui::Metrics,
    pub(crate) left: usize,
    pub(crate) top: usize,
}

pub(crate) struct ButtonSpec {
    pub(crate) action: UiAction,
    pub(crate) label: &'static str,
    pub(crate) enabled: bool,
}

/// Geometry needed to turn a terminal-cell click into a chess square.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoardHitbox {
    pub(crate) left: usize,
    pub(crate) top: usize,
    pub(crate) cell_w: usize,
    pub(crate) cell_h: usize,
    pub(crate) flipped: bool,
}

impl BoardHitbox {
    pub(crate) fn square_at(self, column: u16, row: u16) -> Option<board::Square> {
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
    /// A screen that has not drawn anything yet. The first frame measures the
    /// terminal and fills in the layout.
    pub(crate) fn new(
        theme: Theme,
        sound: sound::Player,
        pieces: ui::Pieces,
        player_names: [String; 2],
        compact: bool,
    ) -> Screen {
        Screen {
            theme,
            sound,
            image_protocol: None,
            flipped: false,
            cols: 80,
            rows: 24,
            metrics: ui::Metrics::COMPACT,
            wide_panel: false,
            pieces,
            player_names,
            compact,
            indent: "  ".to_string(),
            board_hitbox: None,
            clock_rows: Vec::new(),
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
            page_offset: 0,
            page_capacity: 0,
            last_frame: Vec::new(),
            last_size: None,
            inline_drawn: false,
            inline_bytes: None,
            last_inline_board: None,
            last_prompt: None,
            redraw: true,
            online: None,
            cursor: None,
            reviewing: None,
            history_hits: Vec::new(),
            window_focused: true,
            title: None,
        }
    }

    /// The screen for an interactive session, styled for the current terminal.
    pub(crate) fn from_options(options: &Options, player_names: [String; 2]) -> Screen {
        let color = options.color.unwrap_or_else(Theme::detect_color);
        let theme = Theme::new(
            color,
            options.ascii,
            color && Theme::detect_live(),
            options.palette,
        )
        .with_depth(options.depth);
        Screen::new(
            theme,
            sound::Player::new(options.sound),
            options.pieces,
            player_names,
            options.compact,
        )
    }

    /// Ask the terminal how big it is and work out the biggest board that
    /// still leaves room for everything drawn around it.
    pub(crate) fn measure(&mut self) {
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
        // A side panel is only worth the columns when it can carry the player
        // cards and the controls together. Beside a short board there are too
        // few rows for both unless the panel is also wide enough to lay the
        // controls out in a row or two; below one there is the whole width.
        let beside = self
            .cols
            .saturating_sub(preferred.board_width() + self.gap_for(preferred) + 2);
        self.wide_panel = beside >= 24 && (preferred.board_height() >= 14 || beside >= 34);
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
    pub(crate) fn gap(&self) -> usize {
        self.gap_for(self.metrics)
    }

    pub(crate) fn gap_for(&self, metrics: ui::Metrics) -> usize {
        if metrics.cell_w >= 6 {
            5
        } else {
            3
        }
    }

    /// How much room is left beside the board. Zero means the window is too
    /// narrow to put anything there at all.
    pub(crate) fn panel_width(&self) -> usize {
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
    pub(crate) fn note(&mut self, text: String) {
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
    pub(crate) fn show(&mut self, lines: Vec<String>) {
        self.page = None;
        self.message = lines;
        self.redraw = true;
    }

    /// A page of text. It takes the screen over when there is a screen to take
    /// over, and is simply printed when the session is a transcript.
    pub(crate) fn open(&mut self, title: &str, lines: Vec<String>) {
        if self.theme.live {
            self.page = Some(Page {
                title: title.to_string(),
                lines,
            });
            self.page_offset = 0;
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
    pub(crate) fn clear_marks(&mut self) {
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

    /// The reminder on the right of the top bar. Each state offers the same
    /// advice at several lengths and the widest one the window can hold
    /// beside the title wins, so the bar never runs its two halves together.
    pub(crate) fn bar_hint(&self, game: &Game) -> &'static str {
        let choices: &[&'static str] = if self.page.is_some() {
            if self.page_capacity > 0
                && self.page_capacity < self.page.as_ref().map_or(0, |page| page.lines.len())
            {
                &[
                    "PgUp/PgDn scrolls  ·  return goes back",
                    "PgUp/PgDn  ·  return",
                    "return goes back",
                ]
            } else {
                &["return goes back", "return"]
            }
        } else if self.reviewing.is_some() {
            &[
                "PgUp/PgDn or arrows step  ·  End back to the game",
                "arrows step  ·  End returns",
                "End returns",
            ]
        } else if outcome(game).is_some() {
            &[
                "Tab moves focus  ·  Enter selects  ·  type a command",
                "Tab controls  ·  Enter selects",
                "Tab controls",
            ]
        } else if game.paused {
            &[
                "game paused  ·  choose Resume or type a command",
                "paused  ·  choose Resume",
                "paused",
            ]
        } else if self.confirming.is_some() {
            &[
                "choose Confirm  ·  Escape cancels",
                "Enter confirms  ·  esc cancels",
                "Enter  ·  esc",
            ]
        } else if self.selected.is_some() {
            &[
                "choose a highlighted square  ·  Escape cancels",
                "choose target  ·  esc cancels",
                "choose target  ·  esc",
            ]
        } else {
            &[
                "type a move  ·  click a piece  ·  Tab controls  ·  ? for help",
                "type a move  ·  Tab controls  ·  ? for help",
                "type a move  ·  ? help",
                "? help",
            ]
        };
        // The bar writes " title ... hint " with at least two spaces between.
        let room = self.cols.saturating_sub(ui::width("C H E S S") + 6);
        choices
            .iter()
            .copied()
            .find(|hint| ui::width(hint) <= room)
            .unwrap_or_else(|| choices.last().copied().expect("every state offers a hint"))
    }

    pub(crate) fn draw(&mut self, game: &Game, mode: Mode, limits: &Limits) {
        self.measure();
        self.board_hitbox = None;
        self.action_hitboxes.clear();
        self.clock_rows.clear();
        self.history_hits.clear();
        // Settle how much of an open page is in view before anything asks.
        let page_lines = self.page.as_ref().map(|page| page.lines.len());
        match page_lines {
            Some(total) => {
                self.page_capacity = self.page_window(total);
                self.page_offset = self
                    .page_offset
                    .min(total.saturating_sub(self.page_capacity));
            }
            None => {
                self.page_capacity = 0;
                self.page_offset = 0;
            }
        }
        let wants_inline_board = self.image_protocol.is_some()
            && self.pieces == ui::Pieces::Auto
            && self.page.is_none()
            && self.metrics.art;
        let rendered = match &self.page {
            Some(page) => RenderedBody {
                lines: self.page_body(page),
                actions: Vec::new(),
                clocks: Vec::new(),
                history_capacity: 0,
                history: Vec::new(),
            },
            None => self.board_body(game, mode, limits),
        };
        let RenderedBody {
            lines: body,
            actions,
            clocks,
            history_capacity,
            history,
        } = rendered;
        self.history_capacity = history_capacity;
        self.set_title(game, mode);

        if !self.theme.live || !self.theme.color {
            println!();
            for line in &body {
                println!("{}", line);
            }
            println!();
            return;
        }

        let hint = self.bar_hint(game);
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
            self.clock_rows = clocks
                .into_iter()
                .map(|card| ClockRow {
                    row: body_start + card.row,
                    ..card
                })
                .collect();
            self.history_hits = history
                .into_iter()
                .map(|hit| HistoryHit {
                    row: body_start + hit.row,
                    ..hit
                })
                .collect();
        }
        frame.extend(body);
        // Pad down the window so the prompt always sits on the bottom row.
        while frame.len() + 1 < self.rows {
            frame.push(String::new());
        }
        frame.truncate(self.rows.saturating_sub(1));

        let inline_key = wants_inline_board.then(|| {
            let view = self.board_view(game);
            InlineBoardKey {
                position: view.pos.hash,
                flipped: view.flipped,
                last: view.last,
                check: view.check,
                selected: view.selected,
                targets: view.targets.to_vec(),
                captures: view.captures.to_vec(),
                invalid: view.invalid,
                promotions: view
                    .promotions
                    .iter()
                    .map(|choice| (choice.square, choice.piece))
                    .collect(),
                cursor: view.cursor,
                palette: self.theme.palette,
                metrics: self.metrics,
                left: self.indent.len() + ui::GUTTER,
                top: body_start + 1,
            }
        });
        let image_changed = inline_key != self.last_inline_board;
        let next_frame: Vec<String> = frame.iter().map(|line| ui::clip(line, self.cols)).collect();
        let size_changed = self.last_size != Some((self.cols, self.rows));
        let inline_transition = self.inline_drawn != wants_inline_board;
        let full_redraw = self.last_frame.is_empty() || size_changed || inline_transition;
        let changed_rows = changed_frame_rows(&self.last_frame, &next_frame, full_redraw);
        // Rewriting a row that crosses the board writes the text board over
        // the picture. Most terminals keep pictures in the cells they cover,
        // so the text erases it, and it is drawn again even when the position
        // has not changed. Even Kitty's own protocol is stored that way by
        // some terminals that speak it, such as VS Code.
        let protocol = self.image_protocol.filter(|_| wants_inline_board);
        let board_top = body_start + 1;
        let board_rows = board_top..board_top + 8 * self.metrics.cell_h;
        let board_overwritten =
            protocol.is_some() && changed_rows.iter().any(|row| board_rows.contains(row));
        let draw_image = image_changed || full_redraw || board_overwritten;

        // Synchronized output lets supporting terminals present text and the
        // replacement board image as one completed frame. Terminals that do
        // not implement mode 2026 safely ignore it. Ordinary frames are row
        // diffs; a full clear is reserved for geometry and image-mode changes.
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        let kitty_drawn =
            self.inline_drawn && self.image_protocol == Some(ui::ImageProtocol::Kitty);
        // Kitty placements that float above the text would otherwise pile up
        // under each redrawn picture.
        if kitty_drawn && draw_image {
            out.push_str(ui::KITTY_DELETE_ALL);
        }
        if full_redraw {
            out.push_str("\x1b[2J");
        }
        for row in changed_rows {
            out.push_str(&format!("\x1b[{};1H{}\x1b[K", row + 1, next_frame[row]));
        }
        // The command line is edited outside the frame cache and therefore
        // must be reset on every structural redraw.
        out.push_str(&format!("\x1b[{};1H\x1b[K", self.rows));
        self.last_prompt = None;
        print!("{}", out);
        let _ = io::stdout().flush();

        match protocol {
            Some(ui::ImageProtocol::Kitty) if draw_image => {
                let image = self.theme.board_image(&self.board_view(game));
                ui::draw_kitty_image(
                    &image,
                    self.metrics,
                    self.indent.len() + ui::GUTTER,
                    board_top,
                );
            }
            Some(protocol) if draw_image => {
                let cached = self
                    .inline_bytes
                    .as_ref()
                    .is_some_and(|(key, _)| Some(key) == inline_key.as_ref());
                if !cached {
                    let bytes = ui::inline_image_bytes(
                        protocol,
                        &self.theme,
                        &self.board_view(game),
                        self.metrics,
                        self.indent.len() + ui::GUTTER,
                        board_top,
                    );
                    self.inline_bytes = inline_key.clone().map(|key| (key, bytes));
                }
                if let Some((_, bytes)) = &self.inline_bytes {
                    let mut stdout = io::stdout();
                    let _ = stdout.write_all(bytes);
                }
            }
            _ => {}
        }
        print!("\x1b[?25l\x1b[?2026l");
        let _ = io::stdout().flush();

        self.last_frame = next_frame;
        self.last_size = Some((self.cols, self.rows));
        self.inline_drawn = wants_inline_board;
        self.last_inline_board = inline_key;
    }

    /// Update only the two player rows when a displayed second changes. This
    /// avoids retransmitting an inline board image once a second, and it
    /// repaints the rows the last frame really drew, so a window too small
    /// for the player cards has nothing written over.
    pub(crate) fn draw_clock_tick(&mut self, game: &Game, mode: Mode, limits: &Limits) {
        self.set_title(game, mode);
        if !self.theme.live || !self.theme.color || self.page.is_some() {
            return;
        }
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        for card in &self.clock_rows {
            let line =
                self.player_line(game, mode, limits, card.color, card.width, !self.wide_panel);
            out.push_str(&format!(
                "\x1b[{};{}H{}\x1b[K",
                card.row + 1,
                card.left + 1,
                ui::clip(&line, card.width)
            ));
        }
        out.push_str(&self.prompt_cursor_escape());
        out.push_str("\x1b[?2026l");
        print!("{}", out);
        let _ = io::stdout().flush();
    }

    pub(crate) fn board_view<'a>(&'a self, game: &'a Game) -> BoardView<'a> {
        if let Some(reviewing) = &self.reviewing {
            let pos = &reviewing.pos;
            return BoardView {
                pos,
                flipped: self.flipped,
                last: reviewing.last,
                check: in_check(pos, pos.side).then(|| pos.king[pos.side.index()]),
                selected: None,
                targets: &[],
                captures: &[],
                invalid: None,
                promotions: &[],
                cursor: None,
            };
        }
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
            cursor: self.cursor,
        }
    }

    pub(crate) fn square_at(&self, column: u16, row: u16) -> Option<board::Square> {
        self.board_hitbox?.square_at(column, row)
    }

    pub(crate) fn action_at(&self, column: u16, row: u16) -> Option<UiAction> {
        self.action_hitboxes
            .iter()
            .find(|target| target.contains(column, row))
            .map(|target| target.action)
    }

    pub(crate) fn focus(&mut self, action: UiAction) -> bool {
        if self.focused == action {
            return false;
        }
        self.focused = action;
        self.redraw = true;
        true
    }

    pub(crate) fn focus_move_input(&mut self) -> bool {
        self.focus(UiAction::MoveInput)
    }

    // -- looking back through the game --------------------------------------

    /// Show the position after `ply` moves, or the live game when `ply` is
    /// the number played so far or more.
    pub(crate) fn review(&mut self, game: &Game, ply: usize) {
        self.redraw = true;
        if ply >= game.sans.len() {
            self.reviewing = None;
            return;
        }
        let mut pos = game.start.clone();
        for undo in &game.undos[..ply] {
            pos.make_move(undo.mv);
        }
        self.clear_marks();
        self.cursor = None;
        self.reviewing = Some(Reviewing {
            ply,
            pos,
            last: ply.checked_sub(1).map(|last| game.undos[last].mv),
        });
    }

    /// One move back or on from where the review is, starting from the live
    /// position. Stepping on past the last move returns to the game.
    pub(crate) fn step_review(&mut self, game: &Game, back: bool) {
        let here = self
            .reviewing
            .as_ref()
            .map_or(game.sans.len(), |reviewing| reviewing.ply);
        if back && here == 0 {
            return;
        }
        let next = if back { here - 1 } else { here + 1 };
        if self.reviewing.is_none() && !back {
            return;
        }
        self.review(game, next);
    }

    /// Back to the live game. Returns whether there was a review to leave.
    pub(crate) fn end_review(&mut self) -> bool {
        let reviewing = self.reviewing.take().is_some();
        if reviewing {
            // Whatever was said was about the position that has gone.
            self.message.clear();
            self.redraw = true;
        }
        reviewing
    }

    /// The move in the list at a clicked cell, as the position after it.
    pub(crate) fn history_at(&self, column: u16, row: u16) -> Option<usize> {
        let (column, row) = (usize::from(column), usize::from(row));
        self.history_hits
            .iter()
            .find(|hit| hit.row == row && column >= hit.left && column < hit.left + hit.width)
            .map(|hit| hit.ply)
    }

    // -- the board cursor -----------------------------------------------------

    /// Show the cursor, or move it one square in `direction` as the board is
    /// drawn, so up is always up the screen.
    pub(crate) fn move_cursor(&mut self, game: &Game, direction: Direction) {
        self.redraw = true;
        let Some(square) = self.cursor else {
            // It starts where the eye already is: the piece being moved, the
            // move just played, or the king's pawn.
            let home = if game.pos.side == Color::White {
                board::sq(4, 1)
            } else {
                board::sq(4, 6)
            };
            self.cursor = Some(
                self.selected
                    .or_else(|| game.last_move().map(|mv| mv.to))
                    .unwrap_or(home),
            );
            return;
        };
        let (file, rank) = (board::file_of(square) as i8, board::rank_of(square) as i8);
        let toward = if self.flipped { -1 } else { 1 };
        let (file, rank) = match direction {
            Direction::Up => (file, rank + toward),
            Direction::Down => (file, rank - toward),
            Direction::Left => (file - toward, rank),
            Direction::Right => (file + toward, rank),
        };
        self.cursor = Some(board::sq(file.clamp(0, 7) as u8, rank.clamp(0, 7) as u8));
    }

    // -- the window around the game -----------------------------------------

    /// What the terminal window's title says: whose move it is, so a game
    /// in a window behind others can still be followed.
    pub(crate) fn title_text(&self, game: &Game, mode: Mode) -> String {
        let state = if let Some(result) = outcome(game) {
            format!(
                "Game over, {} · {}",
                score_tag(game),
                outcome_detail(&result)
            )
        } else if game.paused {
            "Paused".to_string()
        } else if let Some(online) = &self.online {
            if online.seeking {
                "Finding an opponent".to_string()
            } else if online.invite_code.is_some() && !online.black_connected {
                "Waiting for your friend".to_string()
            } else if online.your_side == Some(game.pos.side) {
                match game.clock.initial {
                    Some(_) => format!("Your move · {}", game.clock.format(game.pos.side)),
                    None => "Your move".to_string(),
                }
            } else {
                "Opponent's move".to_string()
            }
        } else {
            match mode.human() {
                Some(side) if side == game.pos.side => "Your move".to_string(),
                Some(_) => "Engine's move".to_string(),
                None => format!("{} to move", game.pos.side.name()),
            }
        };
        format!("{state} \u{2014} chess")
    }

    /// Set the window title if it changed. Terminals keep the title they had
    /// before on a stack, which [`ui::Fullscreen`] pops on the way out.
    pub(crate) fn set_title(&mut self, game: &Game, mode: Mode) {
        if !self.theme.live || !self.theme.color {
            return;
        }
        let title = self.title_text(game, mode);
        if self.title.as_deref() != Some(title.as_str()) {
            print!("\x1b]2;{title}\x07");
            let _ = io::stdout().flush();
            self.title = Some(title);
        }
    }

    /// Ring the terminal's bell, but only while the window is behind others:
    /// a player watching the board needs no bell to see a move arrive.
    pub(crate) fn alert(&self) {
        if self.theme.live && !self.window_focused {
            print!("\x07");
            let _ = io::stdout().flush();
        }
    }

    /// Traverse only controls that are enabled in the current frame. The
    /// hitboxes already follow visual reading order, so keyboard and mouse
    /// share one source of truth.
    pub(crate) fn move_focus(&mut self, reverse: bool) {
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

    pub(crate) fn scroll_history(&mut self, game: &Game, older: bool) {
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
    pub(crate) fn board_body(&self, game: &Game, mode: Mode, limits: &Limits) -> RenderedBody {
        let m = self.metrics;
        let view = self.board_view(game);
        let board = self.theme.board_lines(&view, m);
        let tight = ui::tight(self.rows);
        let mut actions = Vec::new();
        let mut clocks = Vec::new();
        let mut history = Vec::new();
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
            clocks.extend(panel.clocks.into_iter().map(|card| ClockRow {
                left: panel_left + card.left,
                ..card
            }));
            history.extend(panel.history.into_iter().map(|hit| HistoryHit {
                left: panel_left + hit.left,
                ..hit
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
            // Everything below the board has to be paid for out of rows the
            // board has not taken, and the status line and its note are paid
            // for first: a window too small for every control is still a
            // window that has to be able to say "that is not a legal move".
            let spent = 1 // the bar
                + usize::from(!tight) // the blank under the bar
                + m.board_height()
                + usize::from(!tight) // the blank under the board
                + 1 // the status line
                + usize::from(!tight) // the blank above the note
                + 1 // the note
                + 1; // the command line
            let budget = self.rows.saturating_sub(spent);
            let compact = self.compact_panel(
                game,
                mode,
                limits,
                m.board_width(),
                self.cols.saturating_sub(self.indent.len() + 1).max(20),
                budget,
            );
            if !tight && !compact.lines.is_empty() {
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
            clocks.extend(compact.clocks.into_iter().map(|card| ClockRow {
                left: self.indent.len() + card.left,
                row: compact_start + card.row,
                ..card
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
        let note_width = self.cols.saturating_sub(self.indent.len());
        let mut notes: Vec<String> = self
            .analysis
            .iter()
            .chain(self.message.iter())
            .take(room)
            .map(|line| format!("{}{}", self.indent, ui::clip_note(line, note_width)))
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
            clocks,
            history_capacity,
            history,
        }
    }

    /// Rows an open page may spend on its own text, once the bar, the
    /// heading and the command line have taken theirs.
    pub(crate) fn page_rows(&self) -> usize {
        let frame = 1 // the bar
            + usize::from(!ui::tight(self.rows)) // the blank under it
            + 1 // the row a page is offset from the top by
            + 3 // title, rule, blank
            + 1; // the command line
        self.rows.saturating_sub(frame).max(1)
    }

    /// How many of a page's `total` lines are shown at once. A page that does
    /// not fit keeps two rows back for the position indicator underneath it.
    pub(crate) fn page_window(&self, total: usize) -> usize {
        let room = self.page_rows();
        if total <= room {
            total
        } else {
            room.saturating_sub(2).max(1)
        }
    }

    /// Move an over-long page by close to a windowful, the way a pager does.
    /// Returns false when there is no page, so the caller can scroll the move
    /// list instead.
    pub(crate) fn scroll_page(&mut self, back: bool) -> bool {
        let Some(page) = &self.page else {
            return false;
        };
        let total = page.lines.len();
        let capacity = self.page_capacity.max(self.page_window(total));
        if total <= capacity {
            return true;
        }
        let most = total - capacity;
        let step = capacity.saturating_sub(1).max(1);
        let next = if back {
            self.page_offset.saturating_sub(step)
        } else {
            (self.page_offset + step).min(most)
        };
        if next != self.page_offset {
            self.page_offset = next;
            self.redraw = true;
        }
        true
    }

    pub(crate) fn page_body(&self, page: &Page) -> Vec<String> {
        let theme = &self.theme;
        let total = page.lines.len();
        let capacity = self.page_window(total);
        let offset = self.page_offset.min(total.saturating_sub(capacity));
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
        block.extend(page.lines[offset..offset + capacity].iter().cloned());
        if total > capacity {
            let scrolled = format!(
                "{}-{} of {}   {}",
                offset + 1,
                offset + capacity,
                total,
                if offset + capacity < total {
                    "↑↓ or PgUp/PgDn for more"
                } else {
                    "↑↓ or PgUp/PgDn to go back"
                }
            );
            block.push(String::new());
            block.push(theme.dim(&scrolled));
        }
        // Centre on the whole page rather than on the rows in view, so the
        // text does not slide sideways as it is scrolled.
        let widest = page
            .lines
            .iter()
            .chain(std::iter::once(&page.title))
            .map(|line| ui::width(line))
            .max()
            .unwrap_or(0)
            .max(longest);
        let left = " ".repeat(self.cols.saturating_sub(widest.min(self.cols)) / 2);
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
    pub(crate) fn panel(
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
                clocks: Vec::new(),
                history_capacity: 0,
                history: Vec::new(),
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

        let finished = outcome(game);
        // The controls keep the same place on the panel whether the game is
        // still running or already decided, so nothing jumps under the mouse
        // at the moment a game ends.
        let buttons = match finished {
            Some(_) => self.render_buttons(&self.game_over_buttons(game), width),
            None => self.render_buttons(&self.game_buttons(game, mode), width),
        };
        let button_start = height.saturating_sub(buttons.lines.len() + 3);
        // A panel this narrow can wrap the controls past its own bottom edge.
        // Whatever is left out keeps its hitbox out of the frame with it.
        let shown = buttons.lines.len().min(height - button_start);
        let actions = buttons
            .actions
            .into_iter()
            .filter(|target| target.row < shown)
            .map(|target| RelativeAction {
                row: button_start + target.row,
                ..target
            })
            .collect();
        for (offset, line) in buttons.lines.into_iter().take(shown).enumerate() {
            rows[button_start + offset] = line;
        }

        // A finished game announces itself above the move list, which stays
        // where it is: the first thing wanted after a result is the game it
        // came from.
        let mut first = 3;
        if let Some(result) = finished {
            let title = match result {
                Outcome::Checkmate(_) => "CHECKMATE",
                Outcome::Resignation(_) => "RESIGNED",
                Outcome::Timeout(_) => "TIME",
                Outcome::Abandonment(_) => "ABANDONED",
                _ => "DRAW",
            };
            let detail = outcome_detail(&result);
            let score = score_tag(game);
            let mut result_rows = vec![
                self.theme.strong(self.theme.palette.accent, "GAME OVER"),
                self.theme.bold(title),
            ];
            if ui::width(&detail) + ui::width(score) + 2 <= width {
                result_rows.push(format!(
                    "{}  {}",
                    self.theme.dim(&detail),
                    self.theme.accent(score)
                ));
            } else {
                result_rows.push(self.theme.dim(&detail));
                result_rows.push(self.theme.accent(score));
            }
            result_rows.push(self.theme.dim(&game_length(game)));
            for (offset, line) in result_rows.into_iter().enumerate() {
                if 3 + offset < button_start {
                    rows[3 + offset] = line;
                    first = 3 + offset + 1;
                }
            }
            first += 1;
        }

        let mut history_capacity = button_start.saturating_sub(first + 1);
        // Beside a short board the blank row above the move list is worth
        // more as a move.
        if history_capacity == 0 && first > 2 {
            first = 2;
            history_capacity = button_start.saturating_sub(first + 1);
        }
        let mut history = Vec::new();
        if history_capacity > 0 {
            let played = history_rows(game);
            let (start, end) = self.history_window(game, played.len(), history_capacity);
            rows[first] = self.history_heading(played.len(), start, end);
            let reviewed = self.reviewing.as_ref().map(|reviewing| reviewing.ply);
            for (i, line) in played[start..end].iter().enumerate() {
                let latest = reviewed.is_none() && self.history_offset == 0 && i + 1 == end - start;
                let row = first + 1 + i;
                rows[row] = self.history_row(line, reviewed, latest);
                for (left, hit_width, ply) in line.targets() {
                    if left < width {
                        history.push(HistoryHit {
                            left,
                            row,
                            width: hit_width.min(width - left),
                            ply,
                        });
                    }
                }
            }
        }

        RenderedBody {
            lines: rows.iter().map(|row| ui::clip(row, width)).collect(),
            actions,
            clocks: vec![
                ClockRow {
                    left: 0,
                    row: 0,
                    width,
                    color: top,
                },
                ClockRow {
                    left: 0,
                    row: height - 1,
                    width,
                    color: bottom,
                },
            ],
            history_capacity,
            history,
        }
    }

    /// The information strip under the board in a narrow window, fitted to
    /// the rows left over once the board, the status line and its note have
    /// been paid for. Player cards come first, then the controls, and the
    /// move summary only when everything else already fits.
    pub(crate) fn compact_panel(
        &self,
        game: &Game,
        mode: Mode,
        limits: &Limits,
        width: usize,
        outer_width: usize,
        budget: usize,
    ) -> RenderedBody {
        if budget < 2 {
            return RenderedBody {
                lines: Vec::new(),
                actions: Vec::new(),
                clocks: Vec::new(),
                history_capacity: 0,
                history: Vec::new(),
            };
        }
        let mut lines = vec![
            self.player_line(game, mode, limits, Color::White, width, true),
            self.player_line(game, mode, limits, Color::Black, width, true),
        ];

        // Controls may run wider than the board: below it there is nothing
        // to line up with, and a row saved here is a row of chess.
        let buttons = if outcome(game).is_some() {
            self.render_buttons(&self.game_over_buttons(game), outer_width.max(width))
        } else {
            self.render_buttons(&self.game_buttons(game, mode), outer_width.max(width))
        };
        let mut history_capacity = 0;
        if budget > lines.len() + buttons.lines.len() {
            if let (Some(result), None) = (outcome(game), &self.reviewing) {
                lines.push(self.theme.strong(
                    self.theme.palette.accent,
                    &format!("GAME OVER  {}", describe(&result)),
                ));
            } else {
                let played = history_lines(game);
                let (start, end) = self.history_window(game, played.len(), 1);
                let history = played
                    .get(start..end)
                    .and_then(|slice| slice.first())
                    .map(String::as_str)
                    .unwrap_or("No moves yet");
                let heading = if played.len() > 1 {
                    "MOVES PgUp"
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
        }

        // Controls that do not fit are dropped along with their hitboxes, so
        // nothing invisible can be clicked or reached with Tab.
        let button_start = lines.len();
        let shown = budget.saturating_sub(button_start).min(buttons.lines.len());
        lines.extend(buttons.lines.into_iter().take(shown));
        let actions = buttons
            .actions
            .into_iter()
            .filter(|target| target.row < shown)
            .map(|target| RelativeAction {
                row: button_start + target.row,
                ..target
            })
            .collect();

        RenderedBody {
            lines: lines
                .into_iter()
                .map(|line| ui::clip(&line, outer_width.max(width)))
                .collect(),
            actions,
            clocks: vec![
                ClockRow {
                    left: 0,
                    row: 0,
                    width,
                    color: Color::White,
                },
                ClockRow {
                    left: 0,
                    row: 1,
                    width,
                    color: Color::Black,
                },
            ],
            history_capacity,
            history: Vec::new(),
        }
    }

    pub(crate) fn player_line(
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
                let strength = Level::of(limits)
                    .map_or_else(|| budget_text(limits), |level| level.label().to_string());
                format!("{} · {}", color.name().to_ascii_uppercase(), strength)
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

    /// The pieces this side has taken, under its name. An empty row until
    /// there is something to put in it: a standing "no captures" label is a
    /// line of furniture that says nothing about the game.
    pub(crate) fn capture_line(&self, game: &Game, color: Color) -> String {
        let summary = self.capture_summary(game, color);
        if summary.is_empty() {
            return String::new();
        }
        format!("  {}", summary)
    }

    pub(crate) fn capture_summary(&self, game: &Game, color: Color) -> String {
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

    pub(crate) fn history_bounds(&self, len: usize, capacity: usize) -> (usize, usize) {
        let offset = self
            .history_offset
            .min(len.saturating_sub(capacity.min(len)));
        let end = len.saturating_sub(offset);
        (end.saturating_sub(capacity), end)
    }

    /// The lines of the move list in view: where the list has been scrolled
    /// to, moved if need be so that the move being reviewed is among them.
    pub(crate) fn history_window(
        &self,
        game: &Game,
        len: usize,
        capacity: usize,
    ) -> (usize, usize) {
        let (start, end) = self.history_bounds(len, capacity);
        let Some(line) = self
            .reviewing
            .as_ref()
            .and_then(|reviewing| reviewing.ply.checked_sub(1))
            .map(|index| line_of_move(game, index))
        else {
            return (start, end);
        };
        let shown = capacity.min(len);
        if line < start {
            (line, line + shown)
        } else if line >= end {
            (line + 1 - shown, line + 1)
        } else {
            (start, end)
        }
    }

    pub(crate) fn history_heading(&self, len: usize, start: usize, end: usize) -> String {
        if len == 0 {
            return self.theme.label("NO MOVES YET");
        }
        let range = if end - start < len {
            format!("  {}-{} / {}  PgUp", start + 1, end, len)
        } else {
            String::new()
        };
        format!("{}{}", self.theme.label("MOVES"), self.theme.dim(&range))
    }

    /// One line of the move list, the move under review picked out.
    fn history_row(&self, row: &MoveRow, reviewed: Option<usize>, latest: bool) -> String {
        let style = |san: &str, ply: usize| {
            if reviewed == Some(ply) {
                self.theme.focused(self.theme.palette.accent, san)
            } else if latest {
                self.theme.bold(san)
            } else {
                self.theme.dim(san)
            }
        };
        let number = format!("{:>3}. ", row.number);
        let number = if latest {
            self.theme.bold(&number)
        } else {
            self.theme.dim(&number)
        };
        let white = match &row.white {
            Some((ply, san)) => style(san, *ply),
            None => self.theme.dim("..."),
        };
        let mut line = format!("{}{}", number, ui::pad(&white, 7));
        if let Some((ply, san)) = &row.black {
            line.push_str(&style(san, *ply));
        }
        line
    }

    pub(crate) fn game_buttons(&self, game: &Game, mode: Mode) -> Vec<ButtonSpec> {
        let draw_label = match game.draw_offer {
            Some(color) if color != game.pos.side => "Accept",
            Some(_) => "Offered",
            None => "Draw",
        };
        if let Some(online) = &self.online {
            let draw_label = match game.draw_offer {
                Some(color) if Some(color) != online.your_side => "Accept",
                Some(_) => "Offered",
                None => "Draw",
            };
            let mut buttons = vec![
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
            ];
            if online
                .your_side
                .is_some_and(|side| game.draw_offer == Some(side.flip()))
            {
                buttons.push(ButtonSpec {
                    action: UiAction::DeclineDraw,
                    label: "Decline",
                    enabled: online.connection == ConnectionDisplay::Connected
                        && online.white_connected
                        && online.black_connected
                        && outcome(game).is_none(),
                });
            }
            buttons.extend([
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
                self.flip_button(),
                self.size_button(),
                self.pieces_button(),
                ButtonSpec {
                    action: UiAction::Menu,
                    label: if self.confirming == Some(UiAction::Menu) {
                        "Confirm leave"
                    } else {
                        "Menu"
                    },
                    enabled: true,
                },
            ]);
            return buttons;
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
            self.flip_button(),
            self.size_button(),
            self.pieces_button(),
            ButtonSpec {
                action: UiAction::Menu,
                label: "Menu",
                enabled: true,
            },
        ]
    }

    /// The summary's actions: play again, look back, keep the game, leave.
    pub(crate) fn game_over_buttons(&self, game: &Game) -> Vec<ButtonSpec> {
        let mut buttons = Vec::new();
        match &self.online {
            Some(online) => {
                let opponent_here = online.your_side.is_some_and(|side| {
                    online.connection == ConnectionDisplay::Connected
                        && online.connected(side.flip())
                });
                let (label, enabled) = match online.rematch_offer {
                    Some(side) if Some(side) == online.your_side => ("Rematch offered", false),
                    Some(_) => ("Accept rematch", opponent_here),
                    None => ("Rematch", opponent_here),
                };
                buttons.push(ButtonSpec {
                    action: UiAction::Rematch,
                    label,
                    enabled,
                });
                if online.rematch_offer.is_some() && online.rematch_offer != online.your_side {
                    buttons.push(ButtonSpec {
                        action: UiAction::DeclineRematch,
                        label: "Decline",
                        enabled: opponent_here,
                    });
                }
            }
            None => buttons.push(ButtonSpec {
                action: UiAction::Rematch,
                label: "Rematch",
                enabled: true,
            }),
        }
        buttons.extend([
            ButtonSpec {
                action: UiAction::Review,
                label: "Review",
                enabled: !game.sans.is_empty(),
            },
            ButtonSpec {
                action: UiAction::Pgn,
                label: "PGN",
                enabled: true,
            },
            ButtonSpec {
                action: UiAction::Menu,
                label: "Menu",
                enabled: true,
            },
            self.flip_button(),
            self.size_button(),
            self.pieces_button(),
        ]);
        buttons
    }

    /// Turning the board round is the one view control a game at a shared
    /// keyboard reaches for every move, so it sits with the others rather
    /// than only behind a typed command.
    pub(crate) fn flip_button(&self) -> ButtonSpec {
        ButtonSpec {
            action: UiAction::Flip,
            label: "Flip",
            enabled: true,
        }
    }

    pub(crate) fn size_button(&self) -> ButtonSpec {
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

    pub(crate) fn pieces_button(&self) -> ButtonSpec {
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

    pub(crate) fn render_buttons(&self, specs: &[ButtonSpec], width: usize) -> RenderedBody {
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
            clocks: Vec::new(),
            history_capacity: 0,
            history: Vec::new(),
        }
    }

    pub(crate) fn state_line(&self, game: &Game) -> String {
        let theme = &self.theme;
        if let Some(reviewing) = &self.reviewing {
            let arrows = if theme.ascii {
                "PgUp/PgDn"
            } else {
                "\u{2190}\u{2192}"
            };
            // Moves keep their own case: SAN reads B as a bishop, b as a file.
            return format!(
                "{} {}  {}",
                theme.strong(theme.palette.accent, "VIEWING"),
                theme.bold(&move_name(game, reviewing.ply)),
                theme.dim(&format!("{arrows} step · End back to the game"))
            );
        }
        if let Some(result) = outcome(game) {
            let wanted = self.online.as_ref().is_some_and(|online| {
                online.rematch_offer.is_some() && online.rematch_offer != online.your_side
            });
            let next = if wanted {
                "your opponent asks for a rematch"
            } else {
                "Rematch, Review, PGN or Menu"
            };
            return format!(
                "{}  {}",
                theme.strong(theme.palette.accent, &describe(&result)),
                theme.dim(next)
            );
        }
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
                        theme.dim(
                            online
                                .failure_help
                                .as_deref()
                                .unwrap_or("quit and run online resume to return"),
                        )
                    );
                }
                ConnectionDisplay::Connected => {}
            }
            if online.seeking {
                let heading = "FINDING AN OPPONENT";
                let others = online.lobby.map(|lobby| lobby.online.saturating_sub(1));
                return match others {
                    Some(0) => format!(
                        "{}  {}",
                        theme.strong(theme.palette.warn, heading),
                        theme.dim("no one else online · q for the menu")
                    ),
                    Some(others) => format!(
                        "{}  {}",
                        theme.strong(theme.palette.accent, heading),
                        theme.dim(&format!(
                            "{} online · q for the menu",
                            others_online(others)
                        ))
                    ),
                    None => format!(
                        "{}  {}",
                        theme.strong(theme.palette.accent, heading),
                        theme.dim("q for the menu")
                    ),
                };
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

    pub(crate) fn prompt(&self, game: &Game) -> String {
        let arrow = if self.theme.ascii { ">" } else { "\u{203a}" };
        let label = if self.reviewing.is_some() {
            "Viewing"
        } else if outcome(game).is_some() {
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
    pub(crate) fn draw_prompt(&mut self, game: &Game, input: &str) {
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
    pub(crate) fn prompt_cursor_escape(&self) -> String {
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

pub(crate) fn changed_frame_rows(
    previous: &[String],
    next: &[String],
    force_all: bool,
) -> Vec<usize> {
    next.iter()
        .enumerate()
        .filter_map(|(row, line)| (force_all || previous.get(row) != Some(line)).then_some(row))
        .collect()
}

/// One line of the move list: a move number, and the moves made under it,
/// each with the number of moves played once it was made.
pub(crate) struct MoveRow {
    pub(crate) number: u32,
    pub(crate) white: Option<(usize, String)>,
    pub(crate) black: Option<(usize, String)>,
}

impl MoveRow {
    /// Where each move sits on its line, as `(column, width, ply)`: the
    /// number goes with White's move, so a click anywhere on the left half
    /// finds it.
    pub(crate) fn targets(&self) -> Vec<(usize, usize, usize)> {
        const BLACK_COLUMN: usize = 12;
        let mut targets = Vec::new();
        if let Some((ply, _)) = &self.white {
            targets.push((0, BLACK_COLUMN, *ply));
        }
        if let Some((ply, san)) = &self.black {
            targets.push((BLACK_COLUMN, ui::width(san) + 1, *ply));
        }
        targets
    }
}

/// The move list, one line per move number, from whatever side started.
pub(crate) fn history_rows(game: &Game) -> Vec<MoveRow> {
    let mut rows: Vec<MoveRow> = Vec::new();
    let mut number = game.start.fullmove;
    let mut side = game.start.side;
    for (index, text) in game.sans.iter().enumerate() {
        let entry = Some((index + 1, text.clone()));
        if side == Color::White {
            rows.push(MoveRow {
                number,
                white: entry,
                black: None,
            });
        } else {
            match rows.last_mut() {
                Some(row) if row.number == number && row.black.is_none() => row.black = entry,
                _ => rows.push(MoveRow {
                    number,
                    white: None,
                    black: entry,
                }),
            }
            number += 1;
        }
        side = side.flip();
    }
    rows
}

/// Which line of the move list the move at `index` is on.
pub(crate) fn line_of_move(game: &Game, index: usize) -> usize {
    // A game that began with Black to move has its first line to itself.
    let offset = usize::from(game.start.side == Color::Black);
    (index + offset) / 2
}

/// `14... Nf6`, the move that led to the position after `ply` moves.
pub(crate) fn move_name(game: &Game, ply: usize) -> String {
    let Some(index) = ply.checked_sub(1) else {
        return "the start".to_string();
    };
    let offset = usize::from(game.start.side == Color::Black);
    let number = game.start.fullmove as usize + (index + offset) / 2;
    let dots = if (index + offset) % 2 == 0 {
        "."
    } else {
        "..."
    };
    format!("{number}{dots} {}", game.sans[index])
}

/// `38 moves`, counting a move by each side as one.
pub(crate) fn game_length(game: &Game) -> String {
    match game.sans.len().div_ceil(2) {
        1 => "1 move".to_string(),
        moves => format!("{moves} moves"),
    }
}

/// `1. e4 e5` lines, one per move pair, from whatever side started.
pub(crate) fn history_lines(game: &Game) -> Vec<String> {
    history_rows(game)
        .iter()
        .map(|row| {
            let white = row.white.as_ref().map_or("...", |(_, san)| san.as_str());
            let black = row.black.as_ref().map_or("", |(_, san)| san.as_str());
            format!("{:>3}. {:<7}{}", row.number, white, black)
                .trim_end()
                .to_string()
        })
        .collect()
}
