//! Short, embedded chess sounds played through the operating system.
//!
//! The assets are compiled into the executable so an installed `chess`
//! binary never has to find a companion data directory. Audio is best effort:
//! a missing output device makes the game silent, never unplayable.

use std::io::{self, Cursor, IsTerminal};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Source};

const MOVE: &[u8] = include_bytes!("../assets/sounds/kenney/move.ogg");
const CAPTURE: &[u8] = include_bytes!("../assets/sounds/kenney/capture.ogg");
const CASTLE: &[u8] = include_bytes!("../assets/sounds/kenney/castle.ogg");
const CHECK: &[u8] = include_bytes!("../assets/sounds/kenney/check.ogg");
const PROMOTION: &[u8] = include_bytes!("../assets/sounds/kenney/promotion.ogg");
const GAME_END: &[u8] = include_bytes!("../assets/sounds/kenney/game-end.ogg");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Play locally, stay quiet when an SSH session is detected.
    Auto,
    On,
    Off,
}

impl Mode {
    pub fn named(name: &str) -> Option<Mode> {
        match name.to_ascii_lowercase().as_str() {
            "auto" => Some(Mode::Auto),
            "on" | "yes" | "true" => Some(Mode::On),
            "off" | "no" | "false" | "mute" | "muted" => Some(Mode::Off),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Move,
    Capture,
    Castle,
    Check,
    Promotion,
    GameEnd,
}

impl Cue {
    #[cfg(test)]
    const ALL: [Cue; 6] = [
        Cue::Move,
        Cue::Capture,
        Cue::Castle,
        Cue::Check,
        Cue::Promotion,
        Cue::GameEnd,
    ];

    fn bytes(self) -> &'static [u8] {
        match self {
            Cue::Move => MOVE,
            Cue::Capture => CAPTURE,
            Cue::Castle => CASTLE,
            Cue::Check => CHECK,
            Cue::Promotion => PROMOTION,
            Cue::GameEnd => GAME_END,
        }
    }
}

pub struct Player {
    mode: Mode,
    output: Option<MixerDeviceSink>,
}

impl Player {
    pub fn new(mode: Mode) -> Player {
        let mut player = Player { mode, output: None };
        player.open_if_wanted();
        player
    }

    #[cfg(test)]
    pub fn off() -> Player {
        Player {
            mode: Mode::Off,
            output: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_playing(&self) -> bool {
        self.output.is_some()
    }

    /// Apply a runtime preference. Failure is deliberately reported as state
    /// rather than an error because chess remains fully usable without audio.
    pub fn set_mode(&mut self, mode: Mode) -> bool {
        self.mode = mode;
        if !self.wants_output() {
            self.output = None;
        } else if self.output.is_none() {
            self.open_if_wanted();
        }
        self.is_playing()
    }

    pub fn play(&self, cue: Cue) {
        let Some(output) = &self.output else {
            return;
        };
        let Ok(source) = Decoder::try_from(Cursor::new(cue.bytes())) else {
            return;
        };
        // The clips are intentionally restrained; this keeps repeated move
        // sounds tactile without competing with music or voice chat.
        output.mixer().add(source.amplify(0.62));
    }

    fn open_if_wanted(&mut self) {
        if self.wants_output() {
            self.output = DeviceSinkBuilder::open_default_sink().ok();
        }
    }

    fn wants_output(&self) -> bool {
        match self.mode {
            Mode::On => true,
            Mode::Off => false,
            Mode::Auto => io::stdout().is_terminal() && !remote_session(),
        }
    }
}

fn remote_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
        || std::env::var_os("SSH_TTY").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_sound_decodes() {
        for cue in Cue::ALL {
            Decoder::try_from(Cursor::new(cue.bytes()))
                .unwrap_or_else(|error| panic!("{cue:?} does not decode: {error}"));
        }
    }

    #[test]
    fn sound_mode_accepts_friendly_names() {
        assert_eq!(Mode::named("auto"), Some(Mode::Auto));
        assert_eq!(Mode::named("ON"), Some(Mode::On));
        assert_eq!(Mode::named("mute"), Some(Mode::Off));
        assert_eq!(Mode::named("sometimes"), None);
    }
}
