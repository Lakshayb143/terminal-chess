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

use crate::app::account::{account_menu, AccountContext};
use crate::app::cli::{Mode, OnlineIntent, Options, StartChoice};
use crate::app::format::others_online;
use crate::app::online::{random_clock_text, LobbyView, LobbyWatch};
use crate::app::prompt::ask_guest_name;
use crate::app::saves::{color_named, restore_game};
use crate::app::screen::Screen;

/// One thing the home page can start, in the order it is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    White,
    Black,
    Two,
    Continue,
    Random,
    Create,
    Join,
    Rejoin,
    Account,
    Quit,
}

impl Entry {
    fn key(self) -> char {
        match self {
            Entry::White => 'w',
            Entry::Black => 'b',
            Entry::Two => 't',
            Entry::Continue => 'c',
            Entry::Random => 'p',
            Entry::Create => 'o',
            Entry::Join => 'j',
            Entry::Rejoin => 'r',
            Entry::Account => 'a',
            Entry::Quit => 'q',
        }
    }

    fn label(self) -> &'static str {
        match self {
            Entry::White => "Play White",
            Entry::Black => "Play Black",
            Entry::Two => "Two players",
            Entry::Continue => "Continue saved game",
            Entry::Random => "Play a random opponent",
            Entry::Create => "Create a private game",
            Entry::Join => "Join with a code",
            Entry::Rejoin => "Rejoin online game",
            Entry::Account => "Account",
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

/// What picking a menu entry means: a decision to hand back to the caller,
/// or a sub-flow that needs ordinary line input the raw-mode page cannot
/// give it (a guest name, or the account page).
pub(crate) enum Pick {
    Choice(Option<StartChoice>),
    NeedsName(OnlineIntent),
    Account,
}

/// The name a game partner sees: the signed-in username, or the guest name
/// on offer.
fn player_name(account: &AccountContext) -> String {
    account
        .username()
        .map(str::to_string)
        .unwrap_or_else(|| account.guest_name.clone())
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

/// Where the preview board landed on the last frame, for laying a picture of
/// it over the text squares.
#[derive(Clone, Copy, PartialEq)]
struct Placed {
    key: PreviewKey,
    /// The top-left square, in zero-based cells.
    column: usize,
    row: usize,
    /// The first column right of the board, where the rest of a row starts.
    beside: usize,
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
    placed: Option<Placed>,
    /// For each row of the last frame, the byte where the text right of the
    /// board begins, or 0 when the row does not cross the board.
    splits: Vec<usize>,
    /// Who is online, for anyone thinking of playing a stranger.
    pub(crate) lobby: LobbyView,
}

impl Home {
    pub(crate) fn new(info: &HomeInfo) -> Home {
        let mut entries = vec![Entry::White, Entry::Black, Entry::Two];
        if info.saved.is_some() {
            entries.push(Entry::Continue);
        }
        entries.extend([Entry::Random, Entry::Create, Entry::Join]);
        if info.seat.is_some() {
            entries.push(Entry::Rejoin);
        }
        entries.push(Entry::Account);
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
            placed: None,
            splits: Vec::new(),
            lobby: LobbyView::Checking,
        }
    }

    pub(crate) fn focused(&self) -> Entry {
        self.entries[self.focus]
    }

    pub(crate) fn joining(&self) -> bool {
        self.code.is_some()
    }

    /// `Some` once the player has decided something, or a sub-flow the caller
    /// must run before the page can continue. `typed` is the input line as
    /// it stands, for the invite code.
    pub(crate) fn handle(
        &mut self,
        action: Action,
        info: &HomeInfo,
        account: &AccountContext,
        typed: &str,
    ) -> Option<Pick> {
        match action {
            Action::Quit => Some(Pick::Choice(None)),
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
                self.choose(entry, info, account)
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
                    Some(entry) if self.entries.contains(&entry) => {
                        self.choose(entry, info, account)
                    }
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
                        Some(self.online_pick(OnlineIntent::Join(code), account))
                    }
                }
                None => self.choose(self.focused(), info, account),
            },
        }
    }

    fn choose(&mut self, entry: Entry, info: &HomeInfo, account: &AccountContext) -> Option<Pick> {
        if let Some(index) = self.entries.iter().position(|&e| e == entry) {
            self.focus = index;
        }
        self.complaint = None;
        match entry {
            Entry::White => Some(Pick::Choice(Some(StartChoice::Mode(Mode::HumanWhite)))),
            Entry::Black => Some(Pick::Choice(Some(StartChoice::Mode(Mode::HumanBlack)))),
            Entry::Two => Some(Pick::Choice(Some(StartChoice::Mode(Mode::TwoPlayer)))),
            Entry::Continue => match &info.saved {
                Some(Ok(_)) => Some(Pick::Choice(Some(StartChoice::Resume))),
                Some(Err(why)) => {
                    self.complaint = Some(format!("The autosave cannot be restored: {why}"));
                    None
                }
                None => None,
            },
            Entry::Random => Some(self.online_pick(OnlineIntent::Find, account)),
            Entry::Create => Some(self.online_pick(OnlineIntent::Create, account)),
            Entry::Join => {
                self.code = Some(String::new());
                None
            }
            Entry::Rejoin => match &info.seat {
                Some(Ok(_)) => Some(Pick::Choice(Some(StartChoice::Online(
                    OnlineIntent::Resume,
                    player_name(account),
                )))),
                Some(Err(why)) => {
                    self.complaint = Some(format!("The online seat cannot be read: {why}"));
                    None
                }
                None => None,
            },
            Entry::Account => Some(Pick::Account),
            Entry::Quit => Some(Pick::Choice(None)),
        }
    }

    /// Signed-in players start online games right away; guests are asked for
    /// a name first.
    fn online_pick(&self, intent: OnlineIntent, account: &AccountContext) -> Pick {
        match account.username() {
            Some(name) => Pick::Choice(Some(StartChoice::Online(intent, name.to_string()))),
            None => Pick::NeedsName(intent),
        }
    }

    // -- drawing ------------------------------------------------------------

    /// Exactly `rows` lines, none wider than `cols`.
    pub(crate) fn render(
        &mut self,
        theme: &Theme,
        info: &HomeInfo,
        account: &AccountContext,
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
        // A figurine is one character, lost in the middle of a big square.
        let sizes: &[usize] = if pieces == ui::Pieces::Glyph || theme.ascii {
            &[2, 1]
        } else {
            &[3, 2, 1]
        };
        let metrics = sizes.iter().find_map(|&cell_h| {
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

        let (menu, hits) = self.menu(theme, info, account, menu_w, tall);
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
        self.splits = vec![0; rows];
        let body = rows.saturating_sub(1).saturating_sub(top).min(block_h);
        for (row, slot) in frame.iter_mut().enumerate().skip(top).take(body) {
            let mut line = " ".repeat(left);
            let mut split = None;
            if let Some(board_line) = row.checked_sub(board_top).and_then(|i| board.get(i)) {
                line.push_str(board_line);
                split = Some(line.len());
                line.push_str(&" ".repeat(GAP));
            } else {
                line.push_str(&" ".repeat(board_w));
            }
            if let Some(menu_line) = row.checked_sub(menu_top).and_then(|i| menu.get(i)) {
                line.push_str(menu_line);
            }
            *slot = ui::clip(line.trim_end(), cols);
            // Only while clipping left the board part of the row as it was.
            self.splits[row] = split
                .filter(|&at| slot.get(..at) == line.get(..at))
                .unwrap_or(0);
        }
        if rows > 2 {
            frame[rows - 1] = self.footer(theme, info, cols);
        }
        self.placed = metrics
            .zip(self.board_cache.as_ref())
            .map(|(metrics, (key, _))| Placed {
                key: *key,
                column: left + ui::GUTTER,
                row: board_top + 1,
                beside: left + metrics.board_width(),
            });

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
        account: &AccountContext,
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
        let who = match account.username() {
            Some(name) => format!("signed in as {name}"),
            None => "playing as a guest".to_string(),
        };
        lines.push(format!("  {}", theme.dim(&who)));
        lines.push(String::new());

        let sections: [(&str, &[Entry]); 3] = [
            (
                "PLAY",
                &[Entry::White, Entry::Black, Entry::Two, Entry::Continue],
            ),
            (
                "ONLINE",
                &[Entry::Random, Entry::Create, Entry::Join, Entry::Rejoin],
            ),
            ("", &[Entry::Account, Entry::Quit]),
        ];
        for (index, (heading, members)) in sections.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            if !heading.is_empty() {
                let mut line = format!("  {}", theme.strong(theme.palette.label, heading));
                if *heading == "ONLINE" {
                    line.push_str("  ");
                    line.push_str(&self.lobby_badge(theme));
                }
                lines.push(ui::clip(&line, width));
            }
            for &entry in members.iter().filter(|e| self.entries.contains(e)) {
                let focused = self.focused() == entry;
                let (text, row_w) = self.item(theme, info, account, entry, focused, width);
                hits.push((lines.len(), 0, row_w, entry));
                lines.push(text);
            }
        }

        lines.push(String::new());
        lines.push(format!("  {}", theme.rule(width.saturating_sub(2))));
        let room = width.saturating_sub(2);
        for detail in self.detail(theme, info, account) {
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
        account: &AccountContext,
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
        let meta = match (entry, self.lobby) {
            // Someone ready to play at once is worth more than a grey aside.
            (Entry::Random, LobbyView::Known(lobby)) if lobby.seeking > 0 => theme.strong(
                theme.palette.good,
                &format!("{} waiting now", lobby.seeking),
            ),
            _ => theme.dim(&self.meta(info, account, entry).unwrap_or_default()),
        };
        let line = format!(
            "{} {}  {} {}",
            theme.strong(theme.palette.accent, marker),
            key,
            label,
            meta
        );
        let line = ui::clip(line.trim_end(), width);
        let row_w = ui::width(&line);
        (line, row_w)
    }

    /// A few words to the right of a row, where a row has something to add.
    fn meta(&self, info: &HomeInfo, account: &AccountContext, entry: Entry) -> Option<String> {
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
            Entry::Account => Some(match account.username() {
                Some(_) => "your recent games".to_string(),
                None => "sign in or sign up".to_string(),
            }),
            _ => None,
        }
    }

    /// Who is online, on a block of colour beside the ONLINE heading so it
    /// is seen whichever row has the focus: green with company, the warning
    /// colour alone, grey while the server is unknown or away.
    fn lobby_badge(&self, theme: &Theme) -> String {
        let dot = if theme.ascii { "*" } else { "\u{25cf}" };
        match self.lobby {
            LobbyView::Known(lobby) if lobby.online <= 1 => {
                theme.badge(theme.palette.warn, &format!("{dot} only you online"))
            }
            LobbyView::Known(lobby) => theme.badge(
                theme.palette.good,
                &format!("{dot} {} online", lobby.online),
            ),
            LobbyView::Checking => theme.dim("checking who is online…"),
            LobbyView::Unavailable => theme.badge(theme.palette.label, "server offline"),
        }
    }

    /// Two lines under the menu about the focused row, or the code being typed.
    fn detail(&self, theme: &Theme, info: &HomeInfo, account: &AccountContext) -> Vec<String> {
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
            Entry::Random => self.random_detail(theme),
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
            Entry::Account => match account.username() {
                Some(name) => vec![
                    format!("Signed in as {name}."),
                    theme.dim("See your recent games or sign out."),
                ],
                None => vec![
                    "Keep your name and games on every computer.".to_string(),
                    theme.dim("Sign in, or create a free account."),
                ],
            },
            Entry::Quit => vec![
                "Back to the shell.".to_string(),
                theme.dim("Preferences and unfinished games are saved as you play."),
            ],
        }
    }

    /// What playing a stranger would be like right now. Being the only one
    /// online is said plainly, with the choices that need no stranger.
    fn random_detail(&self, theme: &Theme) -> Vec<String> {
        let terms = theme.dim(&format!(
            "{} clock · colours drawn at random",
            random_clock_text()
        ));
        match self.lobby {
            LobbyView::Known(lobby) if lobby.seeking > 0 => {
                let who = match lobby.seeking {
                    1 => "Someone is".to_string(),
                    waiting => format!("{waiting} people are"),
                };
                vec![
                    theme.good(&format!("{who} waiting: you start at once.")),
                    terms,
                ]
            }
            LobbyView::Known(lobby) if lobby.online <= 1 => vec![
                theme.warn("No one else is online right now."),
                theme.dim("Wait for someone, or play Two players or a"),
                theme.dim("private game with a friend instead."),
            ],
            LobbyView::Known(lobby) => vec![
                format!(
                    "{} online: play whoever looks next.",
                    others_online(lobby.online - 1)
                ),
                terms,
            ],
            LobbyView::Checking => vec![
                "Play whoever else is looking for a game.".to_string(),
                theme.dim("Checking who is online…"),
            ],
            LobbyView::Unavailable => vec![
                "Play whoever else is looking for a game.".to_string(),
                theme.warn("The game server cannot be reached."),
            ],
        }
    }

    /// The board the focused choice would open on: the saved position for
    /// Continue, the start position from the player's side otherwise.
    fn preview(&mut self, theme: &Theme, info: &HomeInfo, metrics: Metrics) -> Vec<String> {
        let (game, saved, flipped) = self.previewed(info);
        let key = PreviewKey {
            saved,
            flipped,
            metrics,
            palette: theme.palette,
        };
        if let Some((cached, lines)) = &self.board_cache {
            if *cached == key {
                return lines.clone();
            }
        }
        let lines = theme.board_lines(&preview_view(game, flipped), metrics);
        self.board_cache = Some((key, lines.clone()));
        lines
    }

    /// The game the preview shows, whether it is the autosave, and whether
    /// Black is at the bottom.
    fn previewed<'a>(&'a self, info: &'a HomeInfo) -> (&'a Game, bool, bool) {
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
        let game = saved.map_or(&self.start, |(game, _)| game);
        (game, saved.is_some(), flipped)
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

/// The preview as the board is drawn: no selection, no hints.
fn preview_view(game: &Game, flipped: bool) -> BoardView<'_> {
    let pos = &game.pos;
    BoardView {
        pos,
        flipped,
        last: game.last_move(),
        check: in_check(pos, pos.side).then(|| pos.king[pos.side.index()]),
        selected: None,
        targets: &[],
        captures: &[],
        invalid: None,
        promotions: &[],
    }
}

/// Drawn pieces only where the terminal can show them as a picture. The
/// block-art fallback is too coarse at preview size to tell a queen from a
/// king, so `auto` shows figurines instead; asking for art by name still
/// gets art.
fn preview_pieces(pieces: ui::Pieces, protocol: Option<ui::ImageProtocol>) -> ui::Pieces {
    match (pieces, protocol) {
        (ui::Pieces::Auto, None) => ui::Pieces::Glyph,
        _ => pieces,
    }
}

/// A picture of the preview board laid over its text squares, the same one
/// the game draws, for terminals that can show one. The text board stays
/// underneath as the fallback.
struct Picture {
    protocol: Option<ui::ImageProtocol>,
    /// The board the picture on screen shows, if one is there.
    drawn: Option<Placed>,
    /// The encoded picture, for sending again after a clear without
    /// encoding it again.
    bytes: Option<(Placed, Vec<u8>)>,
}

impl Picture {
    /// What the picture should show on this frame: only drawn pieces, and
    /// only when the player left the piece style on auto, as in the game.
    fn wanted(&self, home: &Home, pieces: ui::Pieces) -> Option<Placed> {
        let placed = home.placed?;
        (self.protocol.is_some() && pieces == ui::Pieces::Auto && placed.key.metrics.art)
            .then_some(placed)
    }

    fn kitty(&self) -> bool {
        self.protocol == Some(ui::ImageProtocol::Kitty)
    }

    /// Escapes to send before a frame's rows. A Kitty picture floats above
    /// the text, so one that is about to change or go has to be taken down.
    fn before(&mut self, wanted: Option<Placed>, cleared: bool) -> &'static str {
        if self.kitty() && self.drawn.is_some() && (cleared || wanted != self.drawn) {
            self.drawn = None;
            ui::KITTY_DELETE_ALL
        } else {
            ""
        }
    }

    /// Draw the picture after the frame's rows, if it is not already there.
    /// A clear takes a picture in the text cells with it.
    fn draw(
        &mut self,
        wanted: Option<Placed>,
        cleared: bool,
        theme: &Theme,
        home: &Home,
        info: &HomeInfo,
    ) {
        let (Some(protocol), Some(placed)) = (self.protocol, wanted) else {
            self.drawn = None;
            return;
        };
        if self.drawn == Some(placed) && !cleared {
            return;
        }
        let (game, _, flipped) = home.previewed(info);
        let view = preview_view(game, flipped);
        let metrics = placed.key.metrics;
        if protocol == ui::ImageProtocol::Kitty {
            let image = theme.board_image(&view);
            ui::draw_kitty_image(&image, metrics, placed.column, placed.row);
        } else {
            if !self.bytes.as_ref().is_some_and(|(key, _)| *key == placed) {
                let bytes = ui::inline_image_bytes(
                    protocol,
                    theme,
                    &view,
                    metrics,
                    placed.column,
                    placed.row,
                );
                self.bytes = Some((placed, bytes));
            }
            if let Some((_, bytes)) = &self.bytes {
                let _ = io::stdout().write_all(bytes);
            }
        }
        self.drawn = Some(placed);
    }

    /// Take the picture down before something else uses the window. A clear
    /// is enough for the other protocols.
    fn remove(&mut self) {
        if self.kitty() && self.drawn.is_some() {
            print!("{}", ui::KITTY_DELETE_ALL);
            let _ = io::stdout().flush();
        }
        self.drawn = None;
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
/// colour terminal: it needs raw input and cursor addressing. Signing in,
/// the account page, and a guest name are all asked with ordinary line
/// input, the same forms the piped-terminal menu uses, so raw mode is
/// suspended for as long as one of those is on screen.
pub(crate) fn run(
    screen: &mut Screen,
    info: &HomeInfo,
    account: &mut AccountContext,
) -> Result<Option<StartChoice>, String> {
    let mut input = TerminalInput::enter(true)?;
    let mut home = Home::new(info);
    let mut lobby = LobbyWatch::start(&info.server_url);
    let mut shown: Vec<String> = Vec::new();
    let mut picture = Picture {
        protocol: screen.image_protocol,
        drawn: None,
        bytes: None,
    };
    let pieces = preview_pieces(screen.pieces, screen.image_protocol);
    loop {
        home.lobby = lobby.poll();
        let (cols, rows) = ui::terminal_size().unwrap_or((80, 24));
        let frame = home.render(
            &screen.theme,
            info,
            account,
            pieces,
            cols.max(30),
            rows.max(12),
        );
        let wanted = picture.wanted(&home, pieces);
        let cleared = frame.len() != shown.len();
        // One synchronized update: the rows, then the picture over them.
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        out.push_str(picture.before(wanted, cleared));
        let beside = home.placed.map_or(0, |placed| placed.beside);
        paint(&frame, &home.splits, beside, &mut shown, &mut out);
        print!("{out}");
        let _ = io::stdout().flush();
        picture.draw(wanted, cleared, &screen.theme, &home, info);
        print!("\x1b[?2026l");
        let _ = io::stdout().flush();

        let action = input.read_for(Duration::from_secs(1))?;
        let was_menu = !home.joining();
        let decided = home.handle(action, info, account, input.buffer());
        // Letters are shortcuts on the menu; only the code field keeps them.
        if was_menu || !home.joining() {
            input.clear_buffer();
        }
        if decided.is_some() {
            picture.remove();
        }
        match decided {
            None => {}
            Some(Pick::Choice(choice)) => return Ok(choice),
            Some(Pick::Account) => {
                input.suspend()?;
                let mut stdin = io::stdin().lock();
                let outcome = account_menu(&mut stdin, screen, account);
                drop(stdin);
                input.resume()?;
                shown.clear();
                outcome?;
            }
            Some(Pick::NeedsName(intent)) => {
                input.suspend()?;
                let mut stdin = io::stdin().lock();
                let name = ask_guest_name(&mut stdin, screen, account);
                drop(stdin);
                input.resume()?;
                shown.clear();
                if let Some(name) = name? {
                    return Ok(Some(StartChoice::Online(intent, name)));
                }
            }
        }
    }
}

/// Add the escapes that rewrite only the rows that changed. Where a row's
/// board part is as it was, only the text beside it is rewritten, from
/// column `beside`, so moving through the menu leaves a picture of the board
/// alone.
fn paint(
    frame: &[String],
    splits: &[usize],
    beside: usize,
    shown: &mut Vec<String>,
    out: &mut String,
) {
    let resized = frame.len() != shown.len();
    if resized {
        out.push_str("\x1b[2J");
    }
    for (row, line) in frame.iter().enumerate() {
        let before = shown.get(row);
        if !resized && before == Some(line) {
            continue;
        }
        let at = splits.get(row).copied().unwrap_or(0);
        if !resized && at > 0 && before.and_then(|b| b.get(..at)) == line.get(..at) {
            out.push_str(&format!(
                "\x1b[{};{}H\x1b[0m{}\x1b[K",
                row + 1,
                beside + 1,
                &line[at..]
            ));
        } else {
            out.push_str(&format!("\x1b[{};1H{}\x1b[K", row + 1, line));
        }
    }
    *shown = frame.to_vec();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_protocol::Lobby;

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

    /// Never signed in and never touches a real server: safe for tests.
    fn guest() -> AccountContext {
        AccountContext::load(
            Path::new("/nonexistent/config.json"),
            "ws://example.test/ws",
            "Guest".to_string(),
        )
    }

    fn key(
        home: &mut Home,
        info: &HomeInfo,
        account: &AccountContext,
        typed: &str,
    ) -> Option<Pick> {
        home.handle(Action::Prompt, info, account, typed)
    }

    #[test]
    fn letters_and_old_numbers_start_games() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        assert!(matches!(
            key(&mut home, &info, &account, "b"),
            Some(Pick::Choice(Some(StartChoice::Mode(Mode::HumanBlack))))
        ));
        assert!(matches!(
            key(&mut home, &info, &account, "3"),
            Some(Pick::Choice(Some(StartChoice::Mode(Mode::TwoPlayer))))
        ));
        assert!(matches!(
            key(&mut home, &info, &account, "q"),
            Some(Pick::Choice(None))
        ));
        // Continue is not on offer without an autosave.
        assert!(key(&mut home, &info, &account, "c").is_none());
        assert!(home.complaint.is_some());
    }

    #[test]
    fn the_random_opponent_row_says_who_is_online() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        assert!(matches!(
            key(&mut home, &info, &account, "p"),
            Some(Pick::NeedsName(OnlineIntent::Find))
        ));
        assert_eq!(home.focused(), Entry::Random);
        let mut shown = |lobby: Lobby| {
            home.lobby = LobbyView::Known(lobby);
            home.render(&theme(), &info, &account, ui::Pieces::Glyph, 120, 40)
                .join("\n")
        };

        let alone = shown(Lobby {
            online: 1,
            seeking: 0,
        });
        assert!(alone.contains("only you online"));
        assert!(alone.contains("No one else is online right now."));
        assert!(
            alone.contains("private game with a friend instead."),
            "{alone}"
        );

        let someone_waits = shown(Lobby {
            online: 4,
            seeking: 1,
        });
        assert!(someone_waits.contains("1 waiting now"));
        assert!(someone_waits.contains("Someone is waiting: you start at once."));

        let others = shown(Lobby {
            online: 3,
            seeking: 0,
        });
        assert!(others.contains("3 online"));
        assert!(others.contains("2 others online: play whoever looks next."));

        // The count needs no focus: it shows while another row is chosen.
        let mut elsewhere = Home::new(&info);
        assert_eq!(elsewhere.focused(), Entry::White);
        elsewhere.lobby = LobbyView::Known(Lobby {
            online: 1,
            seeking: 0,
        });
        let page = elsewhere
            .render(&theme(), &info, &account, ui::Pieces::Glyph, 120, 40)
            .join("\n");
        assert!(page.contains("only you online"), "{page}");

        // The longest explanation still fits the smallest window.
        home.lobby = LobbyView::Known(Lobby {
            online: 1,
            seeking: 0,
        });
        for (cols, rows) in [(30, 12), (60, 20), (80, 24)] {
            let frame = home.render(&theme(), &info, &account, ui::Pieces::Glyph, cols, rows);
            assert_eq!(frame.len(), rows);
            assert!(frame.iter().all(|line| ui::width(line) <= cols));
        }
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
        let account = guest();
        let mut home = Home::new(&info);
        home.handle(Action::Focus { reverse: true }, &info, &account, "");
        assert_eq!(home.focused(), Entry::Quit);
        home.handle(Action::Focus { reverse: true }, &info, &account, "");
        assert_eq!(home.focused(), Entry::Account);
        home.handle(Action::Focus { reverse: true }, &info, &account, "");
        assert_eq!(home.focused(), Entry::Rejoin);
        home.handle(Action::Focus { reverse: false }, &info, &account, "");
        home.handle(Action::Focus { reverse: false }, &info, &account, "");
        home.handle(Action::Focus { reverse: false }, &info, &account, "");
        assert_eq!(home.focused(), Entry::White);
    }

    #[test]
    fn joining_as_a_guest_asks_for_a_name() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        assert!(key(&mut home, &info, &account, "j").is_none());
        assert!(home.joining());
        // Letters are part of the code now, not shortcuts.
        assert!(key(&mut home, &info, &account, "q").is_none());
        assert!(home
            .handle(Action::Submit(" ".to_string()), &info, &account, "")
            .is_none());
        assert!(home.complaint.is_some());
        match home.handle(Action::Submit("ab-12c".to_string()), &info, &account, "") {
            Some(Pick::NeedsName(OnlineIntent::Join(code))) => {
                assert_eq!(code, "AB12C")
            }
            _ => panic!("expected a join to need a guest name"),
        }
    }

    #[test]
    fn escape_leaves_the_code_field() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        key(&mut home, &info, &account, "j");
        home.handle(Action::Cancel, &info, &account, "");
        assert!(!home.joining());
        assert_eq!(home.focused(), Entry::Join);
    }

    #[test]
    fn frames_fit_every_window_and_rows_answer_clicks() {
        let info = info(true, true);
        let account = guest();
        for (cols, rows) in [(30, 12), (60, 20), (80, 24), (120, 40), (200, 60)] {
            let mut home = Home::new(&info);
            let frame = home.render(&theme(), &info, &account, ui::Pieces::Glyph, cols, rows);
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
                &account,
                "",
            );
            assert!(
                matches!(
                    chosen,
                    Some(Pick::Choice(Some(StartChoice::Mode(Mode::TwoPlayer))))
                ),
                "{cols}x{rows}"
            );
        }
    }

    #[test]
    fn a_wide_window_shows_the_preview_board() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        let frame = home.render(&theme(), &info, &account, ui::Pieces::Glyph, 120, 40);
        let text = frame.join("\n");
        assert!(text.contains('\u{265a}') && text.contains('\u{2654}'));
        assert!(text.contains("PLAY") && text.contains("ONLINE"));
    }

    #[test]
    fn moving_through_the_menu_leaves_the_board_alone() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        let mut shown = Vec::new();
        let first = home.render(&theme(), &info, &account, ui::Pieces::Auto, 120, 40);
        paint(&first, &home.splits, 0, &mut shown, &mut String::new());
        let placed = home.placed.expect("a wide window places the board");
        assert!(placed.key.metrics.art);

        // White to Two players: the preview stays the same board.
        home.handle(Action::Focus { reverse: false }, &info, &account, "");
        home.handle(Action::Focus { reverse: false }, &info, &account, "");
        let second = home.render(&theme(), &info, &account, ui::Pieces::Auto, 120, 40);
        assert!(home.placed == Some(placed));
        let mut out = String::new();
        paint(&second, &home.splits, placed.beside, &mut shown, &mut out);
        let squares = placed.row..placed.row + 8 * placed.key.metrics.cell_h;
        let beside = squares
            .clone()
            .filter(|row| out.contains(&format!("\x1b[{};{}H", row + 1, placed.beside + 1)))
            .count();
        assert!(beside > 0, "the focus moved on rows beside the board");
        for row in squares {
            assert!(!out.contains(&format!("\x1b[{};1H", row + 1)), "row {row}");
        }
        // Painting beside the board ends up with the same frame on screen.
        assert_eq!(shown, second);

        // Play Black turns the board round, so the picture has to follow.
        home.handle(Action::Focus { reverse: true }, &info, &account, "");
        home.render(&theme(), &info, &account, ui::Pieces::Auto, 120, 40);
        assert!(home
            .placed
            .is_some_and(|now| now.key.flipped && now != placed));
    }

    #[test]
    fn without_pictures_the_preview_uses_figurines_on_medium_squares() {
        let kitty = Some(ui::ImageProtocol::Kitty);
        assert_eq!(preview_pieces(ui::Pieces::Auto, None), ui::Pieces::Glyph);
        assert_eq!(preview_pieces(ui::Pieces::Auto, kitty), ui::Pieces::Auto);
        assert_eq!(preview_pieces(ui::Pieces::Art, None), ui::Pieces::Art);

        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        let pieces = preview_pieces(ui::Pieces::Auto, None);
        home.render(&theme(), &info, &account, pieces, 200, 60);
        let metrics = home
            .placed
            .expect("a big window places the board")
            .key
            .metrics;
        assert!(!metrics.art);
        assert_eq!((metrics.cell_w, metrics.cell_h), (4, 2));
    }

    #[test]
    fn a_kitty_picture_is_taken_down_before_it_changes() {
        let info = info(false, false);
        let account = guest();
        let mut home = Home::new(&info);
        home.render(&theme(), &info, &account, ui::Pieces::Auto, 120, 40);
        let mut picture = Picture {
            protocol: Some(ui::ImageProtocol::Kitty),
            drawn: None,
            bytes: None,
        };
        let wanted = picture.wanted(&home, ui::Pieces::Auto);
        assert!(wanted.is_some());
        assert!(picture.wanted(&home, ui::Pieces::Glyph).is_none());
        picture.drawn = wanted;
        assert_eq!(picture.before(wanted, false), "");
        assert_eq!(picture.before(wanted, true), ui::KITTY_DELETE_ALL);
        picture.drawn = wanted;
        assert_eq!(picture.before(None, false), ui::KITTY_DELETE_ALL);
        assert!(picture.drawn.is_none());
    }
}
