//! Interactive terminal input with conservative, click-only mouse tracking.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Something the game loop can act on immediately.
pub enum Action {
    Submit(String),
    Click {
        column: u16,
        row: u16,
    },
    Cancel,
    /// Tab, or Shift+Tab when `reverse`.
    Focus {
        reverse: bool,
    },
    /// An arrow key pressed with nothing typed.
    Arrow(Direction),
    /// Home, or End when `end`, pressed with nothing typed.
    Edge {
        end: bool,
    },
    Prompt,
    Resize,
    Tick,
    /// Page Up, or Page Down when not `older`.
    History {
        older: bool,
    },
    /// The mouse wheel, up when `older`.
    Scroll {
        older: bool,
    },
    /// The terminal window gained or lost the focus. Only terminals that
    /// report focus send these.
    WindowFocus(bool),
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    /// Up and left go back through a list; down and right go on.
    pub fn backwards(self) -> bool {
        matches!(self, Direction::Up | Direction::Left)
    }
}

/// Owns raw mode and mouse reporting for as long as the live game runs.
///
/// Crossterm's standard mouse command also requests hover events. We only
/// enable normal button tracking plus SGR coordinates, which keeps remote SSH
/// sessions quiet until the player actually clicks.
pub struct TerminalInput {
    active: bool,
    buffer: String,
}

impl TerminalInput {
    pub fn enter(active: bool) -> Result<TerminalInput, String> {
        let mut input = TerminalInput {
            active: false,
            buffer: String::new(),
        };
        if active {
            input.resume()?;
        }
        Ok(input)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Forget what has been typed, for screens where a key is a shortcut
    /// rather than the start of a command.
    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
    }

    /// Temporarily return stdin to ordinary line input, for a confirmation
    /// whose wording is already handled by the existing prompt code.
    pub fn suspend(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        print!("\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1000l");
        io::stdout().flush().map_err(|e| e.to_string())?;
        disable_raw_mode().map_err(|e| format!("could not leave raw mode: {}", e))?;
        self.active = false;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), String> {
        if self.active {
            return Ok(());
        }
        enable_raw_mode().map_err(|e| format!("could not enter raw mode: {}", e))?;
        // 1000 reports button presses/releases; 1006 gives unbounded, precise
        // terminal-cell coordinates. Deliberately do not enable 1002/1003.
        // 1004 reports the window gaining and losing focus.
        print!("\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1004h");
        if let Err(error) = io::stdout().flush() {
            let _ = disable_raw_mode();
            return Err(error.to_string());
        }
        self.active = true;
        Ok(())
    }

    /// Wait for a complete command or an event the game cares about. The
    /// timeout lets clocks advance even while nobody is pressing a key.
    pub fn read_for(&mut self, timeout: Duration) -> Result<Action, String> {
        loop {
            if !event::poll(timeout).map_err(|e| format!("could not poll terminal input: {}", e))? {
                return Ok(Action::Tick);
            }
            let event =
                event::read().map_err(|e| format!("could not read terminal input: {}", e))?;
            match event {
                Event::Resize(_, _) => return Ok(Action::Resize),
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        return Ok(Action::Click {
                            column: mouse.column,
                            row: mouse.row,
                        });
                    }
                    MouseEventKind::ScrollUp => return Ok(Action::Scroll { older: true }),
                    MouseEventKind::ScrollDown => return Ok(Action::Scroll { older: false }),
                    _ => {}
                },
                Event::Paste(text) => {
                    for c in text.chars().take_while(|&c| c != '\r' && c != '\n') {
                        if !c.is_control() {
                            self.buffer.push(c);
                        }
                    }
                    return Ok(Action::Prompt);
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = self.key(key) {
                        return Ok(action);
                    }
                }
                Event::FocusGained => return Ok(Action::WindowFocus(true)),
                Event::FocusLost => return Ok(Action::WindowFocus(false)),
                _ => {}
            }
        }
    }

    fn key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Some(Action::Quit),
                KeyCode::Char('d') if self.buffer.is_empty() => Some(Action::Quit),
                KeyCode::Char('u') => {
                    self.buffer.clear();
                    Some(Action::Prompt)
                }
                KeyCode::Char('l') => Some(Action::Resize),
                _ => None,
            };
        }

        match key.code {
            KeyCode::Enter => Some(Action::Submit(std::mem::take(&mut self.buffer))),
            KeyCode::Esc => {
                self.buffer.clear();
                Some(Action::Cancel)
            }
            KeyCode::Backspace => {
                self.buffer.pop();
                Some(Action::Prompt)
            }
            KeyCode::PageUp if self.buffer.is_empty() => Some(Action::History { older: true }),
            KeyCode::PageDown if self.buffer.is_empty() => Some(Action::History { older: false }),
            KeyCode::BackTab if self.buffer.is_empty() => Some(Action::Focus { reverse: true }),
            KeyCode::Tab if self.buffer.is_empty() => Some(Action::Focus {
                reverse: key.modifiers.contains(KeyModifiers::SHIFT),
            }),
            KeyCode::Up if self.buffer.is_empty() => Some(Action::Arrow(Direction::Up)),
            KeyCode::Down if self.buffer.is_empty() => Some(Action::Arrow(Direction::Down)),
            KeyCode::Left if self.buffer.is_empty() => Some(Action::Arrow(Direction::Left)),
            KeyCode::Right if self.buffer.is_empty() => Some(Action::Arrow(Direction::Right)),
            KeyCode::Home if self.buffer.is_empty() => Some(Action::Edge { end: false }),
            KeyCode::End if self.buffer.is_empty() => Some(Action::Edge { end: true }),
            KeyCode::Char(c) if !c.is_control() => {
                self.buffer.push(c);
                Some(Action::Prompt)
            }
            _ => None,
        }
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        let _ = self.suspend();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> TerminalInput {
        TerminalInput {
            active: false,
            buffer: String::new(),
        }
    }

    #[test]
    fn tab_arrows_and_edges_navigate_when_the_prompt_is_empty() {
        let mut input = input();
        assert!(matches!(
            input.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some(Action::Focus { reverse: false })
        ));
        assert!(matches!(
            input.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Action::Focus { reverse: true })
        ));
        assert!(matches!(
            input.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(Action::Arrow(Direction::Left))
        ));
        assert!(matches!(
            input.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            Some(Action::Edge { end: true })
        ));

        // Once something is typed, the keys belong to the text.
        input.buffer.push('e');
        assert!(input
            .key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .is_none());
        assert!(input
            .key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .is_none());
    }
}
