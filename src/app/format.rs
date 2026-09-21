//! Text formatting shared by the board panel, pages, and engine output.

use chess_core::board::{Color, Move, PieceKind, Position};
use chess_core::movegen::generate_legal;
use chess_core::san::to_san;
use chess_core::search::Limits;
use chess_core::{board, search};

pub(crate) fn kind_name(kind: PieceKind) -> &'static str {
    match kind {
        PieceKind::Pawn => "pawn",
        PieceKind::Knight => "knight",
        PieceKind::Bishop => "bishop",
        PieceKind::Rook => "rook",
        PieceKind::Queen => "queen",
        PieceKind::King => "king",
    }
}

/// Piece worth in pawns, for the material count beside the board.
pub(crate) fn pawns_worth(kind: PieceKind) -> i32 {
    [1, 3, 3, 5, 9, 0][kind.index()]
}

pub(crate) fn material(pos: &Position, color: Color) -> i32 {
    board::all_squares()
        .filter_map(|s| pos.at(s))
        .filter(|piece| piece.color == color)
        .map(|piece| pawns_worth(piece.kind))
        .sum()
}

/// Everyone else a lobby count covers: "1 other" or "3 others".
pub(crate) fn others_online(others: u32) -> String {
    match others {
        1 => "1 other".to_string(),
        others => format!("{others} others"),
    }
}

/// How long the engine gets, in the words used by the flags that set it.
pub(crate) fn budget_text(limits: &Limits) -> String {
    match limits.movetime {
        Some(budget) => {
            let secs = budget.as_secs_f64();
            if secs >= 1.0 {
                format!("{}s a move", trim_number(secs))
            } else {
                format!("{}ms a move", (secs * 1000.0).round() as u64)
            }
        }
        None => format!("depth {}", limits.depth),
    }
}

pub(crate) fn trim_number(value: f64) -> String {
    let text = format!("{:.1}", value);
    text.trim_end_matches(".0").to_string()
}

/// Centipawns as pawns, or the distance to mate, always from White's side of
/// the board - the one point of view that does not move around during a game.
pub(crate) fn format_score(white_pov: i32) -> String {
    if white_pov.abs() >= search::MATE_THRESHOLD {
        let plies = search::MATE - white_pov.abs();
        let moves = (plies + 1) / 2;
        let winner = if white_pov > 0 { "White" } else { "Black" };
        format!("{} mates in {}", winner, moves.max(1))
    } else {
        format!("{:+.2}", white_pov as f64 / 100.0)
    }
}

/// The engine scores for the side it is moving for; the board is White's.
pub(crate) fn white_pov(pos: &Position, score: i32) -> i32 {
    if pos.side == Color::White {
        score
    } else {
        -score
    }
}

pub(crate) fn node_count(nodes: u64) -> String {
    match nodes {
        0..=9_999 => nodes.to_string(),
        10_000..=999_999 => format!("{:.0}k", nodes as f64 / 1_000.0),
        _ => format!("{:.2}M", nodes as f64 / 1_000_000.0),
    }
}

/// Render a line of play as SAN, stopping if it runs past what is legal.
pub(crate) fn pv_text(pos: &Position, pv: &[Move], most: usize) -> String {
    let mut work = pos.clone();
    let mut parts = Vec::new();
    let mut undos = Vec::new();
    for &mv in pv.iter().take(most) {
        // A truncated line can outlive its position; stop rather than guess.
        if !generate_legal(&work).contains(&mv) {
            break;
        }
        parts.push(to_san(&work, mv));
        undos.push(work.make_move(mv));
    }
    while let Some(undo) = undos.pop() {
        work.unmake_move(undo);
    }
    parts.join(" ")
}

/// Fold a long list of words into indented lines.
pub(crate) fn wrap(words: &[String], width: usize, indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in words {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(format!("{}{}", indent, line));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(format!("{}{}", indent, line));
    }
    lines
}
