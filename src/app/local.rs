//! The application loop: the home page, and the local games started from it
//! against the engine or another person at the same keyboard. Every game
//! ends back on the home page; only leaving from there, or Ctrl+C, ends the
//! program.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chess::input::{Action as InputAction, TerminalInput};
use chess::{sound, storage, ui};
use chess_core::board::{PieceKind, Position};
use chess_core::eval;
use chess_core::game::{outcome, Game};
use chess_core::search::{Limits, Search};

use crate::app::account::AccountContext;
use crate::app::actions::{
    confirm_new, engine_move, handle_board_click, handle_ui_action, hint, make_move,
    play_promotion, rematch, set_depth, set_level, set_pieces, set_size, set_sound, set_theme,
    set_time, show_pgn, split_command, start_review, undo, After,
};
use crate::app::cli::{names_a_file, Mode, Options, StartChoice, HOSTED_FILES};
use crate::app::format::{format_score, white_pov};
use crate::app::home::{self, HomeInfo};
use crate::app::online::{play_online, Ended};
use crate::app::pages::{
    export_pgn, help_lines, history_page, import_pgn, load_saved_game, moves_lines,
    set_player_name, setup_lines, show_piece_moves,
};
use crate::app::parse::{command_guess, looks_like_command};
use crate::app::prompt::{ask_mode, read_line};
use crate::app::saves::{restore_game, runtime_preferences, save_current_game};
use crate::app::screen::{move_name, Screen, UiAction};
use crate::app::settings::Level;

/// What to play next.
enum Start {
    Choice(StartChoice),
    /// A saved game named on the command line, already read.
    Restored(Box<(Game, Mode)>),
}

/// How a local game begins, which decides what the first note says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opening {
    New,
    Restored,
}

pub(crate) fn play(mut options: Options, loaded: storage::LoadedPreferences) -> Result<(), String> {
    let config_path = loaded.path.clone();
    let session_path = storage::default_session_path(&config_path);
    if let Some(fen) = &options.fen {
        Position::from_fen(fen).map_err(|why| format!("bad --fen: {}", why))?;
    }
    // The command line can ask for the first game. After it, and after
    // every game, the home page decides.
    let requested_load = options
        .load
        .clone()
        .or_else(|| options.resume.then(|| session_path.clone()));
    let mut first = if let Some(intent) = options.online.take() {
        Some(Start::Choice(StartChoice::Online(
            intent,
            options.online_name.clone(),
        )))
    } else if let Some(path) = requested_load {
        Some(Start::Restored(Box::new(restore_game(
            storage::load_game(&path)?,
        )?)))
    } else {
        options
            .mode
            .map(|mode| Start::Choice(StartChoice::Mode(mode)))
    };

    let mut screen = Screen::from_options(&options, options.player_names.clone());
    // Held for as long as the program runs. Whatever was on the terminal
    // before comes back when this is dropped, however the program ends.
    let mut _fullscreen = Some(ui::Fullscreen::enter(&screen.theme));
    // Probe once up front even when another piece style was requested. The
    // in-game Piece control can switch to Auto later without querying the
    // terminal in the middle of raw event input.
    if screen.theme.live && !screen.theme.ascii {
        screen.image_protocol = ui::detect_image_protocol();
    }
    let mut account = AccountContext::load(
        &config_path,
        &options.server_url,
        options.online_name.clone(),
    );
    let mut session = Session {
        options,
        config_path,
        session_path,
        screen,
        last_preferences: loaded.preferences,
        backup_config_before_save: loaded.backup_before_save,
        persistence_error_reported: false,
        config_warning: loaded.warning,
        first_run: loaded.first_run,
    };

    loop {
        let start = match first.take() {
            Some(start) => start,
            None => match session.menu(&mut account)? {
                Some(choice) => Start::Choice(choice),
                None => return Ok(()),
            },
        };
        let ended = match start {
            Start::Choice(StartChoice::Online(intent, name)) => {
                // The online game sets up its own screen.
                _fullscreen = None;
                let mut online = session.options.clone();
                online.online = Some(intent);
                online.online_name = name;
                let ended = play_online(
                    online,
                    session.config_path.clone(),
                    session.last_preferences.clone(),
                )?;
                session.pick_up_saved_look();
                _fullscreen = Some(ui::Fullscreen::enter(&session.screen.theme));
                ended
            }
            Start::Choice(StartChoice::Mode(mode)) => {
                let start = match &session.options.fen {
                    Some(fen) => {
                        Position::from_fen(fen).map_err(|why| format!("bad --fen: {}", why))?
                    }
                    None => Position::startpos(),
                };
                let game =
                    Game::with_clock(start, session.options.clock, session.options.increment);
                session.play_local(game, mode, Opening::New)?
            }
            Start::Choice(StartChoice::Resume) => {
                let (game, mode) = restore_game(storage::load_game(&session.session_path)?)?;
                session.play_local(game, mode, Opening::Restored)?
            }
            Start::Restored(restored) => {
                let (game, mode) = *restored;
                session.play_local(game, mode, Opening::Restored)?
            }
        };
        if ended == Ended::Done {
            return Ok(());
        }
    }
}

/// Everything that lasts from one game to the next.
struct Session {
    options: Options,
    config_path: PathBuf,
    session_path: PathBuf,
    screen: Screen,
    last_preferences: storage::Preferences,
    backup_config_before_save: bool,
    persistence_error_reported: bool,
    /// Shown once, at the start of the first game.
    config_warning: Option<String>,
    first_run: bool,
}

impl Session {
    /// The home page, or the numbered prompt where there is no screen to draw
    /// it on. `None` when the player leaves.
    fn menu(&mut self, account: &mut AccountContext) -> Result<Option<StartChoice>, String> {
        if self.screen.theme.live && self.screen.theme.color {
            let seat_path = storage::default_online_session_path(&self.config_path);
            let mut info =
                HomeInfo::load(&self.session_path, &seat_path, &self.options, &self.screen);
            home::run(
                &mut self.screen,
                &mut info,
                account,
                &mut self.options,
                &self.config_path,
            )
        } else {
            let mut stdin = io::stdin().lock();
            ask_mode(
                &mut stdin,
                &mut self.screen,
                self.session_path.exists(),
                account,
            )
        }
    }

    /// An online game may have changed the look, and saved it; take it back
    /// for the games that follow.
    fn pick_up_saved_look(&mut self) {
        let saved = storage::load_preferences().preferences;
        if let Some(palette) = ui::palette(&saved.theme) {
            self.screen.theme.palette = palette;
            self.options.palette = palette;
        }
        if let Some(pieces) = ui::pieces_named(&saved.pieces) {
            self.screen.pieces = pieces;
            self.options.pieces = pieces;
        }
        if let Some(mode) = sound::Mode::named(&saved.sound) {
            if !self.options.hosted {
                self.screen.sound.set_mode(mode);
                self.options.sound = mode;
            }
        }
        self.screen.compact = saved.compact;
        self.options.compact = saved.compact;
        self.last_preferences = saved;
    }

    /// Save preferences that changed, and the game if it did.
    fn persist(&mut self, game: &mut Game, mode: Mode, saved_revision: &mut u64, limits: &Limits) {
        let mut preferences = runtime_preferences(&self.screen, game);
        preferences.engine_level = Level::of(limits).map_or_else(
            || self.last_preferences.engine_level.clone(),
            |level| level.name().to_string(),
        );
        // Against the engine the board turns to the player's side; only a
        // two-player game says how someone likes it.
        if mode != Mode::TwoPlayer {
            preferences.flipped = self.last_preferences.flipped;
        }
        if preferences != self.last_preferences {
            let backup = if self.backup_config_before_save {
                storage::backup_invalid_preferences(&self.config_path).map(|_| ())
            } else {
                Ok(())
            };
            if backup.is_ok() {
                self.backup_config_before_save = false;
            }
            let saved =
                backup.and_then(|_| storage::save_preferences(&self.config_path, &preferences));
            if let Err(error) = saved {
                if !self.persistence_error_reported {
                    self.screen.note(
                        self.screen
                            .theme
                            .warn(&format!("Preferences were not saved: {error}")),
                    );
                    self.persistence_error_reported = true;
                }
            }
            self.last_preferences = preferences;
        }
        if game.revision != *saved_revision {
            if let Err(error) = save_current_game(&self.session_path, game, mode) {
                if !self.persistence_error_reported {
                    self.screen
                        .note(self.screen.theme.warn(&format!("Autosave failed: {error}")));
                    self.persistence_error_reported = true;
                }
            }
            *saved_revision = game.revision;
        }
    }

    /// Play one local game until the player goes back to the menu.
    fn play_local(
        &mut self,
        mut game: Game,
        mut mode: Mode,
        opening: Opening,
    ) -> Result<Ended, String> {
        let hosted = self.options.hosted;
        let mut limits = self.options.limits;
        let mut engine = Search::new();
        {
            let screen = &mut self.screen;
            screen.online = None;
            screen.page = None;
            screen.clear_marks();
            screen.end_review();
            screen.cursor = None;
            screen.focused = UiAction::MoveInput;
            // The home page drew over whatever the last game left.
            screen.last_frame.clear();
            screen.last_size = None;
            screen.inline_drawn = false;
            screen.last_inline_board = None;
            screen.title = None;
            screen.redraw = true;
            screen.flipped = match mode {
                Mode::HumanWhite => false,
                Mode::HumanBlack => true,
                Mode::TwoPlayer => self.options.flipped,
            };
            screen.message = if let Some(warning) = self.config_warning.take() {
                vec![screen
                    .theme
                    .warn(&format!("{} — using safe defaults.", warning))]
            } else if opening == Opening::Restored {
                vec![screen.theme.good("Saved game restored.")]
            } else if std::mem::take(&mut self.first_run) {
                vec![screen.theme.accent(
                    "Welcome — type a move, click a piece, or use the arrow keys. Tab reaches the controls.",
                )]
            } else {
                vec![screen
                    .theme
                    .dim("Type a move, click a piece, or use the arrow keys and Enter.")]
            };
            if screen.pieces != ui::Pieces::Glyph && screen.theme.live {
                if let Some(tip) = ui::terminal_tip(screen.image_protocol) {
                    screen.message.push(screen.theme.dim(tip));
                }
            }
        }
        // Raw events are only appropriate while we own an interactive colour
        // terminal. Piped input retains the original line-oriented interface.
        let live = self.screen.theme.live && self.screen.theme.color;
        let mut terminal_input = TerminalInput::enter(live)?;
        let mut saved_revision = game.revision;

        loop {
            self.persist(&mut game, mode, &mut saved_revision, &limits);
            let screen = &mut self.screen;
            let flag_before = game.clock.flagged;
            if game.clock.tick() {
                if flag_before.is_none() && game.clock.flagged.is_some() {
                    game_ended_on_time(&mut game, screen);
                } else {
                    screen.draw_clock_tick(&game, mode, &limits);
                }
            }
            if screen.redraw {
                screen.redraw = false;
                screen.draw(&game, mode, &limits);
            }

            let finished = outcome(&game).is_some();
            let engine_to_move = mode.human().is_some_and(|human| human != game.pos.side);
            if !finished && !game.paused && engine_to_move {
                screen.end_review();
                engine_move(&mut game, &mut engine, &limits, screen);
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
                        let chosen = screen.focused;
                        if handle_ui_action(chosen, &mut game, &mut mode, screen) == After::Menu {
                            let _ = save_current_game(&self.session_path, &mut game, mode);
                            return Ok(Ended::Menu);
                        }
                        continue;
                    }
                    // Enter on the board cursor does what a click would.
                    if line.is_empty() && screen.page.is_none() {
                        if let Some(square) = screen.cursor {
                            if screen.reviewing.is_some() {
                                explain_review(&game, screen);
                            } else {
                                let finished = outcome(&game).is_some();
                                handle_board_click(&mut game, screen, Some(square), finished);
                            }
                            continue;
                        }
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
                    // Nothing on an open page can take focus, so Tab moves
                    // the page itself.
                    if screen.page.is_some() {
                        screen.scroll_page(reverse);
                    } else {
                        screen.move_focus(reverse);
                    }
                    continue;
                }
                InputAction::Arrow(direction) => {
                    if screen.page.is_some() {
                        screen.scroll_page(direction.backwards());
                    } else if screen.reviewing.is_some() {
                        screen.step_review(&game, direction.backwards());
                    } else if screen.focused != UiAction::MoveInput {
                        screen.move_focus(direction.backwards());
                    } else {
                        screen.move_cursor(&game, direction);
                    }
                    continue;
                }
                InputAction::Edge { end } => {
                    if screen.page.is_none() {
                        if end {
                            screen.end_review();
                        } else if !game.sans.is_empty() {
                            screen.review(&game, 0);
                        }
                    }
                    continue;
                }
                InputAction::Resize => {
                    screen.redraw = true;
                    continue;
                }
                InputAction::Tick => continue,
                InputAction::WindowFocus(focused) => {
                    screen.window_focused = focused;
                    continue;
                }
                InputAction::History { older } => {
                    // Page Up and Down read an open page, or step through
                    // the game.
                    if !screen.scroll_page(older) {
                        screen.step_review(&game, older);
                    }
                    continue;
                }
                InputAction::Scroll { older } => {
                    // The wheel reads an open page, or the move list.
                    if !screen.scroll_page(older) {
                        screen.scroll_history(&game, older);
                    }
                    continue;
                }
                InputAction::Cancel => {
                    step_back(screen);
                    continue;
                }
                InputAction::Click { column, row } => {
                    let flag_before = game.clock.flagged;
                    game.clock.tick();
                    if flag_before.is_none() && game.clock.flagged.is_some() {
                        game_ended_on_time(&mut game, screen);
                        continue;
                    }
                    if screen.page.take().is_some() {
                        screen.redraw = true;
                        continue;
                    }
                    if let Some(action) = screen.action_at(column, row) {
                        screen.focus(action);
                        if handle_ui_action(action, &mut game, &mut mode, screen) == After::Menu {
                            let _ = save_current_game(&self.session_path, &mut game, mode);
                            return Ok(Ended::Menu);
                        }
                        continue;
                    }
                    if let Some(ply) = screen.history_at(column, row) {
                        screen.review(&game, ply);
                        continue;
                    }
                    screen.confirming = None;
                    screen.cursor = None;
                    let square = screen.square_at(column, row);
                    if screen.reviewing.is_some() {
                        if square.is_some() {
                            explain_review(&game, screen);
                        }
                        continue;
                    }
                    let finished = outcome(&game).is_some();
                    handle_board_click(&mut game, screen, square, finished);
                    continue;
                }
                // Ctrl+C, or the end of piped input.
                InputAction::Quit => {
                    let _ = save_current_game(&self.session_path, &mut game, mode);
                    println!();
                    return Ok(Ended::Done);
                }
            };

            let flag_before = game.clock.flagged;
            game.clock.tick();
            if flag_before.is_none() && game.clock.flagged.is_some() {
                game_ended_on_time(&mut game, screen);
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
                    kind @ (PieceKind::Queen
                    | PieceKind::Rook
                    | PieceKind::Bishop
                    | PieceKind::Knight),
                ) = kind
                {
                    play_promotion(&mut game, screen, kind);
                    continue;
                }
            }
            let (word, rest) = split_command(input);
            if hosted && names_a_file(&word, rest) {
                screen.note(screen.theme.warn(HOSTED_FILES));
                continue;
            }

            match word.as_str() {
                "quit" | "exit" | "q" | "menu" => {
                    let _ = save_current_game(&self.session_path, &mut game, mode);
                    return Ok(Ended::Menu);
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
                        screen.end_review();
                        show_piece_moves(screen, &game.pos, rest);
                    }
                    continue;
                }
                "history" | "moveslist" => {
                    let lines = history_page(&screen.theme, &game);
                    screen.open("THE GAME SO FAR", lines);
                    continue;
                }
                "review" => {
                    start_review(&game, screen);
                    continue;
                }
                "live" | "end" => {
                    screen.end_review();
                    continue;
                }
                "pgn" => {
                    show_pgn(&game, screen);
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
                    screen.end_review();
                    if rest.is_empty() {
                        screen.note(screen.theme.dim("Use `import game.pgn`."));
                    } else if let Err(error) = import_pgn(Path::new(rest), &mut game, screen) {
                        screen.note(screen.theme.warn(&error));
                    }
                    continue;
                }
                "save" => {
                    let path = if rest.is_empty() {
                        self.session_path.clone()
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
                    screen.end_review();
                    let path = if rest.is_empty() {
                        self.session_path.clone()
                    } else {
                        PathBuf::from(rest)
                    };
                    if let Err(error) = load_saved_game(&path, &mut game, &mut mode, screen) {
                        screen.note(screen.theme.warn(&error));
                    }
                    continue;
                }
                "setup" | "config" => {
                    screen.open(
                        "LOCAL SETUP",
                        setup_lines(&screen.theme, &self.config_path, &self.session_path),
                    );
                    continue;
                }
                "name" => {
                    set_player_name(screen, rest);
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
                    set_theme(screen, rest);
                    continue;
                }
                "pieces" => {
                    set_pieces(screen, rest);
                    continue;
                }
                "sound" | "sounds" => {
                    set_sound(screen, rest);
                    continue;
                }
                "size" => {
                    set_size(screen, rest);
                    continue;
                }
                "hint" => {
                    screen.end_review();
                    hint(&game, &mut engine, &limits, screen);
                    continue;
                }
                "level" => {
                    set_level(&mut limits, screen, rest, hosted);
                    continue;
                }
                "time" => {
                    set_time(&mut limits, screen, rest, hosted);
                    continue;
                }
                "depth" => {
                    set_depth(&mut limits, screen, rest, hosted);
                    continue;
                }
                "undo" | "u" | "back" | "takeback" => {
                    screen.end_review();
                    undo(&mut game, mode, screen);
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
                        handle_ui_action(UiAction::Pause, &mut game, &mut mode, screen);
                    }
                    continue;
                }
                "draw" => {
                    handle_ui_action(UiAction::Draw, &mut game, &mut mode, screen);
                    continue;
                }
                "new" | "restart" => {
                    if confirm_new(&mut terminal_input, &game, screen)? {
                        game.restart();
                        screen.end_review();
                        screen.clear_marks();
                        screen.note(screen.theme.good("New game."));
                        screen.redraw = true;
                    }
                    continue;
                }
                "rematch" => {
                    if finished || confirm_new(&mut terminal_input, &game, screen)? {
                        rematch(&mut game, &mut mode, screen);
                        screen.redraw = true;
                    }
                    continue;
                }
                "resign" => {
                    if finished {
                        screen.note(screen.theme.dim("The game is already over."));
                    } else {
                        screen.end_review();
                        game.resigned = Some(game.pos.side);
                        game.changed();
                        game.clock.pause();
                        screen.analysis.clear();
                        screen.message.clear();
                        screen.sound.play(sound::Cue::GameEnd);
                        screen.redraw = true;
                    }
                    continue;
                }
                _ => {}
            }

            if screen.reviewing.is_some() {
                explain_review(&game, screen);
                continue;
            }
            if finished {
                screen.note(
                    screen
                        .theme
                        .dim("The game is over. Choose Rematch, Review, PGN or Menu."),
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

            make_move(&mut game, input, screen);
        }
    }
}

fn game_ended_on_time(game: &mut Game, screen: &mut Screen) {
    game.changed();
    screen.sound.play(sound::Cue::GameEnd);
    screen.clear_marks();
    screen.page = None;
    screen.redraw = true;
}

/// A move tried while an earlier position is on the board.
pub(crate) fn explain_review(game: &Game, screen: &mut Screen) {
    let Some(reviewing) = &screen.reviewing else {
        return;
    };
    let text = format!(
        "You are looking at {}. Press End to go back to the game.",
        move_name(game, reviewing.ply)
    );
    screen.note(screen.theme.dim(&text));
}

/// Escape: one step back from wherever the player is, ending on the Menu
/// button, from which Enter leaves the game. It never leaves on its own.
pub(crate) fn step_back(screen: &mut Screen) {
    screen.redraw = true;
    if screen.page.take().is_some() {
        return;
    }
    let pointing = screen.selected.take().is_some()
        || !screen.targets.is_empty()
        || !screen.promotions.is_empty()
        || screen.invalid.take().is_some();
    if pointing || screen.confirming.take().is_some() {
        screen.targets.clear();
        screen.captures.clear();
        screen.promotions.clear();
        return;
    }
    if screen.end_review() {
        return;
    }
    if screen.cursor.take().is_some() {
        return;
    }
    if screen.focused != UiAction::MoveInput {
        screen.focus_move_input();
        screen.message.clear();
        return;
    }
    screen.focus(UiAction::Menu);
    let text = if screen.online.is_some() {
        "Enter on Menu leaves this game for the menu; Escape stays."
    } else {
        "Enter on Menu goes back to the menu; the game is saved. Escape stays."
    };
    screen.note(screen.theme.dim(text));
}
