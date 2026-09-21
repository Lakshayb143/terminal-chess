//! Line-oriented input, the start menu, and the forms drawn like it.

use std::io::{self, BufRead, IsTerminal, Write};

use chess::ui;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;

use crate::app::account::{account_menu, AccountContext};
use crate::app::cli::{Mode, OnlineIntent, StartChoice};
use crate::app::screen::Screen;

/// `Ok(None)` at end of input.
pub(crate) fn read_line(stdin: &mut io::StdinLock, prompt: &str) -> Result<Option<String>, String> {
    print!("{}", prompt);
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    match stdin.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(line)),
        Err(e) => Err(format!("could not read input: {}", e)),
    }
}

/// Read a line without showing it, drawing a dot for each character typed.
/// `Ok(None)` when the person presses Esc or Ctrl+C to go back. Input that is
/// not a terminal is read as an ordinary line.
pub(crate) fn read_secret(
    stdin: &mut io::StdinLock,
    prompt: &str,
    ascii: bool,
) -> Result<Option<String>, String> {
    if !io::stdin().is_terminal() {
        return Ok(
            read_line(stdin, prompt)?.map(|line| line.trim_end_matches(['\r', '\n']).to_string())
        );
    }
    print!("{}", prompt);
    io::stdout().flush().map_err(|e| e.to_string())?;
    let _raw = RawMode::enter()?;
    let dot = if ascii { "*" } else { "\u{2022}" };
    let mut secret = String::new();
    loop {
        let typed = match event::read().map_err(|e| format!("could not read input: {e}"))? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let control = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Enter => break,
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if control => return Ok(None),
                    KeyCode::Char('d') if control && secret.is_empty() => return Ok(None),
                    KeyCode::Char('u') if control => {
                        print!("{}", "\u{8} \u{8}".repeat(secret.chars().count()));
                        secret.clear();
                        String::new()
                    }
                    KeyCode::Backspace => {
                        if secret.pop().is_some() {
                            print!("\u{8} \u{8}");
                        }
                        String::new()
                    }
                    KeyCode::Char(c) if !control => c.to_string(),
                    _ => String::new(),
                }
            }
            Event::Paste(text) => text.chars().filter(|c| !c.is_control()).collect(),
            _ => String::new(),
        };
        for c in typed.chars() {
            secret.push(c);
            print!("{}", dot);
        }
        io::stdout().flush().map_err(|e| e.to_string())?;
    }
    Ok(Some(secret))
}

/// Raw mode for as long as it is held, so a secret is never echoed.
struct RawMode;

impl RawMode {
    fn enter() -> Result<RawMode, String> {
        terminal::enable_raw_mode().map_err(|e| format!("could not hide input: {e}"))?;
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        // Raw mode swallowed the Enter that ended the line.
        print!("\r\n");
        let _ = io::stdout().flush();
    }
}

/// Clear the window and draw `block` in the middle of it. Returns the left
/// margin, so a prompt printed underneath lines up with the block.
pub(crate) fn draw_centered(screen: &mut Screen, block: &[String]) -> String {
    screen.measure();
    let theme = &screen.theme;
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
    for line in block {
        if line.is_empty() {
            println!();
        } else {
            println!("{}{}", left, line);
        }
    }
    println!();
    left
}

/// The prompt printed under a centred block: the margin, then `label` and an
/// arrow.
pub(crate) fn centered_prompt(screen: &Screen, left: &str, label: &str) -> String {
    let arrow = if screen.theme.ascii { ">" } else { "\u{203a}" };
    if label.is_empty() {
        format!("{}  {} ", left, screen.theme.dim(arrow))
    } else {
        format!("{}  {} {} ", left, label, screen.theme.dim(arrow))
    }
}

pub(crate) fn ask_mode(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    can_resume: bool,
    account: &mut AccountContext,
) -> Result<Option<StartChoice>, String> {
    let mut complaint = String::new();
    loop {
        if let Some(notice) = account.notice.take() {
            complaint = screen.theme.accent(&format!("  {notice}"));
        }
        let theme = &screen.theme;
        let who = match account.username() {
            Some(name) => format!("signed in as {}", theme.bold(name)),
            None => "playing as a guest".to_string(),
        };
        let mut block = vec![
            theme.strong(theme.palette.accent, "C H E S S"),
            theme.rule(32),
            theme.dim(&format!("  {who}")),
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
        block.push(format!("  {}   play a random opponent", theme.bold("p")));
        block.push(format!("  {}   play a friend online", theme.bold("o")));
        block.push(format!("  {}   join a friend's game", theme.bold("j")));
        block.push(if account.username().is_some() {
            format!("  {}   your account and games", theme.bold("a"))
        } else {
            format!("  {}   sign in or create an account", theme.bold("a"))
        });
        block.push(String::new());
        block.push(theme.dim("  q   leave"));
        if !complaint.is_empty() {
            block.push(String::new());
            block.push(complaint.clone());
        }
        let left = draw_centered(screen, &block);
        let line = match read_line(stdin, &centered_prompt(screen, &left, ""))? {
            Some(line) => line,
            None => {
                println!();
                return Ok(None);
            }
        };
        complaint.clear();
        match line.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "w" | "white" => return Ok(Some(StartChoice::Mode(Mode::HumanWhite))),
            "2" | "b" | "black" => return Ok(Some(StartChoice::Mode(Mode::HumanBlack))),
            "3" | "t" | "two" => return Ok(Some(StartChoice::Mode(Mode::TwoPlayer))),
            "4" | "r" | "resume" if can_resume => return Ok(Some(StartChoice::Resume)),
            "p" | "random" | "find" => {
                if let Some(name) = ask_guest_name(stdin, screen, account)? {
                    return Ok(Some(StartChoice::Online(OnlineIntent::Find, name)));
                }
            }
            "o" | "online" | "create" => {
                if let Some(name) = ask_guest_name(stdin, screen, account)? {
                    return Ok(Some(StartChoice::Online(OnlineIntent::Create, name)));
                }
            }
            "j" | "join" => {
                if let Some(code) = ask_invite_code(stdin, screen)? {
                    if let Some(name) = ask_guest_name(stdin, screen, account)? {
                        return Ok(Some(StartChoice::Online(OnlineIntent::Join(code), name)));
                    }
                }
            }
            "a" | "account" | "login" | "signup" => account_menu(stdin, screen, account)?,
            "q" | "quit" | "exit" => return Ok(None),
            _ => {
                complaint = screen.theme.warn(if can_resume {
                    "  Choose 1, 2, 3, 4, p, o, j or a."
                } else {
                    "  Choose 1, 2, 3, p, o, j or a."
                })
            }
        }
    }
}

/// The six-character code of a friend's game, or `None` to go back.
fn ask_invite_code(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
) -> Result<Option<String>, String> {
    let mut complaint = String::new();
    loop {
        let theme = &screen.theme;
        let mut block = vec![
            theme.strong(theme.palette.accent, "JOIN A FRIEND'S GAME"),
            theme.rule(32),
            String::new(),
            "  Type the six-character code your".to_string(),
            "  friend sees beside their board.".to_string(),
            String::new(),
            theme.dim("  Press Enter on its own to go back."),
        ];
        if !complaint.is_empty() {
            block.push(String::new());
            block.push(complaint.clone());
        }
        let left = draw_centered(screen, &block);
        let Some(line) = read_line(stdin, &centered_prompt(screen, &left, "code"))? else {
            return Ok(None);
        };
        let code = line
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        if code.is_empty() {
            return Ok(None);
        }
        if code.len() == 6 && code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(Some(code));
        }
        complaint = screen
            .theme
            .warn("  A code is six letters and digits, like 7F3K9Q.");
    }
}

/// The name a guest shows their opponent, or `None` to go back. Signed-in
/// players always play under their username, so they are not asked.
pub(crate) fn ask_guest_name(
    stdin: &mut io::StdinLock,
    screen: &mut Screen,
    account: &AccountContext,
) -> Result<Option<String>, String> {
    if let Some(username) = account.username() {
        return Ok(Some(username.to_string()));
    }
    let suggested = account.guest_name.clone();
    let theme = &screen.theme;
    let block = vec![
        theme.strong(theme.palette.accent, "PLAY AS A GUEST"),
        theme.rule(32),
        String::new(),
        "  What name should your opponent see?".to_string(),
        theme.dim(&format!("  Press Enter to use {suggested}.")),
        String::new(),
        theme.dim("  Sign in from the menu (a) to keep"),
        theme.dim("  your games in your history."),
    ];
    let left = draw_centered(screen, &block);
    let Some(line) = read_line(stdin, &centered_prompt(screen, &left, "name"))? else {
        return Ok(None);
    };
    let name: String = line
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    Ok(Some(if name.is_empty() { suggested } else { name }))
}
