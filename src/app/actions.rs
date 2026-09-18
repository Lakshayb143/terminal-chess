//! Game actions shared by both loops: moves, clicks, promotion, engine,
//! hints, undo, and runtime settings.

use std::io::{self, Write};
use std::time::Duration;

use chess::input::TerminalInput;
use chess::ui::Theme;
use chess::{sound, ui};
use chess_core::board::{Move, MoveKind, Piece, PieceKind, Position};
use chess_core::game::{outcome, Game};
use chess_core::movegen::{generate_legal, in_check};
use chess_core::san::{parse_move, to_san, ParseError};
use chess_core::search::{Limits, Search, SearchResult};
use chess_core::{board, search};

use crate::app::cli::Mode;
use crate::app::format::{budget_text, format_score, kind_name, node_count, pv_text, white_pov};
use crate::app::parse::{nearby_moves, promotion_default, under_specified};
use crate::app::prompt::read_line;
use crate::app::screen::{Screen, UiAction};

/// Make one real game move and emit exactly one matching audio cue. Keeping
/// this at the mutation boundary means mouse, keyboard, and engine moves can
/// never drift into different feedback behavior.
pub(crate) fn play_move(game: &mut Game, mv: Move, screen: &mut Screen) -> String {
    let text = game.play(mv);
    if outcome(game).is_some() {
        game.clock.pause();
    }
    screen.sound.play(sound_after_move(game));
    text
}

pub(crate) fn sound_after_move(game: &Game) -> sound::Cue {
    if outcome(game).is_some() {
        return sound::Cue::GameEnd;
    }
    if in_check(&game.pos, game.pos.side) {
        return sound::Cue::Check;
    }
    let Some(undo) = game.undos.last() else {
        return sound::Cue::Move;
    };
    if undo.mv.promo.is_some() {
        sound::Cue::Promotion
    } else if matches!(undo.mv.kind, MoveKind::CastleKing | MoveKind::CastleQueen) {
        sound::Cue::Castle
    } else if undo.captured.is_some() {
        sound::Cue::Capture
    } else {
        sound::Cue::Move
    }
}

/// The first word, lower-cased, and whatever followed it.
pub(crate) fn split_command(input: &str) -> (String, &str) {
    match input.split_once(char::is_whitespace) {
        Some((word, rest)) => (word.to_ascii_lowercase(), rest.trim()),
        None => (input.to_ascii_lowercase(), ""),
    }
}

/// Returns true when the application should close.
pub(crate) fn handle_ui_action(
    action: UiAction,
    game: &mut Game,
    mode: Mode,
    screen: &mut Screen,
) -> bool {
    match action {
        UiAction::MoveInput => {
            screen.focus_move_input();
        }
        UiAction::Pause => {
            if outcome(game).is_none() {
                game.toggle_pause();
                screen.clear_marks();
                let message = if game.paused {
                    screen
                        .theme
                        .accent("Game paused. Choose Resume when you are ready.")
                } else {
                    screen.theme.good("Game resumed.")
                };
                screen.note(message);
            }
        }
        UiAction::Undo => {
            screen.confirming = None;
            undo(game, mode, screen);
        }
        UiAction::Draw => {
            screen.confirming = None;
            if mode != Mode::TwoPlayer {
                screen.note(
                    screen
                        .theme
                        .dim("Draw offers are available in two-player games."),
                );
            } else if game.draw_offer == Some(game.pos.side.flip()) {
                game.agreed_draw = true;
                game.draw_offer = None;
                game.changed();
                game.clock.pause();
                screen.clear_marks();
                screen.sound.play(sound::Cue::GameEnd);
                screen.redraw = true;
            } else if game.draw_offer == Some(game.pos.side) {
                screen.note(
                    screen
                        .theme
                        .dim("Your draw offer is waiting for the next player."),
                );
            } else {
                game.draw_offer = Some(game.pos.side);
                game.changed();
                screen.note(screen.theme.accent(&format!(
                    "{} offers a draw. Play your move; the opponent can then accept.",
                    game.pos.side.name()
                )));
            }
        }
        UiAction::DeclineDraw => {}
        UiAction::Resign => {
            if screen.confirming == Some(UiAction::Resign) {
                game.resigned = Some(game.pos.side);
                game.changed();
                game.clock.pause();
                screen.clear_marks();
                screen.sound.play(sound::Cue::GameEnd);
                screen.redraw = true;
            } else {
                screen.confirming = Some(UiAction::Resign);
                screen.note(
                    screen
                        .theme
                        .warn("Choose Confirm to resign, or press Escape."),
                );
            }
        }
        UiAction::Restart => {
            if screen.confirming == Some(UiAction::Restart) {
                game.restart();
                screen.clear_marks();
                screen.note(screen.theme.good("New game."));
            } else {
                screen.confirming = Some(UiAction::Restart);
                screen.note(
                    screen
                        .theme
                        .warn("Choose Confirm to restart, or press Escape."),
                );
            }
        }
        UiAction::ToggleSize => toggle_size(screen),
        UiAction::CyclePieces => cycle_pieces(screen),
        UiAction::Flip => flip_board(screen),
        UiAction::Rematch => {
            game.restart();
            screen.clear_marks();
            screen.note(screen.theme.good("Rematch started."));
        }
        UiAction::Quit => return true,
    }
    false
}

pub(crate) fn make_move(game: &mut Game, input: &str, screen: &mut Screen) {
    match parse_move(&game.pos, input) {
        Ok(mv) => {
            let mover = game.pos.side;
            let text = play_move(game, mv, screen);
            screen.clear_marks();
            screen.redraw = true;
            // Confirm sloppy input by echoing how it was read, but do not
            // repeat a move back that was already typed as it is written.
            if !text.eq_ignore_ascii_case(input) {
                screen.note(format!(
                    "{} {}",
                    screen.theme.dim(&format!("{} plays", mover.name())),
                    screen.theme.bold(&text)
                ));
            }
        }
        Err(ParseError::Illegal(text)) => {
            // A pawn reaching the last rank without being told what to become.
            if let Some(mv) = promotion_default(&game.pos, input) {
                let square = board::square_name(mv.to);
                let name = play_move(game, mv, screen);
                screen.clear_marks();
                screen.redraw = true;
                screen.note(format!(
                    "{} {}",
                    screen.theme.bold(&name),
                    screen.theme.dim(&format!(
                        "- say {0}=N, {0}=R or {0}=B for anything but a queen",
                        square
                    ))
                ));
                return;
            }
            let choices = under_specified(&game.pos, input);
            if choices.len() > 1 {
                screen.note(format!(
                    "{} {}",
                    screen.theme.warn(&format!("`{}` could be", text)),
                    screen.theme.bold(&choices.join(" or "))
                ));
                return;
            }
            let near = nearby_moves(&game.pos, input);
            let hint = if near.is_empty() {
                screen.theme.dim("- `moves` lists what is legal")
            } else {
                screen
                    .theme
                    .dim(&format!("- did you mean {}?", near.join(" or ")))
            };
            screen.note(format!(
                "{} {}",
                screen
                    .theme
                    .warn(&format!("`{}` is not a legal move", text)),
                hint
            ));
        }
        Err(ParseError::Ambiguous(text, candidates)) => {
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("`{}` could mean", text)),
                screen.theme.bold(&candidates.join(" or "))
            ));
        }
    }
}

/// Chess.com-style two-click movement: choose a friendly piece, then one of
/// its legal destinations. Clicking another friendly piece simply reselects.
pub(crate) fn handle_board_click(
    game: &mut Game,
    screen: &mut Screen,
    square: Option<board::Square>,
    finished: bool,
) {
    let square = match square {
        Some(square) => square,
        None => {
            if screen.selected.take().is_some() {
                screen.targets.clear();
                screen.captures.clear();
                screen.promotions.clear();
                screen.invalid = None;
                screen.redraw = true;
            }
            return;
        }
    };

    if finished {
        screen.note(screen.theme.dim("The game is over. Try `new` or `undo`."));
        return;
    }
    if game.paused {
        screen.note(
            screen
                .theme
                .dim("The game is paused. Choose Resume before moving."),
        );
        return;
    }
    screen.invalid = None;

    if let Some(choice) = screen
        .promotions
        .iter()
        .find(|choice| choice.square == square)
        .copied()
    {
        play_move(game, choice.movement, screen);
        screen.clear_marks();
        screen.redraw = true;
        return;
    }
    screen.promotions.clear();

    let legal = generate_legal(&game.pos);
    if let Some(from) = screen.selected {
        if square == from {
            screen.selected = None;
            screen.targets.clear();
            screen.captures.clear();
            screen.redraw = true;
            return;
        }

        let mut choices = legal
            .iter()
            .copied()
            .filter(|mv| mv.from == from && mv.to == square);
        if let Some(first) = choices.next() {
            let choices: Vec<Move> = std::iter::once(first).chain(choices).collect();
            if choices.iter().any(|mv| mv.promo.is_some()) {
                open_promotion_menu(game, screen, &choices);
                return;
            }
            play_move(game, first, screen);
            screen.clear_marks();
            screen.redraw = true;
            return;
        }
    }

    match game.pos.at(square) {
        Some(piece) if piece.color == game.pos.side => {
            screen.selected = Some(square);
            let moves: Vec<Move> = legal
                .iter()
                .filter(|mv| mv.from == square)
                .copied()
                .collect();
            screen.targets = moves.iter().map(|mv| mv.to).collect();
            screen.captures = moves
                .iter()
                .filter(|mv| mv.kind == MoveKind::EnPassant || game.pos.at(mv.to).is_some())
                .map(|mv| mv.to)
                .collect();
            screen.message = if moves.is_empty() {
                vec![screen.theme.dim(&format!(
                    "The {} on {} has no legal moves.",
                    kind_name(piece.kind),
                    board::square_name(square)
                ))]
            } else {
                Vec::new()
            };
            screen.redraw = true;
        }
        _ if screen.selected.is_none() => {
            screen.invalid = Some(square);
            let guidance = match game.pos.at(square) {
                Some(piece) => format!(
                    "That {} belongs to {}. Choose a {} piece.",
                    kind_name(piece.kind),
                    piece.color.name(),
                    game.pos.side.name()
                ),
                None => format!(
                    "Choose a {} piece before choosing a destination.",
                    game.pos.side.name()
                ),
            };
            screen.message = vec![screen.theme.dim(&guidance)];
            screen.redraw = true;
        }
        _ => {
            screen.invalid = Some(square);
            screen.message = vec![screen.theme.warn(&format!(
                "{} is not a legal destination for the selected piece.",
                board::square_name(square)
            ))];
            screen.redraw = true;
        }
    }
}

pub(crate) fn open_promotion_menu(game: &Game, screen: &mut Screen, moves: &[Move]) {
    let destination = moves[0].to;
    let file = board::file_of(destination);
    let rank = board::rank_of(destination);
    let step: i8 = if rank == 7 { -1 } else { 1 };
    let kinds = [
        PieceKind::Queen,
        PieceKind::Rook,
        PieceKind::Bishop,
        PieceKind::Knight,
    ];

    screen.promotions = kinds
        .iter()
        .enumerate()
        .filter_map(|(offset, &kind)| {
            let movement = moves.iter().find(|mv| mv.promo == Some(kind)).copied()?;
            let display_rank = (rank as i8 + step * offset as i8) as u8;
            Some(ui::PromotionOption {
                square: board::sq(file, display_rank),
                piece: Piece::new(game.pos.side, kind),
                movement,
            })
        })
        .collect();
    screen.analysis.clear();
    screen.message = vec![format!(
        "{}  {}",
        screen.theme.accent("Promotion"),
        screen.theme.dim("choose a piece, or type Q, R, B or N")
    )];
    screen.redraw = true;
}

pub(crate) fn play_promotion(game: &mut Game, screen: &mut Screen, kind: PieceKind) {
    if let Some(choice) = screen
        .promotions
        .iter()
        .find(|choice| choice.piece.kind == kind)
        .copied()
    {
        play_move(game, choice.movement, screen);
        screen.clear_marks();
        screen.redraw = true;
    }
}

pub(crate) fn engine_move(
    game: &mut Game,
    engine: &mut Search,
    limits: &Limits,
    screen: &mut Screen,
) {
    engine.set_history(game.prior_positions());
    let indent = screen.indent.clone();
    let result = {
        let theme = &screen.theme;
        let pos = &game.pos;
        let mut report = |snapshot: &SearchResult| {
            if theme.live {
                print!("\r\x1b[K{}{}", indent, thinking_line(theme, pos, snapshot));
                let _ = io::stdout().flush();
            }
        };
        engine.think(pos, limits, &mut report)
    };
    screen.theme.erase_line();

    let flag_before = game.clock.flagged;
    game.clock.tick();
    if flag_before.is_none() && game.clock.flagged.is_some() {
        game.changed();
        screen.sound.play(sound::Cue::GameEnd);
        screen.clear_marks();
        screen.page = None;
        screen.redraw = true;
        return;
    }

    let mv = match result.best {
        Some(mv) => mv,
        None => {
            screen.note(screen.theme.warn("The engine has no legal move."));
            return;
        }
    };
    let score = white_pov(&game.pos, result.score);
    let text = play_move(game, mv, screen);
    screen.clear_marks();

    let theme = &screen.theme;
    let mut lines = vec![format!(
        "{} {}   {}",
        theme.dim("Engine plays"),
        theme.strong(theme.palette.accent, &text),
        theme.dim(&format!(
            "depth {}  \u{b7}  {}  \u{b7}  {} nodes  \u{b7}  {:.1}s",
            result.depth,
            format_score(score),
            node_count(result.nodes),
            result.elapsed.as_secs_f64()
        ))
    )];
    // The rest of the line it expects, from the position it has just reached.
    let expected = pv_text(&game.pos, result.pv.get(1..).unwrap_or(&[]), 6);
    if !expected.is_empty() {
        lines.push(format!(
            "{} {}",
            theme.dim("expects"),
            theme.label(&expected)
        ));
    }
    screen.analysis = lines;
}

pub(crate) fn thinking_line(theme: &Theme, pos: &Position, snapshot: &SearchResult) -> String {
    format!(
        "{}  {}  {}  {}",
        theme.dim("thinking"),
        theme.label(&format!("depth {}", snapshot.depth)),
        theme.accent(&format_score(white_pov(pos, snapshot.score))),
        theme.dim(&pv_text(pos, &snapshot.pv, 4))
    )
}

pub(crate) fn hint(game: &Game, engine: &mut Search, limits: &Limits, screen: &mut Screen) {
    if outcome(game).is_some() {
        screen.note(screen.theme.dim("The game is over."));
        return;
    }
    // A hint should come back quickly even when the engine has a long budget.
    let budget = Limits {
        depth: limits.depth,
        movetime: Some(
            limits
                .movetime
                .unwrap_or(Duration::from_secs(1))
                .min(Duration::from_secs(1)),
        ),
    };
    engine.set_history(game.prior_positions());
    let indent = screen.indent.clone();
    let result = {
        let theme = &screen.theme;
        let pos = &game.pos;
        let mut report = |snapshot: &SearchResult| {
            if theme.live {
                print!("\r\x1b[K{}{}", indent, thinking_line(theme, pos, snapshot));
                let _ = io::stdout().flush();
            }
        };
        engine.think(pos, &budget, &mut report)
    };
    screen.theme.erase_line();

    let theme = &screen.theme;
    match result.best {
        Some(mv) => {
            let line = pv_text(&game.pos, &result.pv, 6);
            let mut lines = vec![format!(
                "{} {}   {}",
                theme.dim("Try"),
                theme.strong(theme.palette.accent, &to_san(&game.pos, mv)),
                theme.dim(&format!(
                    "depth {}  \u{b7}  {}",
                    result.depth,
                    format_score(white_pov(&game.pos, result.score))
                ))
            )];
            if !line.is_empty() {
                lines.push(format!("{} {}", theme.dim("expects"), theme.label(&line)));
            }
            screen.analysis = lines;
            screen.targets = vec![mv.from, mv.to];
            screen.captures = if mv.kind == MoveKind::EnPassant || game.pos.at(mv.to).is_some() {
                vec![mv.to]
            } else {
                Vec::new()
            };
            screen.redraw = true;
        }
        None => {
            let text = theme.dim("There is nothing to play.");
            screen.note(text);
        }
    }
}

pub(crate) fn undo(game: &mut Game, mode: Mode, screen: &mut Screen) {
    // Take back the engine's reply as well, so the human lands back on their
    // own turn rather than watching it answer again.
    let plies = if mode == Mode::TwoPlayer { 1 } else { 2 };
    let mut taken = Vec::new();
    while taken.len() < plies {
        match game.take_back() {
            Some(text) => taken.push(text),
            None => break,
        }
    }
    if taken.is_empty() {
        screen.note(screen.theme.dim("Nothing to take back."));
        return;
    }
    taken.reverse();
    screen.clear_marks();
    screen.note(format!(
        "{} {}",
        screen.theme.dim("Took back"),
        screen.theme.bold(&taken.join(" and "))
    ));
    screen.redraw = true;
}

pub(crate) fn set_time(limits: &mut Limits, screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        screen.note(
            screen
                .theme
                .dim(&format!("The engine gets {}.", budget_text(limits))),
        );
        return;
    }
    match rest.parse::<f64>() {
        Ok(secs) if secs.is_finite() && secs > 0.0 => {
            limits.movetime = Some(Duration::from_secs_f64(secs));
            screen.note(
                screen
                    .theme
                    .good(&format!("The engine now gets {}.", budget_text(limits))),
            );
        }
        _ => screen.note(
            screen
                .theme
                .warn(&format!("`{}` is not a number of seconds.", rest)),
        ),
    }
}

pub(crate) fn set_depth(limits: &mut Limits, screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        screen.note(
            screen
                .theme
                .dim(&format!("Depth is capped at {}.", limits.depth)),
        );
        return;
    }
    match rest.parse::<u32>() {
        Ok(depth) if (1..=search::MAX_DEPTH).contains(&depth) => {
            limits.depth = depth;
            // A depth asked for by name is a depth to reach, not to give up on.
            limits.movetime = None;
            screen.note(
                screen
                    .theme
                    .good(&format!("The engine now searches to depth {}.", depth)),
            );
        }
        _ => screen.note(screen.theme.warn(&format!(
            "Depth must be a number from 1 to {}.",
            search::MAX_DEPTH
        ))),
    }
}

pub(crate) fn set_theme(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        screen.note(screen.theme.dim(&format!("Themes: {}.", ui::theme_names())));
        return;
    }
    match ui::palette(rest) {
        Some(palette) => {
            screen.theme.palette = palette;
            screen.redraw = true;
        }
        None => screen.note(screen.theme.warn(&format!(
            "No theme called `{}`. Try {}.",
            rest,
            ui::theme_names()
        ))),
    }
}

/// Drawn pieces or figurines. Drawn pieces need a square at least three rows
/// tall, so on a small window this is a preference rather than an order.
pub(crate) fn set_pieces(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        let text = screen.theme.dim("Pieces are `art`, `glyph` or `auto`.");
        screen.note(text);
        return;
    }
    match ui::pieces_named(rest) {
        Some(pieces) => apply_pieces(screen, pieces),
        None => {
            let text = screen
                .theme
                .warn(&format!("`{}` is not art, glyph or auto.", rest));
            screen.note(text);
        }
    }
}

pub(crate) fn apply_pieces(screen: &mut Screen, pieces: ui::Pieces) {
    screen.pieces = pieces;
    screen.redraw = true;
}

/// The UI cycles from automatic images directly to font glyphs first. That
/// gives terminals with soft image scaling a crisp escape hatch in one key or
/// click, while portable block art remains the third choice.
pub(crate) fn cycle_pieces(screen: &mut Screen) {
    let pieces = match screen.pieces {
        ui::Pieces::Auto => ui::Pieces::Glyph,
        ui::Pieces::Glyph => ui::Pieces::Art,
        ui::Pieces::Art => ui::Pieces::Auto,
    };
    apply_pieces(screen, pieces);
    let detail = match pieces {
        ui::Pieces::Auto => match screen.image_protocol {
            Some(protocol) => protocol.name(),
            None => "the best available fallback",
        },
        ui::Pieces::Glyph => "your terminal font for maximum sharpness",
        ui::Pieces::Art => "portable block artwork",
    };
    screen.note(screen.theme.good(&format!(
        "Piece style: {} — using {}.",
        pieces.name(),
        detail
    )));
}

pub(crate) fn set_sound(screen: &mut Screen, rest: &str) {
    if rest.is_empty() {
        let mode = match screen.sound.mode() {
            sound::Mode::Auto => "auto",
            sound::Mode::On => "on",
            sound::Mode::Off => "off",
        };
        let state = if let Some(backend) = screen.sound.backend() {
            format!("{} is ready", backend)
        } else if screen.sound.mode() == sound::Mode::Off {
            "muted".to_string()
        } else if let Some(error) = screen.sound.last_error() {
            format!("audio could not start: {}", error)
        } else {
            "no local audio output was found".to_string()
        };
        screen.note(
            screen
                .theme
                .dim(&format!("Sound is `{}` - {}.", mode, state)),
        );
        return;
    }

    if rest.eq_ignore_ascii_case("test") {
        let message = if let Some(backend) = screen.sound.backend() {
            if screen.sound.play(sound::Cue::Move) {
                screen
                    .theme
                    .good(&format!("Playing a test move through {}.", backend))
            } else {
                screen.theme.warn("The test sound could not be queued.")
            }
        } else if screen.sound.mode() == sound::Mode::Off {
            screen.theme.dim("Sound is off. Use `sound on` first.")
        } else {
            screen.theme.warn(&format!(
                "Audio is unavailable: {}.",
                screen
                    .sound
                    .last_error()
                    .unwrap_or("no output device was found")
            ))
        };
        screen.note(message);
        return;
    }

    let Some(mode) = sound::Mode::named(rest) else {
        screen.note(
            screen
                .theme
                .warn(&format!("`{}` is not `auto`, `on` or `off`.", rest)),
        );
        return;
    };
    let active = screen.sound.set_mode(mode);
    let message = match (mode, active) {
        (sound::Mode::Off, _) => screen.theme.dim("Sound effects are off."),
        (_, true) => {
            let backend = screen.sound.backend().unwrap_or("the audio device");
            let _ = screen.sound.play(sound::Cue::Move);
            screen.theme.good(&format!(
                "Sound effects are on through {}. Playing a test move.",
                backend
            ))
        }
        _ => screen.theme.warn(&format!(
            "No local audio output is available: {}.",
            screen
                .sound
                .last_error()
                .unwrap_or("no output device was found")
        )),
    };
    screen.note(message);
}

/// Fill the window, or hold the board at the size it used to be.
pub(crate) fn set_size(screen: &mut Screen, rest: &str) {
    let compact = match rest.to_ascii_lowercase().as_str() {
        "" => !screen.compact,
        "big" | "large" | "full" | "fill" => false,
        "small" | "compact" | "tiny" => true,
        _ => {
            let text = screen
                .theme
                .warn(&format!("`{}` is not `big` or `small`.", rest));
            screen.note(text);
            return;
        }
    };
    screen.compact = compact;
    screen.redraw = true;
}

pub(crate) fn flip_board(screen: &mut Screen) {
    screen.flipped = !screen.flipped;
    screen.redraw = true;
}

pub(crate) fn toggle_size(screen: &mut Screen) {
    screen.compact = !screen.compact;
    let size = if screen.compact { "small" } else { "big" };
    screen.note(screen.theme.good(&format!("Board size: {}.", size)));
}

/// Throwing a game away is the one thing `undo` cannot rescue, so ask first.
pub(crate) fn confirm_new(
    input: &mut TerminalInput,
    game: &Game,
    screen: &Screen,
) -> Result<bool, String> {
    if game.sans.is_empty() || !screen.theme.live {
        return Ok(true);
    }
    let prompt = format!(
        "  {} ",
        screen
            .theme
            .warn("Start a new game and lose this one? [y/N]")
    );
    input.suspend()?;
    let answer = {
        let mut stdin = io::stdin().lock();
        read_line(&mut stdin, &prompt)
    };
    let resumed = input.resume();
    let line = answer?;
    resumed?;
    match line {
        Some(line) => Ok(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        )),
        None => Ok(false),
    }
}
