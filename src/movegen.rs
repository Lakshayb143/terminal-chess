//! Attack detection and legal move generation.

use crate::board::*;

const KNIGHT_STEPS: [i16; 8] = [33, 31, 18, 14, -33, -31, -18, -14];
const BISHOP_STEPS: [i16; 4] = [17, 15, -15, -17];
const ROOK_STEPS: [i16; 4] = [16, 1, -1, -16];
const KING_STEPS: [i16; 8] = [17, 16, 15, 1, -1, -15, -16, -17];

/// Is `target` attacked by any piece of `by`? Used for check and castling tests.
pub fn is_attacked(pos: &Position, target: Square, by: Color) -> bool {
    // Pawns: step backwards from the target along the attacker's capture rays.
    let pawn_back: i16 = if by == Color::White { -16 } else { 16 };
    for side_step in [-1i16, 1] {
        let from = target as i16 + pawn_back + side_step;
        if on_board(from) {
            if let Some(p) = pos.squares[from as usize] {
                if p.color == by && p.kind == PieceKind::Pawn {
                    return true;
                }
            }
        }
    }

    for step in KNIGHT_STEPS {
        let from = target as i16 + step;
        if on_board(from) {
            if let Some(p) = pos.squares[from as usize] {
                if p.color == by && p.kind == PieceKind::Knight {
                    return true;
                }
            }
        }
    }

    for step in KING_STEPS {
        let from = target as i16 + step;
        if on_board(from) {
            if let Some(p) = pos.squares[from as usize] {
                if p.color == by && p.kind == PieceKind::King {
                    return true;
                }
            }
        }
    }

    for (steps, slider) in [
        (&BISHOP_STEPS[..], PieceKind::Bishop),
        (&ROOK_STEPS[..], PieceKind::Rook),
    ] {
        for &step in steps {
            let mut s = target as i16 + step;
            while on_board(s) {
                if let Some(p) = pos.squares[s as usize] {
                    if p.color == by && (p.kind == slider || p.kind == PieceKind::Queen) {
                        return true;
                    }
                    break;
                }
                s += step;
            }
        }
    }

    false
}

pub fn in_check(pos: &Position, color: Color) -> bool {
    is_attacked(pos, pos.king[color.index()], color.flip())
}

fn push_pawn_move(out: &mut Vec<Move>, from: Square, to: Square, kind: MoveKind) {
    let last_rank = rank_of(to) == 7 || rank_of(to) == 0;
    if last_rank {
        for promo in [
            PieceKind::Queen,
            PieceKind::Rook,
            PieceKind::Bishop,
            PieceKind::Knight,
        ] {
            out.push(Move {
                from,
                to,
                promo: Some(promo),
                kind,
            });
        }
    } else {
        out.push(Move {
            from,
            to,
            promo: None,
            kind,
        });
    }
}

/// Moves that follow the movement rules but may leave the king in check.
pub fn generate_pseudo_legal(pos: &Position, captures_only: bool) -> Vec<Move> {
    let us = pos.side;
    let mut out: Vec<Move> = Vec::with_capacity(48);

    for from in all_squares() {
        let piece = match pos.at(from) {
            Some(p) if p.color == us => p,
            _ => continue,
        };

        match piece.kind {
            PieceKind::Pawn => {
                let step: i16 = if us == Color::White { 16 } else { -16 };
                let start_rank = if us == Color::White { 1 } else { 6 };

                if !captures_only {
                    let one = from as i16 + step;
                    if on_board(one) && pos.squares[one as usize].is_none() {
                        push_pawn_move(&mut out, from, one as Square, MoveKind::Normal);
                        let two = from as i16 + 2 * step;
                        if rank_of(from) == start_rank
                            && on_board(two)
                            && pos.squares[two as usize].is_none()
                        {
                            out.push(Move {
                                from,
                                to: two as Square,
                                promo: None,
                                kind: MoveKind::DoublePush,
                            });
                        }
                    }
                }

                for side_step in [-1i16, 1] {
                    let to = from as i16 + step + side_step;
                    if !on_board(to) {
                        continue;
                    }
                    let to_sq = to as Square;
                    match pos.squares[to as usize] {
                        Some(p) if p.color != us => {
                            push_pawn_move(&mut out, from, to_sq, MoveKind::Normal)
                        }
                        None if pos.ep == Some(to_sq) => out.push(Move {
                            from,
                            to: to_sq,
                            promo: None,
                            kind: MoveKind::EnPassant,
                        }),
                        _ => {}
                    }
                }
            }

            PieceKind::Knight | PieceKind::King => {
                let steps: &[i16] = if piece.kind == PieceKind::Knight {
                    &KNIGHT_STEPS
                } else {
                    &KING_STEPS
                };
                for &step in steps {
                    let to = from as i16 + step;
                    if !on_board(to) {
                        continue;
                    }
                    match pos.squares[to as usize] {
                        Some(p) if p.color == us => {}
                        Some(_) => out.push(Move::normal(from, to as Square)),
                        None if !captures_only => out.push(Move::normal(from, to as Square)),
                        None => {}
                    }
                }
            }

            PieceKind::Bishop | PieceKind::Rook | PieceKind::Queen => {
                let steps: &[i16] = match piece.kind {
                    PieceKind::Bishop => &BISHOP_STEPS,
                    PieceKind::Rook => &ROOK_STEPS,
                    _ => &KING_STEPS, // queen = both ray sets
                };
                for &step in steps {
                    let mut to = from as i16 + step;
                    while on_board(to) {
                        match pos.squares[to as usize] {
                            Some(p) => {
                                if p.color != us {
                                    out.push(Move::normal(from, to as Square));
                                }
                                break;
                            }
                            None => {
                                if !captures_only {
                                    out.push(Move::normal(from, to as Square));
                                }
                                to += step;
                            }
                        }
                    }
                }
            }
        }
    }

    if !captures_only {
        generate_castles(pos, &mut out);
    }
    out
}

fn generate_castles(pos: &Position, out: &mut Vec<Move>) {
    let us = pos.side;
    let them = us.flip();
    let home = if us == Color::White { 0 } else { 7 };
    let king_sq = sq(4, home);

    // Rights alone are not enough: the king must actually be home, and the
    // squares it crosses must be empty and unattacked.
    if pos.at(king_sq) != Some(Piece::new(us, PieceKind::King)) {
        return;
    }
    if is_attacked(pos, king_sq, them) {
        return;
    }

    let (king_flag, queen_flag) = if us == Color::White {
        (CASTLE_WK, CASTLE_WQ)
    } else {
        (CASTLE_BK, CASTLE_BQ)
    };

    if pos.castling & king_flag != 0
        && pos.at(sq(7, home)) == Some(Piece::new(us, PieceKind::Rook))
        && pos.at(sq(5, home)).is_none()
        && pos.at(sq(6, home)).is_none()
        && !is_attacked(pos, sq(5, home), them)
        && !is_attacked(pos, sq(6, home), them)
    {
        out.push(Move {
            from: king_sq,
            to: sq(6, home),
            promo: None,
            kind: MoveKind::CastleKing,
        });
    }

    if pos.castling & queen_flag != 0
        && pos.at(sq(0, home)) == Some(Piece::new(us, PieceKind::Rook))
        && pos.at(sq(1, home)).is_none()
        && pos.at(sq(2, home)).is_none()
        && pos.at(sq(3, home)).is_none()
        && !is_attacked(pos, sq(3, home), them)
        && !is_attacked(pos, sq(2, home), them)
    {
        out.push(Move {
            from: king_sq,
            to: sq(2, home),
            promo: None,
            kind: MoveKind::CastleQueen,
        });
    }
}

/// Fully legal moves: pseudo-legal moves that do not leave our king attacked.
pub fn generate_legal(pos: &Position) -> Vec<Move> {
    filter_legal(pos, generate_pseudo_legal(pos, false))
}

pub fn generate_legal_captures(pos: &Position) -> Vec<Move> {
    filter_legal(pos, generate_pseudo_legal(pos, true))
}

fn filter_legal(pos: &Position, moves: Vec<Move>) -> Vec<Move> {
    let us = pos.side;
    let mut work = pos.clone();
    moves
        .into_iter()
        .filter(|&mv| {
            let undo = work.make_move(mv);
            let ok = !in_check(&work, us);
            work.unmake_move(undo);
            ok
        })
        .collect()
}

/// Node count of the move tree to `depth`. The standard correctness test.
pub fn perft(pos: &mut Position, depth: u32) -> u64 {
    if depth == 0 {
        return 1;
    }
    let moves = generate_legal(pos);
    if depth == 1 {
        return moves.len() as u64;
    }
    let mut nodes = 0;
    for mv in moves {
        let undo = pos.make_move(mv);
        nodes += perft(pos, depth - 1);
        pos.unmake_move(undo);
    }
    nodes
}
