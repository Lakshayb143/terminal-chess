//! Signing in, creating an account, and the account page, all reached from
//! the start menu. Each request opens a short connection to the game server;
//! the session token it returns is kept so this computer stays signed in.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chess::client::OnlineClient;
use chess::storage::{self, SavedAccount};
use chess_protocol::{ClientCommand, ErrorCode, GameRecord, GameResult, ServerEvent};

use crate::app::prompt::{centered_prompt, draw_centered, read_line, read_secret};
use crate::app::screen::Screen;

/// Signing in hashes the password on the server, which takes a moment.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MIN_PASSWORD_CHARS: usize = 8;
const HISTORY_LENGTH: u32 = 20;

/// Who is using this computer, as far as the configured server is concerned.
pub(crate) struct AccountContext {
    path: PathBuf,
    pub(crate) server_url: String,
    saved: Option<SavedAccount>,
    /// Handed over by the SSH gateway: signing in links the visitor's key,
    /// so the next `ssh` signs them in without a password.
    link_ticket: Option<String>,
    /// Suggested to guests as the name their opponent sees.
    pub(crate) guest_name: String,
    /// Something to tell the person on the next menu they see.
    pub(crate) notice: Option<String>,
}

impl AccountContext {
    pub(crate) fn load(config_path: &Path, server_url: &str, guest_name: String) -> AccountContext {
        let path = storage::default_account_path(config_path);
        let handed_over = handed_over_session(server_url);
        let loaded = match handed_over {
            Some(account) => Ok(Some(account)),
            None => storage::load_account(&path),
        };
        let (saved, notice) = match loaded {
            // An account on another server does not sign in to this one.
            Ok(saved) => (
                saved.filter(|account| account.server_url == server_url),
                None,
            ),
            Err(error) => (None, Some(format!("Could not read your sign-in: {error}"))),
        };
        AccountContext {
            path,
            server_url: server_url.to_string(),
            saved,
            link_ticket: std::env::var("CHESS_SSH_LINK_TICKET")
                .ok()
                .filter(|ticket| !ticket.is_empty()),
            guest_name,
            notice,
        }
    }

    pub(crate) fn username(&self) -> Option<&str> {
        self.saved.as_ref().map(|account| account.username.as_str())
    }

    pub(crate) fn session_token(&self) -> Option<&str> {
        self.saved
            .as_ref()
            .map(|account| account.session_token.as_str())
    }

    fn remember(&mut self, username: String, session_token: String) -> Result<(), String> {
        let account = SavedAccount::new(self.server_url.clone(), username, session_token);
        storage::save_account(&self.path, &account)?;
        self.saved = Some(account);
        Ok(())
    }

    /// Stop signing in on this computer, for example after the server said the
    /// session has ended.
    pub(crate) fn forget(&mut self) {
        self.saved = None;
        if let Err(error) = storage::forget_account(&self.path) {
            self.notice = Some(error);
        }
    }
}

/// A session the SSH gateway started because the visitor's key is linked to
/// an account.
fn handed_over_session(server_url: &str) -> Option<SavedAccount> {
    let token = std::env::var("CHESS_SESSION_TOKEN").ok()?;
    let username = std::env::var("CHESS_SESSION_USERNAME").ok()?;
    (!token.is_empty() && !username.is_empty())
        .then(|| SavedAccount::new(server_url.to_string(), username, token))
}

/// The account page. Returns when the person goes back to the start menu.
pub(crate) fn account_menu(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    account: &mut AccountContext,
) -> Result<(), String> {
    let mut complaint = account
        .notice
        .take()
        .map(|notice| screen.theme.warn(&notice));
    loop {
        let signed_in = account.username().map(str::to_string);
        let theme = &screen.theme;
        let mut block = match &signed_in {
            Some(username) => vec![
                theme.strong(theme.palette.accent, &username.to_uppercase()),
                theme.rule(32),
                theme.dim("  signed in on this computer"),
                String::new(),
                format!("  {}   your recent games", theme.bold("1")),
                format!("  {}   sign out", theme.bold("2")),
            ],
            None => vec![
                theme.strong(theme.palette.accent, "YOUR ACCOUNT"),
                theme.rule(32),
                String::new(),
                "  An account keeps your name and".to_string(),
                "  your games, on every computer.".to_string(),
                String::new(),
                format!("  {}   sign in", theme.bold("1")),
                format!("  {}   create an account", theme.bold("2")),
            ],
        };
        block.push(String::new());
        block.push(theme.dim("  Press Enter on its own to go back."));
        if let Some(complaint) = complaint.take() {
            block.push(String::new());
            block.push(complaint);
        }
        let left = draw_centered(screen, &block);
        let Some(line) = read_line(stdin, &centered_prompt(screen, &left, ""))? else {
            return Ok(());
        };
        let choice = line.trim().to_ascii_lowercase();
        let outcome = match (signed_in.is_some(), choice.as_str()) {
            (_, "") | (_, "q" | "back") => return Ok(()),
            (false, "1" | "sign in" | "login") => sign_in(stdin, screen, account)?,
            (false, "2" | "create" | "signup") => create_account(stdin, screen, account)?,
            (true, "1" | "games" | "history") => recent_games(stdin, screen, account)?,
            (true, "2" | "sign out" | "logout") => sign_out(account),
            _ => Some(screen.theme.warn("  Choose 1 or 2.")),
        };
        complaint = outcome;
        if account.username().is_some() && signed_in.is_none() {
            // Just signed in: straight back to the menu to play.
            return Ok(());
        }
    }
}

/// Ask for a username and password and sign in with them. Returns a message
/// for the account page, or `None` when there is nothing to say.
fn sign_in(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    account: &mut AccountContext,
) -> Result<Option<String>, String> {
    let mut complaint: Option<String> = None;
    loop {
        let intro = vec![
            "  Welcome back.".to_string(),
            screen.theme.dim("  Press Enter on its own to go back."),
        ];
        let mut fields = Fields::new("SIGN IN", intro, complaint.take());
        let Some(username) = fields.ask(stdin, screen, "username")? else {
            return Ok(None);
        };
        let Some(password) = fields.ask_secret(stdin, screen, "password")? else {
            continue;
        };
        fields.status(screen, "Signing in…");
        let client = OnlineClient::connect(account.server_url.clone());
        let answer = client.request(
            ClientCommand::LogIn {
                username: username.clone(),
                password,
            },
            REQUEST_TIMEOUT,
        );
        match signed_in(answer, account) {
            Ok(username) => {
                let linked = link_ssh_key(&client, account);
                account.notice = Some(welcome(&username, linked));
                return Ok(None);
            }
            Err(message) => complaint = Some(screen.theme.warn(&format!("  {message}"))),
        }
    }
}

fn create_account(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    account: &mut AccountContext,
) -> Result<Option<String>, String> {
    let mut complaint: Option<String> = None;
    loop {
        let theme = &screen.theme;
        let intro = vec![
            "  Pick a username: 3 to 20 letters,".to_string(),
            "  digits, - or _. Your password needs".to_string(),
            "  at least 8 characters.".to_string(),
            String::new(),
            theme.dim("  Press Enter on its own to go back."),
        ];
        let mut fields = Fields::new("CREATE AN ACCOUNT", intro, complaint.take());
        let Some(username) = fields.ask(stdin, screen, "username")? else {
            return Ok(None);
        };
        let Some(password) = fields.ask_secret(stdin, screen, "password")? else {
            continue;
        };
        if password.chars().count() < MIN_PASSWORD_CHARS {
            complaint = Some(
                screen
                    .theme
                    .warn("  That password is too short: use at least 8 characters."),
            );
            continue;
        }
        let Some(again) = fields.ask_secret(stdin, screen, "   again")? else {
            continue;
        };
        if again != password {
            complaint = Some(
                screen
                    .theme
                    .warn("  The two passwords were different. Try again."),
            );
            continue;
        }
        fields.status(screen, "Creating your account…");
        let client = OnlineClient::connect(account.server_url.clone());
        let answer = client.request(
            ClientCommand::Register { username, password },
            REQUEST_TIMEOUT,
        );
        match signed_in(answer, account) {
            Ok(username) => {
                let linked = link_ssh_key(&client, account);
                account.notice = Some(welcome(&username, linked));
                return Ok(None);
            }
            Err(message) => complaint = Some(screen.theme.warn(&format!("  {message}"))),
        }
    }
}

/// Remember the account from a `SignedIn` answer, or say what went wrong.
fn signed_in(
    answer: Result<ServerEvent, String>,
    account: &mut AccountContext,
) -> Result<String, String> {
    match answer {
        Ok(ServerEvent::SignedIn {
            account: signed_in,
            session_token: Some(token),
        }) => {
            account.remember(signed_in.username.clone(), token)?;
            Ok(signed_in.username)
        }
        Ok(ServerEvent::Error { message, .. }) => Err(sentence(&message)),
        Ok(other) => Err(format!("The server answered unexpectedly ({other:?}).")),
        Err(error) => Err(sentence(&error)),
    }
}

/// Link the SSH key this session arrived with, if any. Returns whether it
/// was linked.
fn link_ssh_key(client: &OnlineClient, account: &mut AccountContext) -> bool {
    let Some(ticket) = account.link_ticket.take() else {
        return false;
    };
    matches!(
        client.request(ClientCommand::LinkSshKey { ticket }, REQUEST_TIMEOUT),
        Ok(ServerEvent::SshKeyLinked)
    )
}

fn welcome(username: &str, linked_ssh_key: bool) -> String {
    if linked_ssh_key {
        format!("Welcome, {username}! Next time, ssh signs you in by itself.")
    } else {
        format!("Welcome, {username}!")
    }
}

fn sign_out(account: &mut AccountContext) -> Option<String> {
    if let Some(token) = account.session_token() {
        let client = OnlineClient::connect(account.server_url.clone());
        let revoked = client
            .request(
                ClientCommand::Authenticate {
                    session_token: token.to_string(),
                },
                REQUEST_TIMEOUT,
            )
            .and_then(|_| client.request(ClientCommand::LogOut, REQUEST_TIMEOUT));
        // Forget the token here even when the server could not be reached; it
        // then expires on its own after 90 days unused.
        account.forget();
        if revoked.is_err() {
            account.notice = Some("Signed out on this computer.".to_string());
            return None;
        }
    }
    account.notice = Some("Signed out. You can still play as a guest.".to_string());
    None
}

fn recent_games(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    account: &mut AccountContext,
) -> Result<Option<String>, String> {
    let (Some(username), Some(token)) = (
        account.username().map(str::to_string),
        account.session_token().map(str::to_string),
    ) else {
        return Ok(None);
    };
    let block = vec![
        screen
            .theme
            .strong(screen.theme.palette.accent, "YOUR RECENT GAMES"),
        screen.theme.rule(32),
        String::new(),
        screen.theme.dim("  Fetching your games…"),
    ];
    draw_centered(screen, &block);
    let client = OnlineClient::connect(account.server_url.clone());
    let answer = client
        .request(
            ClientCommand::Authenticate {
                session_token: token,
            },
            REQUEST_TIMEOUT,
        )
        .and_then(|answer| match answer {
            ServerEvent::SignedIn { .. } => client.request(
                ClientCommand::ListGames {
                    limit: HISTORY_LENGTH,
                },
                REQUEST_TIMEOUT,
            ),
            other => Ok(other),
        });
    let games = match answer {
        Ok(ServerEvent::GameList { games }) => games,
        Ok(ServerEvent::Error {
            code: ErrorCode::InvalidSession,
            message,
        }) => {
            account.forget();
            account.notice = Some(sentence(&message));
            return Ok(None);
        }
        Ok(ServerEvent::Error { message, .. }) => {
            return Ok(Some(
                screen.theme.warn(&format!("  {}", sentence(&message))),
            ))
        }
        Ok(other) => {
            return Ok(Some(screen.theme.warn(&format!(
                "  The server answered unexpectedly ({other:?})."
            ))))
        }
        Err(error) => return Ok(Some(screen.theme.warn(&format!("  {}", sentence(&error))))),
    };

    let theme = &screen.theme;
    let mut block = vec![
        theme.strong(theme.palette.accent, "YOUR RECENT GAMES"),
        theme.rule(56),
        String::new(),
    ];
    if games.is_empty() {
        block.push("  No finished online games yet.".to_string());
        block.push(theme.dim("  Press o on the menu to play a friend."));
    }
    for game in &games {
        let line = history_line(game, &username);
        block.push(match line.outcome {
            Some(true) => format!("  {}", theme.good(&line.text)),
            Some(false) => format!("  {}", theme.warn(&line.text)),
            None => format!("  {}", line.text),
        });
    }
    block.push(String::new());
    block.push(theme.dim("  Press Enter to go back."));
    let left = draw_centered(screen, &block);
    read_line(stdin, &centered_prompt(screen, &left, ""))?;
    Ok(None)
}

struct HistoryLine {
    text: String,
    /// `Some(true)` for a win, `Some(false)` for a loss, `None` for a draw.
    outcome: Option<bool>,
}

fn history_line(game: &GameRecord, username: &str) -> HistoryLine {
    let played_white = game.white.registered && game.white.name.eq_ignore_ascii_case(username);
    let opponent = if played_white {
        &game.black
    } else {
        &game.white
    };
    let outcome = match game.result {
        GameResult::Draw => None,
        GameResult::WhiteWins => Some(played_white),
        GameResult::BlackWins => Some(!played_white),
    };
    let verdict = match outcome {
        Some(true) => "won ",
        Some(false) => "lost",
        None => "draw",
    };
    let opponent_name = if opponent.registered {
        opponent.name.clone()
    } else {
        format!("{} (guest)", opponent.name)
    };
    let reason = serde_json::to_value(game.reason)
        .ok()
        .and_then(|value| value.as_str().map(|text| text.replace('_', " ")))
        .unwrap_or_default();
    let moves = game.moves.len().div_ceil(2);
    let clock = if game.time_control.initial_ms == 0 {
        "untimed".to_string()
    } else {
        format!(
            "{}+{}",
            game.time_control.initial_ms / 60_000,
            game.time_control.increment_ms / 1_000
        )
    };
    HistoryLine {
        text: format!(
            "{}  {}  {} vs {:<24} {:<14} {:>3} moves  {}",
            short_date(game.ended_at_ms),
            verdict,
            if played_white { "W" } else { "B" },
            opponent_name,
            reason,
            moves,
            clock
        ),
        outcome,
    }
}

/// `18 Sep` for a Unix time in milliseconds, in UTC.
fn short_date(unix_ms: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Howard Hinnant's days-to-civil conversion.
    let days = (unix_ms / 86_400_000) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    format!("{:>2} {}", day, MONTHS[(month - 1) as usize])
}

/// Server and transport messages are lower-case phrases; the forms show
/// them as sentences.
fn sentence(message: &str) -> String {
    let mut characters = message.trim().chars();
    let mut text = match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
        None => return String::new(),
    };
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

/// A small form: a title, some guidance, and the answers given so far, redrawn
/// before each question so earlier answers stay on screen.
struct Fields {
    title: &'static str,
    intro: Vec<String>,
    complaint: Option<String>,
    answered: Vec<(String, String)>,
    left: String,
}

impl Fields {
    fn new(title: &'static str, intro: Vec<String>, complaint: Option<String>) -> Fields {
        Fields {
            title,
            intro,
            complaint,
            answered: Vec::new(),
            left: String::new(),
        }
    }

    fn draw(&mut self, screen: &mut Screen) {
        let theme = &screen.theme;
        let mut block = vec![
            theme.strong(theme.palette.accent, self.title),
            theme.rule(40),
            String::new(),
        ];
        block.extend(self.intro.iter().cloned());
        if let Some(complaint) = &self.complaint {
            block.push(String::new());
            block.push(complaint.clone());
        }
        block.push(String::new());
        for (label, value) in &self.answered {
            block.push(format!("  {} {} {}", label, theme.dim("·"), value));
        }
        self.left = draw_centered(screen, &block);
    }

    /// `Ok(None)` when the answer is empty, which means "go back".
    fn ask(
        &mut self,
        stdin: &mut io::StdinLock,
        screen: &mut Screen,
        label: &str,
    ) -> Result<Option<String>, String> {
        self.draw(screen);
        let answer = read_line(stdin, &centered_prompt(screen, &self.left, label))?
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty());
        if let Some(answer) = &answer {
            self.answered.push((label.to_string(), answer.clone()));
        }
        Ok(answer)
    }

    fn ask_secret(
        &mut self,
        stdin: &mut io::StdinLock,
        screen: &mut Screen,
        label: &str,
    ) -> Result<Option<String>, String> {
        self.draw(screen);
        let prompt = centered_prompt(screen, &self.left, label);
        let answer = read_secret(stdin, &prompt, screen.theme.ascii)?.filter(|s| !s.is_empty());
        if let Some(answer) = &answer {
            let dot = if screen.theme.ascii { "*" } else { "\u{2022}" };
            self.answered.push((
                label.to_string(),
                dot.repeat(answer.chars().count().min(16)),
            ));
        }
        Ok(answer)
    }

    /// Redraw the form with a line saying what is happening while the server
    /// is asked.
    fn status(&mut self, screen: &mut Screen, text: &str) {
        self.complaint = Some(screen.theme.dim(&format!("  {text}")));
        self.draw(screen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chess_protocol::{FinishReason, RecordedPlayer, TimeControl};

    #[test]
    fn dates_are_shown_as_day_and_month() {
        assert_eq!(short_date(0), " 1 Jan");
        // 2026-09-18T12:00:00Z
        assert_eq!(short_date(1_789_732_800_000), "18 Sep");
        // 2024-02-29, a leap day.
        assert_eq!(short_date(1_709_164_800_000), "29 Feb");
    }

    #[test]
    fn a_history_line_is_told_from_the_players_side() {
        let game = GameRecord {
            game_id: "g".to_string(),
            white: RecordedPlayer {
                name: "Grace".to_string(),
                registered: false,
            },
            black: RecordedPlayer {
                name: "Ada".to_string(),
                registered: true,
            },
            time_control: TimeControl {
                initial_ms: 600_000,
                increment_ms: 5_000,
            },
            moves: vec!["f2f3".into(), "e7e5".into(), "g2g4".into(), "d8h4".into()],
            result: GameResult::BlackWins,
            reason: FinishReason::Checkmate,
            started_at_ms: 0,
            ended_at_ms: 0,
        };
        let line = history_line(&game, "ada");
        assert_eq!(line.outcome, Some(true));
        assert!(line.text.contains("won"));
        assert!(line.text.contains("B vs Grace (guest)"));
        assert!(line.text.contains("checkmate"));
        assert!(line.text.contains("2 moves"));
        assert!(line.text.ends_with("10+5"));
    }

    #[test]
    fn messages_become_sentences() {
        assert_eq!(
            sentence("wrong username or password"),
            "Wrong username or password."
        );
        assert_eq!(sentence("Done!"), "Done!");
        assert_eq!(sentence(""), "");
    }
}
