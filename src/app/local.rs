//! Local play against the engine or another person at the same keyboard.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chess::input::{Action as InputAction, TerminalInput};
use chess::{sound, storage, ui};
use chess_core::board::{Color, PieceKind, Position};
use chess_core::eval;
use chess_core::game::{outcome, Game};
use chess_core::search::Search;

use crate::app::account::AccountContext;
use crate::app::actions::{
    confirm_new, engine_move, handle_board_click, handle_ui_action, hint, make_move,
    play_promotion, set_depth, set_pieces, set_size, set_sound, set_theme, set_time, split_command,
    undo,
};
use crate::app::cli::{names_a_file, Mode, Options, StartChoice, HOSTED_FILES};
use crate::app::format::{format_score, white_pov};
use crate::app::home::{self, HomeInfo};
use crate::app::online::play_online;
use crate::app::pages::{
    export_pgn, help_lines, history_page, import_pgn, load_saved_game, moves_lines, pgn_lines,
    set_player_name, setup_lines, show_piece_moves,
};
use crate::app::parse::{command_guess, looks_like_command};
use crate::app::prompt::{ask_mode, read_line};
use crate::app::saves::{restore_game, runtime_preferences, save_current_game};
use crate::app::screen::{Screen, UiAction};

pub(crate) fn play(options: Options, loaded: storage::LoadedPreferences) -> Result<(), String> {
    if options.online.is_some() {
        return play_online(options, loaded.path, loaded.preferences);
    }
    let config_path = loaded.path;
    let session_path = storage::default_session_path(&config_path);
    let mut last_preferences = loaded.preferences;
    let mut backup_config_before_save = loaded.backup_before_save;
    let first_run = loaded.first_run;
    let config_warning = loaded.warning;
    let requested_load = options
        .load
        .clone()
        .or_else(|| options.resume.then(|| session_path.clone()));
    let mut resuming = requested_load.is_some();
    let mut restored = match requested_load {
        Some(path) => Some(restore_game(storage::load_game(&path)?)?),
        None => None,
    };
    let start = match (&restored, &options.fen) {
        (Some(_), _) => None,
        (None, Some(fen)) => {
            Some(Position::from_fen(fen).map_err(|why| format!("bad --fen: {}", why))?)
        }
        (None, None) => Some(Position::startpos()),
    };

    let mut screen = Screen::from_options(&options, options.player_names.clone());
    // Held for as long as the game lasts. Whatever was on the terminal before
    // comes back when this is dropped, however the program ends.
    let _fullscreen = ui::Fullscreen::enter(&screen.theme);
    // Probe once up front even when another piece style was requested. The
    // in-game Piece control can switch to Auto later without querying the
    // terminal in the middle of raw event input.
    if screen.theme.live && !screen.theme.ascii {
        screen.image_protocol = ui::detect_image_protocol();
    }

    let (mut game, mut mode) = match restored.take() {
        Some((game, mode)) => (game, mode),
        None => {
            let mut account = AccountContext::load(
                &config_path,
                &options.server_url,
                options.online_name.clone(),
            );
            let choice = match options.mode {
                Some(mode) => StartChoice::Mode(mode),
                None => {
                    let picked = if screen.theme.live && screen.theme.color {
                        let seat_path = storage::default_online_session_path(&config_path);
                        let info = HomeInfo::load(&session_path, &seat_path, &options, &screen);
                        home::run(&mut screen, &info, &mut account)?
                    } else {
                        let mut stdin = io::stdin().lock();
                        ask_mode(&mut stdin, &mut screen, session_path.exists(), &mut account)?
                    };
                    match picked {
                        Some(choice) => choice,
                        None => return Ok(()),
                    }
                }
            };
            match choice {
                StartChoice::Online(intent, name) => {
                    // The online game sets up its own screen.
                    drop(_fullscreen);
                    let mut options = options;
                    options.online = Some(intent);
                    options.online_name = name;
                    return play_online(options, config_path, last_preferences);
                }
                StartChoice::Mode(mode) => (
                    Game::with_clock(
                        start.expect("new games have a starting position"),
                        options.clock,
                        options.increment,
                    ),
                    mode,
                ),
                StartChoice::Resume => {
                    resuming = true;
                    restore_game(storage::load_game(&session_path)?)?
                }
            }
        }
    };

    // Raw events are only appropriate while we own an interactive colour
    // terminal. Piped input retains the original line-oriented interface.
    let mut terminal_input = TerminalInput::enter(screen.theme.live && screen.theme.color)?;

    let mut engine = Search::new();
    let mut limits = options.limits;
    screen.flipped = options.flipped || mode == Mode::HumanBlack;
    screen.message = if let Some(warning) = config_warning {
        vec![screen
            .theme
            .warn(&format!("{} — using safe defaults.", warning))]
    } else if resuming {
        vec![screen.theme.good("Saved game restored.")]
    } else if first_run {
        vec![screen.theme.accent(
            "Welcome — type a move, click a piece, or press Tab. Preferences save automatically.",
        )]
    } else {
        vec![screen
            .theme
            .dim("Type a move, click a piece, or press Tab for controls.")]
    };
    if screen.pieces != ui::Pieces::Glyph && screen.theme.live {
        if let Some(tip) = ui::terminal_tip(screen.image_protocol) {
            screen.message.push(screen.theme.dim(tip));
        }
    }
    let mut saved_revision = game.revision;
    let mut persistence_error_reported = false;

    loop {
        let preferences = runtime_preferences(&screen, &game);
        if preferences != last_preferences {
            let backup = if backup_config_before_save {
                storage::backup_invalid_preferences(&config_path).map(|_| ())
            } else {
                Ok(())
            };
            if backup.is_ok() {
                backup_config_before_save = false;
            }
            let saved = backup.and_then(|_| storage::save_preferences(&config_path, &preferences));
            if let Err(error) = saved {
                if !persistence_error_reported {
                    screen.note(
                        screen
                            .theme
                            .warn(&format!("Preferences were not saved: {error}")),
                    );
                    persistence_error_reported = true;
                }
            }
            last_preferences = preferences;
        }
        if game.revision != saved_revision {
            if let Err(error) = save_current_game(&session_path, &mut game, mode) {
                if !persistence_error_reported {
                    screen.note(screen.theme.warn(&format!("Autosave failed: {error}")));
                    persistence_error_reported = true;
                }
            }
            saved_revision = game.revision;
        }
        let flag_before = game.clock.flagged;
        if game.clock.tick() {
            if flag_before.is_none() && game.clock.flagged.is_some() {
                game.changed();
                screen.sound.play(sound::Cue::GameEnd);
                screen.clear_marks();
                screen.page = None;
                screen.redraw = true;
            } else {
                screen.draw_clock_tick(&game, mode, &limits);
            }
        }
        if screen.redraw {
            screen.redraw = false;
            screen.draw(&game, mode, &limits);
        }

        let finished = outcome(&game).is_some();
        let engine_to_move = match mode {
            Mode::HumanWhite => game.pos.side == Color::Black,
            Mode::HumanBlack => game.pos.side == Color::White,
            Mode::TwoPlayer => false,
        };
        if !finished && !game.paused && engine_to_move {
            engine_move(&mut game, &mut engine, &limits, &mut screen);
            screen.redraw = true;
            continue;
        }

        let action = if terminal_input.is_active() {
            screen.draw_prompt(&game, terminal_input.buffer());
            terminal_input.read_for(Duration::from_millis(200))?
        } else {
            let mut stdin = io::stdin().lock();
            match read_line(&mut stdin, &screen.prompt(&game))? {
                Some(line) => InputAction::Submit(line),
                None => InputAction::Quit,
            }
        };

        let line = match action {
            InputAction::Submit(line) => {
                if line.is_empty() && screen.focused != UiAction::MoveInput {
                    if handle_ui_action(screen.focused, &mut game, mode, &mut screen) {
                        let _ = save_current_game(&session_path, &mut game, mode);
                        return Ok(());
                    }
                    continue;
                }
                line
            }
            InputAction::Prompt => {
                let cleared_invalid = screen.invalid.take().is_some();
                if screen.focus_move_input() || cleared_invalid {
                    screen.redraw = true;
                    continue;
                }
                screen.draw_prompt(&game, terminal_input.buffer());
                continue;
            }
            InputAction::Focus { reverse } => {
                // Nothing on an open page can take focus, so the arrows and
                // Tab move the page itself.
                if screen.page.is_some() {
                    screen.scroll_page(reverse);
                } else {
                    screen.move_focus(reverse);
                }
                continue;
            }
            InputAction::Resize => {
                screen.redraw = true;
                continue;
            }
            InputAction::Tick => continue,
            InputAction::History { older } => {
                // An open page is what the wheel and the paging keys are
                // pointed at; the move list is underneath it.
                if !screen.scroll_page(older) {
                    screen.scroll_history(&game, older);
                }
                continue;
            }
            InputAction::Cancel => {
                let focus_changed = screen.focus_move_input();
                if screen.page.take().is_some() {
                    screen.redraw = true;
                } else {
                    let cancelled = screen.selected.take().is_some()
                        || !screen.targets.is_empty()
                        || !screen.promotions.is_empty();
                    let confirmation = screen.confirming.take().is_some();
                    if cancelled || confirmation || focus_changed {
                        screen.targets.clear();
                        screen.promotions.clear();
                        screen.redraw = true;
                    } else {
                        screen.draw_prompt(&game, terminal_input.buffer());
                    }
                }
                continue;
            }
            InputAction::Click { column, row } => {
                let flag_before = game.clock.flagged;
                game.clock.tick();
                if flag_before.is_none() && game.clock.flagged.is_some() {
                    game.changed();
                    screen.sound.play(sound::Cue::GameEnd);
                    screen.clear_marks();
                    screen.page = None;
                    screen.redraw = true;
                    continue;
                }
                if screen.page.take().is_some() {
                    screen.redraw = true;
                    continue;
                }
                if let Some(action) = screen.action_at(column, row) {
                    screen.focus(action);
                    if handle_ui_action(action, &mut game, mode, &mut screen) {
                        let _ = save_current_game(&session_path, &mut game, mode);
                        return Ok(());
                    }
                    continue;
                }
                screen.confirming = None;
                let square = screen.square_at(column, row);
                let finished = outcome(&game).is_some();
                handle_board_click(&mut game, &mut screen, square, finished);
                continue;
            }
            // End of input, Ctrl-C or Ctrl-D.
            InputAction::Quit => {
                let _ = save_current_game(&session_path, &mut game, mode);
                println!();
                return Ok(());
            }
        };

        let flag_before = game.clock.flagged;
        game.clock.tick();
        if flag_before.is_none() && game.clock.flagged.is_some() {
            game.changed();
            screen.sound.play(sound::Cue::GameEnd);
            screen.clear_marks();
            screen.page = None;
            screen.redraw = true;
        }
        let finished = outcome(&game).is_some();
        let input = line.trim();
        // Anything at all puts an open page away and brings the board back.
        if screen.page.take().is_some() {
            screen.redraw = true;
        }
        if input.is_empty() {
            // Return on its own redraws, which is how the frame catches up
            // with a window that has been resized under it.
            screen.redraw = screen.theme.live;
            continue;
        }
        if !screen.promotions.is_empty() && input.len() == 1 {
            let kind = input.chars().next().and_then(PieceKind::from_char);
            if let Some(
                kind @ (PieceKind::Queen | PieceKind::Rook | PieceKind::Bishop | PieceKind::Knight),
            ) = kind
            {
                play_promotion(&mut game, &mut screen, kind);
                continue;
            }
        }
        let (word, rest) = split_command(input);
        if options.hosted && names_a_file(&word, rest) {
            screen.note(screen.theme.warn(HOSTED_FILES));
            continue;
        }

        match word.as_str() {
            "quit" | "exit" | "q" => {
                // In a transcript this is the last line; on a screen we own,
                // the screen is about to be handed back, so it would flash past.
                if !screen.theme.live {
                    println!("  {}", screen.theme.dim("Goodbye."));
                }
                let _ = save_current_game(&session_path, &mut game, mode);
                return Ok(());
            }
            "help" | "h" | "?" => {
                let lines = help_lines(&screen.theme);
                screen.open("HELP", lines);
                continue;
            }
            "board" | "b" => {
                screen.redraw = true;
                continue;
            }
            "moves" | "l" | "m" => {
                if rest.is_empty() {
                    let lines = moves_lines(&screen.theme, &game.pos);
                    screen.open("LEGAL MOVES", lines);
                } else {
                    show_piece_moves(&mut screen, &game.pos, rest);
                }
                continue;
            }
            "history" | "moveslist" => {
                let lines = history_page(&screen.theme, &game);
                screen.open("THE GAME SO FAR", lines);
                continue;
            }
            "pgn" => {
                let lines = pgn_lines(&game, &screen.player_names);
                screen.open("PGN", lines);
                continue;
            }
            "export" => {
                let path = if rest.is_empty() {
                    PathBuf::from("game.pgn")
                } else {
                    PathBuf::from(rest)
                };
                match export_pgn(&path, &game, &screen.player_names) {
                    Ok(()) => screen.note(
                        screen
                            .theme
                            .good(&format!("Exported PGN to {}.", path.display())),
                    ),
                    Err(error) => screen.note(screen.theme.warn(&error)),
                }
                continue;
            }
            "import" => {
                if rest.is_empty() {
                    screen.note(screen.theme.dim("Use `import game.pgn`."));
                } else if let Err(error) = import_pgn(Path::new(rest), &mut game, &mut screen) {
                    screen.note(screen.theme.warn(&error));
                }
                continue;
            }
            "save" => {
                let path = if rest.is_empty() {
                    session_path.clone()
                } else {
                    PathBuf::from(rest)
                };
                match save_current_game(&path, &mut game, mode) {
                    Ok(()) => screen.note(
                        screen
                            .theme
                            .good(&format!("Saved game to {}.", path.display())),
                    ),
                    Err(error) => screen.note(screen.theme.warn(&error)),
                }
                continue;
            }
            "load" => {
                let path = if rest.is_empty() {
                    session_path.clone()
                } else {
                    PathBuf::from(rest)
                };
                if let Err(error) = load_saved_game(&path, &mut game, &mut mode, &mut screen) {
                    screen.note(screen.theme.warn(&error));
                }
                continue;
            }
            "setup" | "config" => {
                screen.open(
                    "LOCAL SETUP",
                    setup_lines(&screen.theme, &config_path, &session_path),
                );
                continue;
            }
            "name" => {
                set_player_name(&mut screen, rest);
                continue;
            }
            "fen" => {
                let text = screen.theme.accent(&game.pos.to_fen());
                screen.note(text);
                continue;
            }
            "eval" => {
                let score = white_pov(&game.pos, eval::evaluate(&game.pos));
                let text = format!(
                    "{}  {}",
                    screen.theme.bold(&format_score(score)),
                    screen.theme.dim("(static, from White's point of view)")
                );
                screen.note(text);
                continue;
            }
            "flip" => {
                screen.flipped = !screen.flipped;
                screen.redraw = true;
                continue;
            }
            "theme" => {
                set_theme(&mut screen, rest);
                continue;
            }
            "pieces" => {
                set_pieces(&mut screen, rest);
                continue;
            }
            "sound" | "sounds" => {
                set_sound(&mut screen, rest);
                continue;
            }
            "size" => {
                set_size(&mut screen, rest);
                continue;
            }
            "hint" => {
                hint(&game, &mut engine, &limits, &mut screen);
                continue;
            }
            "time" => {
                set_time(&mut limits, &mut screen, rest, options.hosted);
                continue;
            }
            "depth" => {
                set_depth(&mut limits, &mut screen, rest, options.hosted);
                continue;
            }
            "undo" | "u" | "back" | "takeback" => {
                undo(&mut game, mode, &mut screen);
                continue;
            }
            "pause" | "resume" => {
                let wants_pause = word == "pause";
                if game.paused == wants_pause {
                    let state = if game.paused {
                        "already paused"
                    } else {
                        "already running"
                    };
                    screen.note(screen.theme.dim(&format!("The game is {}.", state)));
                } else {
                    handle_ui_action(UiAction::Pause, &mut game, mode, &mut screen);
                }
                continue;
            }
            "draw" => {
                handle_ui_action(UiAction::Draw, &mut game, mode, &mut screen);
                continue;
            }
            "new" | "restart" => {
                if confirm_new(&mut terminal_input, &game, &screen)? {
                    game.restart();
                    screen.clear_marks();
                    screen.note(screen.theme.good("New game."));
                    screen.redraw = true;
                }
                continue;
            }
            "resign" => {
                if finished {
                    screen.note(screen.theme.dim("The game is already over."));
                } else {
                    game.resigned = Some(game.pos.side);
                    game.changed();
                    game.clock.pause();
                    screen.analysis.clear();
                    screen.sound.play(sound::Cue::GameEnd);
                    screen.redraw = true;
                }
                continue;
            }
            _ => {}
        }

        if finished {
            screen.note(
                screen
                    .theme
                    .dim("The game is over. Try `new`, `undo` or `quit`."),
            );
            continue;
        }
        if game.paused {
            screen.note(
                screen
                    .theme
                    .dim("The game is paused. Choose Resume before moving."),
            );
            continue;
        }
        if looks_like_command(&word) {
            let hint = match command_guess(&word) {
                Some(command) => format!("Did you mean `{}`?", command),
                None => "Type `help` for the commands.".to_string(),
            };
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("I do not know `{}`.", word)),
                screen.theme.dim(&hint)
            ));
            continue;
        }

        make_move(&mut game, input, &mut screen);
    }
}
