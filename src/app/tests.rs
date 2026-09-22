//! Interaction tests for the screen, hit testing, and both game loops.

use std::time::{Duration, Instant};

use chess::ui::Theme;
use chess::{sound, storage, ui};
use chess_core::board::{self, Color, Piece, PieceKind, Position};
use chess_core::game::{outcome, Game, GameClock, Outcome};
use chess_core::san::parse_move;
use chess_core::search::Limits;
use chess_protocol::Lobby;

use chess::input::Direction;

use crate::app::actions::{
    cycle_pieces, engine_limits, flip_board, handle_board_click, handle_ui_action, set_depth,
    set_time, sound_after_move, toggle_size, After,
};
use crate::app::cli::{Mode, OnlineIntent, Options, HOSTED_MOVETIME};
use crate::app::local::step_back;
use crate::app::online::{online_board_click, online_preferences, show_lobby, unix_time_ms};
use crate::app::pages::import_pgn;
use crate::app::saves::{restore_game, saved_game};
use crate::app::screen::{
    changed_frame_rows, history_rows, move_name, ActionHitbox, BoardHitbox, ConnectionDisplay,
    OnlineDisplay, Screen, UiAction,
};
use crate::app::settings::Level;

fn hitbox(flipped: bool) -> BoardHitbox {
    BoardHitbox {
        left: 10,
        top: 4,
        cell_w: 8,
        cell_h: 4,
        flipped,
    }
}

fn screen() -> Screen {
    Screen {
        cols: 100,
        rows: 40,
        wide_panel: true,
        indent: String::new(),
        redraw: false,
        ..Screen::new(
            Theme::new(true, false, false, ui::THEMES[0].1),
            sound::Player::new(sound::Mode::Off),
            ui::Pieces::Glyph,
            ["Player 1".to_string(), "Player 2".to_string()],
            false,
        )
    }
}

#[test]
fn board_hitbox_maps_white_orientation() {
    let board = hitbox(false);
    assert_eq!(board.square_at(10, 4), Some(board::H8 - 7)); // a8
    assert_eq!(board.square_at(73, 4), Some(board::H8));
    assert_eq!(board.square_at(10, 35), Some(board::A1));
    assert_eq!(board.square_at(73, 35), Some(board::H1));
}

#[test]
fn board_hitbox_maps_flipped_orientation() {
    let board = hitbox(true);
    assert_eq!(board.square_at(10, 4), Some(board::H1));
    assert_eq!(board.square_at(73, 4), Some(board::A1));
    assert_eq!(board.square_at(10, 35), Some(board::H8));
    assert_eq!(board.square_at(73, 35), Some(board::H8 - 7)); // a8
}

#[test]
fn board_hitbox_rejects_labels_and_outside_cells() {
    let board = hitbox(false);
    assert_eq!(board.square_at(9, 4), None);
    assert_eq!(board.square_at(10, 3), None);
    assert_eq!(board.square_at(74, 4), None);
    assert_eq!(board.square_at(10, 36), None);
}

#[test]
fn clicking_a_piece_then_a_target_plays_the_move() {
    let mut game = Game::new(Position::startpos());
    let mut screen = screen();
    let e2 = board::parse_square("e2").unwrap();
    let e3 = board::parse_square("e3").unwrap();
    let e4 = board::parse_square("e4").unwrap();

    handle_board_click(&mut game, &mut screen, Some(e2), false);
    assert_eq!(screen.selected, Some(e2));
    assert!(screen.targets.contains(&e3));
    assert!(screen.targets.contains(&e4));

    handle_board_click(&mut game, &mut screen, Some(e4), false);
    assert_eq!(game.pos.at(e2), None);
    assert_eq!(
        game.pos.at(e4),
        Some(Piece::new(Color::White, PieceKind::Pawn))
    );
    assert_eq!(game.pos.side, Color::Black);
    assert_eq!(screen.selected, None);
    assert!(screen.targets.is_empty());
}

#[test]
fn clicking_another_friendly_piece_reselects() {
    let mut game = Game::new(Position::startpos());
    let mut screen = screen();
    let e2 = board::parse_square("e2").unwrap();
    let g1 = board::parse_square("g1").unwrap();
    let f3 = board::parse_square("f3").unwrap();

    handle_board_click(&mut game, &mut screen, Some(e2), false);
    handle_board_click(&mut game, &mut screen, Some(g1), false);
    assert_eq!(screen.selected, Some(g1));
    assert_eq!(screen.targets.len(), 2);
    assert!(screen.targets.contains(&f3));
    assert!(screen.targets.contains(&board::parse_square("h3").unwrap()));
}

#[test]
fn capture_targets_are_distinct_from_quiet_moves() {
    let position = Position::from_fen("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1").unwrap();
    let mut game = Game::new(position);
    let mut screen = screen();
    let e4 = board::parse_square("e4").unwrap();
    let d5 = board::parse_square("d5").unwrap();
    let e5 = board::parse_square("e5").unwrap();

    handle_board_click(&mut game, &mut screen, Some(e4), false);

    assert!(screen.targets.contains(&d5));
    assert!(screen.targets.contains(&e5));
    assert_eq!(screen.captures, vec![d5]);
}

#[test]
fn invalid_destination_gets_local_feedback() {
    let mut game = Game::new(Position::startpos());
    let mut screen = screen();
    let e2 = board::parse_square("e2").unwrap();
    let e5 = board::parse_square("e5").unwrap();

    handle_board_click(&mut game, &mut screen, Some(e2), false);
    handle_board_click(&mut game, &mut screen, Some(e5), false);

    assert_eq!(screen.invalid, Some(e5));
    assert_eq!(screen.selected, Some(e2));
    assert!(!screen.message.is_empty());
}

#[test]
fn promotion_menu_accepts_a_piece_click() {
    let position = Position::from_fen("4k3/P7/8/8/8/8/8/4K3 w - - 0 1").unwrap();
    let mut game = Game::new(position);
    let mut screen = screen();
    let a7 = board::parse_square("a7").unwrap();
    let a8 = board::parse_square("a8").unwrap();
    let bishop_choice = board::parse_square("a6").unwrap();

    handle_board_click(&mut game, &mut screen, Some(a7), false);
    handle_board_click(&mut game, &mut screen, Some(a8), false);
    assert_eq!(screen.promotions.len(), 4);

    handle_board_click(&mut game, &mut screen, Some(bishop_choice), false);
    assert_eq!(
        game.pos.at(a8),
        Some(Piece::new(Color::White, PieceKind::Bishop))
    );
    assert!(screen.promotions.is_empty());
}

fn cue_after(fen: &str, notation: &str) -> sound::Cue {
    let mut game = Game::new(Position::from_fen(fen).unwrap());
    let movement = match parse_move(&game.pos, notation) {
        Ok(movement) => movement,
        Err(_) => panic!("test move {notation} did not parse"),
    };
    game.play(movement);
    sound_after_move(&game)
}

#[test]
fn moves_choose_their_most_meaningful_sound() {
    assert_eq!(cue_after(board::START_FEN, "e4"), sound::Cue::Move);
    assert_eq!(
        cue_after("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "exd5"),
        sound::Cue::Capture
    );
    assert_eq!(
        cue_after("4k3/8/8/8/8/8/4R3/4K3 w - - 0 1", "Re7+"),
        sound::Cue::Check
    );
    assert_eq!(
        cue_after("4k3/8/8/8/8/8/8/4K2R w K - 0 1", "O-O"),
        sound::Cue::Castle
    );
    assert_eq!(
        cue_after("8/P6k/8/8/8/8/8/4K3 w - - 0 1", "a8=Q"),
        sound::Cue::Promotion
    );
}

#[test]
fn clock_flags_the_side_that_runs_out_of_time() {
    let mut clock = GameClock::new(Some(Duration::from_secs(60)), Duration::ZERO, Color::White);
    clock.remaining[Color::White.index()] = Duration::from_millis(10);
    clock.running = Some((Color::White, Instant::now() - Duration::from_millis(20)));

    assert!(clock.tick());
    assert_eq!(clock.flagged, Some(Color::White));
    assert_eq!(clock.format(Color::White), "0:00");
}

#[test]
fn clock_adds_increment_and_switches_sides() {
    let mut clock = GameClock::new(
        Some(Duration::from_secs(60)),
        Duration::from_secs(2),
        Color::White,
    );
    clock.complete_move(Color::White, Color::Black);

    assert!(clock.remaining[Color::White.index()] > Duration::from_secs(61));
    assert_eq!(clock.running.map(|(color, _)| color), Some(Color::Black));
}

#[test]
fn pausing_stops_and_resumes_the_active_clock() {
    let mut game = Game::with_clock(
        Position::startpos(),
        Some(Duration::from_secs(60)),
        Duration::ZERO,
    );

    game.toggle_pause();
    assert!(game.paused);
    assert!(game.clock.running.is_none());

    game.toggle_pause();
    assert!(!game.paused);
    assert_eq!(
        game.clock.running.map(|(color, _)| color),
        Some(Color::White)
    );
}

#[test]
fn saved_game_round_trips_moves_clock_mode_and_pause() {
    let mut game = Game::with_clock(
        Position::startpos(),
        Some(Duration::from_secs(300)),
        Duration::from_secs(2),
    );
    for notation in ["e4", "e5", "Nf3"] {
        let movement = parse_move(&game.pos, notation).ok().unwrap();
        game.play(movement);
    }
    game.toggle_pause();

    let saved = saved_game(&mut game, Mode::TwoPlayer);
    let (restored, mode) = restore_game(saved).unwrap();

    assert_eq!(mode, Mode::TwoPlayer);
    assert_eq!(restored.pos.to_fen(), game.pos.to_fen());
    assert_eq!(restored.sans, game.sans);
    assert!(restored.paused);
    assert_eq!(restored.clock.increment, Duration::from_secs(2));
    assert!(restored.clock.running.is_none());
}

#[test]
fn pgn_import_replays_the_main_line() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("game.pgn");
    std::fs::write(
        &path,
        "[Event \"Test\"]\n\n1. e4 e5 2. Nf3 (2. Bc4) Nc6 *\n",
    )
    .unwrap();
    let mut game = Game::new(Position::startpos());
    let mut screen = screen();

    import_pgn(&path, &mut game, &mut screen).unwrap();

    assert_eq!(game.sans, ["e4", "e5", "Nf3", "Nc6"]);
    assert_eq!(game.pos.side, Color::White);
}

#[test]
fn draw_offer_can_be_accepted_after_the_offering_move() {
    let mut game = Game::new(Position::startpos());
    let mut screen = screen();
    screen.theme.live = true;

    assert_eq!(
        handle_ui_action(UiAction::Draw, &mut game, &mut Mode::TwoPlayer, &mut screen),
        After::Stay
    );
    assert_eq!(game.draw_offer, Some(Color::White));

    let movement = parse_move(&game.pos, "e4").ok().unwrap();
    game.play(movement);
    handle_ui_action(UiAction::Draw, &mut game, &mut Mode::TwoPlayer, &mut screen);
    assert!(game.agreed_draw);
    assert!(matches!(outcome(&game), Some(Outcome::DrawAgreement)));
}

#[test]
fn destructive_mouse_action_requires_confirmation() {
    let mut game = Game::new(Position::startpos());
    let mut screen = screen();
    screen.theme.live = true;

    handle_ui_action(
        UiAction::Resign,
        &mut game,
        &mut Mode::TwoPlayer,
        &mut screen,
    );
    assert_eq!(screen.confirming, Some(UiAction::Resign));
    assert_eq!(game.resigned, None);

    handle_ui_action(
        UiAction::Resign,
        &mut game,
        &mut Mode::TwoPlayer,
        &mut screen,
    );
    assert_eq!(game.resigned, Some(Color::White));
}

#[test]
fn compact_buttons_wrap_without_losing_hit_targets() {
    let game = Game::new(Position::startpos());
    let screen = screen();
    let buttons = screen.render_buttons(&screen.game_buttons(&game, Mode::TwoPlayer), 24);

    assert!(buttons.lines.len() >= 2);
    assert_eq!(buttons.actions.len(), 9); // Undo starts disabled.
    assert!(buttons
        .actions
        .iter()
        .all(|target| target.left + target.width <= 24));
}

#[test]
fn short_side_panel_gives_controls_priority_over_history() {
    let game = Game::new(Position::startpos());
    let screen = screen();
    let panel = screen.panel(&game, Mode::TwoPlayer, &Limits::default(), 10, 24);

    assert_eq!(panel.lines.len(), 10);
    assert_eq!(panel.history_capacity, 0);
    assert!(panel.actions.iter().all(|target| target.row < 8));
}

#[test]
fn keyboard_focus_follows_the_visible_control_order() {
    let mut screen = screen();
    screen.action_hitboxes = vec![
        ActionHitbox {
            left: 0,
            top: 0,
            width: 8,
            action: UiAction::MoveInput,
        },
        ActionHitbox {
            left: 10,
            top: 0,
            width: 8,
            action: UiAction::Draw,
        },
        ActionHitbox {
            left: 20,
            top: 0,
            width: 12,
            action: UiAction::ToggleSize,
        },
    ];

    screen.move_focus(false);
    assert_eq!(screen.focused, UiAction::Draw);
    screen.move_focus(false);
    assert_eq!(screen.focused, UiAction::ToggleSize);
    screen.move_focus(false);
    assert_eq!(screen.focused, UiAction::MoveInput);
    screen.move_focus(true);
    assert_eq!(screen.focused, UiAction::ToggleSize);
}

#[test]
fn view_controls_cycle_without_a_typed_command() {
    let mut screen = screen();
    assert!(!screen.compact);
    assert_eq!(screen.pieces, ui::Pieces::Glyph);

    toggle_size(&mut screen);
    cycle_pieces(&mut screen);
    flip_board(&mut screen);

    assert!(screen.compact);
    assert_eq!(screen.pieces, ui::Pieces::Art);
    assert!(screen.flipped);
    assert!(screen.redraw);
}

#[test]
fn move_history_scrolls_away_from_and_back_to_the_latest_move() {
    let mut game = Game::new(Position::startpos());
    for notation in ["e4", "e5", "Nf3", "Nc6"] {
        let movement = parse_move(&game.pos, notation).ok().unwrap();
        game.play(movement);
    }
    let mut screen = screen();
    screen.history_capacity = 1;

    screen.scroll_history(&game, true);
    assert_eq!(screen.history_offset, 1);
    screen.scroll_history(&game, false);
    assert_eq!(screen.history_offset, 0);
}

#[test]
fn a_long_page_scrolls_instead_of_being_cut() {
    let mut screen = screen();
    screen.theme = Theme::new(true, false, true, ui::THEMES[0].1);
    screen.rows = 24;
    let total = 60;
    screen.open(
        "HELP",
        (1..=total).map(|line| format!("line {line}")).collect(),
    );
    let capacity = screen.page_window(total);
    screen.page_capacity = capacity;
    assert!(capacity > 0 && capacity < total);

    let shown = |screen: &Screen| screen.page_body(screen.page.as_ref().expect("the page is open"));
    let top = shown(&screen);
    assert!(top.iter().any(|line| line.contains("line 1")));
    assert!(top.iter().any(|line| line.contains("of 60")));
    assert!(!top.iter().any(|line| line.contains("line 60")));

    assert!(screen.scroll_page(false));
    assert!(screen.page_offset > 0);

    // Past the end is the end, and the last line is reachable from there.
    screen.page_offset = total;
    let bottom = shown(&screen);
    assert!(bottom.iter().any(|line| line.contains("line 60")));

    while screen.page_offset > 0 {
        screen.scroll_page(true);
    }
    assert!(shown(&screen).iter().any(|line| line.contains("line 1")));
}

#[test]
fn a_page_that_fits_says_nothing_about_scrolling() {
    let mut screen = screen();
    screen.theme = Theme::new(true, false, true, ui::THEMES[0].1);
    screen.rows = 40;
    screen.open("FEN", vec!["one".to_string(), "two".to_string()]);
    screen.page_capacity = screen.page_window(2);

    let body = screen.page_body(screen.page.as_ref().expect("the page is open"));
    assert!(!body.iter().any(|line| line.contains("of 2")));
}

#[test]
fn a_finished_game_keeps_its_move_list_beside_the_board() {
    let mut game = Game::new(Position::startpos());
    for notation in ["e4", "e5", "Bc4", "Nc6", "Qh5", "Nf6", "Qxf7"] {
        let movement = parse_move(&game.pos, notation).ok().unwrap();
        game.play(movement);
    }
    assert!(outcome(&game).is_some());

    let screen = screen();
    let panel = screen.panel(&game, Mode::TwoPlayer, &Limits::default(), 18, 30);
    let text = panel.lines.join("\n");
    assert!(text.contains("GAME OVER"));
    assert!(text.contains("CHECKMATE"));
    assert!(panel.history_capacity > 0);
    assert!(text.contains("Qxf7#"));
    // The controls stay where they were while the game was running.
    assert!(panel
        .actions
        .iter()
        .all(|target| target.row + 1 < panel.lines.len()));
}

#[test]
fn a_short_window_keeps_the_status_line_before_the_controls() {
    let game = Game::new(Position::startpos());
    let mut screen = screen();
    screen.cols = 46;
    screen.rows = 16;
    screen.wide_panel = false;
    screen.message = vec!["not a legal move".to_string()];
    let body = screen.board_body(&game, Mode::TwoPlayer, &Limits::default());

    // The bar above and the command line below both have to fit as well.
    assert!(body.lines.len() + 2 <= screen.rows);
    let text = body.lines.join("\n");
    assert!(text.contains("to move"));
    assert!(text.contains("not a legal move"));
    // Nothing may be clickable on a row that was never drawn.
    assert!(body
        .actions
        .iter()
        .all(|target| target.row < body.lines.len()));
}

#[test]
fn the_top_bar_never_runs_its_two_halves_together() {
    let game = Game::new(Position::startpos());
    let mut screen = screen();
    for cols in 30..=120 {
        screen.cols = cols;
        let hint = screen.bar_hint(&game);
        let bar = screen.theme.bar("C H E S S", hint, cols);
        assert!(ui::width(&bar) <= cols, "the bar overflows at {cols}");
        if cols >= ui::width("C H E S S") + 6 + ui::width(hint) {
            assert!(
                bar.contains("C H E S S  ") || ui::width(hint) == 0,
                "no gap after the title at {cols}"
            );
        }
    }
}

#[test]
fn frame_diff_only_repaints_changed_rows() {
    let previous = vec![
        "title".to_string(),
        "board".to_string(),
        "status".to_string(),
    ];
    let next = vec![
        "title".to_string(),
        "board".to_string(),
        "new status".to_string(),
    ];

    assert_eq!(changed_frame_rows(&previous, &next, false), vec![2]);
    assert_eq!(changed_frame_rows(&previous, &next, true), vec![0, 1, 2]);
}

#[test]
fn clock_repaint_restores_the_prompt_cursor() {
    let mut screen = screen();
    screen.cols = 80;
    screen.rows = 32;
    screen.last_prompt = Some("    White › e4".to_string());

    assert_eq!(screen.prompt_cursor_escape(), "\x1b[32;15H\x1b[?25h");

    screen.focused = UiAction::Draw;
    assert_eq!(screen.prompt_cursor_escape(), "\x1b[?25l");
}

#[test]
fn online_cli_parses_create_join_and_server_url() {
    let preferences = storage::Preferences::default();
    let create = Options::parse(
        [
            "online",
            "create",
            "--server",
            "wss://play.example/ws",
            "--name",
            "Ada",
        ]
        .into_iter()
        .map(str::to_string),
        &preferences,
    )
    .unwrap()
    .unwrap();
    assert_eq!(create.online, Some(OnlineIntent::Create));
    assert_eq!(create.server_url, "wss://play.example/ws");
    assert_eq!(create.online_name, "Ada");

    let join = Options::parse(
        ["online", "join", "ab12cd"].into_iter().map(str::to_string),
        &preferences,
    )
    .unwrap()
    .unwrap();
    assert_eq!(join.online, Some(OnlineIntent::Join("AB12CD".to_string())));
}

#[test]
fn online_click_builds_a_request_without_advancing_locally() {
    let game = Game::new(Position::startpos());
    let mut screen = screen();
    let e2 = board::parse_square("e2").unwrap();
    let e4 = board::parse_square("e4").unwrap();

    assert!(online_board_click(&game, &mut screen, Some(e2)).is_none());
    let movement = online_board_click(&game, &mut screen, Some(e4)).unwrap();

    assert_eq!(movement.to_uci(), "e2e4");
    assert_eq!(
        game.pos.at(e2),
        Some(Piece::new(Color::White, PieceKind::Pawn))
    );
    assert_eq!(game.pos.at(e4), None);
}

#[test]
fn online_game_over_status_outranks_invite_and_disconnect_states() {
    let mut game = Game::new(Position::startpos());
    game.resigned = Some(Color::Black);
    let mut screen = screen();
    screen.online = Some(OnlineDisplay {
        connection: ConnectionDisplay::Connected,
        invite_code: Some("ABC123".to_string()),
        your_side: Some(Color::White),
        white_connected: true,
        black_connected: false,
        reconnect_deadline_ms: Some(unix_time_ms() + 30_000),
        move_pending: false,
        failure_help: None,
        seeking: false,
        lobby: None,
        rematch_offer: None,
    });

    let status = screen.state_line(&game);
    assert!(status.contains("resigns"));
    assert!(status.contains("Rematch, Review, PGN or Menu"));
    assert!(!status.contains("INVITE"));
    assert!(!status.contains("OPPONENT OFFLINE"));
}

#[test]
fn a_player_looking_for_a_stranger_is_told_when_they_are_alone() {
    let game = Game::new(Position::startpos());
    let mut screen = screen();
    screen.theme.live = true;
    screen.online = Some(OnlineDisplay {
        connection: ConnectionDisplay::Connected,
        invite_code: None,
        your_side: None,
        white_connected: false,
        black_connected: false,
        reconnect_deadline_ms: None,
        move_pending: true,
        failure_help: None,
        seeking: true,
        lobby: None,
        rematch_offer: None,
    });
    let alone = Lobby {
        online: 1,
        seeking: 1,
    };

    show_lobby(alone, &mut screen);
    assert!(screen.message[0].contains("No one else is online right now."));
    assert!(screen.state_line(&game).contains("no one else online"));

    // Said once, not again with every update that changes nothing.
    screen.message.clear();
    show_lobby(alone, &mut screen);
    assert!(screen.message.is_empty());

    show_lobby(
        Lobby {
            online: 3,
            seeking: 1,
        },
        &mut screen,
    );
    assert!(screen.message[0].contains("2 others online now."));
    assert!(screen.state_line(&game).contains("2 others online"));
}

#[test]
fn online_draw_offer_has_accept_and_decline_controls() {
    let mut game = Game::new(Position::startpos());
    game.draw_offer = Some(Color::Black);
    let mut screen = screen();
    screen.online = Some(OnlineDisplay {
        connection: ConnectionDisplay::Connected,
        invite_code: None,
        your_side: Some(Color::White),
        white_connected: true,
        black_connected: true,
        reconnect_deadline_ms: None,
        move_pending: false,
        failure_help: None,
        seeking: false,
        lobby: None,
        rematch_offer: None,
    });

    let buttons = screen.game_buttons(&game, Mode::TwoPlayer);
    assert!(buttons
        .iter()
        .any(|button| button.label == "Accept" && button.enabled));
    assert!(buttons
        .iter()
        .any(|button| button.label == "Decline" && button.enabled));
}

#[test]
fn online_session_does_not_overwrite_local_identity_clock_or_orientation() {
    let previous = storage::Preferences {
        white_name: "Local White".to_string(),
        black_name: "Local Black".to_string(),
        flipped: false,
        clock_enabled: true,
        clock_minutes: 15.0,
        increment_seconds: 10.0,
        engine_level: "club".to_string(),
        ..storage::Preferences::default()
    };
    let mut screen = screen();
    screen.player_names = ["Remote White".to_string(), "Remote Black".to_string()];
    screen.flipped = true;
    let game = Game::with_clock(
        Position::startpos(),
        Some(Duration::from_secs(60)),
        Duration::ZERO,
    );

    let saved = online_preferences(&previous, &screen, &game);

    assert_eq!(saved.white_name, "Local White");
    assert_eq!(saved.black_name, "Local Black");
    assert!(!saved.flipped);
    assert_eq!(saved.clock_minutes, 15.0);
    assert_eq!(saved.increment_seconds, 10.0);
    assert_eq!(saved.engine_level, previous.engine_level);
}

#[test]
fn a_hosted_engine_never_thinks_for_long() {
    let mut screen = screen();
    // Keep notes on screen rather than printed, so they can be read back.
    screen.theme.live = true;
    let mut limits = Limits::default();

    set_time(&mut limits, &mut screen, "99999", true);
    assert_eq!(limits.movetime, Some(HOSTED_MOVETIME));
    assert!(screen.message[0].contains("the most it gets over SSH"));
    set_time(&mut limits, &mut screen, "2", true);
    assert_eq!(limits.movetime, Some(Duration::from_secs(2)));

    // A depth is still searched for at most the hosted limit.
    set_depth(&mut limits, &mut screen, "64", true);
    assert_eq!(limits.depth, 64);
    assert_eq!(limits.movetime, Some(HOSTED_MOVETIME));
    assert!(screen.message[0].contains("at most 5s a move over SSH"));
    set_depth(&mut limits, &mut screen, "30", false);
    assert_eq!(limits.movetime, None);

    // Too big to be a duration is refused, not a crash.
    set_time(&mut limits, &mut screen, "1e300", false);
    assert_eq!(limits.movetime, None);
    assert!(screen.message[0].contains("not a number of seconds"));
}

fn game_after(moves: &[&str]) -> Game {
    let mut game = Game::new(Position::startpos());
    for notation in moves {
        let movement = parse_move(&game.pos, notation).ok().unwrap();
        game.play(movement);
    }
    game
}

#[test]
fn review_steps_through_the_game_and_back_to_it() {
    let game = game_after(&["e4", "e5", "Nf3", "Nc6"]);
    let mut screen = screen();

    // Stepping back from the live game starts one move back.
    screen.step_review(&game, true);
    let reviewing = screen.reviewing.as_ref().unwrap();
    assert_eq!(reviewing.ply, 3);
    assert_eq!(
        reviewing.pos.at(board::sq(2, 5)),
        None,
        "Nc6 not played yet"
    );
    assert_eq!(move_name(&game, 3), "2. Nf3");
    assert_eq!(move_name(&game, 4), "2... Nc6");
    assert_eq!(move_name(&game, 0), "the start");

    screen.review(&game, 0);
    screen.step_review(&game, true);
    assert_eq!(
        screen.reviewing.as_ref().unwrap().ply,
        0,
        "the start is the end"
    );
    let view = screen.board_view(&game);
    assert_eq!(view.last, None);
    assert!(screen.state_line(&game).contains("VIEWING"));

    // Stepping on past the last move is the live game again.
    screen.review(&game, 3);
    screen.step_review(&game, false);
    assert!(screen.reviewing.is_none());
    assert!(!screen.end_review(), "nothing left to end");
}

#[test]
fn moves_in_the_list_are_clickable_by_side() {
    let game = game_after(&["e4", "e5", "Nf3"]);
    let rows = history_rows(&game);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].targets(), vec![(0, 12, 1), (12, 3, 2)]);
    assert_eq!(rows[1].targets(), vec![(0, 12, 3)]);

    let mut screen = screen();
    let panel = screen.panel(&game, Mode::TwoPlayer, &Limits::default(), 24, 36);
    let white = panel
        .history
        .iter()
        .find(|hit| hit.ply == 1)
        .copied()
        .expect("the first move can be clicked");
    screen.history_hits = panel.history;
    let at = |column: usize| screen.history_at(column as u16, white.row as u16);
    assert_eq!(at(white.left + 3), Some(1));
    assert_eq!(at(white.left + 12), Some(2));
    assert_eq!(at(white.left + 20), None);
}

#[test]
fn the_cursor_starts_where_the_eye_is_and_follows_the_board() {
    let mut game = game_after(&[]);
    let mut screen = screen();
    screen.move_cursor(&game, Direction::Up);
    assert_eq!(screen.cursor, Some(board::sq(4, 1)), "e2 first");
    screen.move_cursor(&game, Direction::Up);
    screen.move_cursor(&game, Direction::Up);
    assert_eq!(screen.cursor, Some(board::sq(4, 3)));

    // Enter on the cursor is a click: pick up, then put down.
    handle_board_click(&mut game, &mut screen, Some(board::sq(4, 1)), false);
    assert_eq!(screen.selected, Some(board::sq(4, 1)));
    let cursor = screen.cursor;
    handle_board_click(&mut game, &mut screen, cursor, false);
    assert_eq!(game.sans, ["e4"]);

    // With the board turned round, up the screen is down the ranks.
    screen.flipped = true;
    screen.cursor = Some(board::sq(4, 4));
    screen.move_cursor(&game, Direction::Up);
    assert_eq!(screen.cursor, Some(board::sq(4, 3)));
    screen.move_cursor(&game, Direction::Right);
    assert_eq!(screen.cursor, Some(board::sq(3, 3)));
    // It stops at the edge: turned round, a1 is the top right corner.
    screen.cursor = Some(board::sq(0, 0));
    screen.move_cursor(&game, Direction::Right);
    screen.move_cursor(&game, Direction::Up);
    assert_eq!(screen.cursor, Some(board::sq(0, 0)));
}

#[test]
fn escape_steps_back_one_thing_at_a_time_and_never_leaves() {
    let game = game_after(&["e4"]);
    let mut screen = screen();
    screen.theme.live = true;
    screen.cursor = Some(board::sq(4, 3));
    screen.review(&game, 0);
    screen.cursor = Some(board::sq(4, 3));
    screen.selected = Some(board::sq(4, 3));

    step_back(&mut screen);
    assert!(screen.selected.is_none() && screen.reviewing.is_some());
    step_back(&mut screen);
    assert!(screen.reviewing.is_none() && screen.cursor.is_some());
    step_back(&mut screen);
    assert!(screen.cursor.is_none());
    assert_eq!(screen.focused, UiAction::MoveInput);
    step_back(&mut screen);
    assert_eq!(
        screen.focused,
        UiAction::Menu,
        "Menu is chosen, not pressed"
    );
    step_back(&mut screen);
    assert_eq!(screen.focused, UiAction::MoveInput);
}

#[test]
fn the_engine_spends_its_clock_like_a_player() {
    let level = Level::Strong.limits();
    let untimed = game_after(&[]);
    assert_eq!(engine_limits(&level, &untimed).movetime, level.movetime);

    let rapid = Game::with_clock(
        Position::startpos(),
        Some(Duration::from_secs(600)),
        Duration::ZERO,
    );
    // Ten minutes is plenty for its three seconds.
    assert_eq!(engine_limits(&level, &rapid).movetime, level.movetime);

    let bullet = Game::with_clock(
        Position::startpos(),
        Some(Duration::from_secs(60)),
        Duration::ZERO,
    );
    let budget = engine_limits(&level, &bullet).movetime.unwrap();
    assert!(budget <= Duration::from_secs(2), "{budget:?}");
    assert!(budget >= Duration::from_millis(50));
}

#[test]
fn rematch_buttons_follow_the_offer() {
    let mut game = game_after(&["f3", "e5", "g4", "Qh4#"]);
    let mut screen = screen();
    screen.online = Some(OnlineDisplay {
        connection: ConnectionDisplay::Connected,
        invite_code: None,
        your_side: Some(Color::White),
        white_connected: true,
        black_connected: true,
        reconnect_deadline_ms: None,
        move_pending: false,
        failure_help: None,
        seeking: false,
        lobby: None,
        rematch_offer: None,
    });
    let labels = |screen: &Screen, game: &Game| -> Vec<(&'static str, bool)> {
        screen
            .game_over_buttons(game)
            .iter()
            .map(|spec| (spec.label, spec.enabled))
            .collect()
    };
    assert!(labels(&screen, &game).contains(&("Rematch", true)));
    assert!(labels(&screen, &game).contains(&("Review", true)));

    screen.online.as_mut().unwrap().rematch_offer = Some(Color::Black);
    let offered = labels(&screen, &game);
    assert!(offered.contains(&("Accept rematch", true)));
    assert!(offered.contains(&("Decline", true)));
    assert!(screen.state_line(&game).contains("asks for a rematch"));

    screen.online.as_mut().unwrap().rematch_offer = Some(Color::White);
    assert!(labels(&screen, &game).contains(&("Rematch offered", false)));

    // Nobody to play once the opponent has gone.
    let online = screen.online.as_mut().unwrap();
    online.rematch_offer = None;
    online.black_connected = false;
    assert!(labels(&screen, &game).contains(&("Rematch", false)));

    game.restart();
    assert!(labels(&screen, &game).contains(&("Review", false)));
}

#[test]
fn the_window_title_says_whose_move_it_is() {
    let game = game_after(&["e4"]);
    let mut screen = screen();
    assert_eq!(
        screen.title_text(&game, Mode::HumanBlack),
        "Your move \u{2014} chess"
    );
    assert_eq!(
        screen.title_text(&game, Mode::HumanWhite),
        "Engine's move \u{2014} chess"
    );
    assert_eq!(
        screen.title_text(&game, Mode::TwoPlayer),
        "Black to move \u{2014} chess"
    );
    screen.online = Some(OnlineDisplay {
        connection: ConnectionDisplay::Connected,
        invite_code: None,
        your_side: Some(Color::Black),
        white_connected: true,
        black_connected: true,
        reconnect_deadline_ms: None,
        move_pending: false,
        failure_help: None,
        seeking: false,
        lobby: None,
        rematch_offer: None,
    });
    let timed = {
        let mut game = Game::with_clock(
            Position::startpos(),
            Some(Duration::from_secs(300)),
            Duration::ZERO,
        );
        let e4 = parse_move(&game.pos, "e4").ok().unwrap();
        game.play(e4);
        game
    };
    assert!(screen
        .title_text(&timed, Mode::TwoPlayer)
        .starts_with("Your move · 5:00"));
    let mate = game_after(&["f3", "e5", "g4", "Qh4#"]);
    assert!(screen.title_text(&mate, Mode::TwoPlayer).contains("0-1"));
}
