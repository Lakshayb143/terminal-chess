//! Terminal presentation: colour, the board, and the frame drawn around it.
//!
//! The board is drawn to whatever size the terminal is: squares grow from one
//! row tall to eight, and the pieces grow with them. Everything in here also
//! degrades. With `--no-colour`, in a terminal that says it is dumb, or when
//! the output is a pipe, the same calls produce plain text that lines up just
//! as well without a single escape code in it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, OnceLock};

use chess_core::board::{self, Color, Move, Piece, Position, Square};

// ---------------------------------------------------------------------------
// Palettes
// ---------------------------------------------------------------------------

/// A 24-bit colour. Terminals that only understand the 256-colour palette get
/// the nearest entry in it instead; see [`Depth`].
pub type Rgb = [u8; 3];

const fn hex(value: u32) -> Rgb {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

/// Mix `amount` percent of `tint` into `base`.
fn mix(base: Rgb, tint: Rgb, amount: u16) -> Rgb {
    let channel = |b: u8, t: u8| ((b as u16 * (100 - amount) + t as u16 * amount + 50) / 100) as u8;
    [
        channel(base[0], tint[0]),
        channel(base[1], tint[1]),
        channel(base[2], tint[2]),
    ]
}

/// Squares carry two colours; every highlight is a tint laid over them, so a
/// marked light square still reads as light and a marked dark one as dark.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    light: Rgb,
    dark: Rgb,
    /// The two squares of the move just played.
    last: Rgb,
    /// The square of a king in check.
    check: Rgb,
    /// Squares a piece has been asked about, or a hinted move.
    target: Rgb,
    /// Legal destinations that take an opposing piece.
    capture: Rgb,
    /// A square the player clicked but cannot legally use.
    invalid: Rgb,
    /// The piece currently chosen with the mouse.
    selected: Rgb,
    white_piece: Rgb,
    black_piece: Rgb,
    /// The bar across the top of the screen.
    bar: Rgb,
    pub label: Rgb,
    pub accent: Rgb,
    pub warn: Rgb,
    pub good: Rgb,
    /// Hand-picked 256-colour squares. Rounding a blended tint to the nearest
    /// palette entry often lands back on the plain square, or on grey, so a
    /// terminal without 24-bit colour gets these instead of the blends.
    indexed: Indexed,
}

/// Plain, last move, check, target, capture, invalid, selected: light first.
type Indexed = [[u8; 2]; 7];

/// Why a square is drawn in something other than its plain colour, strongest
/// reason first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mark {
    Check,
    Invalid,
    Selected,
    Capture,
    Target,
    Last,
}

impl Palette {
    /// The background of one square. The strengths are shared by every theme
    /// so that a highlight means the same thing whichever one is chosen: the
    /// last move is a quiet wash, check is unmissable.
    fn square(&self, light: bool, mark: Option<Mark>, depth: Depth) -> Rgb {
        if depth == Depth::Indexed {
            let row = match mark {
                None => 0,
                Some(Mark::Last) => 1,
                Some(Mark::Check) => 2,
                Some(Mark::Target) => 3,
                Some(Mark::Capture) => 4,
                Some(Mark::Invalid) => 5,
                Some(Mark::Selected) => 6,
            };
            return ansi256_rgb(self.indexed[row][usize::from(!light)]);
        }
        let base = if light { self.light } else { self.dark };
        let (tint, amount) = match mark {
            None => return base,
            Some(Mark::Check) => (self.check, 68),
            Some(Mark::Invalid) => (self.invalid, 55),
            Some(Mark::Selected) => (self.selected, 55),
            Some(Mark::Capture) => (self.capture, 50),
            Some(Mark::Target) => (self.target, 40),
            Some(Mark::Last) => (self.last, 42),
        };
        mix(base, tint, amount)
    }

    /// Move dots and capture frames are a deeper shade of the square under
    /// them, which keeps them legible on every tint without shouting.
    fn marker(&self, square: Rgb) -> Rgb {
        mix(square, hex(0x101418), 45)
    }
}

/// The keyboard cursor's frame. Squares are mid-toned so that both near-white
/// and near-black pieces read on them, which makes near-black readable too.
const CURSOR: Rgb = hex(0x101418);

/// Squares are deliberately mid-toned: white pieces are drawn in white and
/// black pieces in near-black, and both have to stay legible on either colour.
pub const THEMES: [(&str, Palette); 4] = [
    (
        "slate",
        Palette {
            light: hex(0x8eaeb4),
            dark: hex(0x5c7f88),
            last: hex(0xf2c95c),
            check: hex(0xe5484d),
            target: hex(0xa6e3a1),
            capture: hex(0xf29e5a),
            invalid: hex(0xe5484d),
            selected: hex(0x9ccfff),
            white_piece: hex(0xf4f4f4),
            black_piece: hex(0x151515),
            bar: hex(0x4b5872),
            label: hex(0x8a9099),
            accent: hex(0x86b3d9),
            warn: hex(0xdc8c8c),
            good: hex(0x8fbf8f),
            indexed: [
                [109, 66],
                [180, 137],
                [174, 131],
                [151, 108],
                [186, 143],
                [174, 131],
                [153, 110],
            ],
        },
    ),
    (
        "wood",
        Palette {
            light: hex(0xd6b088),
            dark: hex(0xa57b53),
            last: hex(0xf5dc6e),
            check: hex(0xe5484d),
            target: hex(0xa6e3a1),
            capture: hex(0x8fc6ff),
            invalid: hex(0xe5484d),
            selected: hex(0x8fc6ff),
            white_piece: hex(0xf4f4f4),
            black_piece: hex(0x151515),
            bar: hex(0x6e5040),
            label: hex(0x928b84),
            accent: hex(0xdcab5c),
            warn: hex(0xdc8c8c),
            good: hex(0x8fbf8f),
            indexed: [
                [180, 137],
                [186, 143],
                [174, 131],
                [151, 108],
                [153, 110],
                [174, 131],
                [153, 110],
            ],
        },
    ),
    (
        "forest",
        Palette {
            light: hex(0x8fb38a),
            dark: hex(0x58805a),
            last: hex(0xecd46a),
            check: hex(0xe5484d),
            target: hex(0xb0dcef),
            capture: hex(0xf29e5a),
            invalid: hex(0xe5484d),
            selected: hex(0xfff09a),
            white_piece: hex(0xf4f4f4),
            black_piece: hex(0x151515),
            bar: hex(0x3e5446),
            label: hex(0x8a948b),
            accent: hex(0x8fbf8a),
            warn: hex(0xdc8c8c),
            good: hex(0x8fd47f),
            indexed: [
                [108, 65],
                [186, 143],
                [174, 131],
                [152, 109],
                [186, 143],
                [174, 131],
                [186, 143],
            ],
        },
    ),
    (
        "mono",
        Palette {
            light: hex(0xa8a8ab),
            dark: hex(0x707074),
            last: hex(0xe8e0b8),
            check: hex(0xe08c8c),
            target: hex(0xbfe0e8),
            capture: hex(0xe8f4ff),
            invalid: hex(0xe08c8c),
            selected: hex(0xf0c8c8),
            white_piece: hex(0xf4f4f4),
            black_piece: hex(0x151515),
            bar: hex(0x3a3a3c),
            label: hex(0x8a8a8a),
            accent: hex(0xd4d4d4),
            warn: hex(0xdcb0b0),
            good: hex(0xd4d4d4),
            indexed: [
                [145, 102],
                [187, 144],
                [181, 138],
                [152, 109],
                [195, 152],
                [181, 138],
                [181, 181],
            ],
        },
    ),
];

pub fn palette(name: &str) -> Option<Palette> {
    THEMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, p)| *p)
}

pub fn theme_names() -> String {
    THEMES
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn palette_name(palette: Palette) -> &'static str {
    THEMES
        .iter()
        .find(|(_, candidate)| *candidate == palette)
        .map(|(name, _)| *name)
        .unwrap_or("slate")
}

// ---------------------------------------------------------------------------
// Colour depth
// ---------------------------------------------------------------------------

/// How many colours the terminal can show. Palettes are written in 24-bit
/// colour; a 256-colour terminal gets each one rounded to its nearest entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    Indexed,
    True,
}

impl Depth {
    /// `COLORTERM` is the convention, but it rarely survives `ssh` or `sudo`,
    /// so the terminals known to handle 24-bit colour are recognised by the
    /// other markers they leave behind. Anything else gets the 256 colours
    /// that every terminal worth colouring for has had for twenty years.
    pub fn detect() -> Depth {
        let get = |key: &str| std::env::var(key).unwrap_or_default().to_ascii_lowercase();
        let set = |key: &str| std::env::var_os(key).is_some_and(|value| !value.is_empty());

        let colorterm = get("COLORTERM");
        if colorterm == "truecolor" || colorterm == "24bit" {
            return Depth::True;
        }
        let term = get("TERM");
        let program = get("TERM_PROGRAM");
        let known_term = term.ends_with("-direct")
            || [
                "kitty",
                "alacritty",
                "ghostty",
                "wezterm",
                "foot",
                "contour",
            ]
            .iter()
            .any(|name| term.contains(name));
        let known_program = matches!(
            program.as_str(),
            "iterm.app" | "wezterm" | "vscode" | "ghostty" | "hyper" | "tabby" | "rio"
        );
        let vte = get("VTE_VERSION")
            .parse::<u32>()
            .is_ok_and(|version| version >= 3600);
        if known_term
            || known_program
            || vte
            || get("LC_TERMINAL") == "iterm2"
            || set("KITTY_WINDOW_ID")
            || set("WT_SESSION")
            || set("KONSOLE_VERSION")
        {
            Depth::True
        } else {
            Depth::Indexed
        }
    }

    pub fn named(name: &str) -> Option<Depth> {
        match name.to_ascii_lowercase().as_str() {
            "24bit" | "truecolor" | "truecolour" | "true" => Some(Depth::True),
            "256" | "indexed" => Some(Depth::Indexed),
            _ => None,
        }
    }

    /// The SGR parameters for `color` as a foreground (`38`) or background
    /// (`48`) colour.
    fn code(self, layer: u8, color: Rgb) -> String {
        match self {
            Depth::True => format!("{};2;{};{};{}", layer, color[0], color[1], color[2]),
            Depth::Indexed => format!("{};5;{}", layer, ansi256_index(color)),
        }
    }

    pub fn fg(self, color: Rgb) -> String {
        self.code(38, color)
    }

    pub fn bg(self, color: Rgb) -> String {
        self.code(48, color)
    }
}

/// The 256-colour entry closest to `color`: the nearer of the best candidate
/// in the 6x6x6 cube and the best in the grey ramp. The first sixteen are
/// left alone because every terminal theme redefines them.
fn ansi256_index(color: Rgb) -> u8 {
    let level = |value: u8| match value {
        0..=47 => 0,
        48..=114 => 1,
        _ => ((value as u16 - 35) / 40) as u8,
    };
    let cube = 16 + 36 * level(color[0]) + 6 * level(color[1]) + level(color[2]);
    let average = (color[0] as u16 + color[1] as u16 + color[2] as u16) / 3;
    let grey = 232 + ((average.saturating_sub(3)) / 10).min(23) as u8;
    let distance = |index: u8| {
        let candidate = ansi256_rgb(index);
        let delta = |channel: usize| color[channel] as i32 - candidate[channel] as i32;
        2 * delta(0).pow(2) + 4 * delta(1).pow(2) + delta(2).pow(2)
    };
    if distance(grey) < distance(cube) {
        grey
    } else {
        cube
    }
}

/// A way of putting a real picture of the board on the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageProtocol {
    /// iTerm2's inline PNGs, which WezTerm, VS Code and others also accept.
    /// The terminal scales the picture to a size given in cells.
    Iterm,
    /// Kitty's graphics protocol: Kitty, Ghostty, Konsole.
    Kitty,
    /// DEC sixel: foot, Konsole, xterm, Windows Terminal, recent VTE. Sixels
    /// are drawn pixel for pixel, so the picture is made at the exact size of
    /// the cells it covers.
    Sixel { cell_width: u16, cell_height: u16 },
}

impl ImageProtocol {
    pub fn name(self) -> &'static str {
        match self {
            ImageProtocol::Iterm => "iTerm images",
            ImageProtocol::Kitty => "Kitty images",
            ImageProtocol::Sixel { .. } => "sixel images",
        }
    }
}

/// Probe for a real inline-image protocol. The queries also work through SSH
/// because the bytes are answered by the terminal emulator on the user's
/// machine, not by the remote shell. iTerm-family terminals are recognized by
/// their conventional environment markers before any query is needed.
pub fn detect_image_protocol() -> Option<ImageProtocol> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return None;
    }
    if viuer::is_iterm_supported() {
        return Some(ImageProtocol::Iterm);
    }
    // VS Code only draws pictures once its `enableImages` setting is on, and
    // then says so by answering the Kitty query or offering sixels. Its
    // pictures of every kind live in the text cells, so it is sent iTerm's
    // PNGs, which the board redraws whenever text lands on them; placing
    // Kitty images there would leave them erased by the next panel update.
    let vscode = std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "vscode");
    if viuer::get_kitty_support() != viuer::KittySupport::None {
        return Some(if vscode {
            ImageProtocol::Iterm
        } else {
            ImageProtocol::Kitty
        });
    }
    let reply = query_terminal("\x1b[16t\x1b[14t\x1b[?2;1;0S\x1b[c")?;
    let answers = TerminalAnswers::parse(&reply);
    if !answers.sixel {
        return None;
    }
    if vscode {
        return Some(ImageProtocol::Iterm);
    }
    let (cols, rows) = terminal_size()?;
    // Windows Terminal scales every sixel from the VT340's 10 by 20 pixel
    // cell to its real font, so that size is right there even unanswered.
    let windows_terminal = std::env::var_os("WT_SESSION").is_some();
    let (cell_width, cell_height) = answers
        .cell_size(cols, rows)
        .or(windows_terminal.then_some((10, 20)))?;
    Some(ImageProtocol::Sixel {
        cell_width,
        cell_height,
    })
}

/// Advice for a terminal that could draw the board better than it does with
/// its current settings, shown once when a game starts.
pub fn terminal_tip(protocol: Option<ImageProtocol>) -> Option<&'static str> {
    let vscode = std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "vscode");
    (vscode && protocol.is_none()).then_some(
        "Sharper pieces in VS Code: turn on terminal.integrated.enableImages, \
         or set terminal.integrated.minimumContrastRatio to 1.",
    )
}

/// What a terminal said about itself in reply to [`detect_image_protocol`]'s
/// queries. Any of them may go unanswered except the device attributes.
#[derive(Debug, Default, PartialEq, Eq)]
struct TerminalAnswers {
    /// Primary device attributes list `4`.
    sixel: bool,
    /// `CSI 16 t`: one cell, in pixels, height first.
    cell: Option<(u32, u32)>,
    /// `CSI 14 t`: the text area, in pixels, height first.
    area: Option<(u32, u32)>,
    /// `XTSMGRAPHICS`: the largest sixel the terminal takes, width first. On
    /// xterm.js-based terminals this is the whole canvas.
    geometry: Option<(u32, u32)>,
}

impl TerminalAnswers {
    fn parse(reply: &str) -> TerminalAnswers {
        let mut answers = TerminalAnswers::default();
        for sequence in reply
            .split('\x1b')
            .filter_map(|part| part.strip_prefix('['))
        {
            let Some(last) = sequence.chars().last() else {
                continue;
            };
            let body = &sequence[..sequence.len() - last.len_utf8()];
            let private = body.starts_with('?');
            let numbers: Vec<u32> = body
                .trim_start_matches('?')
                .split(';')
                .map(|number| number.parse().unwrap_or(0))
                .collect();
            let pair = |first: usize| Some((*numbers.get(first)?, *numbers.get(first + 1)?));
            match (private, last, numbers.first()) {
                (true, 'c', _) => answers.sixel = numbers.iter().skip(1).any(|&n| n == 4),
                (false, 't', Some(6)) => answers.cell = pair(1),
                (false, 't', Some(4)) => answers.area = pair(1),
                (true, 'S', Some(2)) if numbers.get(1) == Some(&0) => answers.geometry = pair(2),
                _ => {}
            }
        }
        answers
    }

    /// Width and height of one cell in pixels, from the most direct answer
    /// the terminal gave.
    fn cell_size(&self, cols: usize, rows: usize) -> Option<(u16, u16)> {
        let (cols, rows) = (cols as u32, rows as u32);
        let (width, height) = match (self.cell, self.area, self.geometry) {
            (Some((height, width)), _, _) => (width, height),
            (None, Some((height, width)), _) => (width / cols, height / rows),
            (None, None, Some((width, height))) => (width / cols, height / rows),
            (None, None, None) => return None,
        };
        let plausible = |value: u32| (2..=200).contains(&value);
        (plausible(width) && plausible(height)).then_some((width as u16, height as u16))
    }
}

/// Send `queries`, which must end with a primary device attributes request,
/// and collect the replies. Every terminal answers that request, and answers
/// in order, so its reply marks the end of everything the terminal will say.
/// The terminal is switched to raw input for the exchange, with reads that
/// give up after a tenth of a second, and two seconds in all.
#[cfg(unix)]
fn query_terminal(queries: &str) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let stty = |args: &[&str]| {
        Command::new("stty")
            .args(args)
            .stdin(Stdio::inherit())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
    };
    let saved = String::from_utf8(stty(&["-g"])?.stdout).ok()?;
    let saved = saved.trim();
    stty(&["raw", "-echo", "min", "0", "time", "1"])?;

    let mut reply = Vec::new();
    if let Ok(mut tty) = std::fs::File::open("/dev/tty") {
        let mut out = io::stdout();
        let _ = out.write_all(queries.as_bytes());
        let _ = out.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0u8; 256];
        while Instant::now() < deadline && !ends_with_device_attributes(&reply) {
            match tty.read(&mut buffer) {
                Ok(count) => reply.extend_from_slice(&buffer[..count]),
                Err(_) => break,
            }
        }
    }
    stty(&[saved]);
    ends_with_device_attributes(&reply).then(|| String::from_utf8_lossy(&reply).into_owned())
}

/// The Windows console form of the exchange above, for cmd and PowerShell in
/// Windows Terminal. The console hands replies over as typed characters once
/// virtual terminal input is on, so input is switched to that, unechoed, and
/// polled until the device attributes arrive or two seconds pass.
#[cfg(windows)]
fn query_terminal(queries: &str) -> Option<String> {
    use crossterm_winapi::{Console, ConsoleMode, Handle, InputRecord};
    use std::time::{Duration, Instant};

    const PROCESSED_INPUT: u32 = 0x0001;
    const LINE_INPUT: u32 = 0x0002;
    const ECHO_INPUT: u32 = 0x0004;
    const VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    let handle = Handle::current_in_handle().ok()?;
    let mode = ConsoleMode::from(handle.clone());
    let saved = mode.mode().ok()?;
    mode.set_mode((saved | VIRTUAL_TERMINAL_INPUT) & !(PROCESSED_INPUT | LINE_INPUT | ECHO_INPUT))
        .ok()?;
    let console = Console::from(handle);

    let mut reply = String::new();
    let mut out = io::stdout();
    let _ = out.write_all(queries.as_bytes());
    let _ = out.flush();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !ends_with_device_attributes(reply.as_bytes()) {
        match console.number_of_console_input_events() {
            Ok(0) => std::thread::sleep(Duration::from_millis(10)),
            Ok(_) => {
                let Ok(records) = console.read_console_input() else {
                    break;
                };
                let units: Vec<u16> = records
                    .into_iter()
                    .filter_map(|record| match record {
                        InputRecord::KeyEvent(key) if key.key_down && key.u_char != 0 => {
                            Some(key.u_char)
                        }
                        _ => None,
                    })
                    .collect();
                reply.push_str(&String::from_utf16_lossy(&units));
            }
            Err(_) => break,
        }
    }
    let _ = mode.set_mode(saved);
    ends_with_device_attributes(reply.as_bytes()).then_some(reply)
}

#[cfg(not(any(unix, windows)))]
fn query_terminal(_queries: &str) -> Option<String> {
    None
}

fn ends_with_device_attributes(reply: &[u8]) -> bool {
    let Some(start) = reply.windows(3).rposition(|window| window == b"\x1b[?") else {
        return false;
    };
    let rest = &reply[start + 3..];
    rest.last() == Some(&b'c')
        && rest[..rest.len() - 1]
            .iter()
            .all(|b| b.is_ascii_digit() || *b == b';')
}

/// The bytes that paint an already composed board into its reserved terminal
/// rectangle, leaving the cursor where it was. They are kept apart from
/// writing them so a caller can send the same picture again without encoding
/// it again. Kitty pictures are sent by [`draw_kitty_image`] instead. If the
/// protocol fails, the Unicode board remains visible underneath.
pub fn inline_image_bytes(
    protocol: ImageProtocol,
    theme: &Theme,
    view: &BoardView,
    metrics: Metrics,
    column: usize,
    row: usize,
) -> Vec<u8> {
    let (cols, rows) = (metrics.cell_w * 8, metrics.cell_h * 8);
    let mut out = format!("\x1b7\x1b[{};{}H", row + 1, column + 1).into_bytes();
    match protocol {
        // iTerm receives a compressed PNG, so give it the full-resolution
        // board and let the terminal scale it to the cells.
        ImageProtocol::Iterm => {
            use base64::Engine as _;
            let image = theme.board_image(view);
            let mut png = Vec::new();
            if image
                .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
                .is_err()
            {
                return Vec::new();
            }
            let _ = write!(
                out,
                "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=0:{}\x07",
                png.len(),
                cols,
                rows,
                base64::engine::general_purpose::STANDARD.encode(&png)
            );
        }
        // A sixel is not scaled by the terminal, so it is drawn exactly as
        // many pixels as the cells it covers. The squares are flat colours
        // and the pieces few, so no dithering is needed to shade them.
        ImageProtocol::Sixel {
            cell_width,
            cell_height,
        } => {
            let tile_w = metrics.cell_w * cell_width as usize;
            let tile_h = metrics.cell_h * cell_height as usize;
            let (width, height) = (tile_w * 8, tile_h * 8);
            let pixels = theme.board_image_sized(view, tile_w, tile_h).to_rgba8();
            let options = icy_sixel::EncodeOptions {
                diffusion: 0.0,
                ..icy_sixel::EncodeOptions::default()
            };
            let Ok(sixel) = icy_sixel::sixel_encode(pixels.as_raw(), width, height, &options)
            else {
                return Vec::new();
            };
            out.extend_from_slice(sixel.as_bytes());
        }
        ImageProtocol::Kitty => return Vec::new(),
    }
    out.extend_from_slice(b"\x1b8");
    out
}

/// Takes every Kitty picture off the screen. They float above the text, so
/// rewriting or clearing the rows beneath one leaves it where it was.
pub const KITTY_DELETE_ALL: &str = "\x1b_Ga=d,d=A\x1b\\";

/// Place the board with the Kitty protocol. viuer's remote path sends raw
/// RGBA bytes; cap that copy so an SSH redraw remains responsive.
pub fn draw_kitty_image(image: &image::DynamicImage, metrics: Metrics, column: usize, row: usize) {
    let image = image.resize_exact(512, 512, image::imageops::FilterType::Lanczos3);
    let config = viuer::Config {
        absolute_offset: true,
        x: column.min(u16::MAX as usize) as u16,
        y: row.min(i16::MAX as usize) as i16,
        restore_cursor: true,
        width: Some((metrics.cell_w * 8) as u32),
        height: Some((metrics.cell_h * 8) as u32),
        truecolor: true,
        use_iterm: false,
        ..viuer::Config::default()
    };
    let _ = viuer::print(&image, &config);
}

// ---------------------------------------------------------------------------
// Asking the terminal how big it is
// ---------------------------------------------------------------------------

/// Columns and rows. `stty` is the one way to ask that needs no C library
/// behind it; `$COLUMNS` is a shell variable that is rarely exported, so it
/// only gets a look in when `stty` is not there at all.
pub fn terminal_size() -> Option<(usize, usize)> {
    if let Some(size) = stty_size() {
        return Some(size);
    }
    let cols: usize = std::env::var("COLUMNS").ok()?.parse().ok()?;
    let rows: usize = std::env::var("LINES").ok()?.parse().ok()?;
    (cols > 0 && rows > 0).then_some((cols, rows))
}

fn stty_size() -> Option<(usize, usize)> {
    use std::process::{Command, Stdio};
    // `output()` would hand stty a closed stdin, and stty asks the terminal on
    // its stdin how big it is, so ours has to be passed straight through.
    let output = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut parts = text.split_whitespace();
    let rows: usize = parts.next()?.parse().ok()?;
    let cols: usize = parts.next()?.parse().ok()?;
    (cols > 0 && rows > 0).then_some((cols, rows))
}

// ---------------------------------------------------------------------------
// How big to draw the board
// ---------------------------------------------------------------------------

/// Which set of pieces to put on the squares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pieces {
    /// Drawn pieces when the squares are big enough for them, figurines when
    /// they are not.
    Auto,
    Art,
    Glyph,
}

impl Pieces {
    pub fn name(self) -> &'static str {
        match self {
            Pieces::Auto => "auto",
            Pieces::Art => "art",
            Pieces::Glyph => "glyph",
        }
    }
}

pub fn pieces_named(name: &str) -> Option<Pieces> {
    match name.to_ascii_lowercase().as_str() {
        "auto" => Some(Pieces::Auto),
        "art" | "drawn" | "big" | "vector" => Some(Pieces::Art),
        "glyph" | "glyphs" | "figurine" | "figurines" | "small" => Some(Pieces::Glyph),
        _ => None,
    }
}

/// A window short enough that the blank lines spacing the frame out are
/// better spent on the board itself.
pub fn tight(rows: usize) -> bool {
    rows < 30
}

/// The rank-label gutter down either side of the board.
pub const GUTTER: usize = 3;

/// The size of one square, in terminal cells. A terminal cell is about twice
/// as tall as it is wide, so a square only looks square at two to one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub cell_w: usize,
    pub cell_h: usize,
    /// Drawn pieces rather than figurines.
    pub art: bool,
}

impl Metrics {
    /// The old small board: one row per rank, three columns per square. Still
    /// what a pipe, a dumb terminal and `--compact` get.
    pub const COMPACT: Metrics = Metrics {
        cell_w: 3,
        cell_h: 1,
        art: false,
    };

    /// The biggest board that leaves room for the frame around it: the bar,
    /// the status line, the notes and the prompt, plus the blank lines
    /// between them that a short window does without.
    pub fn fit(cols: usize, rows: usize, pieces: Pieces) -> Metrics {
        Metrics::fit_with_reserve(cols, rows, pieces, 0)
    }

    /// Fit a board while reserving additional rows for a compact information
    /// panel below it. Width only constrains the board itself; whether a side
    /// panel fits is a separate, content-driven layout decision.
    pub fn fit_with_reserve(
        cols: usize,
        rows: usize,
        pieces: Pieces,
        extra_rows: usize,
    ) -> Metrics {
        let reserve = if tight(rows) { 7 } else { 11 };
        let reserve = reserve + extra_rows;
        let mut cell_h = (rows.saturating_sub(reserve) / 8).clamp(1, 8);
        while cell_h > 1 && GUTTER * 2 + 8 * (2 * cell_h) + 2 > cols {
            cell_h -= 1;
        }
        let cell_w = if cell_h == 1 { 3 } else { 2 * cell_h };
        let art = match pieces {
            Pieces::Glyph => false,
            Pieces::Art | Pieces::Auto => cell_h >= 3 && cell_w >= 6,
        };
        Metrics {
            cell_w,
            cell_h,
            art,
        }
    }

    pub fn board_width(&self) -> usize {
        GUTTER * 2 + 8 * self.cell_w
    }

    /// Eight ranks between two file strips.
    pub fn board_height(&self) -> usize {
        2 + 8 * self.cell_h
    }
}

// ---------------------------------------------------------------------------
// Vector pieces
// ---------------------------------------------------------------------------

// A terminal cannot enlarge an ordinary glyph. Large pieces therefore start
// from real SVG artwork and are rasterized to the square's exact dimensions.
// Each terminal cell becomes four quadrants using the standard block-element
// characters. Two locally chosen colours reproduce those four samples. Unlike
// Braille, solid regions stay solid; unlike plain half-blocks, curves get twice
// the horizontal detail.

const PIECE_SVGS: [[&str; 6]; 2] = [
    [
        include_str!("../assets/pieces/rhosgfx/wP.svg"),
        include_str!("../assets/pieces/rhosgfx/wN.svg"),
        include_str!("../assets/pieces/rhosgfx/wB.svg"),
        include_str!("../assets/pieces/rhosgfx/wR.svg"),
        include_str!("../assets/pieces/rhosgfx/wQ.svg"),
        include_str!("../assets/pieces/rhosgfx/wK.svg"),
    ],
    [
        include_str!("../assets/pieces/rhosgfx/bP.svg"),
        include_str!("../assets/pieces/rhosgfx/bN.svg"),
        include_str!("../assets/pieces/rhosgfx/bB.svg"),
        include_str!("../assets/pieces/rhosgfx/bR.svg"),
        include_str!("../assets/pieces/rhosgfx/bQ.svg"),
        include_str!("../assets/pieces/rhosgfx/bK.svg"),
    ],
];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SpriteKey {
    color: usize,
    kind: usize,
    width: usize,
    height: usize,
    hard: bool,
}

type Sprite = Arc<[u8]>;

static SPRITES: OnceLock<Mutex<HashMap<SpriteKey, Sprite>>> = OnceLock::new();

/// Rasterize a piece to an exact pixel size. Keeping terminal-cell metrics out
/// of this boundary prevents callers from accidentally confusing cells with
/// the two-by-two pixel samples used by the portable renderer. `hard`
/// sprites have only opaque and empty pixels, for the block renderer.
fn piece_sprite(piece: Piece, width: usize, height: usize, hard: bool) -> Sprite {
    let key = SpriteKey {
        color: piece.color.index(),
        kind: piece.kind.index(),
        width,
        height,
        hard,
    };
    let sprites = SPRITES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(sprite) = sprites
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .cloned()
    {
        return sprite;
    }

    let tree = resvg::usvg::Tree::from_str(
        PIECE_SVGS[key.color][key.kind],
        &resvg::usvg::Options::default(),
    )
    .expect("bundled chess piece SVG must be valid");
    // Rendered straight to size, a sprite this small is mostly antialiased
    // edge, and each blended edge pixel becomes a grey fringe around the
    // piece. Rendering larger and deciding each pixel by how much of it the
    // piece covers gives a hard silhouette in the piece's own colours.
    let supersample = if hard { 4 } else { 1 };
    let (big_width, big_height) = (key.width * supersample, key.height * supersample);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(big_width as u32, big_height as u32)
        .expect("chess piece sprite dimensions must be non-zero");
    let scale_x = big_width as f32 / tree.size().width();
    let scale_y = big_height as f32 / tree.size().height();
    let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let sprite: Sprite = if hard {
        Arc::from(harden(pixmap.data(), key.width, key.height, supersample))
    } else {
        Arc::from(pixmap.data())
    };

    sprites
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, sprite.clone());
    sprite
}

/// Reduce a premultiplied RGBA render `factor` times the target size to the
/// target size. A pixel the piece covers at least half of becomes opaque, in
/// the average colour of the covered part; any other pixel is left empty.
fn harden(big: &[u8], width: usize, height: usize, factor: usize) -> Vec<u8> {
    let big_width = width * factor;
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0u32; 4];
            for sy in 0..factor {
                for sx in 0..factor {
                    let offset = ((y * factor + sy) * big_width + x * factor + sx) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += big[offset + channel] as u32;
                    }
                }
            }
            let samples = (factor * factor) as u32;
            if sum[3] * 2 < samples * 255 {
                continue;
            }
            // Premultiplied sums divided by the alpha sum give the covered
            // part's own colour, unmixed with whatever lies behind it.
            let offset = (y * width + x) * 4;
            for channel in 0..3 {
                out[offset + channel] = ((sum[channel] * 255 + sum[3] / 2) / sum[3]).min(255) as u8;
            }
            out[offset + 3] = 255;
        }
    }
    out
}

fn ansi256_rgb(index: u8) -> [u8; 3] {
    const BASIC: [[u8; 3]; 16] = [
        [0, 0, 0],
        [128, 0, 0],
        [0, 128, 0],
        [128, 128, 0],
        [0, 0, 128],
        [128, 0, 128],
        [0, 128, 128],
        [192, 192, 192],
        [128, 128, 128],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [0, 0, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    match index {
        0..=15 => BASIC[index as usize],
        16..=231 => {
            let cube = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            [
                component(cube / 36),
                component((cube % 36) / 6),
                component(cube % 6),
            ]
        }
        _ => {
            let value = 8 + (index - 232) * 10;
            [value, value, value]
        }
    }
}

/// tiny-skia stores premultiplied RGBA. Composite one sprite pixel over its
/// square so the SVG's antialiased edge becomes a final opaque terminal color.
fn composite(sprite: &[u8], offset: usize, background: [u8; 3]) -> [u8; 3] {
    let alpha = sprite[offset + 3] as u16;
    let inverse = 255 - alpha;
    let channel = |foreground: u8, background: u8| {
        (foreground as u16 + (background as u16 * inverse + 127) / 255).min(255) as u8
    };
    [
        channel(sprite[offset], background[0]),
        channel(sprite[offset + 1], background[1]),
        channel(sprite[offset + 2], background[2]),
    ]
}

/// The sample in one half of a partition that best stands for the rest of
/// that half. A mean would invent colours that are in neither the piece nor
/// the square, and those in-between shades read as a grey smear.
fn medoid_color(pixels: &[[u8; 3]; 4], mask: u8, foreground: bool) -> [u8; 3] {
    let members: Vec<[u8; 3]> = pixels
        .iter()
        .enumerate()
        .filter(|(index, _)| ((mask >> index) & 1 == 1) == foreground)
        .map(|(_, pixel)| *pixel)
        .collect();
    members
        .iter()
        .copied()
        .min_by_key(|candidate| {
            members
                .iter()
                .map(|pixel| color_distance(*pixel, *candidate))
                .sum::<u32>()
        })
        .expect("every partition half has a member")
}

fn color_distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    let red = a[0] as i32 - b[0] as i32;
    let green = a[1] as i32 - b[1] as i32;
    let blue = a[2] as i32 - b[2] as i32;
    (2 * red * red + 4 * green * green + blue * blue) as u32
}

fn color_error(pixels: &[[u8; 3]; 4], mask: u8, foreground: [u8; 3], background: [u8; 3]) -> u32 {
    pixels
        .iter()
        .enumerate()
        .map(|(index, pixel)| {
            let sample = if (mask >> index) & 1 == 1 {
                foreground
            } else {
                background
            };
            color_distance(*pixel, sample)
        })
        .sum()
}

/// Pick the best two-colour representation of four quadrant samples. Masks
/// 1..=7 cover every partition; 8..=14 are the same partitions with the two
/// colours exchanged.
fn quadrant(pixels: [[u8; 3]; 4]) -> ([u8; 3], [u8; 3], char) {
    const GLYPHS: [char; 8] = [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛'];
    let solid = medoid_color(&pixels, 0, false);
    let mut best = (color_error(&pixels, 0, solid, solid), solid, solid, ' ');

    for mask in 1..=7 {
        let foreground = medoid_color(&pixels, mask, true);
        let background = medoid_color(&pixels, mask, false);
        let error = color_error(&pixels, mask, foreground, background);
        if error < best.0 {
            best = (error, foreground, background, GLYPHS[mask as usize]);
        }
    }
    (best.1, best.2, best.3)
}

fn art_row(piece: Piece, row: usize, metrics: Metrics, background: Rgb, depth: Depth) -> String {
    let sprite = piece_sprite(piece, metrics.cell_w * 2, metrics.cell_h * 2, true);
    let pixel_width = metrics.cell_w * 2;
    debug_assert_eq!(sprite.len(), pixel_width * metrics.cell_h * 2 * 4);
    let mut output = String::with_capacity(metrics.cell_w * 34);

    for column in 0..metrics.cell_w {
        let x = column * 2;
        let y = row * 2;
        let pixels = [
            composite(&sprite, (y * pixel_width + x) * 4, background),
            composite(&sprite, (y * pixel_width + x + 1) * 4, background),
            composite(&sprite, ((y + 1) * pixel_width + x) * 4, background),
            composite(&sprite, ((y + 1) * pixel_width + x + 1) * 4, background),
        ];
        let (foreground, background, glyph) = quadrant(pixels);
        if glyph == ' ' {
            let _ = write!(output, "\x1b[{}m ", depth.bg(background));
        } else {
            let _ = write!(
                output,
                "\x1b[{};{}m{}",
                depth.fg(foreground),
                depth.bg(background),
                glyph
            );
        }
    }
    output
}

// ---------------------------------------------------------------------------
// The theme
// ---------------------------------------------------------------------------

pub struct Theme {
    /// Escape codes allowed.
    pub color: bool,
    /// Letters instead of figurines.
    pub ascii: bool,
    /// Redraw the screen in place rather than scrolling a log past.
    pub live: bool,
    pub palette: Palette,
    /// 24-bit colour, or the nearest of the 256.
    pub depth: Depth,
}

impl Theme {
    pub fn new(color: bool, ascii: bool, live: bool, palette: Palette) -> Theme {
        Theme {
            color,
            ascii,
            live,
            palette,
            depth: Depth::detect(),
        }
    }

    /// Override the detected colour depth, for `--truecolor` and `--256`.
    pub fn with_depth(mut self, depth: Option<Depth>) -> Theme {
        if let Some(depth) = depth {
            self.depth = depth;
        }
        self
    }

    /// Colour unless the user, the terminal or the pipe says otherwise.
    pub fn detect_color() -> bool {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if let Ok(term) = std::env::var("TERM") {
            if term == "dumb" {
                return false;
            }
        }
        io::stdout().is_terminal()
    }

    /// Only take the screen over when both ends are a terminal: a piped stdin
    /// is a script, and a script wants a transcript, not an animation.
    pub fn detect_live() -> bool {
        io::stdout().is_terminal() && io::stdin().is_terminal()
    }

    // -- styling ------------------------------------------------------------

    fn sgr(&self, codes: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{}m{}\x1b[0m", codes, text)
        } else {
            text.to_string()
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.sgr("2", text)
    }

    pub fn bold(&self, text: &str) -> String {
        self.sgr("1", text)
    }

    pub fn fg(&self, color: Rgb, text: &str) -> String {
        self.sgr(&self.depth.fg(color), text)
    }

    pub fn strong(&self, color: Rgb, text: &str) -> String {
        self.sgr(&format!("1;{}", self.depth.fg(color)), text)
    }

    pub fn accent(&self, text: &str) -> String {
        self.fg(self.palette.accent, text)
    }

    pub fn warn(&self, text: &str) -> String {
        self.fg(self.palette.warn, text)
    }

    pub fn good(&self, text: &str) -> String {
        self.fg(self.palette.good, text)
    }

    pub fn label(&self, text: &str) -> String {
        self.fg(self.palette.label, text)
    }

    /// Keyboard focus is deliberately stronger than an ordinary accent: it
    /// must remain unmistakable across every board palette.
    pub fn focused(&self, color: Rgb, text: &str) -> String {
        self.sgr(&format!("1;7;{}", self.depth.fg(color)), text)
    }

    /// Dark bold letters on a solid block of `color`, for news that should be
    /// seen without being looked for. Brackets stand in for the block when
    /// colour is off.
    pub fn badge(&self, color: Rgb, text: &str) -> String {
        if !self.color {
            return format!("[{text}]");
        }
        let codes = format!(
            "1;{};{}",
            self.depth.fg(hex(0x1b1d21)),
            self.depth.bg(color)
        );
        self.sgr(&codes, &format!(" {text} "))
    }

    // -- screen -------------------------------------------------------------

    /// Wipe the screen for the next frame. A no-op unless we own the terminal.
    pub fn clear(&self) {
        if self.live && self.color {
            print!("\x1b[2J\x1b[H");
            let _ = io::stdout().flush();
        }
    }

    /// Erase the line the cursor is on, for a readout that updates in place.
    pub fn erase_line(&self) {
        if self.live && self.color {
            print!("\r\x1b[K");
            let _ = io::stdout().flush();
        }
    }

    /// A horizontal rule. ASCII when figurines are off, since a terminal that
    /// cannot draw a knight cannot draw a line either.
    pub fn rule(&self, width: usize) -> String {
        let bar = if self.ascii { "-" } else { "\u{2500}" };
        self.dim(&bar.repeat(width))
    }

    /// The bar across the top of the screen: a title on the left, a reminder
    /// on the right, the board's own dark tone behind the whole width.
    pub fn bar(&self, left: &str, right: &str, cols: usize) -> String {
        let plain = format!(
            "{}{}{}",
            left,
            " ".repeat(cols.saturating_sub(width(left) + width(right) + 4).max(1)),
            right
        );
        let plain = pad(&format!(" {} ", plain), cols);
        if self.color {
            format!(
                "\x1b[{};{}m{}\x1b[0m",
                self.depth.bg(self.palette.bar),
                self.depth.fg(hex(0xffffff)),
                plain
            )
        } else {
            plain.trim_end().to_string()
        }
    }

    // -- pieces -------------------------------------------------------------

    fn glyph(&self, piece: Piece) -> char {
        if self.ascii {
            return piece.to_char();
        }
        // The outline set for White, the solid set for Black. Fonts differ on
        // how hollow the outline pieces look, so colour carries the meaning
        // and the shape only reinforces it.
        const WHITE: [char; 6] = [
            '\u{2659}', '\u{2658}', '\u{2657}', '\u{2656}', '\u{2655}', '\u{2654}',
        ];
        const BLACK: [char; 6] = [
            '\u{265F}', '\u{265E}', '\u{265D}', '\u{265C}', '\u{265B}', '\u{265A}',
        ];
        let table = match piece.color {
            Color::White => &WHITE,
            Color::Black => &BLACK,
        };
        table[piece.kind.index()]
    }

    /// A piece on its own, for capture lists and prompts.
    pub fn piece(&self, piece: Piece) -> String {
        let glyph = self.glyph(piece).to_string();
        match piece.color {
            Color::White => self.strong(self.palette.white_piece, &glyph),
            Color::Black => self.fg(self.palette.black_piece, &glyph),
        }
    }

    // -- the board ----------------------------------------------------------

    /// One row of one square: `m.cell_w` columns of background with whatever
    /// part of the piece belongs on this row sitting in the middle of it.
    fn cell(&self, view: &BoardView, s: Square, m: Metrics, row: usize) -> String {
        let promotion = view.promotions.iter().find(|choice| choice.square == s);
        let piece = promotion
            .map(|choice| choice.piece)
            .or_else(|| view.pos.at(s));
        let (light, mark) = view.mark(s);
        let target = matches!(mark, Some(Mark::Target | Mark::Capture));
        let capture = mark == Some(Mark::Capture);
        let invalid = mark == Some(Mark::Invalid);

        if !self.color {
            let glyph = match piece {
                Some(p) => self.glyph(p),
                None if self.ascii => '.',
                None => '\u{00B7}',
            };
            // Brackets, parentheses and stars are all one column wide, so a
            // plain board can mark squares without any of it sliding sideways.
            let (open, close) = match mark {
                _ if view.cursor == Some(s) => ('{', '}'),
                Some(Mark::Check) => ('[', ']'),
                Some(Mark::Invalid) => ('!', '!'),
                Some(Mark::Selected) => ('<', '>'),
                Some(Mark::Capture) => ('x', 'x'),
                Some(Mark::Target) => ('*', '*'),
                Some(Mark::Last) => ('(', ')'),
                None => (' ', ' '),
            };
            return format!("{}{}{}", open, glyph, close);
        }

        let p = &self.palette;
        let d = self.depth;
        let bg = p.square(light, mark, d);
        let middle = m.cell_h / 2;
        if view.cursor == Some(s) {
            if let Some(frame) = self.cursor_row(view, s, m, row, bg) {
                return frame;
            }
        }

        match piece {
            Some(piece) => {
                let (fg, weight) = match piece.color {
                    Color::White => (p.white_piece, ";1"),
                    Color::Black => (p.black_piece, ""),
                };
                let content = if m.art {
                    art_row(piece, row, m, bg, d)
                } else if row == middle {
                    center(&self.glyph(piece).to_string(), m.cell_w)
                } else {
                    " ".repeat(m.cell_w)
                };
                if m.art {
                    format!("{}\x1b[0m", content)
                } else {
                    format!(
                        "\x1b[{};{}{}m{}\x1b[0m",
                        d.bg(bg),
                        d.fg(fg),
                        weight,
                        content
                    )
                }
            }
            // An empty square worth looking at gets a dot to look at.
            None if (target || invalid) && row == middle => format!(
                "\x1b[{};{}m{}\x1b[0m",
                d.bg(bg),
                d.fg(p.marker(bg)),
                center(if capture || invalid { "×" } else { "\u{2022}" }, m.cell_w)
            ),
            None => format!("\x1b[{}m{}\x1b[0m", d.bg(bg), " ".repeat(m.cell_w)),
        }
    }

    /// A row of the square under the keyboard cursor, where the cursor takes
    /// it over: a bar across the top and bottom rows of a big square, and
    /// brackets either side of the piece on a small one. `None` for a row the
    /// ordinary drawing keeps.
    fn cursor_row(
        &self,
        view: &BoardView,
        s: Square,
        m: Metrics,
        row: usize,
        bg: Rgb,
    ) -> Option<String> {
        let d = self.depth;
        let middle = m.cell_h / 2;
        let bars = m.cell_h >= 3 && !self.ascii;
        if bars && (row == 0 || row + 1 == m.cell_h) {
            let bar = if row == 0 { "\u{2580}" } else { "\u{2584}" };
            return Some(format!(
                "\x1b[{};{}m{}\x1b[0m",
                d.bg(bg),
                d.fg(CURSOR),
                bar.repeat(m.cell_w)
            ));
        }
        if bars || row != middle {
            return None;
        }
        // The piece or dot the square would show, with the outer columns
        // turned into brackets.
        let inner = match (view.pos.at(s), view.mark(s).1) {
            (Some(piece), _) => self.glyph(piece).to_string(),
            (None, Some(Mark::Capture | Mark::Invalid)) => "×".to_string(),
            (None, Some(Mark::Target)) => "\u{2022}".to_string(),
            (None, _) => String::new(),
        };
        let inner = center(&inner, m.cell_w.saturating_sub(2));
        let fg = match view.pos.at(s).map(|piece| piece.color) {
            Some(Color::White) => format!("{};1", d.fg(self.palette.white_piece)),
            Some(Color::Black) => d.fg(self.palette.black_piece),
            None => d.fg(CURSOR),
        };
        Some(format!(
            "\x1b[{};1;{}m[\x1b[0;{};{}m{}\x1b[{};1;{}m]\x1b[0m",
            d.bg(bg),
            d.fg(CURSOR),
            d.bg(bg),
            fg,
            inner,
            d.bg(bg),
            d.fg(CURSOR),
        ))
    }

    /// Compose the board at a protocol-independent pixel resolution. Terminal
    /// graphics clients scale this clean source into the exact cell rectangle,
    /// while the ordinary renderer continues to use block elements.
    pub fn board_image(&self, view: &BoardView) -> image::DynamicImage {
        // iTerm receives this as a compressed PNG. Ninety-six pixels per
        // square preserve the SVG curves on large and Retina displays; the
        // raw-data Kitty path is reduced separately when it is transmitted.
        self.board_image_sized(view, 96, 96)
    }

    /// Compose the board with squares of exactly `tile_w` by `tile_h`
    /// pixels, for a protocol the terminal does not scale. Drawing the pieces
    /// at that size is sharper, and much faster, than resizing a picture.
    pub fn board_image_sized(
        &self,
        view: &BoardView,
        tile_w: usize,
        tile_h: usize,
    ) -> image::DynamicImage {
        let board_w = tile_w * 8;
        let tile = tile_w.min(tile_h);

        let mut image = image::RgbaImage::new(board_w as u32, (tile_h * 8) as u32);
        let pixels = image.as_mut();

        for display_rank in 0..8usize {
            let rank = if view.flipped {
                display_rank as u8
            } else {
                7 - display_rank as u8
            };
            for display_file in 0..8usize {
                let file = if view.flipped {
                    7 - display_file as u8
                } else {
                    display_file as u8
                };
                let square = board::sq(file, rank);
                let promotion = view
                    .promotions
                    .iter()
                    .find(|choice| choice.square == square);
                let (light, mark) = view.mark(square);
                let target = matches!(mark, Some(Mark::Target | Mark::Capture));
                let capture = mark == Some(Mark::Capture);
                let p = &self.palette;
                // An inline image is 24-bit whatever the text around it is.
                let background = p.square(light, mark, Depth::True);
                let origin_x = display_file * tile_w;
                let origin_y = display_rank * tile_h;

                for y in 0..tile_h {
                    for x in 0..tile_w {
                        let offset = ((origin_y + y) * board_w + origin_x + x) * 4;
                        pixels[offset..offset + 3].copy_from_slice(&background);
                        pixels[offset + 3] = 255;
                    }
                }

                let piece = promotion
                    .map(|choice| choice.piece)
                    .or_else(|| view.pos.at(square));
                if let Some(piece) = piece {
                    let sprite = piece_sprite(piece, tile_w, tile_h, false);
                    debug_assert_eq!(sprite.len(), tile_w * tile_h * 4);
                    for y in 0..tile_h {
                        for x in 0..tile_w {
                            let source = (y * tile_w + x) * 4;
                            let color = composite(&sprite, source, background);
                            let destination = ((origin_y + y) * board_w + origin_x + x) * 4;
                            pixels[destination..destination + 3].copy_from_slice(&color);
                        }
                    }
                } else if target && !capture {
                    let marker = p.marker(background);
                    let radius = (tile / 10) as isize;
                    for y in 0..tile_h {
                        for x in 0..tile_w {
                            let dx = x as isize - (tile_w / 2) as isize;
                            let dy = y as isize - (tile_h / 2) as isize;
                            if dx * dx + dy * dy <= radius * radius {
                                let destination = ((origin_y + y) * board_w + origin_x + x) * 4;
                                pixels[destination..destination + 3].copy_from_slice(&marker);
                            }
                        }
                    }
                }

                // The keyboard cursor: a heavy frame at the square's edge,
                // outside the capture frame so both can show at once.
                if view.cursor == Some(square) {
                    let thickness = (tile / 16).max(3);
                    for y in 0..tile_h {
                        for x in 0..tile_w {
                            let edge = x < thickness
                                || x >= tile_w - thickness
                                || y < thickness
                                || y >= tile_h - thickness;
                            if edge {
                                let destination = ((origin_y + y) * board_w + origin_x + x) * 4;
                                pixels[destination..destination + 3].copy_from_slice(&CURSOR);
                            }
                        }
                    }
                }

                // An occupied legal destination gets a quiet inset frame;
                // empty destinations keep the conventional center dot above.
                if capture {
                    let marker = p.marker(background);
                    let inset = tile / 18;
                    let thickness = (tile / 32).max(2);
                    for y in inset..tile_h - inset {
                        for x in inset..tile_w - inset {
                            let on_vertical =
                                x < inset + thickness || x >= tile_w - inset - thickness;
                            let on_horizontal =
                                y < inset + thickness || y >= tile_h - inset - thickness;
                            if on_vertical || on_horizontal {
                                let destination = ((origin_y + y) * board_w + origin_x + x) * 4;
                                pixels[destination..destination + 3].copy_from_slice(&marker);
                            }
                        }
                    }
                }
            }
        }

        image::DynamicImage::ImageRgba8(image)
    }

    /// A file strip, eight ranks, a file strip - [`Metrics::board_height`]
    /// lines, every one of them [`Metrics::board_width`] columns wide, so a
    /// caller can set a panel beside them without measuring anything.
    pub fn board_lines(&self, view: &BoardView, m: Metrics) -> Vec<String> {
        let ranks: Vec<u8> = if view.flipped {
            (0..8).collect()
        } else {
            (0..8).rev().collect()
        };
        let files: Vec<u8> = if view.flipped {
            (0..8).rev().collect()
        } else {
            (0..8).collect()
        };

        let labels: String = files
            .iter()
            .map(|&file| center(&((b'a' + file) as char).to_string(), m.cell_w))
            .collect();
        let strip = self.label(&format!(
            "{}{}{}",
            " ".repeat(GUTTER),
            labels,
            " ".repeat(GUTTER)
        ));

        let middle = m.cell_h / 2;
        let mut lines = Vec::with_capacity(m.board_height());
        lines.push(strip.clone());
        for rank in ranks {
            for row in 0..m.cell_h {
                let digit = if row == middle {
                    center(&(rank + 1).to_string(), GUTTER)
                } else {
                    " ".repeat(GUTTER)
                };
                let mut line = self.label(&digit);
                for &file in &files {
                    line.push_str(&self.cell(view, board::sq(file, rank), m, row));
                }
                line.push_str(&self.label(&digit));
                lines.push(line);
            }
        }
        lines.push(strip);
        lines
    }
}

pub struct BoardView<'a> {
    pub pos: &'a Position,
    pub flipped: bool,
    /// Highlighted as the move just played.
    pub last: Option<Move>,
    /// The square of a king currently in check.
    pub check: Option<Square>,
    /// The piece chosen for a click-to-move interaction.
    pub selected: Option<Square>,
    /// Squares to point at: where a piece can go, or a suggested move.
    pub targets: &'a [Square],
    /// Legal targets that capture, including an empty en-passant destination.
    pub captures: &'a [Square],
    /// The most recently rejected click, highlighted until the next action.
    pub invalid: Option<Square>,
    /// A temporary, chess-client-style promotion menu drawn over one file.
    pub promotions: &'a [PromotionOption],
    /// The square the keyboard cursor is on. It is drawn as a frame rather
    /// than a colour, so it shows whatever else the square is marked for.
    pub cursor: Option<Square>,
}

impl BoardView<'_> {
    /// Whether `square` is a light one, and the strongest reason to highlight
    /// it. A promotion menu hides the selection and capture marks beneath it.
    fn mark(&self, square: Square) -> (bool, Option<Mark>) {
        let light = (board::file_of(square) + board::rank_of(square)) % 2 == 1;
        let promotion = self.promotions.iter().any(|choice| choice.square == square);
        let mark = if self.check == Some(square) {
            Some(Mark::Check)
        } else if self.invalid == Some(square) {
            Some(Mark::Invalid)
        } else if !promotion && self.selected == Some(square) {
            Some(Mark::Selected)
        } else if !promotion && self.captures.contains(&square) {
            Some(Mark::Capture)
        } else if promotion || self.targets.contains(&square) {
            Some(Mark::Target)
        } else if self
            .last
            .is_some_and(|mv| mv.from == square || mv.to == square)
        {
            Some(Mark::Last)
        } else {
            None
        };
        (light, mark)
    }
}

/// One clickable entry in the promotion menu.
#[derive(Clone, Copy)]
pub struct PromotionOption {
    pub square: Square,
    pub piece: Piece,
    pub movement: Move,
}

// ---------------------------------------------------------------------------
// Taking the screen over
// ---------------------------------------------------------------------------

/// Switches the terminal to its alternate screen, the way a pager or an editor
/// does, and switches it back however the program ends - including on a panic,
/// which would otherwise leave the shell without a cursor.
pub struct Fullscreen {
    active: bool,
}

impl Fullscreen {
    pub fn enter(theme: &Theme) -> Fullscreen {
        let active = theme.live && theme.color;
        if active {
            // Keep the window's title on the terminal's title stack, since
            // the game sets its own; terminals without one ignore this.
            print!("\x1b[22;0t\x1b[?1049h\x1b[2J\x1b[H");
            let _ = io::stdout().flush();
        }
        Fullscreen { active }
    }
}

impl Drop for Fullscreen {
    fn drop(&mut self) {
        if self.active {
            print!("\x1b[?25h\x1b[?1049l\x1b[23;0t");
            let _ = io::stdout().flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Set `right` beside `left`, which must be `width` columns wide throughout.
pub fn beside(left: &[String], right: &[String], width: usize, gap: usize) -> Vec<String> {
    let blank = " ".repeat(width);
    let rows = left.len().max(right.len());
    (0..rows)
        .map(|i| {
            let l = left.get(i).map(String::as_str).unwrap_or(&blank);
            let r = right.get(i).map(String::as_str).unwrap_or("");
            let line = format!("{}{}{}", l, " ".repeat(gap), r);
            line.trim_end().to_string()
        })
        .collect()
}

/// Centre `text` in `columns`, for a letter over a file or a piece on a square.
pub fn center(text: &str, columns: usize) -> String {
    let visible = width(text);
    if visible >= columns {
        return text.to_string();
    }
    let left = (columns - visible) / 2;
    format!(
        "{}{}{}",
        " ".repeat(left),
        text,
        " ".repeat(columns - visible - left)
    )
}

/// Display columns, ignoring escape sequences. Good enough for the text this
/// program lays out, which is ASCII apart from the pieces.
pub fn width(text: &str) -> usize {
    let mut columns = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            columns += 1;
        }
    }
    columns
}

/// Pad `text` to `columns`, counting what is actually visible.
pub fn pad(text: &str, columns: usize) -> String {
    let visible = width(text);
    if visible >= columns {
        text.to_string()
    } else {
        format!("{}{}", text, " ".repeat(columns - visible))
    }
}

/// Cut `text` down to `columns`, marking the cut with an ellipsis so that a
/// note trimmed by a narrow window does not read as a finished sentence.
pub fn clip_note(text: &str, columns: usize) -> String {
    if width(text) <= columns || columns == 0 {
        return text.to_string();
    }
    format!("{}\u{2026}", clip(text, columns - 1))
}

/// Cut `text` down to `columns`, escape codes not counting towards the total.
pub fn clip(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.to_string();
    }
    let mut out = String::new();
    let mut seen = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            if seen == columns {
                break;
            }
            out.push(c);
            seen += 1;
        }
    }
    out.push_str("\x1b[0m");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_board_is_a_square_opaque_raster() {
        let position = Position::startpos();
        let theme = Theme::new(true, false, false, THEMES[0].1);
        let view = BoardView {
            pos: &position,
            flipped: false,
            last: None,
            check: None,
            selected: None,
            targets: &[],
            captures: &[],
            invalid: None,
            promotions: &[],
            cursor: None,
        };

        let board = theme.board_image(&view).to_rgba8();
        assert_eq!(board.dimensions(), (768, 768));
        assert!(board.pixels().all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn indexed_colours_round_to_their_own_palette_entry() {
        // Every cube and grey entry is its own nearest match.
        for index in 16..=255u8 {
            assert_eq!(ansi256_index(ansi256_rgb(index)), index);
        }
        assert_eq!(Depth::Indexed.bg(hex(0x87afaf)), "48;5;109");
        assert_eq!(Depth::True.fg(hex(0x87afaf)), "38;2;135;175;175");
    }

    #[test]
    fn highlights_keep_light_and_dark_squares_apart() {
        for (name, palette) in THEMES {
            for mark in [
                None,
                Some(Mark::Last),
                Some(Mark::Check),
                Some(Mark::Target),
                Some(Mark::Capture),
                Some(Mark::Selected),
            ] {
                let light = palette.square(true, mark, Depth::True);
                let dark = palette.square(false, mark, Depth::True);
                let luma = |c: Rgb| 2 * c[0] as u32 + 5 * c[1] as u32 + c[2] as u32;
                assert!(luma(light) > luma(dark), "{name} {mark:?}");
                if mark.is_some() {
                    assert_ne!(light, palette.light, "{name} {mark:?}");
                    assert_ne!(dark, palette.dark, "{name} {mark:?}");
                    // The 256-colour fallback must also show every mark.
                    for side in [true, false] {
                        assert_ne!(
                            palette.square(side, mark, Depth::Indexed),
                            palette.square(side, None, Depth::Indexed),
                            "{name} {mark:?} indexed"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_strongest_highlight_wins() {
        let position = Position::startpos();
        let e4 = board::sq(4, 3);
        let d5 = board::sq(3, 4);
        let last = Move::normal(board::sq(4, 1), e4);
        let view = BoardView {
            pos: &position,
            flipped: false,
            last: Some(last),
            check: None,
            selected: Some(e4),
            targets: &[d5],
            captures: &[d5],
            invalid: None,
            promotions: &[],
            cursor: None,
        };
        assert_eq!(view.mark(e4).1, Some(Mark::Selected));
        assert_eq!(view.mark(d5).1, Some(Mark::Capture));
        assert_eq!(view.mark(board::sq(4, 1)).1, Some(Mark::Last));
        assert_eq!(view.mark(board::sq(0, 0)), (false, None));
    }

    #[test]
    fn the_cursor_is_drawn_as_a_shape_at_every_size() {
        let position = Position::startpos();
        let e2 = board::sq(4, 1);
        let view = BoardView {
            pos: &position,
            flipped: false,
            last: None,
            check: None,
            selected: None,
            targets: &[],
            captures: &[],
            invalid: None,
            promotions: &[],
            cursor: Some(e2),
        };
        let plain = |line: &str| {
            let mut out = String::new();
            let mut escape = false;
            for c in line.chars() {
                match (escape, c) {
                    (_, '\x1b') => escape = true,
                    (true, 'm') => escape = false,
                    (true, _) => {}
                    (false, c) => out.push(c),
                }
            }
            out
        };
        // Small squares: brackets round the pawn, and still three columns.
        let theme = Theme::new(true, false, false, THEMES[0].1);
        let small = theme.board_lines(&view, Metrics::COMPACT);
        let rank2 = plain(&small[7]);
        assert!(rank2.contains("[\u{2659}]"), "{rank2}");
        assert_eq!(width(&small[7]), width(&small[6]));

        // Big squares: bars over and under, the piece left alone.
        let big = Metrics {
            cell_w: 8,
            cell_h: 4,
            art: false,
        };
        let lines = theme.board_lines(&view, big);
        let top = 1 + 6 * 4;
        assert!(plain(&lines[top]).contains(&"\u{2580}".repeat(8)));
        assert!(plain(&lines[top + 3]).contains(&"\u{2584}".repeat(8)));
        assert!(plain(&lines[top + 2]).contains('\u{2659}'));
        assert_eq!(width(&lines[top]), width(&lines[top + 1]));

        // Without colour: braces, which no other mark uses.
        let mono = Theme::new(false, false, false, THEMES[0].1);
        assert!(mono.board_lines(&view, Metrics::COMPACT)[7].contains("{\u{2659}}"));

        // A picture gets a frame in the cursor's colour at the square's edge.
        let image = theme.board_image(&view).to_rgba8();
        let (x, y) = (4 * 96 + 1, 6 * 96 + 48);
        assert_eq!(&image.get_pixel(x, y).0[..3], &CURSOR);
    }

    #[test]
    fn terminal_answers_find_sixel_and_cell_size() {
        // foot: cell size answered directly.
        let foot = TerminalAnswers::parse("\x1b[6;20;10t\x1b[4;480;800t\x1b[?62;4;22c");
        assert!(foot.sixel);
        assert_eq!(foot.cell_size(80, 24), Some((10, 20)));

        // xterm.js with images on: no window reports, but the sixel canvas.
        let vscode = TerminalAnswers::parse("\x1b[?2;0;960;480S\x1b[?62;4;9;22c");
        assert!(vscode.sixel);
        assert_eq!(vscode.cell_size(120, 24), Some((8, 20)));

        // Only the text area: divided by the grid.
        let area = TerminalAnswers::parse("\x1b[4;480;800t\x1b[?62;4c");
        assert_eq!(area.cell_size(80, 24), Some((10, 20)));

        // VTE without sixel, and a VT100-level reply whose `4` is not sixel.
        assert!(!TerminalAnswers::parse("\x1b[6;17;8t\x1b[?65;1;9c").sixel);
        assert!(!TerminalAnswers::parse("\x1b[?4;1c").sixel);

        // Nothing but the device attributes: sixel, but no size to draw at.
        assert_eq!(
            TerminalAnswers::parse("\x1b[?62;4c").cell_size(80, 24),
            None
        );
    }

    #[test]
    fn device_attributes_end_a_reply() {
        assert!(ends_with_device_attributes(b"\x1b[6;20;10t\x1b[?62;4;22c"));
        assert!(!ends_with_device_attributes(b"\x1b[6;20;10t"));
        assert!(!ends_with_device_attributes(b"\x1b[?62;4"));
        assert!(!ends_with_device_attributes(b""));
    }

    #[test]
    fn hard_sprites_have_no_partial_pixels() {
        let piece = Piece::new(Color::White, board::PieceKind::Knight);
        let sprite = piece_sprite(piece, 16, 8, true);
        assert!(sprite
            .chunks(4)
            .all(|pixel| pixel[3] == 0 || pixel[3] == 255));
        assert!(sprite.chunks(4).any(|pixel| pixel[3] == 255));
    }
}
