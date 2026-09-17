//! Short, embedded chess sounds played through the operating system.
//!
//! The assets are compiled into the executable so an installed `chess`
//! binary never has to find a companion data directory. Audio is best effort:
//! a missing output device makes the game silent, never unplayable.

use std::io::{self, Cursor, IsTerminal};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player as AudioPlayer};

#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::mpsc::{self, SyncSender};
#[cfg(target_os = "macos")]
use std::thread::{self, JoinHandle};

const MOVE: &[u8] = include_bytes!("../assets/sounds/kenney/move.ogg");
const CAPTURE: &[u8] = include_bytes!("../assets/sounds/kenney/capture.ogg");
const CASTLE: &[u8] = include_bytes!("../assets/sounds/kenney/castle.ogg");
const CHECK: &[u8] = include_bytes!("../assets/sounds/kenney/check.ogg");
const PROMOTION: &[u8] = include_bytes!("../assets/sounds/kenney/promotion.ogg");
const GAME_END: &[u8] = include_bytes!("../assets/sounds/kenney/game-end.ogg");

// `afplay`, the native macOS audio player, does not support Ogg Vorbis. Keep
// small PCM copies for that backend; other platforms only embed the Ogg files.
#[cfg(target_os = "macos")]
const MOVE_WAV: &[u8] = include_bytes!("../assets/sounds/kenney/move.wav");
#[cfg(target_os = "macos")]
const CAPTURE_WAV: &[u8] = include_bytes!("../assets/sounds/kenney/capture.wav");
#[cfg(target_os = "macos")]
const CASTLE_WAV: &[u8] = include_bytes!("../assets/sounds/kenney/castle.wav");
#[cfg(target_os = "macos")]
const CHECK_WAV: &[u8] = include_bytes!("../assets/sounds/kenney/check.wav");
#[cfg(target_os = "macos")]
const PROMOTION_WAV: &[u8] = include_bytes!("../assets/sounds/kenney/promotion.wav");
#[cfg(target_os = "macos")]
const GAME_END_WAV: &[u8] = include_bytes!("../assets/sounds/kenney/game-end.wav");

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

    pub fn name(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::On => "on",
            Mode::Off => "off",
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
    #[cfg(any(test, target_os = "macos"))]
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

    #[cfg(target_os = "macos")]
    fn filename(self) -> &'static str {
        match self {
            Cue::Move => "move.wav",
            Cue::Capture => "capture.wav",
            Cue::Castle => "castle.wav",
            Cue::Check => "check.wav",
            Cue::Promotion => "promotion.wav",
            Cue::GameEnd => "game-end.wav",
        }
    }

    #[cfg(target_os = "macos")]
    fn native_bytes(self) -> &'static [u8] {
        match self {
            Cue::Move => MOVE_WAV,
            Cue::Capture => CAPTURE_WAV,
            Cue::Castle => CASTLE_WAV,
            Cue::Check => CHECK_WAV,
            Cue::Promotion => PROMOTION_WAV,
            Cue::GameEnd => GAME_END_WAV,
        }
    }
}

pub struct Player {
    mode: Mode,
    output: Option<Output>,
    last_error: Option<String>,
}

enum Output {
    #[cfg(target_os = "macos")]
    Mac(MacOutput),
    Rodio(RodioOutput),
}

impl Output {
    fn play(&self, cue: Cue) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Output::Mac(output) => output.play(cue),
            Output::Rodio(output) => output.play(cue),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            Output::Mac(_) => "macOS system audio",
            Output::Rodio(_) => "Rodio",
        }
    }
}

/// Cross-platform output used directly on Linux and Windows, and retained as
/// a fallback on macOS if its built-in player cannot be prepared.
struct RodioOutput {
    _device: MixerDeviceSink,
    player: AudioPlayer,
}

impl RodioOutput {
    fn open() -> Result<RodioOutput, String> {
        let mut device = DeviceSinkBuilder::open_default_sink().map_err(|error| error.to_string())?;
        // Dropping the stream is an ordinary result of `sound off` or quitting;
        // Rodio's diagnostic would otherwise be printed through the TUI.
        device.log_on_drop(false);
        let player = AudioPlayer::connect_new(device.mixer());
        player.set_volume(0.82);
        Ok(RodioOutput {
            _device: device,
            player,
        })
    }

    fn play(&self, cue: Cue) -> bool {
        let Ok(source) = Decoder::try_from(Cursor::new(cue.bytes())) else {
            return false;
        };
        self.player.append(source);
        true
    }
}

/// macOS ships a reliable CoreAudio command-line player. Feeding it PCM files
/// avoids the silent-but-open stream seen with some Rodio/CoreAudio setups.
#[cfg(target_os = "macos")]
struct MacOutput {
    sender: Option<SyncSender<Cue>>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl MacOutput {
    fn open() -> Result<MacOutput, String> {
        let executable = Path::new("/usr/bin/afplay");
        if !executable.is_file() {
            return Err("/usr/bin/afplay was not found".to_string());
        }

        let directory = tempfile::Builder::new()
            .prefix("terminal-chess-audio-")
            .tempdir()
            .map_err(|error| format!("could not create the audio cache: {error}"))?;
        for cue in Cue::ALL {
            fs::write(directory.path().join(cue.filename()), cue.native_bytes())
                .map_err(|error| format!("could not prepare {}: {error}", cue.filename()))?;
        }

        // One queued effect is enough for normal chess play and keeps rapid UI
        // actions from creating an unbounded procession of `afplay` processes.
        let (sender, receiver) = mpsc::sync_channel::<Cue>(1);
        let worker = thread::Builder::new()
            .name("chess-audio".to_string())
            .spawn(move || {
                // The temporary files live exactly as long as this worker.
                let directory = directory;
                while let Ok(cue) = receiver.recv() {
                    let _ = Command::new("/usr/bin/afplay")
                        .arg("-v")
                        .arg("0.82")
                        .arg(directory.path().join(cue.filename()))
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            })
            .map_err(|error| format!("could not start the audio worker: {error}"))?;

        Ok(MacOutput {
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    fn play(&self, cue: Cue) -> bool {
        self.sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(cue).is_ok())
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacOutput {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Player {
    pub fn new(mode: Mode) -> Player {
        let mut player = Player {
            mode,
            output: None,
            last_error: None,
        };
        player.open_if_wanted();
        player
    }

    #[cfg(test)]
    pub fn off() -> Player {
        Player {
            mode: Mode::Off,
            output: None,
            last_error: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_playing(&self) -> bool {
        self.output.is_some()
    }

    pub fn backend(&self) -> Option<&'static str> {
        self.output.as_ref().map(Output::name)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Apply a runtime preference. Failure is deliberately reported as state
    /// rather than an error because chess remains fully usable without audio.
    pub fn set_mode(&mut self, mode: Mode) -> bool {
        self.mode = mode;
        if !self.wants_output() {
            self.output = None;
            self.last_error = None;
        } else if self.output.is_none() {
            self.open_if_wanted();
        }
        self.is_playing()
    }

    pub fn play(&self, cue: Cue) -> bool {
        let Some(output) = &self.output else {
            return false;
        };
        output.play(cue)
    }

    fn open_if_wanted(&mut self) {
        if self.wants_output() {
            match open_output() {
                Ok(output) => {
                    self.output = Some(output);
                    self.last_error = None;
                }
                Err(error) => {
                    self.output = None;
                    self.last_error = Some(error);
                }
            }
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

fn open_output() -> Result<Output, String> {
    #[cfg(target_os = "macos")]
    if let Ok(output) = MacOutput::open() {
        return Ok(Output::Mac(output));
    }

    RodioOutput::open().map(Output::Rodio)
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
