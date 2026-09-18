//! Standard Algebraic Notation: rendering moves, and parsing what people type.

use crate::board::*;
use crate::movegen::{generate_legal, in_check};

/// Render `mv` as SAN in `pos`, including the `+` / `#` suffix.
pub fn to_san(pos: &Position, mv: Move) -> String {
    let legal = generate_legal(pos);
    to_san_with(pos, mv, &legal)
}

/// Same, when the caller already has the legal move list.
pub fn to_san_with(pos: &Position, mv: Move, legal: &[Move]) -> String {
    let mut s = match mv.kind {
        MoveKind::CastleKing => "O-O".to_string(),
        MoveKind::CastleQueen => "O-O-O".to_string(),
        _ => {
            let piece = match pos.at(mv.from) {
                Some(p) => p,
                None => return mv.to_uci(),
            };
            let is_capture = pos.at(mv.to).is_some() || mv.kind == MoveKind::EnPassant;
            let mut s = String::new();

            if piece.kind == PieceKind::Pawn {
                if is_capture {
                    s.push((b'a' + file_of(mv.from)) as char);
                    s.push('x');
                }
                s.push_str(&square_name(mv.to));
                if let Some(promo) = mv.promo {
                    s.push('=');
                    s.push(promo.to_char());
                }
            } else {
                s.push(piece.kind.to_char());
                s.push_str(&disambiguation(pos, mv, piece, legal));
                if is_capture {
                    s.push('x');
                }
                s.push_str(&square_name(mv.to));
            }
            s
        }
    };

    let mut after = pos.clone();
    let undo = after.make_move(mv);
    if in_check(&after, after.side) {
        s.push(if generate_legal(&after).is_empty() {
            '#'
        } else {
            '+'
        });
    }
    after.unmake_move(undo);
    s
}

/// The minimum file/rank hint that separates `mv` from its twins.
fn disambiguation(pos: &Position, mv: Move, piece: Piece, legal: &[Move]) -> String {
    let rivals: Vec<Move> = legal
        .iter()
        .copied()
        .filter(|&other| {
            other.to == mv.to
                && other.from != mv.from
                && pos.at(other.from).map(|p| p.kind) == Some(piece.kind)
                && pos.at(other.from).map(|p| p.color) == Some(piece.color)
        })
        .collect();

    if rivals.is_empty() {
        return String::new();
    }
    if rivals.iter().all(|r| file_of(r.from) != file_of(mv.from)) {
        return ((b'a' + file_of(mv.from)) as char).to_string();
    }
    if rivals.iter().all(|r| rank_of(r.from) != rank_of(mv.from)) {
        return ((b'1' + rank_of(mv.from)) as char).to_string();
    }
    square_name(mv.from)
}

/// Strip decoration that carries no meaning for matching.
fn tidy(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '+' | '#' | '!' | '?' | ' ' | '\t'))
        .collect()
}

/// Also drop capture/promotion/castle punctuation, for sloppy input.
fn loose(text: &str) -> String {
    tidy(text)
        .chars()
        .filter(|c| !matches!(c, 'x' | 'X' | '=' | '-' | '0'))
        .collect()
}

pub enum ParseError {
    Illegal(String),
    Ambiguous(String, Vec<String>),
}

/// Parse a move the user typed. Accepts SAN (`Nf3`, `exd5`, `O-O`, `e8=Q`),
/// coordinate/UCI form (`e2e4`, `e7e8q`), and common sloppy spellings.
pub fn parse_move(pos: &Position, input: &str) -> Result<Move, ParseError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(ParseError::Illegal(raw.to_string()));
    }

    let legal = generate_legal(pos);
    let rendered: Vec<(String, Move)> = legal
        .iter()
        .map(|&mv| (to_san_with(pos, mv, &legal), mv))
        .collect();

    let want = tidy(raw);
    // Castling is written half a dozen ways; normalise the zero spellings.
    let want = match want.replace('0', "O").as_str() {
        "O-O" | "OO" => "O-O".to_string(),
        "O-O-O" | "OOO" => "O-O-O".to_string(),
        _ => want,
    };

    // 1. Exact SAN.
    let hits: Vec<Move> = rendered
        .iter()
        .filter(|(san, _)| tidy(san) == want)
        .map(|(_, mv)| *mv)
        .collect();
    if let Some(mv) = unique(&hits) {
        return Ok(mv);
    }

    // 2. Coordinate / UCI form.
    if let Some(mv) = parse_coordinate(&want, &legal) {
        return Ok(mv);
    }

    // 3. Same, ignoring case (`nf3`, `E4`).
    let hits: Vec<Move> = rendered
        .iter()
        .filter(|(san, _)| tidy(san).eq_ignore_ascii_case(&want))
        .map(|(_, mv)| *mv)
        .collect();
    match unique(&hits) {
        Some(mv) => return Ok(mv),
        None if hits.len() > 1 => return Err(ambiguity(raw, &hits, pos, &legal)),
        None => {}
    }

    // 4. Punctuation-insensitive (`ed5`, `e8Q`, `Ne1f3`).
    for case_sensitive in [true, false] {
        let hits: Vec<Move> = rendered
            .iter()
            .filter(|(san, _)| {
                let a = loose(san);
                let b = loose(&want);
                if case_sensitive {
                    a == b
                } else {
                    a.eq_ignore_ascii_case(&b)
                }
            })
            .map(|(_, mv)| *mv)
            .collect();
        match unique(&hits) {
            Some(mv) => return Ok(mv),
            None if hits.len() > 1 => return Err(ambiguity(raw, &hits, pos, &legal)),
            None => {}
        }
    }

    Err(ParseError::Illegal(raw.to_string()))
}

fn unique(hits: &[Move]) -> Option<Move> {
    if hits.len() == 1 {
        Some(hits[0])
    } else {
        None
    }
}

fn ambiguity(raw: &str, hits: &[Move], pos: &Position, legal: &[Move]) -> ParseError {
    ParseError::Ambiguous(
        raw.to_string(),
        hits.iter().map(|&mv| to_san_with(pos, mv, legal)).collect(),
    )
}

fn parse_coordinate(text: &str, legal: &[Move]) -> Option<Move> {
    let cleaned: String = text.chars().filter(|&c| c != '-').collect();
    let bytes = cleaned.as_bytes();
    if bytes.len() < 4 || bytes.len() > 5 {
        return None;
    }
    let from = parse_square(&cleaned[0..2])?;
    let to = parse_square(&cleaned[2..4])?;
    let promo = if bytes.len() == 5 {
        Some(PieceKind::from_char(cleaned.as_bytes()[4] as char)?)
    } else {
        None
    };

    let candidates: Vec<Move> = legal
        .iter()
        .copied()
        .filter(|mv| mv.from == from && mv.to == to)
        .collect();
    match promo {
        // A coordinate move onto the last rank without a promotion letter is
        // taken as a queen, which is what nearly everyone means.
        None => candidates
            .iter()
            .find(|mv| mv.promo.is_none())
            .or_else(|| {
                candidates
                    .iter()
                    .find(|mv| mv.promo == Some(PieceKind::Queen))
            })
            .copied(),
        Some(kind) => candidates.iter().find(|mv| mv.promo == Some(kind)).copied(),
    }
}
