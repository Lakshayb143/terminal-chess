//! Position representation: 0x88 board, make/unmake, FEN, Zobrist hashing.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Color {
    White,
    Black,
}

impl Color {
    #[inline]
    pub fn flip(self) -> Color {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn name(self) -> &'static str {
        match self {
            Color::White => "White",
            Color::Black => "Black",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum PieceKind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceKind {
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_char(c: char) -> Option<PieceKind> {
        Some(match c.to_ascii_uppercase() {
            'P' => PieceKind::Pawn,
            'N' => PieceKind::Knight,
            'B' => PieceKind::Bishop,
            'R' => PieceKind::Rook,
            'Q' => PieceKind::Queen,
            'K' => PieceKind::King,
            _ => return None,
        })
    }

    /// Upper-case letter as used in SAN and FEN.
    pub fn to_char(self) -> char {
        match self {
            PieceKind::Pawn => 'P',
            PieceKind::Knight => 'N',
            PieceKind::Bishop => 'B',
            PieceKind::Rook => 'R',
            PieceKind::Queen => 'Q',
            PieceKind::King => 'K',
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Piece {
    pub color: Color,
    pub kind: PieceKind,
}

impl Piece {
    #[inline]
    pub fn new(color: Color, kind: PieceKind) -> Piece {
        Piece { color, kind }
    }

    /// FEN letter: upper-case for white, lower-case for black.
    pub fn to_char(self) -> char {
        let c = self.kind.to_char();
        match self.color {
            Color::White => c,
            Color::Black => c.to_ascii_lowercase(),
        }
    }

    pub fn from_char(c: char) -> Option<Piece> {
        let kind = PieceKind::from_char(c)?;
        let color = if c.is_ascii_uppercase() {
            Color::White
        } else {
            Color::Black
        };
        Some(Piece { color, kind })
    }

    #[inline]
    pub fn index(self) -> usize {
        self.color.index() * 6 + self.kind.index()
    }
}

// ---------------------------------------------------------------------------
// Squares (0x88 layout: index = rank * 16 + file, off-board iff index & 0x88)
// ---------------------------------------------------------------------------

pub type Square = u8;

pub const A1: Square = 0;
pub const E1: Square = 4;
pub const H1: Square = 7;
pub const A8: Square = 112;
pub const E8: Square = 116;
pub const H8: Square = 119;

#[inline]
pub fn sq(file: u8, rank: u8) -> Square {
    rank * 16 + file
}
#[inline]
pub fn file_of(s: Square) -> u8 {
    s & 7
}
#[inline]
pub fn rank_of(s: Square) -> u8 {
    s >> 4
}
#[inline]
pub fn on_board(s: i16) -> bool {
    s >= 0 && s < 128 && (s & 0x88) == 0
}
/// Compress a 0x88 square to 0..64 (rank * 8 + file), for tables.
#[inline]
pub fn sq64(s: Square) -> usize {
    (rank_of(s) * 8 + file_of(s)) as usize
}

pub fn square_name(s: Square) -> String {
    format!(
        "{}{}",
        (b'a' + file_of(s)) as char,
        (b'1' + rank_of(s)) as char
    )
}

pub fn parse_square(text: &str) -> Option<Square> {
    let b = text.as_bytes();
    if b.len() != 2 {
        return None;
    }
    let file = b[0].to_ascii_lowercase().checked_sub(b'a')?;
    let rank = b[1].checked_sub(b'1')?;
    if file > 7 || rank > 7 {
        return None;
    }
    Some(sq(file, rank))
}

/// Every on-board square, a1..h8.
pub fn all_squares() -> impl Iterator<Item = Square> {
    (0u8..128).filter(|s| (s & 0x88) == 0)
}

// ---------------------------------------------------------------------------
// Castling rights
// ---------------------------------------------------------------------------

pub const CASTLE_WK: u8 = 1;
pub const CASTLE_WQ: u8 = 2;
pub const CASTLE_BK: u8 = 4;
pub const CASTLE_BQ: u8 = 8;

/// Rights that survive any move touching `s` (as origin or destination).
fn castle_mask(s: Square) -> u8 {
    match s {
        A1 => !CASTLE_WQ,
        H1 => !CASTLE_WK,
        E1 => !(CASTLE_WK | CASTLE_WQ),
        A8 => !CASTLE_BQ,
        H8 => !CASTLE_BK,
        E8 => !(CASTLE_BK | CASTLE_BQ),
        _ => 0xFF,
    }
}

// ---------------------------------------------------------------------------
// Moves
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveKind {
    Normal,
    DoublePush,
    EnPassant,
    CastleKing,
    CastleQueen,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    pub from: Square,
    pub to: Square,
    pub promo: Option<PieceKind>,
    pub kind: MoveKind,
}

impl Move {
    pub fn normal(from: Square, to: Square) -> Move {
        Move { from, to, promo: None, kind: MoveKind::Normal }
    }

    /// Long algebraic / UCI form, e.g. `e2e4`, `e7e8q`.
    pub fn to_uci(self) -> String {
        let mut s = format!("{}{}", square_name(self.from), square_name(self.to));
        if let Some(p) = self.promo {
            s.push(p.to_char().to_ascii_lowercase());
        }
        s
    }
}

/// Everything needed to reverse a move.
#[derive(Clone, Copy)]
pub struct Undo {
    pub mv: Move,
    pub captured: Option<Piece>,
    pub captured_sq: Square,
    pub castling: u8,
    pub ep: Option<Square>,
    pub halfmove: u32,
    pub hash: u64,
}

// ---------------------------------------------------------------------------
// Zobrist keys (deterministic, generated once from a fixed seed)
// ---------------------------------------------------------------------------

struct Zobrist {
    pieces: [[u64; 128]; 12],
    side: u64,
    castling: [u64; 16],
    ep_file: [u64; 8],
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn zobrist() -> &'static Zobrist {
    static Z: OnceLock<Zobrist> = OnceLock::new();
    Z.get_or_init(|| {
        let mut state = 0x00C0_FFEE_1234_5678u64;
        let mut z = Zobrist {
            pieces: [[0; 128]; 12],
            side: 0,
            castling: [0; 16],
            ep_file: [0; 8],
        };
        for p in z.pieces.iter_mut() {
            for s in p.iter_mut() {
                *s = splitmix64(&mut state);
            }
        }
        z.side = splitmix64(&mut state);
        for c in z.castling.iter_mut() {
            *c = splitmix64(&mut state);
        }
        for f in z.ep_file.iter_mut() {
            *f = splitmix64(&mut state);
        }
        z
    })
}

// ---------------------------------------------------------------------------
// Position
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Position {
    pub squares: [Option<Piece>; 128],
    pub side: Color,
    pub castling: u8,
    /// Set whenever a pawn double-pushes, so FEN round-trips like other tools.
    pub ep: Option<Square>,
    pub halfmove: u32,
    pub fullmove: u32,
    /// Cached king squares, indexed by colour.
    pub king: [Square; 2],
    pub hash: u64,
}

pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

impl Position {
    pub fn empty() -> Position {
        Position {
            squares: [None; 128],
            side: Color::White,
            castling: 0,
            ep: None,
            halfmove: 0,
            fullmove: 1,
            king: [E1, E8],
            hash: 0,
        }
    }

    pub fn startpos() -> Position {
        Position::from_fen(START_FEN).expect("start position FEN is valid")
    }

    #[inline]
    pub fn at(&self, s: Square) -> Option<Piece> {
        self.squares[s as usize]
    }

    /// True when an enemy pawn is actually placed to take en passant. Only then
    /// does the ep square change the position's identity for repetition.
    fn ep_is_capturable(&self, ep: Square) -> bool {
        let mover = self.side;
        let dir: i16 = if mover == Color::White { 16 } else { -16 };
        for side_step in [-1i16, 1] {
            let from = ep as i16 - dir + side_step;
            if on_board(from) {
                if let Some(p) = self.squares[from as usize] {
                    if p.color == mover && p.kind == PieceKind::Pawn {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn compute_hash(&self) -> u64 {
        let z = zobrist();
        let mut h = 0u64;
        for s in all_squares() {
            if let Some(p) = self.at(s) {
                h ^= z.pieces[p.index()][s as usize];
            }
        }
        if self.side == Color::Black {
            h ^= z.side;
        }
        h ^= z.castling[(self.castling & 0xF) as usize];
        if let Some(ep) = self.ep {
            if self.ep_is_capturable(ep) {
                h ^= z.ep_file[file_of(ep) as usize];
            }
        }
        h
    }

    fn refresh_kings(&mut self) {
        for s in all_squares() {
            if let Some(p) = self.at(s) {
                if p.kind == PieceKind::King {
                    self.king[p.color.index()] = s;
                }
            }
        }
    }

    // -- FEN ----------------------------------------------------------------

    pub fn from_fen(fen: &str) -> Result<Position, String> {
        let mut pos = Position::empty();
        let fields: Vec<&str> = fen.split_whitespace().collect();
        if fields.len() < 4 {
            return Err("FEN needs at least 4 fields".to_string());
        }

        let mut rank: i32 = 7;
        let mut file: i32 = 0;
        for c in fields[0].chars() {
            match c {
                '/' => {
                    if file != 8 {
                        return Err(format!("rank {} has {} files, expected 8", rank + 1, file));
                    }
                    rank -= 1;
                    file = 0;
                    if rank < 0 {
                        return Err("too many ranks in FEN".to_string());
                    }
                }
                '1'..='8' => file += c as i32 - '0' as i32,
                _ => {
                    let piece = Piece::from_char(c).ok_or(format!("bad piece '{}' in FEN", c))?;
                    if file > 7 || rank < 0 {
                        return Err("FEN board overflows the 8x8 grid".to_string());
                    }
                    pos.squares[sq(file as u8, rank as u8) as usize] = Some(piece);
                    file += 1;
                }
            }
        }
        if rank != 0 || file != 8 {
            return Err("FEN board is not 8x8".to_string());
        }

        pos.side = match fields[1] {
            "w" | "W" => Color::White,
            "b" | "B" => Color::Black,
            other => return Err(format!("bad side to move '{}'", other)),
        };

        pos.castling = 0;
        if fields[2] != "-" {
            for c in fields[2].chars() {
                match c {
                    'K' => pos.castling |= CASTLE_WK,
                    'Q' => pos.castling |= CASTLE_WQ,
                    'k' => pos.castling |= CASTLE_BK,
                    'q' => pos.castling |= CASTLE_BQ,
                    _ => return Err(format!("bad castling flag '{}'", c)),
                }
            }
        }

        pos.ep = if fields[3] == "-" {
            None
        } else {
            Some(parse_square(fields[3]).ok_or(format!("bad en passant square '{}'", fields[3]))?)
        };

        pos.halfmove = fields.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
        pos.fullmove = fields.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);

        // A position without exactly one king each is not something the rest of
        // the engine can reason about, so reject it here rather than later.
        for color in [Color::White, Color::Black] {
            let kings = all_squares()
                .filter(|&s| pos.at(s) == Some(Piece::new(color, PieceKind::King)))
                .count();
            if kings != 1 {
                return Err(format!("{} must have exactly one king, found {}", color.name(), kings));
            }
        }

        pos.refresh_kings();
        pos.hash = pos.compute_hash();
        Ok(pos)
    }

    pub fn to_fen(&self) -> String {
        let mut out = String::new();
        for rank in (0..8u8).rev() {
            let mut run = 0;
            for file in 0..8u8 {
                match self.at(sq(file, rank)) {
                    Some(p) => {
                        if run > 0 {
                            out.push_str(&run.to_string());
                            run = 0;
                        }
                        out.push(p.to_char());
                    }
                    None => run += 1,
                }
            }
            if run > 0 {
                out.push_str(&run.to_string());
            }
            if rank > 0 {
                out.push('/');
            }
        }

        out.push(' ');
        out.push(if self.side == Color::White { 'w' } else { 'b' });

        out.push(' ');
        if self.castling == 0 {
            out.push('-');
        } else {
            for (flag, c) in [
                (CASTLE_WK, 'K'),
                (CASTLE_WQ, 'Q'),
                (CASTLE_BK, 'k'),
                (CASTLE_BQ, 'q'),
            ] {
                if self.castling & flag != 0 {
                    out.push(c);
                }
            }
        }

        out.push(' ');
        match self.ep {
            Some(s) => out.push_str(&square_name(s)),
            None => out.push('-'),
        }

        out.push_str(&format!(" {} {}", self.halfmove, self.fullmove));
        out
    }

    // -- make / unmake ------------------------------------------------------

    #[inline]
    fn put(&mut self, s: Square, piece: Piece, h: &mut u64) {
        let z = zobrist();
        self.squares[s as usize] = Some(piece);
        *h ^= z.pieces[piece.index()][s as usize];
    }

    #[inline]
    fn take(&mut self, s: Square, h: &mut u64) -> Option<Piece> {
        let z = zobrist();
        let piece = self.squares[s as usize].take();
        if let Some(p) = piece {
            *h ^= z.pieces[p.index()][s as usize];
        }
        piece
    }

    pub fn make_move(&mut self, mv: Move) -> Undo {
        let z = zobrist();
        let mover = self.side;
        let undo_hash = self.hash;
        let mut h = self.hash;

        // Drop the old ep key before the ep square changes.
        if let Some(ep) = self.ep {
            if self.ep_is_capturable(ep) {
                h ^= z.ep_file[file_of(ep) as usize];
            }
        }
        h ^= z.castling[(self.castling & 0xF) as usize];

        let undo = Undo {
            mv,
            captured: None,
            captured_sq: mv.to,
            castling: self.castling,
            ep: self.ep,
            halfmove: self.halfmove,
            hash: undo_hash,
        };

        let piece = self.squares[mv.from as usize].expect("move originates from an occupied square");

        // Locate the captured piece (en passant sits behind the target square).
        let captured_sq = if mv.kind == MoveKind::EnPassant {
            let back: i16 = if mover == Color::White { -16 } else { 16 };
            (mv.to as i16 + back) as Square
        } else {
            mv.to
        };
        let captured = self.take(captured_sq, &mut h);

        self.take(mv.from, &mut h);
        let landing = match mv.promo {
            Some(kind) => Piece::new(mover, kind),
            None => piece,
        };
        self.put(mv.to, landing, &mut h);

        // Castling drags the rook along with the king.
        match mv.kind {
            MoveKind::CastleKing => {
                let rook_from = sq(7, rank_of(mv.from));
                let rook_to = sq(5, rank_of(mv.from));
                let rook = self.take(rook_from, &mut h).expect("king-side rook present");
                self.put(rook_to, rook, &mut h);
            }
            MoveKind::CastleQueen => {
                let rook_from = sq(0, rank_of(mv.from));
                let rook_to = sq(3, rank_of(mv.from));
                let rook = self.take(rook_from, &mut h).expect("queen-side rook present");
                self.put(rook_to, rook, &mut h);
            }
            _ => {}
        }

        if piece.kind == PieceKind::King {
            self.king[mover.index()] = mv.to;
        }

        self.castling &= castle_mask(mv.from) & castle_mask(mv.to);
        h ^= z.castling[(self.castling & 0xF) as usize];

        self.ep = if mv.kind == MoveKind::DoublePush {
            let step: i16 = if mover == Color::White { 16 } else { -16 };
            Some((mv.from as i16 + step) as Square)
        } else {
            None
        };

        if piece.kind == PieceKind::Pawn || captured.is_some() {
            self.halfmove = 0;
        } else {
            self.halfmove += 1;
        }
        if mover == Color::Black {
            self.fullmove += 1;
        }

        self.side = mover.flip();
        h ^= z.side;

        // The new ep key depends on the side now to move, so add it last.
        if let Some(ep) = self.ep {
            if self.ep_is_capturable(ep) {
                h ^= z.ep_file[file_of(ep) as usize];
            }
        }
        self.hash = h;

        Undo { captured, captured_sq, ..undo }
    }

    pub fn unmake_move(&mut self, undo: Undo) {
        let mv = undo.mv;
        let mover = self.side.flip();

        self.side = mover;
        self.castling = undo.castling;
        self.ep = undo.ep;
        self.halfmove = undo.halfmove;
        self.hash = undo.hash;
        if mover == Color::Black {
            self.fullmove -= 1;
        }

        let landed = self.squares[mv.to as usize].expect("moved piece is on its destination");
        self.squares[mv.to as usize] = None;
        let original = if mv.promo.is_some() {
            Piece::new(mover, PieceKind::Pawn)
        } else {
            landed
        };
        self.squares[mv.from as usize] = Some(original);

        if let Some(cap) = undo.captured {
            self.squares[undo.captured_sq as usize] = Some(cap);
        }

        match mv.kind {
            MoveKind::CastleKing => {
                let rook_from = sq(7, rank_of(mv.from));
                let rook_to = sq(5, rank_of(mv.from));
                self.squares[rook_from as usize] = self.squares[rook_to as usize].take();
            }
            MoveKind::CastleQueen => {
                let rook_from = sq(0, rank_of(mv.from));
                let rook_to = sq(3, rank_of(mv.from));
                self.squares[rook_from as usize] = self.squares[rook_to as usize].take();
            }
            _ => {}
        }

        if original.kind == PieceKind::King {
            self.king[mover.index()] = mv.from;
        }
    }

    /// Pass the turn without moving. Used only by the search's null-move pruning.
    pub fn make_null(&mut self) -> Undo {
        let z = zobrist();
        let undo = Undo {
            mv: Move::normal(0, 0),
            captured: None,
            captured_sq: 0,
            castling: self.castling,
            ep: self.ep,
            halfmove: self.halfmove,
            hash: self.hash,
        };
        if let Some(ep) = self.ep {
            if self.ep_is_capturable(ep) {
                self.hash ^= z.ep_file[file_of(ep) as usize];
            }
        }
        self.ep = None;
        self.side = self.side.flip();
        self.hash ^= z.side;
        self.halfmove += 1;
        undo
    }

    pub fn unmake_null(&mut self, undo: Undo) {
        self.side = self.side.flip();
        self.ep = undo.ep;
        self.castling = undo.castling;
        self.halfmove = undo.halfmove;
        self.hash = undo.hash;
    }

    /// Material counts by kind for one side, plus the squares of its bishops.
    pub fn count_material(&self, color: Color) -> ([u32; 6], Vec<Square>) {
        let mut counts = [0u32; 6];
        let mut bishops = Vec::new();
        for s in all_squares() {
            if let Some(p) = self.at(s) {
                if p.color == color {
                    counts[p.kind.index()] += 1;
                    if p.kind == PieceKind::Bishop {
                        bishops.push(s);
                    }
                }
            }
        }
        (counts, bishops)
    }
}
