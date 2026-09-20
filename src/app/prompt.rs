//! Line-oriented input and the start menu.

use std::io::{self, BufRead, Write};

use chess::ui;

use crate::app::cli::{Mode, StartChoice};
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

pub(crate) fn ask_mode(
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
