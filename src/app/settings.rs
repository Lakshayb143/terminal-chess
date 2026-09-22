//! Engine levels, clock presets, and the settings page that chooses them
//! before a game, along with the board's look and the sound.

use std::io::{self, Write};
use std::time::Duration;

use chess::input::{Action, TerminalInput};
use chess::{sound, storage, ui};
use chess_core::search::{Limits, MAX_DEPTH};

use crate::app::cli::{cap_hosted, Options};
use crate::app::format::trim_number;
use crate::app::screen::Screen;

/// How well the engine plays. Measured against each other, each level won
/// all 24 games against the one below it (Strong with only a second a move),
/// and Beginner all 24 against random moves: the steps are big ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Beginner,
    Casual,
    Club,
    Strong,
}

impl Level {
    pub(crate) const ALL: [Level; 4] = [Level::Beginner, Level::Casual, Level::Club, Level::Strong];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Level::Beginner => "beginner",
            Level::Casual => "casual",
            Level::Club => "club",
            Level::Strong => "strong",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Level::Beginner => "Beginner",
            Level::Casual => "Casual",
            Level::Club => "Club",
            Level::Strong => "Strong",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|level| level.name().eq_ignore_ascii_case(name.trim()))
    }

    /// What the engine searches, and how far from its best move it may
    /// stray. The weaker levels see little and choose loosely; the time is
    /// only a ceiling, which they rarely reach.
    pub(crate) fn limits(self) -> Limits {
        let (depth, randomness, seconds) = match self {
            Level::Beginner => (1, 350, 1),
            Level::Casual => (2, 120, 1),
            Level::Club => (4, 30, 2),
            Level::Strong => (MAX_DEPTH, 0, 3),
        };
        Limits {
            depth,
            movetime: Some(Duration::from_secs(seconds)),
            randomness,
        }
    }

    /// The level `limits` belong to. `--time`, `--depth` and the commands of
    /// the same names make limits of their own, which belong to none.
    pub(crate) fn of(limits: &Limits) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|level| level.limits() == *limits)
    }

    pub(crate) fn about(self) -> &'static str {
        match self {
            Level::Beginner => "Thinks one move ahead and makes big mistakes.",
            Level::Casual => "Thinks two moves ahead; its mistakes can be punished.",
            Level::Club => "Thinks four moves ahead and seldom blunders.",
            Level::Strong => "Plays its best, thinking up to 3 seconds a move.",
        }
    }
}

/// A clock setting: each player's starting time, `None` for no clock, and
/// the time added after each move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Clock {
    pub(crate) initial: Option<Duration>,
    pub(crate) increment: Duration,
}

impl Clock {
    const fn minutes(minutes: u64, increment: u64) -> Clock {
        Clock {
            initial: Some(Duration::from_secs(minutes * 60)),
            increment: Duration::from_secs(increment),
        }
    }

    const UNTIMED: Clock = Clock {
        initial: None,
        increment: Duration::ZERO,
    };

    /// The usual choices, quickest first.
    pub(crate) const PRESETS: [(&'static str, Clock); 6] = [
        ("Bullet", Clock::minutes(1, 0)),
        ("Blitz", Clock::minutes(3, 2)),
        ("Blitz", Clock::minutes(5, 0)),
        ("Rapid", Clock::minutes(10, 0)),
        ("Rapid", Clock::minutes(15, 10)),
        ("Untimed", Clock::UNTIMED),
    ];

    pub(crate) fn of(options: &Options) -> Clock {
        Clock {
            initial: options.clock,
            increment: options.increment,
        }
    }

    /// `10+5`: minutes, then the seconds added each move.
    pub(crate) fn text(self) -> String {
        match self.initial {
            Some(initial) => format!(
                "{}+{}",
                trim_number(initial.as_secs_f64() / 60.0),
                trim_number(self.increment.as_secs_f64())
            ),
            None => "untimed".to_string(),
        }
    }

    /// `Rapid 10+0`, `Untimed`, or `Custom 7+3`.
    pub(crate) fn label(self) -> String {
        match Clock::PRESETS.iter().find(|(_, preset)| *preset == self) {
            Some((_, preset)) if preset.initial.is_none() => "Untimed".to_string(),
            Some((name, preset)) => format!("{name} {}", preset.text()),
            None => format!("Custom {}", self.text()),
        }
    }

    /// How long a game on this clock might last, for ordering: forty moves
    /// each, as tournament rules reckon it.
    fn length(self) -> f64 {
        self.initial.map_or(f64::INFINITY, |initial| {
            initial.as_secs_f64() + 40.0 * self.increment.as_secs_f64()
        })
    }

    /// The preset beside this clock, or the nearest one on that side when
    /// it is not a preset.
    fn step(self, later: bool) -> Clock {
        let here = Clock::PRESETS
            .iter()
            .position(|(_, preset)| *preset == self);
        let index = match (here, later) {
            (Some(index), true) => (index + 1).min(Clock::PRESETS.len() - 1),
            (Some(index), false) => index.saturating_sub(1),
            (None, true) => Clock::PRESETS
                .iter()
                .position(|(_, preset)| preset.length() > self.length())
                .unwrap_or(Clock::PRESETS.len() - 1),
            (None, false) => Clock::PRESETS
                .iter()
                .rposition(|(_, preset)| preset.length() < self.length())
                .unwrap_or(0),
        };
        Clock::PRESETS[index].1
    }

    /// `7`, `7+3`, `1.5+0` or `untimed`, as typed on the settings page.
    pub(crate) fn parse(text: &str) -> Result<Clock, String> {
        let text = text.trim().to_ascii_lowercase();
        if matches!(text.as_str(), "untimed" | "none" | "off" | "no clock") {
            return Ok(Clock::UNTIMED);
        }
        let (minutes, increment) = text.split_once('+').unwrap_or((&text, "0"));
        let number = |part: &str| part.trim().parse::<f64>().ok().filter(|n| n.is_finite());
        let (Some(minutes), Some(increment)) = (number(minutes), number(increment)) else {
            return Err("Type minutes and seconds added, like 7+3.".to_string());
        };
        if !(minutes > 0.0 && minutes <= 24.0 * 60.0) {
            return Err("The starting time must be between 0 and 1440 minutes.".to_string());
        }
        if !(0.0..=3600.0).contains(&increment) {
            return Err("The seconds added must be between 0 and 3600.".to_string());
        }
        Ok(Clock {
            initial: Some(Duration::from_secs_f64(minutes * 60.0)),
            increment: Duration::from_secs_f64(increment),
        })
    }
}

/// One row of the settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Engine,
    Clock,
    Board,
    Pieces,
    Sound,
}

const ROWS: [Row; 5] = [Row::Engine, Row::Clock, Row::Board, Row::Pieces, Row::Sound];

impl Row {
    fn label(self) -> &'static str {
        match self {
            Row::Engine => "Engine",
            Row::Clock => "Clock",
            Row::Board => "Board",
            Row::Pieces => "Pieces",
            Row::Sound => "Sound",
        }
    }
}

/// Where a click lands on the settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Row(Row),
    Less(Row),
    More(Row),
    Back,
}

#[derive(Clone, Copy, Debug)]
struct Hit {
    row: usize,
    left: usize,
    width: usize,
    target: Target,
}

/// The page's own state; everything it changes lives in the options and
/// the screen, so a game started afterwards picks it up.
pub(crate) struct Settings {
    focus: usize,
    /// A clock being typed, such as `7+3`.
    typing: Option<String>,
    complaint: Option<String>,
    hits: Vec<Hit>,
    hosted: bool,
}

impl Settings {
    pub(crate) fn new(hosted: bool) -> Settings {
        Settings {
            focus: 0,
            typing: None,
            complaint: None,
            hits: Vec::new(),
            hosted,
        }
    }

    fn focused(&self) -> Row {
        ROWS[self.focus]
    }

    /// Returns true when the player is done with the page.
    pub(crate) fn handle(
        &mut self,
        action: Action,
        typed: &str,
        options: &mut Options,
        screen: &mut Screen,
    ) -> bool {
        match action {
            Action::Quit => return true,
            Action::Cancel => {
                // Esc puts a half-typed clock away first, then leaves.
                if self.typing.take().is_none() {
                    return true;
                }
                self.complaint = None;
            }
            Action::Arrow(direction) if self.typing.is_none() => match direction {
                chess::input::Direction::Up => self.move_focus(true),
                chess::input::Direction::Down => self.move_focus(false),
                chess::input::Direction::Left => {
                    self.change(self.focused(), false, options, screen)
                }
                chess::input::Direction::Right => {
                    self.change(self.focused(), true, options, screen)
                }
            },
            Action::Focus { reverse }
            | Action::History { older: reverse }
            | Action::Scroll { older: reverse } => {
                if self.typing.is_none() {
                    self.move_focus(reverse);
                }
            }
            Action::Click { column, row } => {
                let (column, row) = (usize::from(column), usize::from(row));
                let hit = self
                    .hits
                    .iter()
                    .find(|hit| {
                        hit.row == row && column >= hit.left && column < hit.left + hit.width
                    })
                    .map(|hit| hit.target);
                self.typing = None;
                match hit {
                    Some(Target::Back) => return true,
                    Some(Target::Row(row)) => self.focus_row(row),
                    Some(Target::Less(row)) => {
                        self.focus_row(row);
                        self.change(row, false, options, screen);
                    }
                    Some(Target::More(row)) => {
                        self.focus_row(row);
                        self.change(row, true, options, screen);
                    }
                    None => {}
                }
            }
            Action::Prompt => {
                let key = typed.chars().last();
                if self.focused() == Row::Clock
                    && (self.typing.is_some()
                        || key.is_some_and(|key| key.is_ascii_digit() || key == '.'))
                {
                    self.typing = Some(
                        typed
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .take(12)
                            .collect(),
                    );
                    self.complaint = None;
                } else if key.is_some_and(|key| key.eq_ignore_ascii_case(&'q')) {
                    return true;
                }
            }
            Action::Submit(text) => match self.typing.take() {
                Some(_) => match Clock::parse(&text) {
                    Ok(clock) => {
                        options.clock = clock.initial;
                        options.increment = clock.increment;
                        self.complaint = None;
                    }
                    Err(why) => {
                        self.typing = Some(text);
                        self.complaint = Some(why);
                    }
                },
                None => self.change(self.focused(), true, options, screen),
            },
            Action::Arrow(_)
            | Action::Edge { .. }
            | Action::Resize
            | Action::Tick
            | Action::WindowFocus(_) => {}
        }
        false
    }

    fn move_focus(&mut self, back: bool) {
        self.complaint = None;
        self.focus = if back {
            (self.focus + ROWS.len() - 1) % ROWS.len()
        } else {
            (self.focus + 1) % ROWS.len()
        };
    }

    fn focus_row(&mut self, row: Row) {
        self.complaint = None;
        if let Some(index) = ROWS.iter().position(|&r| r == row) {
            self.focus = index;
        }
    }

    /// Step `row` to its next (or previous) value.
    fn change(&mut self, row: Row, forward: bool, options: &mut Options, screen: &mut Screen) {
        self.complaint = None;
        match row {
            Row::Engine => {
                // A custom engine from --time or --depth sits between Casual
                // and Club as far as stepping goes.
                let index = Level::of(&options.limits)
                    .and_then(|level| Level::ALL.iter().position(|&l| l == level))
                    .unwrap_or(if forward { 1 } else { 2 });
                let next = if forward {
                    (index + 1).min(Level::ALL.len() - 1)
                } else {
                    index.saturating_sub(1)
                };
                let custom = Level::of(&options.limits).is_none();
                if custom || next != index {
                    options.limits = Level::ALL[next].limits();
                    if options.hosted {
                        cap_hosted(&mut options.limits);
                    }
                }
            }
            Row::Clock => {
                let next = Clock::of(options).step(forward);
                options.clock = next.initial;
                options.increment = next.increment;
            }
            Row::Board => {
                let names: Vec<&str> = ui::THEMES.iter().map(|(name, _)| *name).collect();
                let here = names
                    .iter()
                    .position(|&name| name == ui::palette_name(screen.theme.palette))
                    .unwrap_or(0);
                let next = cycle(here, names.len(), forward);
                screen.theme.palette = ui::THEMES[next].1;
            }
            Row::Pieces => {
                if screen.theme.ascii {
                    self.complaint =
                        Some("Plain ASCII boards always use letters for pieces.".to_string());
                    return;
                }
                const ORDER: [ui::Pieces; 3] =
                    [ui::Pieces::Auto, ui::Pieces::Glyph, ui::Pieces::Art];
                let here = ORDER.iter().position(|&p| p == screen.pieces).unwrap_or(0);
                screen.pieces = ORDER[cycle(here, ORDER.len(), forward)];
            }
            Row::Sound => {
                if self.hosted {
                    self.complaint = Some(
                        "Sound stays off over SSH: it would play on the server, not for you."
                            .to_string(),
                    );
                    return;
                }
                const ORDER: [sound::Mode; 3] =
                    [sound::Mode::Auto, sound::Mode::On, sound::Mode::Off];
                let here = ORDER
                    .iter()
                    .position(|&mode| mode == screen.sound.mode())
                    .unwrap_or(0);
                screen
                    .sound
                    .set_mode(ORDER[cycle(here, ORDER.len(), forward)]);
            }
        }
    }

    // -- drawing ------------------------------------------------------------

    /// Exactly `rows` lines, none wider than `cols`.
    pub(crate) fn render(
        &mut self,
        options: &Options,
        screen: &Screen,
        cols: usize,
        rows: usize,
    ) -> Vec<String> {
        const LABEL: usize = 8;
        const VALUE: usize = 16;
        let theme = &screen.theme;
        let (less, more) = if theme.ascii {
            ("<", ">")
        } else {
            ("\u{2039}", "\u{203a}")
        };

        let mut block: Vec<String> = vec![
            theme.strong(theme.palette.accent, "SETTINGS"),
            theme.rule(44),
            String::new(),
        ];
        let mut targets: Vec<(usize, usize, usize, Target)> = Vec::new();
        for (index, &row) in ROWS.iter().enumerate() {
            let focused = index == self.focus;
            let marker = match (focused, theme.ascii) {
                (false, _) => " ",
                (true, true) => ">",
                (true, false) => "\u{203a}",
            };
            let fixed = (row == Row::Sound && self.hosted) || (row == Row::Pieces && theme.ascii);
            let value = match (&self.typing, row) {
                (Some(typed), Row::Clock) => {
                    let caret = if theme.ascii { "_" } else { "\u{2581}" };
                    format!("{typed}{caret}")
                }
                _ => value_text(row, options, screen, self.hosted),
            };
            let value = ui::pad(&value, VALUE);
            let value = if focused && !fixed {
                theme.focused(theme.palette.accent, &format!(" {value} "))
            } else if fixed {
                theme.dim(&format!(" {value} "))
            } else {
                format!(" {value} ")
            };
            let arrow = |text: &str| {
                if fixed {
                    theme.dim(text)
                } else {
                    theme.strong(theme.palette.accent, text)
                }
            };
            let line = format!(
                "{} {}  {} {} {}",
                theme.strong(theme.palette.accent, marker),
                ui::pad(row.label(), LABEL),
                arrow(less),
                value,
                arrow(more)
            );
            let at = block.len();
            // marker(1) + space + label + two spaces, then the arrows either
            // side of the value.
            let arrow_left = 2 + LABEL + 2;
            let arrow_right = arrow_left + 2 + VALUE + 2 + 1;
            targets.push((at, 0, arrow_left, Target::Row(row)));
            targets.push((at, arrow_left, 2, Target::Less(row)));
            targets.push((at, arrow_left + 2, VALUE + 2, Target::Row(row)));
            targets.push((at, arrow_right, 2, Target::More(row)));
            block.push(line);
        }
        block.push(String::new());
        block.push(theme.rule(44));
        for line in self.detail(options, screen) {
            block.push(line);
        }
        if let Some(complaint) = &self.complaint {
            block.push(theme.warn(complaint));
        }
        block.push(String::new());
        let back = "[ Back ]";
        targets.push((block.len(), 0, ui::width(back), Target::Back));
        block.push(theme.accent(back));

        let widest = 46;
        let left = cols.saturating_sub(widest) / 2;
        let space = rows.saturating_sub(2);
        let top = 1 + space.saturating_sub(block.len()) / 2;
        let mut frame = vec![String::new(); rows];
        frame[0] = theme.bar("SETTINGS", "applies to the next game", cols);
        for (offset, line) in block.iter().enumerate() {
            let row = top + offset;
            if row + 1 < rows {
                frame[row] = ui::clip(&format!("{}{}", " ".repeat(left), line), cols);
            }
        }
        if rows > 2 {
            let hints = match (&self.typing, theme.ascii) {
                (Some(_), _) => " type minutes+seconds · Enter sets · Esc cancels",
                (None, true) => " up/down choose · left/right change · Esc back",
                (None, false) => " \u{2191}\u{2193} choose · \u{2190}\u{2192} change · Esc back",
            };
            frame[rows - 1] = theme.dim(&ui::clip(hints, cols));
        }
        self.hits = targets
            .into_iter()
            .filter(|(at, ..)| top + at + 1 < rows)
            .map(|(at, offset, width, target)| Hit {
                row: top + at,
                left: left + offset,
                width,
                target,
            })
            .collect();
        frame
    }

    /// What the focused row means, under the rows.
    fn detail(&self, options: &Options, screen: &Screen) -> Vec<String> {
        let theme = &screen.theme;
        match self.focused() {
            Row::Engine => match Level::of(&options.limits) {
                Some(level) => vec![level.about().to_string()],
                None => vec![
                    "Set by --time or --depth.".to_string(),
                    theme.dim("Choose a level to play at one instead."),
                ],
            },
            Row::Clock => vec![
                "Each player's time, then seconds added a move.".to_string(),
                theme.dim("Type your own, like 7+3. Also for private games."),
            ],
            Row::Board => vec!["The colours of the squares.".to_string()],
            Row::Pieces => vec![match screen.pieces {
                ui::Pieces::Auto => "Pictures where the terminal can show them.",
                ui::Pieces::Glyph => "Chess figures from your terminal's font.",
                ui::Pieces::Art => "Pieces drawn with block characters.",
            }
            .to_string()],
            Row::Sound if self.hosted => vec![theme.dim("Off: playing over SSH.")],
            Row::Sound => vec!["Sounds for moves, captures, and checks.".to_string()],
        }
    }
}

fn cycle(index: usize, len: usize, forward: bool) -> usize {
    if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    }
}

fn value_text(row: Row, options: &Options, screen: &Screen, hosted: bool) -> String {
    match row {
        Row::Engine => Level::of(&options.limits)
            .map_or("Custom".to_string(), |level| level.label().to_string()),
        Row::Clock => Clock::of(options).label(),
        Row::Board => ui::palette_name(screen.theme.palette).to_string(),
        Row::Pieces => screen.pieces.name().to_string(),
        Row::Sound if hosted => "off".to_string(),
        Row::Sound => screen.sound.mode().name().to_string(),
    }
}

/// Show the settings page until the player goes back, then save what they
/// chose. `input` is the home page's, already in raw mode.
pub(crate) fn run(
    input: &mut TerminalInput,
    options: &mut Options,
    screen: &mut Screen,
    config_path: &std::path::Path,
) -> Result<(), String> {
    let mut page = Settings::new(options.hosted);
    let mut shown: Vec<String> = Vec::new();
    loop {
        let (cols, rows) = ui::terminal_size().unwrap_or((80, 24));
        let frame = page.render(options, screen, cols.max(30), rows.max(12));
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        crate::app::home::paint(&frame, &[], 0, &mut shown, &mut out);
        out.push_str("\x1b[?2026l");
        print!("{out}");
        let _ = io::stdout().flush();

        let action = input.read_for(Duration::from_secs(1))?;
        let done = page.handle(action, input.buffer(), options, screen);
        if page.typing.is_none() {
            input.clear_buffer();
        }
        if done {
            break;
        }
    }
    // Online games set their screen up from the options.
    options.palette = screen.theme.palette;
    options.pieces = screen.pieces;
    if !options.hosted {
        options.sound = screen.sound.mode();
    }
    save(options, screen, config_path)
}

/// Write the settings into the preferences file, leaving everything else in
/// it as it was.
fn save(options: &Options, screen: &Screen, config_path: &std::path::Path) -> Result<(), String> {
    let mut preferences = storage::load_preferences().preferences;
    preferences.theme = ui::palette_name(screen.theme.palette).to_string();
    preferences.pieces = screen.pieces.name().to_string();
    if !options.hosted {
        preferences.sound = screen.sound.mode().name().to_string();
    }
    preferences.clock_enabled = options.clock.is_some();
    if let Some(initial) = options.clock {
        preferences.clock_minutes = initial.as_secs_f64() / 60.0;
    }
    preferences.increment_seconds = options.increment.as_secs_f64();
    if let Some(level) = Level::of(&options.limits) {
        preferences.engine_level = level.name().to_string();
    }
    storage::save_preferences(config_path, &preferences)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_round_trip_through_their_names_and_limits() {
        for level in Level::ALL {
            assert_eq!(Level::named(level.name()), Some(level));
            assert_eq!(Level::of(&level.limits()), Some(level));
        }
        assert_eq!(Level::named(" Club "), Some(Level::Club));
        let custom = Limits {
            depth: 8,
            movetime: None,
            randomness: 0,
        };
        assert_eq!(Level::of(&custom), None);
        // The strongest level is the engine as it always played.
        let strong = Level::Strong.limits();
        assert_eq!(strong.randomness, 0);
        assert_eq!(strong.movetime, Limits::default().movetime);
    }

    #[test]
    fn clocks_are_named_typed_and_stepped_in_order() {
        assert_eq!(Clock::minutes(10, 0).label(), "Rapid 10+0");
        assert_eq!(Clock::UNTIMED.label(), "Untimed");
        let typed = Clock::parse("7+3").unwrap();
        assert_eq!(typed.label(), "Custom 7+3");
        assert_eq!(Clock::parse("1.5").unwrap().text(), "1.5+0");
        assert_eq!(Clock::parse("untimed").unwrap(), Clock::UNTIMED);
        for bad in ["", "abc", "0", "-3+2", "5+-1", "2000", "5+9999"] {
            assert!(Clock::parse(bad).is_err(), "{bad:?} should be refused");
        }

        // A custom clock steps to the nearest preset on either side.
        assert_eq!(typed.step(false), Clock::minutes(5, 0));
        assert_eq!(typed.step(true), Clock::minutes(10, 0));
        // The ends stay put.
        assert_eq!(Clock::minutes(1, 0).step(false), Clock::minutes(1, 0));
        assert_eq!(Clock::UNTIMED.step(true), Clock::UNTIMED);
        assert_eq!(Clock::minutes(3, 2).step(true), Clock::minutes(5, 0));
    }
}
