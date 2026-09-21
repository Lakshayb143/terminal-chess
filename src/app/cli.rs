//! Command-line options and the help text.

use std::path::PathBuf;
use std::time::Duration;

use chess::{sound, storage, ui};
use chess_core::search::{self, Limits};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The human has White, the engine answers as Black.
    HumanWhite,
    HumanBlack,
    /// Two people sharing the keyboard; the engine only gives hints.
    TwoPlayer,
}

impl Mode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Mode::HumanWhite => "white",
            Mode::HumanBlack => "black",
            Mode::TwoPlayer => "two",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Mode> {
        match name {
            "white" => Some(Mode::HumanWhite),
            "black" => Some(Mode::HumanBlack),
            "two" => Some(Mode::TwoPlayer),
            _ => None,
        }
    }
}

pub(crate) enum StartChoice {
    Mode(Mode),
    Resume,
    /// Play online, under the given name if not signed in.
    Online(OnlineIntent, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OnlineIntent {
    Create,
    Join(String),
    Resume,
}

pub(crate) struct Options {
    /// `None` until the opening menu asks which side to take.
    pub(crate) mode: Option<Mode>,
    /// `None` means the ordinary starting position.
    pub(crate) fen: Option<String>,
    pub(crate) limits: Limits,
    pub(crate) ascii: bool,
    /// `None` means "colour if this is a terminal".
    pub(crate) color: Option<bool>,
    /// `None` means "whatever the terminal appears to support".
    pub(crate) depth: Option<ui::Depth>,
    pub(crate) palette: ui::Palette,
    pub(crate) pieces: ui::Pieces,
    /// Keep the old small board rather than filling the window.
    pub(crate) compact: bool,
    pub(crate) sound: sound::Mode,
    pub(crate) player_names: [String; 2],
    pub(crate) flipped: bool,
    /// Each player's starting time. `None` is an untimed game.
    pub(crate) clock: Option<Duration>,
    pub(crate) increment: Duration,
    /// Restore the default autosave, or a specifically named saved game.
    pub(crate) resume: bool,
    pub(crate) load: Option<PathBuf>,
    pub(crate) online: Option<OnlineIntent>,
    pub(crate) server_url: String,
    pub(crate) online_name: String,
    /// Running on a server for someone else, as the SSH gateway does: the
    /// files are the server's, not the player's, so file commands are off.
    pub(crate) hosted: bool,
}

/// What file commands say when the game is hosted.
pub(crate) const HOSTED_FILES: &str =
    "Files are off when playing over SSH. Type `pgn` to show the game, then copy it from the screen.";

/// The longest a hosted engine thinks about one move. Every SSH visitor's
/// search runs on the server's cores, and one with no time limit would keep
/// a core busy for as long as the visitor liked.
pub(crate) const HOSTED_MOVETIME: Duration = Duration::from_secs(5);

/// Keep a hosted engine within [`HOSTED_MOVETIME`]. Returns whether that
/// changed what was asked for.
pub(crate) fn cap_hosted(limits: &mut Limits) -> bool {
    let budget = limits
        .movetime
        .map_or(HOSTED_MOVETIME, |budget| budget.min(HOSTED_MOVETIME));
    let changed = limits.movetime != Some(budget);
    limits.movetime = Some(budget);
    changed
}

/// Whether `word` with argument `rest` reads or writes a file of the
/// player's choosing, which a hosted game must not do.
pub(crate) fn names_a_file(word: &str, rest: &str) -> bool {
    match word {
        "export" | "import" => true,
        "save" | "load" => !rest.is_empty(),
        _ => false,
    }
}

pub(crate) const HELP: &str = "\
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
        --truecolor      use 24-bit colour even if the terminal does not say
                         it supports it (detected from COLORTERM and friends)
        --256            round every colour to the 256-colour palette
    -h, --help           print this help

IN THE GAME:
    Click a piece, then click a highlighted square to move it.
    Type a move as SAN (Nf3, exd5, O-O, e8=Q) or as coordinates (e2e4, e7e8q).
    Type `help` at the prompt for the list of commands.

    The board is drawn as big as the window allows, and grows when the window
    does. Give it room and the pieces are drawn rather than lettered.";

impl Options {
    /// `Ok(None)` means help was printed and there is nothing left to do.
    pub(crate) fn parse(
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
            depth: None,
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
            hosted: std::env::var("CHESS_HOSTED").is_ok_and(|value| value == "1"),
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
                "--truecolor" | "--truecolour" => options.depth = Some(ui::Depth::True),
                "--256" => options.depth = Some(ui::Depth::Indexed),
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
                    options.limits.movetime = Some(
                        Duration::try_from_secs_f64(secs)
                            .map_err(|_| format!("--time {} is too long", text))?,
                    );
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
        if options.hosted {
            cap_hosted(&mut options.limits);
        }
        if options.hosted && options.load.is_some() {
            return Err(HOSTED_FILES.to_string());
        }
        if options.online.is_some() && options.online_name.is_empty() {
            return Err("--name cannot be empty".to_string());
        }
        Ok(Some(options))
    }
}

#[cfg(test)]
mod tests {
    use super::names_a_file;

    #[test]
    fn hosted_games_refuse_only_commands_that_name_a_file() {
        assert!(names_a_file("export", ""));
        assert!(names_a_file("import", "game.pgn"));
        assert!(names_a_file("save", "/etc/passwd"));
        assert!(names_a_file("load", "../../chess.db"));
        // The default autosave lives in the visitor's own temporary folder.
        assert!(!names_a_file("save", ""));
        assert!(!names_a_file("load", ""));
        assert!(!names_a_file("pgn", ""));
    }
}
