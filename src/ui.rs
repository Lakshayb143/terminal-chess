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

/// Probe for a real inline-image protocol. The Kitty query also works through
/// SSH because the bytes are answered by the terminal emulator on the user's
/// machine, not by the remote shell. iTerm-family terminals are recognized by
/// their conventional environment markers before the active probe is needed.
pub fn detect_inline_images() -> bool {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return false;
    }
    if viuer::is_iterm_supported() {
        return true;
    }
    viuer::get_kitty_support() != viuer::KittySupport::None
}

/// iTerm's inline images replace the cells they cover. Kitty placements are
/// separate objects and must be removed before a new anonymous placement is
/// drawn in the same rectangle.
pub fn inline_images_replace_in_place() -> bool {
    viuer::is_iterm_supported()
}

/// Paint an already composed board into its reserved terminal rectangle. If a
/// protocol fails, the caller's Unicode board remains visible underneath.
pub fn draw_inline_image(image: &image::DynamicImage, metrics: Metrics, column: usize, row: usize) {
    // iTerm receives a compressed PNG, so give it the full-resolution board.
    // viuer's remote Kitty path sends raw RGBA bytes; cap that copy so an SSH
    // redraw remains responsive without lowering iTerm's image quality.
    let kitty_image;
    let image = if viuer::is_iterm_supported() {
        image
    } else {
        kitty_image = image.resize_exact(512, 512, image::imageops::FilterType::Lanczos3);
        &kitty_image
    };
    let config = viuer::Config {
        absolute_offset: true,
        x: column.min(u16::MAX as usize) as u16,
        y: row.min(i16::MAX as usize) as i16,
        restore_cursor: true,
        width: Some((metrics.cell_w * 8) as u32),
        height: Some((metrics.cell_h * 8) as u32),
        truecolor: true,
        ..viuer::Config::default()
    };
    let _ = viuer::print(image, &config);
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
}

type Sprite = Arc<[u8]>;

static SPRITES: OnceLock<Mutex<HashMap<SpriteKey, Sprite>>> = OnceLock::new();

/// Rasterize a piece to an exact pixel size. Keeping terminal-cell metrics out
/// of this boundary prevents callers from accidentally confusing cells with
/// the two-by-two pixel samples used by the portable renderer.
fn piece_sprite(piece: Piece, width: usize, height: usize) -> Sprite {
    let key = SpriteKey {
        color: piece.color.index(),
        kind: piece.kind.index(),
        width,
        height,
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
    let mut pixmap = resvg::tiny_skia::Pixmap::new(key.width as u32, key.height as u32)
        .expect("chess piece sprite dimensions must be non-zero");
    let scale_x = key.width as f32 / tree.size().width();
    let scale_y = key.height as f32 / tree.size().height();
    let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let sprite: Sprite = Arc::from(pixmap.data());

    sprites
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, sprite.clone());
    sprite
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

fn mean_color(pixels: &[[u8; 3]; 4], mask: u8, foreground: bool) -> [u8; 3] {
    let mut sum = [0u16; 3];
    let mut count = 0u16;
    for (index, pixel) in pixels.iter().enumerate() {
        if ((mask >> index) & 1 == 1) == foreground {
            for channel in 0..3 {
                sum[channel] += pixel[channel] as u16;
            }
            count += 1;
        }
    }
    [
        ((sum[0] + count / 2) / count) as u8,
        ((sum[1] + count / 2) / count) as u8,
        ((sum[2] + count / 2) / count) as u8,
    ]
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
            let red = pixel[0] as i32 - sample[0] as i32;
            let green = pixel[1] as i32 - sample[1] as i32;
            let blue = pixel[2] as i32 - sample[2] as i32;
            (2 * red * red + 4 * green * green + blue * blue) as u32
        })
        .sum()
}

/// Pick the best two-colour representation of four quadrant samples. Masks
/// 1..=7 cover every partition; 8..=14 are the same partitions with the two
/// colours exchanged.
fn quadrant(pixels: [[u8; 3]; 4]) -> ([u8; 3], [u8; 3], char) {
    const GLYPHS: [char; 8] = [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛'];
    let solid = mean_color(&pixels, 0, false);
    let mut best = (color_error(&pixels, 0, solid, solid), solid, solid, ' ');

    for mask in 1..=7 {
        let foreground = mean_color(&pixels, mask, true);
        let background = mean_color(&pixels, mask, false);
        let error = color_error(&pixels, mask, foreground, background);
        if error < best.0 {
            best = (error, foreground, background, GLYPHS[mask as usize]);
        }
    }
    (best.1, best.2, best.3)
}

fn art_row(piece: Piece, row: usize, metrics: Metrics, background: Rgb, depth: Depth) -> String {
    let sprite = piece_sprite(piece, metrics.cell_w * 2, metrics.cell_h * 2);
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

    /// Compose the board at a protocol-independent pixel resolution. Terminal
    /// graphics clients scale this clean source into the exact cell rectangle,
    /// while the ordinary renderer continues to use block elements.
    pub fn board_image(&self, view: &BoardView) -> image::DynamicImage {
        // iTerm receives this as a compressed PNG. Ninety-six pixels per
        // square preserve the SVG curves on large and Retina displays; the
        // raw-data Kitty path is reduced separately when it is transmitted.
        const TILE: usize = 96;
        const BOARD: usize = TILE * 8;

        let mut image = image::RgbaImage::new(BOARD as u32, BOARD as u32);
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
                let origin_x = display_file * TILE;
                let origin_y = display_rank * TILE;

                for y in 0..TILE {
                    for x in 0..TILE {
                        let offset = ((origin_y + y) * BOARD + origin_x + x) * 4;
                        pixels[offset..offset + 3].copy_from_slice(&background);
                        pixels[offset + 3] = 255;
                    }
                }

                let piece = promotion
                    .map(|choice| choice.piece)
                    .or_else(|| view.pos.at(square));
                if let Some(piece) = piece {
                    let sprite = piece_sprite(piece, TILE, TILE);
                    debug_assert_eq!(sprite.len(), TILE * TILE * 4);
                    for y in 0..TILE {
                        for x in 0..TILE {
                            let source = (y * TILE + x) * 4;
                            let color = composite(&sprite, source, background);
                            let destination = ((origin_y + y) * BOARD + origin_x + x) * 4;
                            pixels[destination..destination + 3].copy_from_slice(&color);
                        }
                    }
                } else if target && !capture {
                    let marker = p.marker(background);
                    let radius = (TILE / 10) as isize;
                    let center = (TILE / 2) as isize;
                    for y in 0..TILE {
                        for x in 0..TILE {
                            let dx = x as isize - center;
                            let dy = y as isize - center;
                            if dx * dx + dy * dy <= radius * radius {
                                let destination = ((origin_y + y) * BOARD + origin_x + x) * 4;
                                pixels[destination..destination + 3].copy_from_slice(&marker);
                            }
                        }
                    }
                }

                // An occupied legal destination gets a quiet inset frame;
                // empty destinations keep the conventional center dot above.
                if capture {
                    let marker = p.marker(background);
                    let inset = TILE / 18;
                    let thickness = (TILE / 32).max(2);
                    for y in inset..TILE - inset {
                        for x in inset..TILE - inset {
                            let on_vertical =
                                x < inset + thickness || x >= TILE - inset - thickness;
                            let on_horizontal =
                                y < inset + thickness || y >= TILE - inset - thickness;
                            if on_vertical || on_horizontal {
                                let destination = ((origin_y + y) * BOARD + origin_x + x) * 4;
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
            print!("\x1b[?1049h\x1b[2J\x1b[H");
            let _ = io::stdout().flush();
        }
        Fullscreen { active }
    }
}

impl Drop for Fullscreen {
    fn drop(&mut self) {
        if self.active {
            print!("\x1b[?25h\x1b[?1049l");
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
        };
        assert_eq!(view.mark(e4).1, Some(Mark::Selected));
        assert_eq!(view.mark(d5).1, Some(Mark::Capture));
        assert_eq!(view.mark(board::sq(4, 1)).1, Some(Mark::Last));
        assert_eq!(view.mark(board::sq(0, 0)), (false, None));
    }
}
