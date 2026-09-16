//! Static evaluation: tapered material + piece-square tables, plus a handful
//! of positional terms. Scores are in centipawns from the mover's point of view.

use crate::board::*;

pub const MG_VALUE: [i32; 6] = [100, 320, 330, 500, 900, 0];
pub const EG_VALUE: [i32; 6] = [120, 320, 330, 530, 950, 0];

/// Rough worth of a piece, for move ordering and material counting.
pub fn piece_value(kind: PieceKind) -> i32 {
    MG_VALUE[kind.index()]
}

// Tables are written from White's view with rank 8 on the first row.
#[rustfmt::skip]
const PAWN_MG: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
    50, 50, 50, 50, 50, 50, 50, 50,
    10, 10, 20, 30, 30, 20, 10, 10,
     5,  5, 10, 25, 25, 10,  5,  5,
     0,  0,  0, 20, 20,  0,  0,  0,
     5, -5,-10,  0,  0,-10, -5,  5,
     5, 10, 10,-20,-20, 10, 10,  5,
     0,  0,  0,  0,  0,  0,  0,  0,
];

#[rustfmt::skip]
const PAWN_EG: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
    90, 90, 90, 90, 90, 90, 90, 90,
    55, 55, 55, 55, 55, 55, 55, 55,
    30, 30, 30, 30, 30, 30, 30, 30,
    18, 18, 18, 18, 18, 18, 18, 18,
     8,  8,  8,  8,  8,  8,  8,  8,
     4,  4,  4,  4,  4,  4,  4,  4,
     0,  0,  0,  0,  0,  0,  0,  0,
];

#[rustfmt::skip]
const KNIGHT: [i32; 64] = [
   -50,-40,-30,-30,-30,-30,-40,-50,
   -40,-20,  0,  0,  0,  0,-20,-40,
   -30,  0, 10, 15, 15, 10,  0,-30,
   -30,  5, 15, 20, 20, 15,  5,-30,
   -30,  0, 15, 20, 20, 15,  0,-30,
   -30,  5, 10, 15, 15, 10,  5,-30,
   -40,-20,  0,  5,  5,  0,-20,-40,
   -50,-40,-30,-30,-30,-30,-40,-50,
];

#[rustfmt::skip]
const BISHOP: [i32; 64] = [
   -20,-10,-10,-10,-10,-10,-10,-20,
   -10,  0,  0,  0,  0,  0,  0,-10,
   -10,  0,  5, 10, 10,  5,  0,-10,
   -10,  5,  5, 10, 10,  5,  5,-10,
   -10,  0, 10, 10, 10, 10,  0,-10,
   -10, 10, 10, 10, 10, 10, 10,-10,
   -10,  5,  0,  0,  0,  0,  5,-10,
   -20,-10,-10,-10,-10,-10,-10,-20,
];

#[rustfmt::skip]
const ROOK: [i32; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
     5, 10, 10, 10, 10, 10, 10,  5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
     0,  0,  0,  5,  5,  0,  0,  0,
];

#[rustfmt::skip]
const QUEEN: [i32; 64] = [
   -20,-10,-10, -5, -5,-10,-10,-20,
   -10,  0,  0,  0,  0,  0,  0,-10,
   -10,  0,  5,  5,  5,  5,  0,-10,
    -5,  0,  5,  5,  5,  5,  0, -5,
     0,  0,  5,  5,  5,  5,  0, -5,
   -10,  5,  5,  5,  5,  5,  0,-10,
   -10,  0,  5,  0,  0,  0,  0,-10,
   -20,-10,-10, -5, -5,-10,-10,-20,
];

#[rustfmt::skip]
const KING_MG: [i32; 64] = [
   -30,-40,-40,-50,-50,-40,-40,-30,
   -30,-40,-40,-50,-50,-40,-40,-30,
   -30,-40,-40,-50,-50,-40,-40,-30,
   -30,-40,-40,-50,-50,-40,-40,-30,
   -20,-30,-30,-40,-40,-30,-30,-20,
   -10,-20,-20,-20,-20,-20,-20,-10,
    20, 20,  0,  0,  0,  0, 20, 20,
    20, 30, 10,  0,  0, 10, 30, 20,
];

#[rustfmt::skip]
const KING_EG: [i32; 64] = [
   -50,-40,-30,-20,-20,-30,-40,-50,
   -30,-20,-10,  0,  0,-10,-20,-30,
   -30,-10, 20, 30, 30, 20,-10,-30,
   -30,-10, 30, 40, 40, 30,-10,-30,
   -30,-10, 30, 40, 40, 30,-10,-30,
   -30,-10, 20, 30, 30, 20,-10,-30,
   -30,-30,  0,  0,  0,  0,-30,-30,
   -50,-30,-30,-30,-30,-30,-30,-50,
];

const PASSED_PAWN: [i32; 8] = [0, 8, 14, 26, 45, 75, 120, 0];
const BISHOP_PAIR: i32 = 30;
const DOUBLED_PAWN: i32 = -12;
const ISOLATED_PAWN: i32 = -15;
const ROOK_OPEN_FILE: i32 = 18;
const ROOK_SEMI_OPEN: i32 = 9;
const ROOK_ON_SEVENTH: i32 = 22;
const KING_SHIELD: i32 = 9;
const TEMPO: i32 = 8;

/// Index into a White-oriented table, mirroring the rank for Black.
#[inline]
fn table_index(s: Square, color: Color) -> usize {
    let (file, rank) = (file_of(s) as usize, rank_of(s) as usize);
    match color {
        Color::White => (7 - rank) * 8 + file,
        Color::Black => rank * 8 + file,
    }
}

#[inline]
fn table_for(kind: PieceKind, endgame: bool) -> &'static [i32; 64] {
    match kind {
        PieceKind::Pawn => if endgame { &PAWN_EG } else { &PAWN_MG },
        PieceKind::Knight => &KNIGHT,
        PieceKind::Bishop => &BISHOP,
        PieceKind::Rook => &ROOK,
        PieceKind::Queen => &QUEEN,
        PieceKind::King => if endgame { &KING_EG } else { &KING_MG },
    }
}

/// 24 in the opening, 0 with only kings and pawns left.
fn phase_of(counts: &[[u32; 6]; 2]) -> i32 {
    let mut phase = 0i32;
    for c in 0..2 {
        phase += counts[c][PieceKind::Knight.index()] as i32;
        phase += counts[c][PieceKind::Bishop.index()] as i32;
        phase += counts[c][PieceKind::Rook.index()] as i32 * 2;
        phase += counts[c][PieceKind::Queen.index()] as i32 * 4;
    }
    phase.min(24)
}

pub fn evaluate(pos: &Position) -> i32 {
    let mut mg = 0i32;
    let mut eg = 0i32;
    let mut counts = [[0u32; 6]; 2];
    // Pawn squares per colour, and a per-file count for structure terms.
    let mut pawns: [Vec<Square>; 2] = [Vec::with_capacity(8), Vec::with_capacity(8)];
    let mut pawn_files = [[0u8; 8]; 2];

    for s in all_squares() {
        if let Some(p) = pos.at(s) {
            counts[p.color.index()][p.kind.index()] += 1;
            if p.kind == PieceKind::Pawn {
                pawns[p.color.index()].push(s);
                pawn_files[p.color.index()][file_of(s) as usize] += 1;
            }
        }
    }

    let phase = phase_of(&counts);

    for s in all_squares() {
        let p = match pos.at(s) {
            Some(p) => p,
            None => continue,
        };
        let sign = if p.color == Color::White { 1 } else { -1 };
        let idx = table_index(s, p.color);
        mg += sign * (MG_VALUE[p.kind.index()] + table_for(p.kind, false)[idx]);
        eg += sign * (EG_VALUE[p.kind.index()] + table_for(p.kind, true)[idx]);
    }

    for color in [Color::White, Color::Black] {
        let ci = color.index();
        let them = color.flip().index();
        let sign = if color == Color::White { 1 } else { -1 };
        let mut score_mg = 0i32;
        let mut score_eg = 0i32;

        if counts[ci][PieceKind::Bishop.index()] >= 2 {
            score_mg += BISHOP_PAIR;
            score_eg += BISHOP_PAIR + 10;
        }

        for file in 0..8usize {
            let n = pawn_files[ci][file];
            if n > 1 {
                score_mg += DOUBLED_PAWN * (n as i32 - 1);
                score_eg += DOUBLED_PAWN * (n as i32 - 1);
            }
            if n > 0 {
                let left = file > 0 && pawn_files[ci][file - 1] > 0;
                let right = file < 7 && pawn_files[ci][file + 1] > 0;
                if !left && !right {
                    score_mg += ISOLATED_PAWN;
                    score_eg += ISOLATED_PAWN;
                }
            }
        }

        for &ps in &pawns[ci] {
            let file = file_of(ps) as i32;
            let rank = rank_of(ps) as i32;
            // Passed: no enemy pawn ahead on this or an adjacent file.
            let blocked = pawns[them].iter().any(|&es| {
                let ef = file_of(es) as i32;
                let er = rank_of(es) as i32;
                (ef - file).abs() <= 1
                    && if color == Color::White { er > rank } else { er < rank }
            });
            if !blocked {
                let advance = if color == Color::White { rank } else { 7 - rank } as usize;
                score_mg += PASSED_PAWN[advance] / 2;
                score_eg += PASSED_PAWN[advance];
            }
        }

        for s in all_squares() {
            if pos.at(s) != Some(Piece::new(color, PieceKind::Rook)) {
                continue;
            }
            let file = file_of(s) as usize;
            if pawn_files[ci][file] == 0 {
                if pawn_files[them][file] == 0 {
                    score_mg += ROOK_OPEN_FILE;
                    score_eg += ROOK_OPEN_FILE / 2;
                } else {
                    score_mg += ROOK_SEMI_OPEN;
                }
            }
            let seventh = if color == Color::White { 6 } else { 1 };
            if rank_of(s) == seventh {
                score_mg += ROOK_ON_SEVENTH;
                score_eg += ROOK_ON_SEVENTH / 2;
            }
        }

        // Pawns standing in front of a castled king are worth something while
        // queens and rooks are still around.
        let ksq = pos.king[ci];
        let forward: i16 = if color == Color::White { 16 } else { -16 };
        for side_step in [-1i16, 0, 1] {
            let front = ksq as i16 + forward + side_step;
            if on_board(front) && pos.squares[front as usize] == Some(Piece::new(color, PieceKind::Pawn)) {
                score_mg += KING_SHIELD;
            }
        }

        mg += sign * score_mg;
        eg += sign * score_eg;
    }

    let blended = (mg * phase + eg * (24 - phase)) / 24;
    let white_pov = blended;
    let score = if pos.side == Color::White { white_pov } else { -white_pov };
    score + TEMPO
}
