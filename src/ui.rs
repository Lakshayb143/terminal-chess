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

use crate::board::{self, Color, Move, Piece, Position, Square};

// ---------------------------------------------------------------------------
// Palettes
// ---------------------------------------------------------------------------

/// Colours are 256-colour indices, which every terminal worth colouring for
/// has had for twenty years.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    light: u8,
    dark: u8,
    /// The two squares of the move just played.
    light_last: u8,
    dark_last: u8,
    /// The square of a king in check.
    light_check: u8,
    dark_check: u8,
    /// Squares a piece has been asked about, or a hinted move.
    light_target: u8,
    dark_target: u8,
    /// Legal destinations that take an opposing piece.
    light_capture: u8,
    dark_capture: u8,
    /// A square the player clicked but cannot legally use.
    light_invalid: u8,
    dark_invalid: u8,
    /// The piece currently chosen with the mouse.
    light_selected: u8,
    dark_selected: u8,
    white_piece: u8,
    black_piece: u8,
    /// The bar across the top of the screen.
    bar: u8,
    pub label: u8,
    pub accent: u8,
    pub warn: u8,
    pub good: u8,
}

/// Squares are deliberately mid-toned: white pieces are drawn in white and
/// black pieces in near-black, and both have to stay legible on either colour.
pub const THEMES: [(&str, Palette); 4] = [
    (
        "slate",
        Palette {
            light: 109,
            dark: 66,
            light_last: 180,
            dark_last: 137,
            light_check: 174,
            dark_check: 131,
            light_target: 151,
            dark_target: 108,
            light_capture: 186,
            dark_capture: 143,
            light_invalid: 174,
            dark_invalid: 131,
            light_selected: 153,
            dark_selected: 110,
            white_piece: 255,
            black_piece: 233,
            bar: 60,
            label: 245,
            accent: 110,
            warn: 174,
            good: 108,
        },
    ),
    (
        "wood",
        Palette {
            light: 180,
            dark: 137,
            light_last: 186,
            dark_last: 143,
            light_check: 174,
            dark_check: 131,
            light_target: 151,
            dark_target: 108,
            light_capture: 153,
            dark_capture: 110,
            light_invalid: 174,
            dark_invalid: 131,
            light_selected: 153,
            dark_selected: 110,
            white_piece: 255,
            black_piece: 233,
            bar: 95,
            label: 245,
            accent: 179,
            warn: 174,
            good: 108,
        },
    ),
    (
        "forest",
        Palette {
            light: 108,
            dark: 65,
            light_last: 186,
            dark_last: 143,
            light_check: 174,
            dark_check: 131,
            light_target: 152,
            dark_target: 109,
            light_capture: 186,
            dark_capture: 143,
            light_invalid: 174,
            dark_invalid: 131,
            light_selected: 186,
            dark_selected: 143,
            white_piece: 255,
            black_piece: 233,
            bar: 59,
            label: 245,
            accent: 108,
            warn: 174,
            good: 114,
        },
    ),
    (
        "mono",
        Palette {
            light: 145,
            dark: 102,
            light_last: 187,
            dark_last: 144,
            light_check: 181,
            dark_check: 138,
            light_target: 152,
            dark_target: 109,
            light_capture: 195,
            dark_capture: 152,
            light_invalid: 181,
            dark_invalid: 138,
            light_selected: 181,
            dark_selected: 181,
            white_piece: 255,
            black_piece: 233,
            bar: 238,
            label: 245,
            accent: 252,
            warn: 181,
            good: 252,
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

fn art_row(piece: Piece, row: usize, metrics: Metrics, background: u8) -> String {
    let sprite = piece_sprite(piece, metrics.cell_w * 2, metrics.cell_h * 2);
    let background = ansi256_rgb(background);
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
            let _ = write!(
                output,
                "\x1b[48;2;{};{};{}m ",
                background[0], background[1], background[2]
            );
        } else {
            let _ = write!(
                output,
                "\x1b[38;2;{};{};{};48;2;{};{};{}m{}",
                foreground[0],
                foreground[1],
                foreground[2],
                background[0],
                background[1],
                background[2],
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
}

impl Theme {
    pub fn new(color: bool, ascii: bool, live: bool, palette: Palette) -> Theme {
        Theme {
            color,
            ascii,
            live,
            palette,
        }
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

    pub fn fg(&self, color: u8, text: &str) -> String {
        self.sgr(&format!("38;5;{}", color), text)
    }

    pub fn strong(&self, color: u8, text: &str) -> String {
        self.sgr(&format!("1;38;5;{}", color), text)
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
    pub fn focused(&self, color: u8, text: &str) -> String {
        self.sgr(&format!("1;7;38;5;{}", color), text)
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
            format!("\x1b[48;5;{};38;5;231m{}\x1b[0m", self.palette.bar, plain)
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
        let light = (board::file_of(s) + board::rank_of(s)) % 2 == 1;
        let last = view.last.map_or(false, |mv| mv.from == s || mv.to == s);
        let check = view.check == Some(s);
        let invalid = view.invalid == Some(s);
        let selected = promotion.is_none() && view.selected == Some(s);
        let target = promotion.is_some() || view.targets.contains(&s);
        let capture = promotion.is_none() && view.captures.contains(&s);

        if !self.color {
            let glyph = match piece {
                Some(p) => self.glyph(p),
                None if self.ascii => '.',
                None => '\u{00B7}',
            };
            // Brackets, parentheses and stars are all one column wide, so a
            // plain board can mark squares without any of it sliding sideways.
            let (open, close) = if check {
                ('[', ']')
            } else if invalid {
                ('!', '!')
            } else if selected {
                ('<', '>')
            } else if capture {
                ('x', 'x')
            } else if target {
                ('*', '*')
            } else if last {
                ('(', ')')
            } else {
                (' ', ' ')
            };
            return format!("{}{}{}", open, glyph, close);
        }

        let p = &self.palette;
        let bg = match (check, invalid, selected, capture, target, last, light) {
            (true, _, _, _, _, _, true) => p.light_check,
            (true, _, _, _, _, _, false) => p.dark_check,
            (_, true, _, _, _, _, true) => p.light_invalid,
            (_, true, _, _, _, _, false) => p.dark_invalid,
            (_, _, true, _, _, _, true) => p.light_selected,
            (_, _, true, _, _, _, false) => p.dark_selected,
            (_, _, _, true, _, _, true) => p.light_capture,
            (_, _, _, true, _, _, false) => p.dark_capture,
            (_, _, _, _, true, _, true) => p.light_target,
            (_, _, _, _, true, _, false) => p.dark_target,
            (_, _, _, _, _, true, true) => p.light_last,
            (_, _, _, _, _, true, false) => p.dark_last,
            (_, _, _, _, _, false, true) => p.light,
            (_, _, _, _, _, false, false) => p.dark,
        };
        let middle = m.cell_h / 2;

        match piece {
            Some(piece) => {
                let (fg, weight) = match piece.color {
                    Color::White => (p.white_piece, ";1"),
                    Color::Black => (p.black_piece, ""),
                };
                let content = if m.art {
                    art_row(piece, row, m, bg)
                } else if row == middle {
                    center(&self.glyph(piece).to_string(), m.cell_w)
                } else {
                    " ".repeat(m.cell_w)
                };
                if m.art {
                    format!("{}\x1b[0m", content)
                } else {
                    format!("\x1b[48;5;{};38;5;{}{}m{}\x1b[0m", bg, fg, weight, content)
                }
            }
            // An empty square worth looking at gets a dot to look at.
            None if (target || invalid) && row == middle => format!(
                "\x1b[48;5;{};38;5;{}m{}\x1b[0m",
                bg,
                p.label,
                center(if capture || invalid { "×" } else { "\u{2022}" }, m.cell_w)
            ),
            None => format!("\x1b[48;5;{}m{}\x1b[0m", bg, " ".repeat(m.cell_w)),
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
                let light = (file + rank) % 2 == 1;
                let last = view
                    .last
                    .map_or(false, |mv| mv.from == square || mv.to == square);
                let check = view.check == Some(square);
                let invalid = view.invalid == Some(square);
                let selected = promotion.is_none() && view.selected == Some(square);
                let target = promotion.is_some() || view.targets.contains(&square);
                let capture = promotion.is_none() && view.captures.contains(&square);
                let p = &self.palette;
                let background = ansi256_rgb(
                    match (check, invalid, selected, capture, target, last, light) {
                        (true, _, _, _, _, _, true) => p.light_check,
                        (true, _, _, _, _, _, false) => p.dark_check,
                        (_, true, _, _, _, _, true) => p.light_invalid,
                        (_, true, _, _, _, _, false) => p.dark_invalid,
                        (_, _, true, _, _, _, true) => p.light_selected,
                        (_, _, true, _, _, _, false) => p.dark_selected,
                        (_, _, _, true, _, _, true) => p.light_capture,
                        (_, _, _, true, _, _, false) => p.dark_capture,
                        (_, _, _, _, true, _, true) => p.light_target,
                        (_, _, _, _, true, _, false) => p.dark_target,
                        (_, _, _, _, _, true, true) => p.light_last,
                        (_, _, _, _, _, true, false) => p.dark_last,
                        (_, _, _, _, _, false, true) => p.light,
                        (_, _, _, _, _, false, false) => p.dark,
                    },
                );
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
                    let marker = ansi256_rgb(p.label);
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
                    let marker = ansi256_rgb(p.label);
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
}
