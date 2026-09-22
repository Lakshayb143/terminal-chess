//! Pages printed on request: help, move lists, history, PGN, and setup.

use std::path::Path;

use chess::ui::Theme;
use chess::{storage, ui};
use chess_core::board::{self, Color, Move, MoveKind, PieceKind, Position};
use chess_core::game::{score_tag, Game};
use chess_core::movegen::generate_legal;
use chess_core::san::{parse_move, to_san_with};

use crate::app::cli::Mode;
use crate::app::format::{kind_name, wrap};
use crate::app::parse::COMMANDS;
use crate::app::saves::{color_named, restore_game};
use crate::app::screen::{history_lines, Screen};

pub(crate) fn help_lines(theme: &Theme) -> Vec<String> {
    let mut lines = vec![
        theme.bold("MOUSE"),
        format!(
            "  {}",
            theme.dim("Click a piece, then its highlighted target. Escape cancels.")
        ),
        format!(
            "  {}",
            theme.dim("Click the buttons, or a move in the list to look back at it.")
        ),
        format!(
            "  {}",
            theme.dim("The wheel scrolls this page and the move list.")
        ),
        String::new(),
        theme.bold("KEYBOARD"),
        format!(
            "  {}",
            theme.dim("Arrows move a cursor on the board; Enter picks up and puts down.")
        ),
        format!(
            "  {}",
            theme.dim("PgUp and PgDn step through the game; Home is its start, End now.")
        ),
        format!(
            "  {}",
            theme.dim("Tab reaches the controls and Enter chooses. Escape steps back.")
        ),
        format!(
            "  {}",
            theme.dim("Start typing at any time to enter a move or a command.")
        ),
        String::new(),
        theme.bold("MOVES"),
        format!(
            "  {}   {}",
            theme.accent("e4  Nf3  exd5  O-O  e8=Q"),
            theme.dim("standard notation")
        ),
        format!(
            "  {}                {}",
            theme.accent("e2e4  e7e8q"),
            theme.dim("coordinates; a promotion defaults to a queen")
        ),
        String::new(),
        theme.bold("COMMANDS"),
    ];
    // Two columns, so the list stays one glance rather than one scroll.
    let half = COMMANDS.len().div_ceil(2);
    for row in 0..half {
        let mut line = String::new();
        for column in [row, row + half] {
            if let Some((name, what)) = COMMANDS.get(column) {
                let cell = format!(
                    "{}  {}",
                    theme.accent(&format!("{:<8}", name)),
                    theme.dim(what)
                );
                line.push_str(&ui::pad(&cell, 36));
            }
        }
        lines.push(format!("  {}", line.trim_end()));
    }
    lines.push(String::new());
    lines.push(theme.dim("  `moves e2` points at one piece on the board"));
    lines.push(
        theme.dim("  `level club`, `theme wood`, `pieces art`, `size small` all take a value"),
    );
    lines.push(theme.dim("  `time 5` and `depth 8` set the engine by hand, at full strength"));
    lines.push(
        theme.dim("  `save`, `load`, `import game.pgn` and `export game.pgn` keep games local"),
    );
    lines.push(theme.dim("  Scores are in pawns, always from White's point of view."));
    lines
}

pub(crate) fn moves_lines(theme: &Theme, pos: &Position) -> Vec<String> {
    let legal = generate_legal(pos);
    let groups = [
        (PieceKind::Pawn, "Pawns"),
        (PieceKind::Knight, "Knights"),
        (PieceKind::Bishop, "Bishops"),
        (PieceKind::Rook, "Rooks"),
        (PieceKind::Queen, "Queen"),
        (PieceKind::King, "King"),
    ];
    let mut lines = Vec::new();
    for (kind, label) in groups {
        let mut moves: Vec<String> = legal
            .iter()
            .filter(|mv| pos.at(mv.from).map(|piece| piece.kind) == Some(kind))
            .map(|&mv| to_san_with(pos, mv, &legal))
            .collect();
        moves.sort();
        if moves.is_empty() {
            continue;
        }
        for (i, line) in wrap(&moves, 56, "").into_iter().enumerate() {
            let head = if i == 0 { label } else { "" };
            lines.push(format!(
                "{}{}",
                theme.label(&format!("{:<9}", head)),
                theme.accent(&line)
            ));
        }
    }
    lines.push(String::new());
    lines.push(theme.dim(&format!(
        "{} legal moves  \u{b7}  `moves e2` points at the board instead",
        legal.len()
    )));
    lines
}

/// Mark where one piece can go, on the board, where the question was asked.
pub(crate) fn show_piece_moves(screen: &mut Screen, pos: &Position, filter: &str) {
    let from = match board::parse_square(&filter.to_ascii_lowercase()) {
        Some(from) => from,
        None => {
            let text = screen.theme.warn(&format!("`{}` is not a square.", filter));
            screen.note(text);
            return;
        }
    };
    let legal = generate_legal(pos);
    let moves: Vec<Move> = legal.iter().copied().filter(|mv| mv.from == from).collect();
    let square = board::square_name(from);
    if moves.is_empty() {
        let why = match pos.at(from) {
            None => format!("There is nothing on {}.", square),
            Some(piece) if piece.color != pos.side => format!(
                "The {} on {} is {}'s, and it is {} to move.",
                kind_name(piece.kind),
                square,
                piece.color.name(),
                pos.side.name()
            ),
            Some(piece) => format!(
                "The {} on {} has nowhere to go.",
                kind_name(piece.kind),
                square
            ),
        };
        let text = screen.theme.dim(&why);
        screen.note(text);
        return;
    }

    let mut sans: Vec<String> = moves
        .iter()
        .map(|&mv| to_san_with(pos, mv, &legal))
        .collect();
    sans.sort();
    let name = pos.at(from).map_or("piece", |piece| kind_name(piece.kind));
    let mut message = vec![screen.theme.dim(&format!(
        "The {} on {} has {} move{}:",
        name,
        square,
        sans.len(),
        if sans.len() == 1 { "" } else { "s" }
    ))];
    message.extend(
        wrap(&sans, 60, "")
            .iter()
            .map(|line| screen.theme.accent(line)),
    );
    screen.targets = moves.iter().map(|mv| mv.to).collect();
    screen.captures = moves
        .iter()
        .filter(|mv| mv.kind == MoveKind::EnPassant || pos.at(mv.to).is_some())
        .map(|mv| mv.to)
        .collect();
    screen.selected = Some(from);
    screen.show(message);
}

pub(crate) fn history_page(theme: &Theme, game: &Game) -> Vec<String> {
    let played = history_lines(game);
    if played.is_empty() {
        return vec![theme.dim("No moves played yet.")];
    }
    // Two columns of move pairs, oldest first.
    let half = played.len().div_ceil(2);
    (0..half)
        .map(|row| {
            let mut line = String::new();
            for column in [row, row + half] {
                if let Some(text) = played.get(column) {
                    line.push_str(&format!("{:<24}", text));
                }
            }
            theme.accent(line.trim_end())
        })
        .collect()
}

/// Left plain on purpose: PGN is for pasting somewhere else, and escape codes
/// would go with it.
pub(crate) fn pgn_lines(game: &Game, names: &[String; 2]) -> Vec<String> {
    let mut lines = vec![
        "[Event \"Casual game\"]".to_string(),
        "[Site \"Terminal\"]".to_string(),
        "[Date \"????.??.??\"]".to_string(),
        format!("[White \"{}\"]", pgn_escape(&names[Color::White.index()])),
        format!("[Black \"{}\"]", pgn_escape(&names[Color::Black.index()])),
        format!("[Result \"{}\"]", score_tag(game)),
    ];
    if game.start.to_fen() != board::START_FEN {
        lines.push("[SetUp \"1\"]".to_string());
        lines.push(format!("[FEN \"{}\"]", game.start.to_fen()));
    }
    lines.push(String::new());

    let mut words: Vec<String> = Vec::new();
    let mut number = game.start.fullmove;
    let mut side = game.start.side;
    for text in &game.sans {
        if side == Color::White {
            words.push(format!("{}.", number));
        } else if words.is_empty() {
            words.push(format!("{}...", number));
        }
        words.push(text.clone());
        if side == Color::Black {
            number += 1;
        }
        side = side.flip();
    }
    words.push(score_tag(game).to_string());
    lines.extend(wrap(&words, 66, ""));
    lines
}

pub(crate) fn pgn_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn load_saved_game(
    path: &Path,
    game: &mut Game,
    mode: &mut Mode,
    screen: &mut Screen,
) -> Result<(), String> {
    let (loaded, loaded_mode) = restore_game(storage::load_game(path)?)?;
    *game = loaded;
    *mode = loaded_mode;
    screen.clear_marks();
    screen.page = None;
    screen.note(screen.theme.good(&format!("Restored {}.", path.display())));
    Ok(())
}

pub(crate) fn import_pgn(path: &Path, game: &mut Game, screen: &mut Screen) -> Result<(), String> {
    let imported = storage::read_pgn(path)?;
    let start = match imported.start_fen {
        Some(fen) => Position::from_fen(&fen)
            .map_err(|error| format!("PGN starting position is invalid: {error}"))?,
        None => Position::startpos(),
    };
    let mut candidate = Game::with_clock(start, game.clock.initial, game.clock.increment);
    for (index, notation) in imported.moves.iter().enumerate() {
        let movement = parse_move(&candidate.pos, notation).map_err(|_| {
            format!(
                "PGN move {} (`{}`) is not legal in its position",
                index + 1,
                notation
            )
        })?;
        candidate.play(movement);
    }
    candidate.revision = game.revision.wrapping_add(1);
    *game = candidate;
    screen.clear_marks();
    screen.page = None;
    screen.note(screen.theme.good(&format!(
        "Imported {} moves from {}.",
        game.sans.len(),
        path.display()
    )));
    Ok(())
}

pub(crate) fn export_pgn(path: &Path, game: &Game, names: &[String; 2]) -> Result<(), String> {
    let mut text = pgn_lines(game, names).join("\n");
    text.push('\n');
    storage::write_text(path, &text)
}

pub(crate) fn set_player_name(screen: &mut Screen, rest: &str) {
    let Some((side, name)) = rest.split_once(char::is_whitespace) else {
        screen.note(
            screen
                .theme
                .dim("Use `name white Lakshay` or `name black Guest`."),
        );
        return;
    };
    let Some(color) = color_named(side) else {
        screen.note(
            screen
                .theme
                .warn("Choose `white` or `black` before the name."),
        );
        return;
    };
    let name: String = name
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(32)
        .collect();
    if name.is_empty() {
        screen.note(screen.theme.warn("A player name cannot be empty."));
        return;
    }
    screen.player_names[color.index()] = name.clone();
    screen.note(
        screen
            .theme
            .good(&format!("{} is now {}.", color.name(), name)),
    );
}

pub(crate) fn setup_lines(theme: &Theme, config: &Path, session: &Path) -> Vec<String> {
    vec![
        theme.bold("YOUR LOCAL GAME"),
        theme.dim("Moves, clicks, clocks and sounds work without a browser."),
        String::new(),
        format!("{}  {}", theme.label("Preferences"), config.display()),
        format!("{}  {}", theme.label("Autosave"), session.display()),
        String::new(),
        theme.bold("QUICK SETUP"),
        format!("  {}", theme.accent("name white Lakshay")),
        format!(
            "  {}",
            theme.accent("theme forest  ·  pieces glyph  ·  sound on")
        ),
        format!("  {}", theme.accent("pause  ·  save  ·  load")),
        String::new(),
        theme.dim("Preferences are saved automatically. Edit config.toml when the game is closed."),
        theme.dim("Press Escape or Return to go back to the board."),
    ]
}
