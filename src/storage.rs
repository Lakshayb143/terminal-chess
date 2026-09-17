//! Durable local preferences, resumable sessions, and PGN file helpers.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CONFIG_VERSION: u32 = 1;
const SESSION_VERSION: u32 = 1;
const ONLINE_SESSION_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub version: u32,
    pub white_name: String,
    pub black_name: String,
    pub theme: String,
    pub pieces: String,
    pub compact: bool,
    pub sound: String,
    pub flipped: bool,
    pub clock_enabled: bool,
    pub clock_minutes: f64,
    pub increment_seconds: f64,
    pub onboarding_complete: bool,
}

impl Default for Preferences {
    fn default() -> Preferences {
        Preferences {
            version: CONFIG_VERSION,
            white_name: "Player 1".to_string(),
            black_name: "Player 2".to_string(),
            theme: "slate".to_string(),
            pieces: "auto".to_string(),
            compact: false,
            sound: "auto".to_string(),
            flipped: false,
            clock_enabled: true,
            clock_minutes: 10.0,
            increment_seconds: 0.0,
            onboarding_complete: false,
        }
    }
}

impl Preferences {
    fn normalize(mut self) -> Preferences {
        self.version = CONFIG_VERSION;
        self.white_name = clean_name(&self.white_name, "Player 1");
        self.black_name = clean_name(&self.black_name, "Player 2");
        if !matches!(self.theme.as_str(), "slate" | "wood" | "forest" | "mono") {
            self.theme = "slate".to_string();
        }
        if !matches!(self.pieces.as_str(), "auto" | "art" | "glyph") {
            self.pieces = "auto".to_string();
        }
        if !matches!(self.sound.as_str(), "auto" | "on" | "off") {
            self.sound = "auto".to_string();
        }
        if !self.clock_minutes.is_finite() || self.clock_minutes <= 0.0 {
            self.clock_minutes = 10.0;
        }
        if !self.increment_seconds.is_finite() || self.increment_seconds < 0.0 {
            self.increment_seconds = 0.0;
        }
        self
    }
}

pub struct LoadedPreferences {
    pub preferences: Preferences,
    pub path: PathBuf,
    pub first_run: bool,
    pub warning: Option<String>,
    pub backup_before_save: bool,
}

pub fn load_preferences() -> LoadedPreferences {
    let path = config_path();
    if !path.exists() {
        return LoadedPreferences {
            preferences: Preferences::default(),
            path,
            first_run: true,
            warning: None,
            backup_before_save: false,
        };
    }
    match fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Preferences>(&text) {
            Ok(preferences) => LoadedPreferences {
                preferences: preferences.normalize(),
                path,
                first_run: false,
                warning: None,
                backup_before_save: false,
            },
            Err(error) => LoadedPreferences {
                preferences: Preferences::default(),
                path,
                first_run: false,
                warning: Some(format!("could not read preferences: {error}")),
                backup_before_save: true,
            },
        },
        Err(error) => LoadedPreferences {
            preferences: Preferences::default(),
            path,
            first_run: false,
            warning: Some(format!("could not open preferences: {error}")),
            backup_before_save: false,
        },
    }
}

pub fn backup_invalid_preferences(path: &Path) -> Result<PathBuf, String> {
    let mut backup = path.with_extension("toml.invalid");
    let mut suffix = 1usize;
    while backup.exists() {
        backup = path.with_extension(format!("toml.invalid.{suffix}"));
        suffix += 1;
    }
    fs::copy(path, &backup).map_err(|error| {
        format!(
            "could not preserve invalid preferences as {}: {error}",
            backup.display()
        )
    })?;
    Ok(backup)
}

pub fn save_preferences(path: &Path, preferences: &Preferences) -> Result<(), String> {
    let text = toml::to_string_pretty(&preferences.clone().normalize())
        .map_err(|error| format!("could not encode preferences: {error}"))?;
    atomic_write(path, text.as_bytes())
}

pub fn default_session_path(config: &Path) -> PathBuf {
    config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("session.json")
}

pub fn default_online_session_path(config: &Path) -> PathBuf {
    config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("online-session.json")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedOnlineSeat {
    pub version: u32,
    pub server_url: String,
    pub game_id: String,
    pub reconnect_token: String,
    pub side: String,
}

impl SavedOnlineSeat {
    pub fn new(
        server_url: String,
        game_id: String,
        reconnect_token: String,
        side: String,
    ) -> SavedOnlineSeat {
        SavedOnlineSeat {
            version: ONLINE_SESSION_VERSION,
            server_url,
            game_id,
            reconnect_token,
            side,
        }
    }
}

pub fn save_online_seat(path: &Path, seat: &SavedOnlineSeat) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(seat)
        .map_err(|error| format!("could not encode online session: {error}"))?;
    atomic_write(path, &text)
}

pub fn load_online_seat(path: &Path) -> Result<SavedOnlineSeat, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let seat: SavedOnlineSeat = serde_json::from_str(&text)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if seat.version != ONLINE_SESSION_VERSION {
        return Err(format!(
            "{} uses unsupported online session version {}",
            path.display(),
            seat.version
        ));
    }
    Ok(seat)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedGame {
    pub version: u32,
    pub start_fen: String,
    pub moves: Vec<String>,
    pub mode: String,
    pub initial_clock_ms: Option<u64>,
    pub increment_ms: u64,
    pub remaining_ms: [u64; 2],
    pub paused: bool,
    pub resigned: Option<String>,
    pub draw_offer: Option<String>,
    pub agreed_draw: bool,
}

impl SavedGame {
    pub fn new() -> SavedGame {
        SavedGame {
            version: SESSION_VERSION,
            start_fen: String::new(),
            moves: Vec::new(),
            mode: "two".to_string(),
            initial_clock_ms: None,
            increment_ms: 0,
            remaining_ms: [0; 2],
            paused: false,
            resigned: None,
            draw_offer: None,
            agreed_draw: false,
        }
    }
}

pub fn save_game(path: &Path, game: &SavedGame) -> Result<(), String> {
    let text = serde_json::to_string_pretty(game)
        .map_err(|error| format!("could not encode saved game: {error}"))?;
    atomic_write(path, text.as_bytes())
}

pub fn load_game(path: &Path) -> Result<SavedGame, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let saved: SavedGame = serde_json::from_str(&text)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if saved.version != SESSION_VERSION {
        return Err(format!(
            "{} uses unsupported save version {}",
            path.display(),
            saved.version
        ));
    }
    Ok(saved)
}

pub fn write_text(path: &Path, text: &str) -> Result<(), String> {
    atomic_write(path, text.as_bytes())
}

pub fn read_pgn(path: &Path) -> Result<PgnMainline, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    parse_pgn(&text).map_err(|error| format!("{}: {error}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgnMainline {
    pub start_fen: Option<String>,
    pub moves: Vec<String>,
}

pub fn parse_pgn(text: &str) -> Result<PgnMainline, String> {
    let mut start_fen = None;
    let mut movetext = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if let Some(value) = tag_value(trimmed, "FEN") {
                start_fen = Some(value);
            }
        } else {
            movetext.push_str(line);
            movetext.push('\n');
        }
    }

    let clean = strip_pgn_noise(&movetext);
    let mut moves = Vec::new();
    for token in clean.split_whitespace() {
        let token = strip_move_number(token);
        let token = token.trim_end_matches(['!', '?']);
        if token.is_empty()
            || token.starts_with('$')
            || matches!(token, "1-0" | "0-1" | "1/2-1/2" | "*")
        {
            continue;
        }
        moves.push(token.to_string());
    }
    if moves.is_empty() {
        return Err("PGN contains no main-line moves".to_string());
    }
    Ok(PgnMainline { start_fen, moves })
}

fn tag_value(line: &str, wanted: &str) -> Option<String> {
    let body = line.strip_prefix('[')?.strip_suffix(']')?.trim();
    let (name, rest) = body.split_once(char::is_whitespace)?;
    if !name.eq_ignore_ascii_case(wanted) {
        return None;
    }
    let rest = rest.trim();
    Some(rest.strip_prefix('"')?.strip_suffix('"')?.to_string())
}

fn strip_pgn_noise(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut braces = 0usize;
    let mut variations = 0usize;
    let mut line_comment = false;
    for character in text.chars() {
        match character {
            '\n' if line_comment => {
                line_comment = false;
                clean.push(' ');
            }
            _ if line_comment => {}
            ';' if braces == 0 && variations == 0 => line_comment = true,
            '{' if variations == 0 => braces += 1,
            '}' if braces > 0 => braces -= 1,
            '(' if braces == 0 => variations += 1,
            ')' if braces == 0 && variations > 0 => variations -= 1,
            _ if braces == 0 && variations == 0 => clean.push(character),
            _ => {}
        }
    }
    clean
}

fn strip_move_number(token: &str) -> &str {
    let bytes = token.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 || index == bytes.len() || bytes[index] != b'.' {
        return token;
    }
    while index < bytes.len() && bytes[index] == b'.' {
        index += 1;
    }
    &token[index..]
}

fn config_path() -> PathBuf {
    if let Some(path) = env::var_os("TERMINAL_CHESS_CONFIG") {
        return PathBuf::from(path);
    }
    if let Some(base) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(base).join("terminal-chess/config.toml");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config/terminal-chess/config.toml");
    }
    PathBuf::from("terminal-chess.toml")
}

fn clean_name(name: &str, fallback: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(32)
        .collect();
    if name.is_empty() {
        fallback.to_string()
    } else {
        name
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("could not create a temporary save: {error}"))?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.flush())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| format!("could not replace {}: {}", path.display(), error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_as_readable_toml() {
        let preferences = Preferences::default();
        let text = toml::to_string_pretty(&preferences).unwrap();
        let restored: Preferences = toml::from_str(&text).unwrap();
        assert_eq!(restored, preferences);
        assert!(text.contains("theme = \"slate\""));
    }

    #[test]
    fn pgn_reader_keeps_only_the_main_line() {
        let pgn = r#"
[Event "Example"]
[FEN "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"]

1. e4 { central } e5 2.Nf3 (2. Bc4? Nc6) Nc6 $1 3. Bb5! *
"#;
        let parsed = parse_pgn(pgn).unwrap();
        assert_eq!(parsed.moves, ["e4", "e5", "Nf3", "Nc6", "Bb5"]);
        assert!(parsed.start_fen.is_some());
    }

    #[test]
    fn atomic_files_can_be_replaced_and_loaded() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let session = directory.path().join("session.json");
        let mut preferences = Preferences {
            clock_enabled: false,
            ..Preferences::default()
        };
        save_preferences(&config, &preferences).unwrap();
        preferences.theme = "forest".to_string();
        save_preferences(&config, &preferences).unwrap();
        let restored: Preferences = toml::from_str(&fs::read_to_string(config).unwrap()).unwrap();
        assert_eq!(restored.theme, "forest");
        assert!(!restored.clock_enabled);

        let mut game = SavedGame::new();
        game.start_fen = "start".to_string();
        save_game(&session, &game).unwrap();
        assert_eq!(load_game(&session).unwrap(), game);
    }

    #[test]
    fn invalid_preferences_are_backed_up_without_overwriting_prior_backup() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        fs::write(&config, "not = [valid").unwrap();

        let first = backup_invalid_preferences(&config).unwrap();
        let second = backup_invalid_preferences(&config).unwrap();

        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), "not = [valid");
        assert_eq!(fs::read_to_string(second).unwrap(), "not = [valid");
    }
}
