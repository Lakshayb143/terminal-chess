//! Understanding what was typed: move suggestions and command guesses.

use chess_core::board::{self, Move, PieceKind, Position};
use chess_core::movegen::generate_legal;
use chess_core::san::to_san_with;

/// Legal moves that `input` names apart from the file or rank hint SAN adds
/// when two pieces can reach the same square. `parse_move` turns an
/// under-specified move down flat, and "not legal" is the wrong thing to say
/// about a move that is legal twice over; these are the moves to offer instead.
pub(crate) fn under_specified(pos: &Position, input: &str) -> Vec<String> {
    let legal = generate_legal(pos);
    let want = bare_form(input);
    legal
        .iter()
        .filter(|&&mv| {
            let piece = match pos.at(mv.from) {
                // Pawn moves name their file rather than the piece, so they
                // are never ambiguous in this way.
                Some(piece) if piece.kind != PieceKind::Pawn => piece,
                _ => return false,
            };
            let mut bare = String::new();
            bare.push(piece.kind.to_char());
            bare.push_str(&board::square_name(mv.to));
            bare_form(&bare) == want
        })
        .map(|&mv| to_san_with(pos, mv, &legal))
        .collect()
}

/// A pawn move onto the last rank that did not say what to promote to. Nearly
/// everyone means a queen, which is also what the coordinate form already does.
pub(crate) fn promotion_default(pos: &Position, input: &str) -> Option<Move> {
    let legal = generate_legal(pos);
    let want = bare_form(input);
    legal.iter().copied().find(|&mv| {
        mv.promo == Some(PieceKind::Queen) && {
            let san = to_san_with(pos, mv, &legal);
            // `e8=Q` named as `e8`, `dxe8=Q` named as `dxe8`.
            bare_form(san.split('=').next().unwrap_or("")) == want
        }
    })
}

/// Legal moves within a typo of what was typed.
pub(crate) fn nearby_moves(pos: &Position, input: &str) -> Vec<String> {
    let legal = generate_legal(pos);
    let want = bare_form(input);
    let mut scored: Vec<(usize, String)> = legal
        .iter()
        .map(|&mv| to_san_with(pos, mv, &legal))
        .filter_map(|san| {
            let distance = edit_distance(&bare_form(&san), &want);
            (distance <= 1).then_some((distance, san))
        })
        .collect();
    scored.sort();
    scored.truncate(3);
    scored.into_iter().map(|(_, san)| san).collect()
}

/// Lower case, with the capture and check marks that carry no meaning dropped.
pub(crate) fn bare_form(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() && !matches!(c, 'x' | 'X'))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Every command and what it does, in the order the help lists them.
pub(crate) const COMMANDS: [(&str, &str); 27] = [
    ("help", "this list"),
    ("board", "redraw the board"),
    ("flip", "turn the board around"),
    ("moves", "list the legal moves"),
    ("history", "the moves so far"),
    ("pgn", "the game as PGN"),
    ("export", "write a PGN file"),
    ("import", "open a PGN file"),
    ("save", "save this game"),
    ("load", "restore a game"),
    ("fen", "the position as FEN"),
    ("eval", "the engine's opinion"),
    ("hint", "ask for a suggestion"),
    ("undo", "take back a move"),
    ("pause", "pause or resume"),
    ("draw", "offer or accept a draw"),
    ("time", "seconds per move"),
    ("depth", "search depth instead"),
    ("theme", "board colours"),
    ("pieces", "drawn, or figurines"),
    ("sound", "auto, on, off, or test"),
    ("size", "fill the window, or not"),
    ("name", "set a player name"),
    ("setup", "local files and setup"),
    ("new", "start again"),
    ("resign", "concede the game"),
    ("quit", "leave"),
];

/// Every move names the rank it ends on, so a word with no digit in it was
/// meant as a command - castling, which names no square, aside.
pub(crate) fn looks_like_command(word: &str) -> bool {
    let castling = matches!(
        word.replace('0', "o").as_str(),
        "o-o" | "oo" | "o-o-o" | "ooo"
    );
    !castling && !word.chars().any(|c| c.is_ascii_digit())
}

/// The command `word` was probably a misspelling of.
pub(crate) fn command_guess(word: &str) -> Option<&'static str> {
    let names = COMMANDS.iter().map(|(name, _)| *name).chain(["exit"]);
    let mut best: Option<(usize, &'static str)> = None;
    for name in names {
        let distance = edit_distance(word, name);
        if distance <= 2 && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, name));
        }
    }
    best.map(|(_, name)| name)
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}
