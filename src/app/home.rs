//! The home page: what an interactive terminal shows before a game begins.
//!
//! Everything a player can start from here was already reachable through
//! command-line flags; the page gathers it in one place, previews the board
//! each choice would open on, and says what the current settings are. Pipes
//! and dumb terminals keep the numbered prompt in `prompt::ask_mode`.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use chess::input::{Action, TerminalInput};
use chess::storage;
use chess::ui::{self, BoardView, Metrics, Theme};
use chess_core::board::{Color, Position};
use chess_core::game::{outcome, outcome_detail, Game};
use chess_core::movegen::in_check;

use crate::app::cli::{Mode, OnlineIntent, Options, StartChoice};
use crate::app::saves::{color_named, restore_game};
use crate::app::screen::Screen;

/// One thing the home page can start, in the order it is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    White,
    Black,
    Two,
    Continue,
    Create,
    Join,
    Rejoin,
    Quit,
}

impl Entry {
    fn key(self) -> char {
        match self {
            Entry::White => 'w',
            Entry::Black => 'b',
            Entry::Two => 't',
            Entry::Continue => 'c',
            Entry::Create => 'o',
            Entry::Join => 'j',
            Entry::Rejoin => 'r',
            Entry::Quit => 'q',
        }
    }

    fn label(self) -> &'static str {
        match self {
            Entry::White => "Play White",
            Entry::Black => "Play Black",
            Entry::Two => "Two players",
            Entry::Continue => "Continue saved game",
            Entry::Create => "Create a private game",
            Entry::Join => "Join with a code",
            Entry::Rejoin => "Rejoin online game",
            Entry::Quit => "Quit",
        }
    }

    /// The numbered menu this page replaced answered to 1-4, and fingers
    /// remember that.
    fn numbered(digit: char) -> Option<Entry> {
        match digit {
            '1' => Some(Entry::White),
            '2' => Some(Entry::Black),
            '3' => Some(Entry::Two),
            '4' => Some(Entry::Continue),
            _ => None,
        }
    }
}

/// What the page knows about the world outside it, gathered once up front.
pub(crate) struct HomeInfo {
    /// `None` when there is no autosave; an error when there is one that
    /// cannot be restored, which the page explains instead of crashing on.
    pub(crate) saved: Option<Result<(Game, Mode), String>>,
    pub(crate) seat: Option<Result<storage::SavedOnlineSeat, String>>,
    pub(crate) server_url: String,
    /// Theme, clock, engine and sound, already worded for the footer.
    pub(crate) settings: String,
}

impl HomeInfo {
    pub(crate) fn load(
        session_path: &Path,
        seat_path: &Path,
        options: &Options,
        screen: &Screen,
    ) -> HomeInfo {
        let saved = session_path
            .exists()
            .then(|| storage::load_game(session_path).and_then(restore_game));
        let seat = seat_path
            .exists()
            .then(|| storage::load_online_seat(seat_path));
        HomeInfo {
            saved,
            seat,
            server_url: options.server_url.clone(),
            settings: settings_line(options, screen),
        }
    }
}

fn settings_line(options: &Options, screen: &Screen) -> String {
    let clock = match options.clock {
        Some(initial) => format!(
            "{}+{} clock",
            trim_number(initial.as_secs_f64() / 60.0),
            trim_number(options.increment.as_secs_f64())
        ),
        None => "no clock".to_string(),
    };
    let engine = match options.limits.movetime {
        Some(budget) => format!("engine {}s", trim_number(budget.as_secs_f64())),
        None => format!("engine depth {}", options.limits.depth),
    };
    format!(
        "{} board · {} · {} · sound {}",
        ui::palette_name(screen.theme.palette),
        clock,
        engine,
        screen.sound.mode().name()
    )
}

/// `10` rather than `10.0`, but `0.5` stays `0.5`.
fn trim_number(value: f64) -> String {
    if value.fract().abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        format!("{:.1}", value)
    }
}

/// A clickable menu row, in frame coordinates.
#[derive(Clone, Copy, Debug)]
struct Hit {
    row: usize,
    left: usize,
    width: usize,
    entry: Entry,
}

/// What the preview board shows. Rendering pieces can be slow when they are
/// drawn rather than lettered, so the lines are kept until this changes.
#[derive(Clone, Copy, PartialEq)]
struct PreviewKey {
    saved: bool,
    flipped: bool,
    metrics: Metrics,
    palette: ui::Palette,
}

pub(crate) struct Home {
    entries: Vec<Entry>,
    focus: usize,
    /// `Some` while the invite code is being typed.
    code: Option<String>,
    complaint: Option<String>,
    start: Game,
    hits: Vec<Hit>,
    board_cache: Option<(PreviewKey, Vec<String>)>,
}

impl Home {
    pub(crate) fn new(info: &HomeInfo) -> Home {
        let mut entries = vec![Entry::White, Entry::Black, Entry::Two];
        if info.saved.is_some() {
            entries.push(Entry::Continue);
        }
        entries.extend([Entry::Create, Entry::Join]);
        if info.seat.is_some() {
            entries.push(Entry::Rejoin);
        }
        entries.push(Entry::Quit);
        // An unfinished game is the likeliest reason to have come back.
        let focus = match &info.saved {
            Some(Ok((game, _))) if outcome(game).is_none() => {
                entries.iter().position(|&e| e == Entry::Continue).unwrap()
            }
            _ => 0,
        };
        Home {
            entries,
            focus,
            code: None,
            complaint: None,
            start: Game::new(Position::startpos()),
            hits: Vec::new(),
            board_cache: None,
        }
    }

    pub(crate) fn focused(&self) -> Entry {
        self.entries[self.focus]
    }

    pub(crate) fn joining(&self) -> bool {
        self.code.is_some()
    }

    /// `Some` once the player has decided: a choice, or `None` to leave.
    /// `typed` is the input line as it stands, for the invite code.
    pub(crate) fn handle(
        &mut self,
        action: Action,
        info: &HomeInfo,
        typed: &str,
    ) -> Option<Option<StartChoice>> {
        match action {
            Action::Quit => Some(None),
            Action::Resize | Action::Tick => None,
            Action::Cancel => {
                self.code = None;
                self.complaint = None;
                None
            }
            Action::Focus { reverse } | Action::History { older: reverse } => {
                if !self.joining() {
                    self.complaint = None;
                    let count = self.entries.len();
                    self.focus = if reverse {
                        (self.focus + count - 1) % count
                    } else {
                        (self.focus + 1) % count
                    };
                }
                None
            }
            Action::Click { column, row } => {
                let (column, row) = (usize::from(column), usize::from(row));
                let hit = self.hits.iter().find(|hit| {
                    hit.row == row && column >= hit.left && column < hit.left + hit.width
                })?;
                let entry = hit.entry;
                if entry == Entry::Join && self.joining() {
                    return None;
                }
                self.code = None;
                self.choose(entry, info)
            }
            Action::Prompt => {
                if self.joining() {
                    self.code = Some(clean_code(typed));
                    self.complaint = None;
                    return None;
                }
                let key = typed.chars().last()?.to_ascii_lowercase();
                let entry = Entry::numbered(key)
                    .or_else(|| self.entries.iter().copied().find(|e| e.key() == key));
                match entry {
                    Some(entry) if self.entries.contains(&entry) => self.choose(entry, info),
                    _ => {
                        self.complaint = Some(
                            "Press one of the highlighted letters, or use the arrow keys."
                                .to_string(),
                        );
                        None
                    }
                }
            }
            Action::Submit(text) => match &self.code {
                Some(_) => {
                    let code = clean_code(&text);
                    if code.is_empty() {
                        self.code = Some(code);
                        self.complaint =
                            Some("Type the invite code your friend sent you.".to_string());
                        None
                    } else {
                        Some(Some(StartChoice::Online(OnlineIntent::Join(code))))
                    }
                }
                None => self.choose(self.focused(), info),
            },
        }
    }

    fn choose(&mut self, entry: Entry, info: &HomeInfo) -> Option<Option<StartChoice>> {
        if let Some(index) = self.entries.iter().position(|&e| e == entry) {
            self.focus = index;
        }
        self.complaint = None;
        let choice = match entry {
            Entry::White => StartChoice::Mode(Mode::HumanWhite),
            Entry::Black => StartChoice::Mode(Mode::HumanBlack),
            Entry::Two => StartChoice::Mode(Mode::TwoPlayer),
            Entry::Continue => match &info.saved {
                Some(Ok(_)) => StartChoice::Resume,
                Some(Err(why)) => {
                    self.complaint = Some(format!("The autosave cannot be restored: {why}"));
                    return None;
                }
                None => return None,
            },
            Entry::Create => StartChoice::Online(OnlineIntent::Create),
            Entry::Join => {
                self.code = Some(String::new());
                return None;
            }
            Entry::Rejoin => match &info.seat {
                Some(Ok(_)) => StartChoice::Online(OnlineIntent::Resume),
                Some(Err(why)) => {
                    self.complaint = Some(format!("The online seat cannot be read: {why}"));
                    return None;
                }
                None => return None,
            },
            Entry::Quit => return Some(None),
        };
        Some(Some(choice))
    }

    // -- drawing ------------------------------------------------------------

    /// Exactly `rows` lines, none wider than `cols`.
    pub(crate) fn render(
        &mut self,
        theme: &Theme,
        info: &HomeInfo,
        pieces: ui::Pieces,
        cols: usize,
        rows: usize,
    ) -> Vec<String> {
        const GAP: usize = 6;
        const MENU_MIN: usize = 44;
        const MENU_MAX: usize = 54;

        // Room between the bar at the top and the key hints at the bottom.
        let space = rows.saturating_sub(4);
        let tall = space >= 24;
        let metrics = [3, 2, 1].into_iter().find_map(|cell_h| {
            let metrics = Metrics {
                cell_w: if cell_h == 1 { 3 } else { 2 * cell_h },
                cell_h,
                art: cell_h >= 3 && pieces != ui::Pieces::Glyph && !theme.ascii,
            };
            let fits = metrics.board_height() <= space
                && metrics.board_width() + GAP + MENU_MIN + 4 <= cols;
            fits.then_some(metrics)
        });
        let board_w = metrics.map_or(0, |m| m.board_width() + GAP);
        let menu_w = cols.saturating_sub(board_w + 4).clamp(1, MENU_MAX);

        let (menu, hits) = self.menu(theme, info, menu_w, tall);
        let board = match metrics {
            Some(metrics) => self.preview(theme, info, metrics),
            None => Vec::new(),
        };

        let block_h = menu.len().max(board.len());
        // Centred on the column's full width, not on what it happens to hold,
        // so the page stays still while the focus and its details change.
        let block_w = board_w + menu_w;
        let left = cols.saturating_sub(block_w) / 2;
        let top = 1 + space.saturating_sub(block_h) / 2 + usize::from(space > block_h);
        let menu_top = top + (block_h - menu.len()) / 2;
        let board_top = top + (block_h - board.len()) / 2;

        let mut frame = vec![String::new(); rows];
        frame[0] = theme.bar(
            if theme.ascii {
                "chess"
            } else {
                "\u{265a}  chess"
            },
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            cols,
        );
        let body = rows.saturating_sub(1).saturating_sub(top).min(block_h);
        for (row, slot) in frame.iter_mut().enumerate().skip(top).take(body) {
            let mut line = " ".repeat(left);
            if let Some(board_line) = row.checked_sub(board_top).and_then(|i| board.get(i)) {
                line.push_str(board_line);
                line.push_str(&" ".repeat(GAP));
            } else {
                line.push_str(&" ".repeat(board_w));
            }
            if let Some(menu_line) = row.checked_sub(menu_top).and_then(|i| menu.get(i)) {
                line.push_str(menu_line);
            }
            *slot = ui::clip(line.trim_end(), cols);
        }
        if rows > 2 {
            frame[rows - 1] = self.footer(theme, info, cols);
        }

        self.hits = hits
            .into_iter()
            .map(|(index, offset, width, entry)| Hit {
                row: menu_top + index,
                left: left + board_w + offset,
                width,
                entry,
            })
            .filter(|hit| hit.row < rows.saturating_sub(1))
            .collect();
        frame
    }

    /// The column of text beside the board, with each menu row's index,
    /// left offset and width for hit testing.
    #[allow(clippy::type_complexity)]
    fn menu(
        &self,
        theme: &Theme,
        info: &HomeInfo,
        width: usize,
        tall: bool,
    ) -> (Vec<String>, Vec<(usize, usize, usize, Entry)>) {
        let accent = theme.palette.accent;
        let mut lines = Vec::new();
        let mut hits = Vec::new();

        if tall && !theme.ascii && width >= 24 {
            for row in TITLE {
                lines.push(format!("  {}", theme.strong(accent, row)));
            }
        } else {
            let title = if theme.ascii {
                "C H E S S"
            } else {
                "\u{265e}  C H E S S"
            };
            lines.push(format!("  {}", theme.strong(accent, title)));
        }
        lines.push(format!("  {}", theme.dim("Play chess in your terminal")));
        lines.push(String::new());

        let sections: [(&str, &[Entry]); 3] = [
            (
                "PLAY",
                &[Entry::White, Entry::Black, Entry::Two, Entry::Continue],
            ),
            ("ONLINE", &[Entry::Create, Entry::Join, Entry::Rejoin]),
            ("", &[Entry::Quit]),
        ];
        for (index, (heading, members)) in sections.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            if !heading.is_empty() {
                lines.push(format!("  {}", theme.strong(theme.palette.label, heading)));
            }
            for &entry in members.iter().filter(|e| self.entries.contains(e)) {
                let focused = self.focused() == entry;
                let (text, row_w) = self.item(theme, info, entry, focused, width);
                hits.push((lines.len(), 0, row_w, entry));
                lines.push(text);
            }
        }

        lines.push(String::new());
        lines.push(format!("  {}", theme.rule(width.saturating_sub(2))));
        let room = width.saturating_sub(2);
        for detail in self.detail(theme, info) {
            lines.push(format!("  {}", ui::clip_note(&detail, room)));
        }
        if let Some(complaint) = &self.complaint {
            lines.push(format!("  {}", theme.warn(&ui::clip_note(complaint, room))));
        }
        (lines, hits)
    }

    /// One menu row and how many columns of it answer a click.
    fn item(
        &self,
        theme: &Theme,
        info: &HomeInfo,
        entry: Entry,
        focused: bool,
        width: usize,
    ) -> (String, usize) {
        const LABEL: usize = 22;
        let unusable = match entry {
            Entry::Continue => matches!(info.saved, Some(Err(_))),
            Entry::Rejoin => matches!(info.seat, Some(Err(_))),
            _ => false,
        };
        let marker = match (focused, theme.ascii) {
            (false, _) => " ",
            (true, true) => ">",
            (true, false) => "\u{203a}",
        };
        let key = entry.key().to_string();
        let key = if unusable {
            theme.dim(&key)
        } else {
            theme.strong(theme.palette.accent, &key)
        };
        let label = format!(" {} ", ui::pad(entry.label(), LABEL));
        let label = if focused {
            theme.focused(theme.palette.accent, &label)
        } else if unusable {
            theme.dim(&label)
        } else {
            label
        };
        let meta = self.meta(info, entry).unwrap_or_default();
        let line = format!(
            "{} {}  {} {}",
            theme.strong(theme.palette.accent, marker),
            key,
            label,
            theme.dim(&meta)
        );
        let line = ui::clip(line.trim_end(), width);
        let row_w = ui::width(&line);
        (line, row_w)
    }

    /// A few words to the right of a row, where a row has something to add.
    fn meta(&self, info: &HomeInfo, entry: Entry) -> Option<String> {
        match entry {
            Entry::Continue => match &info.saved {
                Some(Ok((game, _))) if outcome(game).is_some() => Some("finished".to_string()),
                Some(Ok((game, _))) => Some(format!("move {}", game.sans.len() / 2 + 1)),
                Some(Err(_)) => Some("unreadable".to_string()),
                None => None,
            },
            Entry::Rejoin => match &info.seat {
                Some(Ok(seat)) => Some(format!("as {}", seat.side.to_ascii_lowercase())),
                Some(Err(_)) => Some("unreadable".to_string()),
                None => None,
            },
            _ => None,
        }
    }

    /// Two lines under the menu about the focused row, or the code being typed.
    fn detail(&self, theme: &Theme, info: &HomeInfo) -> Vec<String> {
        let server = |url: &str| theme.dim(&format!("Server {url}"));
        if let Some(code) = &self.code {
            let caret = if theme.ascii { "_" } else { "\u{2581}" };
            let field = format!(" {:<8}", format!("{code}{caret}"));
            return vec![
                format!(
                    "{}  {}",
                    theme.bold("Invite code"),
                    theme.focused(theme.palette.accent, &field)
                ),
                theme.dim("Enter to join as Black · Esc to go back"),
            ];
        }
        match self.focused() {
            Entry::White => vec![
                "You move first; the engine answers as Black.".to_string(),
                theme.dim("Type moves like e4 or Nf3, or click the pieces."),
            ],
            Entry::Black => vec![
                "The engine opens as White and you reply.".to_string(),
                theme.dim("The board turns so your pieces are at the bottom."),
            ],
            Entry::Two => vec![
                "Two people share this keyboard.".to_string(),
                theme.dim("The engine stays quiet unless someone asks for a hint."),
            ],
            Entry::Continue => match &info.saved {
                Some(Ok((game, mode))) => {
                    let who = match mode {
                        Mode::HumanWhite => "Against the engine as White",
                        Mode::HumanBlack => "Against the engine as Black",
                        Mode::TwoPlayer => "Two players at one keyboard",
                    };
                    let state = match outcome(game) {
                        Some(result) => format!("finished: {}", outcome_detail(&result)),
                        None if game.paused => "paused".to_string(),
                        None => format!("{} to move", game.pos.side.name()),
                    };
                    vec![
                        format!("{who} · {state}."),
                        theme.dim("Picks up where you left off, clocks included."),
                    ]
                }
                _ => vec![theme.warn("The autosaved game could not be read.")],
            },
            Entry::Create => vec![
                "Start a private game as White and get an invite code.".to_string(),
                server(&info.server_url),
            ],
            Entry::Join => vec![
                "Play Black in a friend's game with their code.".to_string(),
                server(&info.server_url),
            ],
            Entry::Rejoin => match &info.seat {
                Some(Ok(seat)) => vec![
                    "Reconnect to your last online game.".to_string(),
                    server(&seat.server_url),
                ],
                _ => vec![theme.warn("The last online seat could not be read.")],
            },
            Entry::Quit => vec![
                "Back to the shell.".to_string(),
                theme.dim("Preferences and unfinished games are saved as you play."),
            ],
        }
    }

    /// The board the focused choice would open on: the saved position for
    /// Continue, the start position from the player's side otherwise.
    fn preview(&mut self, theme: &Theme, info: &HomeInfo, metrics: Metrics) -> Vec<String> {
        let saved = match (self.focused(), &info.saved) {
            (Entry::Continue, Some(Ok((game, mode)))) => Some((game, *mode)),
            _ => None,
        };
        let flipped = match self.focused() {
            Entry::Black | Entry::Join => true,
            Entry::Continue => saved.is_some_and(|(_, mode)| mode == Mode::HumanBlack),
            Entry::Rejoin => matches!(
                &info.seat,
                Some(Ok(seat)) if color_named(&seat.side) == Some(Color::Black)
            ),
            _ => false,
        };
        let key = PreviewKey {
            saved: saved.is_some(),
            flipped,
            metrics,
            palette: theme.palette,
        };
        if let Some((cached, lines)) = &self.board_cache {
            if *cached == key {
                return lines.clone();
            }
        }
        let game = saved.map_or(&self.start, |(game, _)| game);
        let pos = &game.pos;
        let view = BoardView {
            pos,
            flipped,
            last: game.last_move(),
            check: in_check(pos, pos.side).then(|| pos.king[pos.side.index()]),
            selected: None,
            targets: &[],
            captures: &[],
            invalid: None,
            promotions: &[],
        };
        let lines = theme.board_lines(&view, metrics);
        self.board_cache = Some((key, lines.clone()));
        lines
    }

    fn footer(&self, theme: &Theme, info: &HomeInfo, cols: usize) -> String {
        let hints = if self.joining() {
            "type the code · Enter join · Esc back"
        } else if theme.ascii {
            "arrows choose · Enter start · click a row · q quit"
        } else {
            "\u{2191}\u{2193} choose · Enter start · click a row · q quit"
        };
        let hints = format!(" {hints}");
        let settings = format!("{} ", info.settings);
        let room = cols.saturating_sub(ui::width(&hints) + 4);
        if ui::width(&settings) <= room {
            let gap = cols - ui::width(&hints) - ui::width(&settings);
            format!(
                "{}{}{}",
                theme.dim(&hints),
                " ".repeat(gap),
                theme.label(&settings)
            )
        } else {
            theme.dim(&ui::clip(&hints, cols))
        }
    }
}

/// Codes are short and upper-case; anything else pasted around them goes.
fn clean_code(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect::<String>()
        .to_ascii_uppercase()
}

const TITLE: [&str; 3] = [
    "\u{2588}\u{2580}\u{2580} \u{2588} \u{2588} \u{2588}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580}",
    "\u{2588}   \u{2588}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580} \u{2580}\u{2580}\u{2588} \u{2580}\u{2580}\u{2588}",
    "\u{2580}\u{2580}\u{2580} \u{2580} \u{2580} \u{2580}\u{2580}\u{2580} \u{2580}\u{2580}\u{2580} \u{2580}\u{2580}\u{2580}",
];

/// Show the home page until the player picks something. Only for a live
/// colour terminal: it needs raw input and cursor addressing.
pub(crate) fn run(screen: &Screen, info: &HomeInfo) -> Result<Option<StartChoice>, String> {
    let mut input = TerminalInput::enter(true)?;
    let mut home = Home::new(info);
    let mut shown: Vec<String> = Vec::new();
    loop {
        let (cols, rows) = ui::terminal_size().unwrap_or((80, 24));
        let frame = home.render(
            &screen.theme,
            info,
            screen.pieces,
            cols.max(30),
            rows.max(12),
        );
        paint(&frame, &mut shown);

        let action = input.read_for(Duration::from_secs(1))?;
        let was_menu = !home.joining();
        let decided = home.handle(action, info, input.buffer());
        // Letters are shortcuts on the menu; only the code field keeps them.
        if was_menu || !home.joining() {
            input.clear_buffer();
        }
        if let Some(choice) = decided {
            return Ok(choice);
        }
    }
}

/// Rewrite only the rows that changed, as one synchronized update.
fn paint(frame: &[String], shown: &mut Vec<String>) {
    let resized = frame.len() != shown.len();
    let mut out = String::from("\x1b[?2026h\x1b[?25l");
    if resized {
        out.push_str("\x1b[2J");
    }
    for (row, line) in frame.iter().enumerate() {
        if resized || shown.get(row) != Some(line) {
            out.push_str(&format!("\x1b[{};1H{}\x1b[K", row + 1, line));
        }
    }
    out.push_str("\x1b[?2026l");
    print!("{out}");
    let _ = io::stdout().flush();
    *shown = frame.to_vec();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(saved: bool, seat: bool) -> HomeInfo {
        HomeInfo {
            saved: saved.then(|| {
                let mut game = Game::new(Position::startpos());
                let e4 = chess_core::san::parse_move(&game.pos, "e4").ok().unwrap();
                game.play(e4);
                Ok((game, Mode::HumanBlack))
            }),
            seat: seat.then(|| {
                Ok(storage::SavedOnlineSeat::new(
                    "ws://example.test/ws".to_string(),
                    "game".to_string(),
                    "token".to_string(),
                    "black".to_string(),
                ))
            }),
            server_url: "ws://127.0.0.1:3000/ws".to_string(),
            settings: "slate board · 10+0 clock · engine 3s · sound auto".to_string(),
        }
    }

    fn theme() -> Theme {
        Theme::new(true, false, true, ui::THEMES[0].1)
    }

    fn key(home: &mut Home, info: &HomeInfo, typed: &str) -> Option<Option<StartChoice>> {
        home.handle(Action::Prompt, info, typed)
    }

    #[test]
    fn letters_and_old_numbers_start_games() {
        let info = info(false, false);
        let mut home = Home::new(&info);
        assert!(matches!(
            key(&mut home, &info, "b"),
            Some(Some(StartChoice::Mode(Mode::HumanBlack)))
        ));
        assert!(matches!(
            key(&mut home, &info, "3"),
            Some(Some(StartChoice::Mode(Mode::TwoPlayer)))
        ));
        assert!(matches!(key(&mut home, &info, "q"), Some(None)));
        // Continue is not on offer without an autosave.
        assert!(key(&mut home, &info, "c").is_none());
        assert!(home.complaint.is_some());
    }

    #[test]
    fn an_unfinished_autosave_is_focused_first() {
        let info = info(true, false);
        let home = Home::new(&info);
        assert_eq!(home.focused(), Entry::Continue);
    }

    #[test]
    fn arrows_wrap_around_the_menu() {
        let info = info(false, true);
        let mut home = Home::new(&info);
        home.handle(Action::Focus { reverse: true }, &info, "");
        assert_eq!(home.focused(), Entry::Quit);
        home.handle(Action::Focus { reverse: true }, &info, "");
        assert_eq!(home.focused(), Entry::Rejoin);
        home.handle(Action::Focus { reverse: false }, &info, "");
        home.handle(Action::Focus { reverse: false }, &info, "");
        assert_eq!(home.focused(), Entry::White);
    }

    #[test]
    fn joining_collects_a_clean_code() {
        let info = info(false, false);
        let mut home = Home::new(&info);
        assert!(key(&mut home, &info, "j").is_none());
        assert!(home.joining());
        // Letters are part of the code now, not shortcuts.
        assert!(key(&mut home, &info, "q").is_none());
        assert!(home
            .handle(Action::Submit(" ".to_string()), &info, "")
            .is_none());
        assert!(home.complaint.is_some());
        match home.handle(Action::Submit("ab-12c".to_string()), &info, "") {
            Some(Some(StartChoice::Online(OnlineIntent::Join(code)))) => {
                assert_eq!(code, "AB12C")
            }
            _ => panic!("expected a join"),
        }
    }

    #[test]
    fn escape_leaves_the_code_field() {
        let info = info(false, false);
        let mut home = Home::new(&info);
        key(&mut home, &info, "j");
        home.handle(Action::Cancel, &info, "");
        assert!(!home.joining());
        assert_eq!(home.focused(), Entry::Join);
    }

    #[test]
    fn frames_fit_every_window_and_rows_answer_clicks() {
        let info = info(true, true);
        for (cols, rows) in [(30, 12), (60, 20), (80, 24), (120, 40), (200, 60)] {
            let mut home = Home::new(&info);
            let frame = home.render(&theme(), &info, ui::Pieces::Glyph, cols, rows);
            assert_eq!(frame.len(), rows);
            for line in &frame {
                assert!(ui::width(line) <= cols, "{cols}x{rows}: {line:?}");
            }
            let hit = *home.hits.iter().find(|h| h.entry == Entry::Two).unwrap();
            let chosen = home.handle(
                Action::Click {
                    column: (hit.left + 3) as u16,
                    row: hit.row as u16,
                },
                &info,
                "",
            );
            assert!(
                matches!(chosen, Some(Some(StartChoice::Mode(Mode::TwoPlayer)))),
                "{cols}x{rows}"
            );
        }
    }

    #[test]
    fn a_wide_window_shows_the_preview_board() {
        let info = info(false, false);
        let mut home = Home::new(&info);
        let frame = home.render(&theme(), &info, ui::Pieces::Glyph, 120, 40);
        let text = frame.join("\n");
        assert!(text.contains('\u{265a}') && text.contains('\u{2654}'));
        assert!(text.contains("PLAY") && text.contains("ONLINE"));
    }
}
