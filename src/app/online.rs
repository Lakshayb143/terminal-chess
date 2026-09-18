//! Online play: the server session, transport events, and the online loop.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chess::client::{OnlineClient, TransportEvent};
use chess::input::{Action as InputAction, TerminalInput};
use chess::ui::Theme;
use chess::{sound, storage, ui};
use chess_core::board::{self, Color, Move, MoveKind, PieceKind, Position};
use chess_core::game::{outcome, Game};
use chess_core::movegen::generate_legal;
use chess_core::san::{parse_move, ParseError};
use chess_core::search::Limits;
use chess_protocol::{
    ClientCommand, ErrorCode, FinishReason, GameSnapshot, GameStatus, MoveRejection, ServerEvent,
    TimeControl,
};

use crate::app::account::AccountContext;
use crate::app::actions::{
    cycle_pieces, flip_board, open_promotion_menu, set_pieces, set_size, set_sound, set_theme,
    sound_after_move, split_command, toggle_size,
};
use crate::app::cli::{names_a_file, Mode, OnlineIntent, Options, HOSTED_FILES};
use crate::app::format::kind_name;
use crate::app::pages::{export_pgn, history_page, pgn_lines};
use crate::app::parse::{nearby_moves, promotion_default};
use crate::app::prompt::read_line;
use crate::app::saves::{color_named, runtime_preferences};
use crate::app::screen::{ConnectionDisplay, OnlineDisplay, Screen, UiAction};

pub(crate) struct OnlineSession {
    pub(crate) client: OnlineClient,
    pub(crate) intent: OnlineIntent,
    pub(crate) server_url: String,
    pub(crate) seat_path: PathBuf,
    pub(crate) game_id: Option<String>,
    pub(crate) reconnect_token: Option<String>,
    pub(crate) side: Option<Color>,
    pub(crate) player_name: String,
    pub(crate) has_snapshot: bool,
    /// Signs each new connection in, so the game is played under the account.
    pub(crate) account: AccountContext,
}

impl OnlineSession {
    pub(crate) fn send(&self, command: ClientCommand, screen: &mut Screen) -> bool {
        match self.client.send(command) {
            Ok(()) => true,
            Err(error) => {
                if let Some(online) = &mut screen.online {
                    online.connection = ConnectionDisplay::Stopped;
                    online.move_pending = false;
                    online.failure_help = Some("quit, then retry your online command".to_string());
                }
                screen.note(screen.theme.warn(&error));
                false
            }
        }
    }

    pub(crate) fn reconnect_command(&self) -> Option<ClientCommand> {
        Some(ClientCommand::Reconnect {
            game_id: self.game_id.clone()?,
            reconnect_token: self.reconnect_token.clone()?,
        })
    }

    pub(crate) fn save_seat(&self) -> Result<(), String> {
        let side = self
            .side
            .ok_or_else(|| "online seat has no assigned side".to_string())?;
        let game_id = self
            .game_id
            .clone()
            .ok_or_else(|| "online seat has no game id".to_string())?;
        let token = self
            .reconnect_token
            .clone()
            .ok_or_else(|| "online seat has no reconnect token".to_string())?;
        storage::save_online_seat(
            &self.seat_path,
            &storage::SavedOnlineSeat::new(
                self.server_url.clone(),
                game_id,
                token,
                side.name().to_ascii_lowercase(),
            ),
        )
    }
}

pub(crate) fn play_online(
    mut options: Options,
    config_path: PathBuf,
    preferences: storage::Preferences,
) -> Result<(), String> {
    let seat_path = storage::default_online_session_path(&config_path);
    let intent = options
        .online
        .take()
        .expect("online mode checked by caller");
    let restored_seat = if intent == OnlineIntent::Resume {
        Some(
            storage::load_online_seat(&seat_path)
                .map_err(|error| format!("could not resume the last online game: {error}"))?,
        )
    } else {
        None
    };
    if let Some(seat) = &restored_seat {
        options.server_url = seat.server_url.clone();
    }
    let account = AccountContext::load(
        &config_path,
        &options.server_url,
        options.online_name.clone(),
    );
    if let Some(username) = account.username() {
        options.online_name = username.to_string();
    }

    let initial_side = restored_seat
        .as_ref()
        .and_then(|seat| color_named(&seat.side))
        .or(match intent {
            OnlineIntent::Create => Some(Color::White),
            OnlineIntent::Join(_) => Some(Color::Black),
            OnlineIntent::Resume => None,
        });
    let mut player_names = [String::new(), String::new()];
    if let Some(side) = initial_side {
        player_names[side.index()] = options.online_name.clone();
    }
    let mut screen = Screen::from_options(&options, player_names);
    screen.flipped = options.flipped || initial_side == Some(Color::Black);
    screen.message = vec![screen.theme.dim("Connecting to the game server…")];
    screen.online = Some(OnlineDisplay {
        connection: ConnectionDisplay::Connecting,
        invite_code: None,
        your_side: initial_side,
        white_connected: false,
        black_connected: false,
        reconnect_deadline_ms: None,
        move_pending: true,
        failure_help: None,
    });
    let _fullscreen = ui::Fullscreen::enter(&screen.theme);
    if screen.theme.live && !screen.theme.ascii {
        screen.inline_images = ui::detect_inline_images();
    }

    let hosted = options.hosted;
    let mut game = Game::with_clock(Position::startpos(), options.clock, options.increment);
    game.clock.pause();
    let client = OnlineClient::connect(options.server_url.clone());
    let mut session = OnlineSession {
        client,
        intent,
        server_url: options.server_url,
        seat_path,
        game_id: restored_seat.as_ref().map(|seat| seat.game_id.clone()),
        reconnect_token: restored_seat
            .as_ref()
            .map(|seat| seat.reconnect_token.clone()),
        side: initial_side,
        player_name: options.online_name,
        has_snapshot: false,
        account,
    };
    let mut terminal_input = TerminalInput::enter(screen.theme.live && screen.theme.color)?;
    let limits = Limits::default();
    let mut last_preferences = preferences;
    let mut persistence_error_reported = false;

    loop {
        while let Some(event) = session.client.try_recv() {
            handle_transport_event(event, &mut session, &mut game, &mut screen)?;
        }

        let preferences = online_preferences(&last_preferences, &screen, &game);
        if preferences != last_preferences {
            if let Err(error) = storage::save_preferences(&config_path, &preferences) {
                if !persistence_error_reported {
                    screen.note(
                        screen
                            .theme
                            .warn(&format!("Preferences were not saved: {error}")),
                    );
                    persistence_error_reported = true;
                }
            } else {
                last_preferences = preferences;
            }
        }

        if game.clock.tick() {
            screen.draw_clock_tick(&game, Mode::TwoPlayer, &limits);
        }
        if screen.redraw {
            screen.redraw = false;
            screen.draw(&game, Mode::TwoPlayer, &limits);
        }

        let action = if terminal_input.is_active() {
            screen.draw_prompt(&game, terminal_input.buffer());
            terminal_input.read_for(Duration::from_millis(100))?
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
                    if handle_online_action(screen.focused, &game, &session, &mut screen) {
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
                } else {
                    screen.draw_prompt(&game, terminal_input.buffer());
                }
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
                let changed = screen.focus_move_input()
                    || screen.page.take().is_some()
                    || screen.selected.take().is_some()
                    || screen.confirming.take().is_some()
                    || !screen.promotions.is_empty();
                screen.targets.clear();
                screen.captures.clear();
                screen.promotions.clear();
                if changed {
                    screen.redraw = true;
                }
                continue;
            }
            InputAction::Click { column, row } => {
                if screen.page.take().is_some() {
                    screen.redraw = true;
                    continue;
                }
                if let Some(action) = screen.action_at(column, row) {
                    screen.focus(action);
                    if handle_online_action(action, &game, &session, &mut screen) {
                        return Ok(());
                    }
                    continue;
                }
                screen.confirming = None;
                let square = screen.square_at(column, row);
                if online_can_move(&screen, &game) {
                    if let Some(movement) = online_board_click(&game, &mut screen, square) {
                        send_online_move(&session, &game, movement, &mut screen);
                    }
                } else {
                    explain_online_wait(&game, &mut screen);
                }
                continue;
            }
            InputAction::Quit => return Ok(()),
        };

        let input = line.trim();
        if screen.page.take().is_some() {
            screen.redraw = true;
        }
        if input.is_empty() {
            screen.redraw = screen.theme.live;
            continue;
        }
        if !screen.promotions.is_empty() && input.len() == 1 {
            let kind = input.chars().next().and_then(PieceKind::from_char);
            if let Some(
                kind @ (PieceKind::Queen | PieceKind::Rook | PieceKind::Bishop | PieceKind::Knight),
            ) = kind
            {
                if let Some(movement) = screen
                    .promotions
                    .iter()
                    .find(|choice| choice.piece.kind == kind)
                    .map(|choice| choice.movement)
                {
                    screen.clear_marks();
                    send_online_move(&session, &game, movement, &mut screen);
                }
                continue;
            }
        }

        let (word, rest) = split_command(input);
        if hosted && names_a_file(&word, rest) {
            screen.note(screen.theme.warn(HOSTED_FILES));
            continue;
        }
        match word.as_str() {
            "quit" | "exit" | "q" => return Ok(()),
            "help" | "h" | "?" => {
                screen.open("ONLINE GAME", online_help_lines(&screen.theme));
                continue;
            }
            "history" | "moveslist" => {
                screen.open("THE GAME SO FAR", history_page(&screen.theme, &game));
                continue;
            }
            "pgn" => {
                screen.open("PGN", pgn_lines(&game, &screen.player_names));
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
            "fen" => {
                screen.note(screen.theme.accent(&game.pos.to_fen()));
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
            "draw" => {
                if rest.eq_ignore_ascii_case("decline") {
                    respond_to_draw(&session, &game, false, &mut screen);
                } else {
                    offer_or_accept_draw(&session, &game, &mut screen);
                }
                continue;
            }
            "resign" => {
                send_resignation(&session, &game, &mut screen);
                continue;
            }
            _ => {}
        }

        if !online_can_move(&screen, &game) {
            explain_online_wait(&game, &mut screen);
            continue;
        }
        request_online_move(&session, &game, input, &mut screen);
    }
}

pub(crate) fn handle_transport_event(
    event: TransportEvent,
    session: &mut OnlineSession,
    game: &mut Game,
    screen: &mut Screen,
) -> Result<(), String> {
    match event {
        TransportEvent::Connecting { attempt } => {
            if let Some(online) = &mut screen.online {
                online.connection = if attempt == 1 && !session.has_snapshot {
                    ConnectionDisplay::Connecting
                } else {
                    ConnectionDisplay::Reconnecting
                };
                online.move_pending = true;
            }
            screen.redraw = true;
        }
        TransportEvent::Connected => {
            if let Some(online) = &mut screen.online {
                online.connection = ConnectionDisplay::Connected;
                online.move_pending = true;
                online.failure_help = None;
            }
            session.send(
                ClientCommand::Hello {
                    client_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                screen,
            );
            // Sent before the game command; the server answers in order, so
            // the seat is taken under the account.
            if let Some(token) = session.account.session_token() {
                let command = ClientCommand::Authenticate {
                    session_token: token.to_string(),
                };
                session.send(command, screen);
            }
            let command = session
                .reconnect_command()
                .unwrap_or_else(|| match &session.intent {
                    OnlineIntent::Create => ClientCommand::CreateGame {
                        player_name: session.player_name.clone(),
                        time_control: TimeControl::new(game.clock.initial, game.clock.increment),
                    },
                    OnlineIntent::Join(code) => ClientCommand::JoinGame {
                        invite_code: code.clone(),
                        player_name: session.player_name.clone(),
                    },
                    OnlineIntent::Resume => unreachable!("resume has reconnect credentials"),
                });
            session.send(command, screen);
            screen.redraw = true;
        }
        TransportEvent::Disconnected { reason, retry_in } => {
            if let Some(online) = &mut screen.online {
                online.connection = ConnectionDisplay::Reconnecting;
                online.move_pending = true;
                online.failure_help = None;
            }
            screen.show(vec![
                screen
                    .theme
                    .warn("Connection lost — reconnecting automatically."),
                screen.theme.dim(&format!(
                    "Retrying in {:.1}s · {reason}",
                    retry_in.as_secs_f32()
                )),
            ]);
        }
        TransportEvent::Stopped(reason) => {
            if let Some(online) = &mut screen.online {
                online.connection = ConnectionDisplay::Stopped;
                online.move_pending = false;
                online.failure_help = Some("quit, then retry your online command".to_string());
            }
            screen.note(screen.theme.warn(&reason));
        }
        TransportEvent::Message(envelope) => match envelope.event {
            ServerEvent::Welcome { .. }
            | ServerEvent::Pong
            | ServerEvent::SignedIn { .. }
            | ServerEvent::SignedOut
            | ServerEvent::GameList { .. }
            | ServerEvent::SshKeyLinked => {}
            ServerEvent::Error {
                code: ErrorCode::InvalidSession,
                ..
            } => {
                session.account.forget();
                screen.note(screen.theme.warn(
                    "Your sign-in has expired, so this game is played as a guest. Sign in again from the menu.",
                ));
            }
            ServerEvent::GameCreated {
                invite_code,
                reconnect_token,
                game: snapshot,
            } => {
                session.game_id = Some(snapshot.game_id.clone());
                session.reconnect_token = Some(reconnect_token);
                session.side = Some(Color::White);
                if let Some(online) = &mut screen.online {
                    online.invite_code = Some(invite_code);
                    online.your_side = session.side;
                }
                session.save_seat()?;
                apply_online_snapshot(snapshot, session, game, screen, true)?;
            }
            ServerEvent::GameJoined {
                reconnect_token,
                game: snapshot,
            } => {
                session.game_id = Some(snapshot.game_id.clone());
                session.reconnect_token = Some(reconnect_token);
                if session.side.is_none() {
                    session.side = Some(Color::Black);
                }
                if let Some(online) = &mut screen.online {
                    online.your_side = session.side;
                }
                session.save_seat()?;
                apply_online_snapshot(snapshot, session, game, screen, true)?;
                screen.note(screen.theme.good("Connected to the game."));
            }
            ServerEvent::GameUpdated { game: snapshot } => {
                apply_online_snapshot(snapshot, session, game, screen, false)?;
            }
            ServerEvent::MoveRejected {
                reason,
                game: snapshot,
                ..
            } => {
                apply_online_snapshot(snapshot, session, game, screen, true)?;
                let reason = match reason {
                    MoveRejection::NotYourTurn => "It is not your turn.",
                    MoveRejection::IllegalMove => "That move is not legal.",
                    MoveRejection::StalePosition => {
                        "The position changed before that move arrived. Try again."
                    }
                    MoveRejection::GameNotActive => "The game is not active.",
                };
                screen.note(screen.theme.warn(reason));
            }
            ServerEvent::OpponentDisconnected {
                reconnect_deadline_ms,
                ..
            } => {
                if let Some(online) = &mut screen.online {
                    online.reconnect_deadline_ms = Some(reconnect_deadline_ms);
                    let opponent = session.side.map(Color::flip);
                    if opponent == Some(Color::White) {
                        online.white_connected = false;
                    } else if opponent == Some(Color::Black) {
                        online.black_connected = false;
                    }
                }
                screen.note(
                    screen
                        .theme
                        .warn("Your opponent disconnected. Their seat is held for 60 seconds."),
                );
            }
            ServerEvent::OpponentReconnected { .. } => {
                if let Some(online) = &mut screen.online {
                    online.reconnect_deadline_ms = None;
                    if session.side == Some(Color::White) {
                        online.black_connected = true;
                    } else {
                        online.white_connected = true;
                    }
                }
                screen.note(screen.theme.good("Your opponent reconnected."));
            }
            ServerEvent::Error { code, message } => {
                let fatal = matches!(
                    code,
                    ErrorCode::UnsupportedProtocol
                        | ErrorCode::GameNotFound
                        | ErrorCode::GameFull
                        | ErrorCode::InvalidReconnectToken
                );
                if fatal {
                    let recovery = match code {
                        ErrorCode::UnsupportedProtocol => {
                            "update Terminal Chess, then try the online command again"
                        }
                        ErrorCode::GameNotFound => {
                            "quit, check the invite code, then run online join again"
                        }
                        ErrorCode::GameFull => {
                            "quit and ask for a new invite; this game already has two players"
                        }
                        ErrorCode::InvalidReconnectToken => {
                            "quit and create or join a new game; this saved seat is no longer valid"
                        }
                        _ => unreachable!("fatal error list is exhaustive"),
                    };
                    if let Some(online) = &mut screen.online {
                        online.connection = ConnectionDisplay::Stopped;
                        online.move_pending = false;
                        online.failure_help = Some(recovery.to_string());
                    }
                    screen.show(vec![
                        screen.theme.warn(&message),
                        screen.theme.dim(recovery),
                    ]);
                } else {
                    screen.note(screen.theme.warn(&message));
                }
            }
        },
    }
    Ok(())
}

pub(crate) fn apply_online_snapshot(
    snapshot: GameSnapshot,
    session: &mut OnlineSession,
    game: &mut Game,
    screen: &mut Screen,
    acknowledge_pending: bool,
) -> Result<(), String> {
    let previous_ply = game.sans.len();
    let previous_revision = game.revision;
    let previous_finished = outcome(game).is_some();
    let first_snapshot = !session.has_snapshot;
    let mut updated = Game::with_clock(
        Position::startpos(),
        snapshot.time_control.initial(),
        snapshot.time_control.increment(),
    );
    for (index, notation) in snapshot.moves.iter().enumerate() {
        let movement = parse_move(&updated.pos, notation).map_err(|_| {
            format!(
                "server snapshot contains an illegal move {} (`{notation}`)",
                index + 1
            )
        })?;
        updated.play(movement);
    }
    if updated.pos.to_fen() != snapshot.fen {
        return Err("server snapshot position does not match its move history".to_string());
    }
    let mut remaining = [snapshot.clock.white_ms, snapshot.clock.black_ms];
    if let Some(running) = snapshot.clock.running {
        let elapsed = unix_time_ms().saturating_sub(snapshot.clock.server_time_ms);
        let index = Color::from(running).index();
        remaining[index] = remaining[index].saturating_sub(elapsed);
    }
    let active = matches!(snapshot.status, GameStatus::Active);
    let clock_side = snapshot
        .clock
        .running
        .map(Color::from)
        .unwrap_or(updated.pos.side);
    updated.clock.restore(
        remaining,
        clock_side,
        active && snapshot.clock.running.is_some(),
    );
    updated.draw_offer = snapshot.draw_offer.map(Color::from);
    apply_finished_status(&snapshot.status, &mut updated);
    updated.revision = snapshot.revision;
    let revision_changed = updated.revision != previous_revision;
    let finished_changed = outcome(&updated).is_some() != previous_finished;

    screen.player_names = [snapshot.white.name.clone(), snapshot.black.name.clone()];
    if let Some(online) = &mut screen.online {
        online.connection = ConnectionDisplay::Connected;
        online.your_side = session.side;
        online.white_connected = snapshot.white.connected;
        online.black_connected = snapshot.black.connected;
        online.reconnect_deadline_ms = None;
        if acknowledge_pending || revision_changed || finished_changed {
            online.move_pending = false;
        }
    }
    let play_move_sound = session.has_snapshot && updated.sans.len() > previous_ply;
    let play_end_sound = session.has_snapshot
        && !previous_finished
        && outcome(&updated).is_some()
        && !play_move_sound;
    *game = updated;
    session.has_snapshot = true;
    // The server publishes clock snapshots every second. Preserve an in-flight
    // click selection across those clock-only updates; clear it only when the
    // position or game state actually changes.
    if first_snapshot || revision_changed || finished_changed {
        screen.clear_marks();
    }
    if play_move_sound {
        screen.sound.play(sound_after_move(game));
    } else if play_end_sound {
        screen.sound.play(sound::Cue::GameEnd);
    }
    screen.redraw = true;
    Ok(())
}

pub(crate) fn apply_finished_status(status: &GameStatus, game: &mut Game) {
    let GameStatus::Finished { result, reason } = status else {
        return;
    };
    let loser = result.loser();
    match reason {
        FinishReason::Resignation => game.resigned = loser,
        FinishReason::Timeout => game.clock.flagged = loser,
        FinishReason::Abandonment => game.abandoned = loser,
        FinishReason::DrawAgreement => game.agreed_draw = true,
        FinishReason::Checkmate
        | FinishReason::Stalemate
        | FinishReason::FiftyMove
        | FinishReason::Threefold
        | FinishReason::InsufficientMaterial => {}
    }
    game.clock.pause();
}

pub(crate) fn handle_online_action(
    action: UiAction,
    game: &Game,
    session: &OnlineSession,
    screen: &mut Screen,
) -> bool {
    match action {
        UiAction::MoveInput => {
            screen.focus_move_input();
        }
        UiAction::Draw => offer_or_accept_draw(session, game, screen),
        UiAction::DeclineDraw => respond_to_draw(session, game, false, screen),
        UiAction::Resign => {
            if !online_connected_for_actions(screen, game) {
                explain_online_wait(game, screen);
            } else if screen.confirming == Some(UiAction::Resign) {
                send_resignation(session, game, screen);
            } else {
                screen.confirming = Some(UiAction::Resign);
                screen.note(
                    screen
                        .theme
                        .warn("Choose Confirm to resign, or press Escape."),
                );
            }
        }
        UiAction::ToggleSize => toggle_size(screen),
        UiAction::CyclePieces => cycle_pieces(screen),
        UiAction::Flip => flip_board(screen),
        UiAction::Quit => return true,
        UiAction::Pause | UiAction::Undo | UiAction::Restart | UiAction::Rematch => {}
    }
    false
}

pub(crate) fn offer_or_accept_draw(session: &OnlineSession, game: &Game, screen: &mut Screen) {
    if !online_draw_available(screen, game, session.side) {
        explain_online_wait(game, screen);
        return;
    }
    let Some(game_id) = session.game_id.clone() else {
        explain_online_wait(game, screen);
        return;
    };
    let Some(side) = session.side else {
        explain_online_wait(game, screen);
        return;
    };
    let command = if game.draw_offer == Some(side.flip()) {
        ClientCommand::RespondDraw {
            game_id,
            accept: true,
        }
    } else if game.draw_offer == Some(side) {
        screen.note(
            screen
                .theme
                .dim("Your draw offer is waiting for your opponent."),
        );
        return;
    } else {
        ClientCommand::OfferDraw { game_id }
    };
    session.send(command, screen);
}

pub(crate) fn respond_to_draw(
    session: &OnlineSession,
    game: &Game,
    accept: bool,
    screen: &mut Screen,
) {
    if !online_connected_for_actions(screen, game) {
        explain_online_wait(game, screen);
        return;
    }
    let Some(side) = session.side else {
        explain_online_wait(game, screen);
        return;
    };
    if game.draw_offer != Some(side.flip()) {
        screen.note(screen.theme.dim("There is no draw offer to answer."));
        return;
    }
    if let Some(game_id) = session.game_id.clone() {
        session.send(ClientCommand::RespondDraw { game_id, accept }, screen);
    }
}

pub(crate) fn send_resignation(session: &OnlineSession, game: &Game, screen: &mut Screen) {
    if outcome(game).is_some() {
        screen.note(screen.theme.dim("The game is already over."));
        return;
    }
    if !online_connected_for_actions(screen, game) {
        explain_online_wait(game, screen);
        return;
    }
    if let Some(game_id) = session.game_id.clone() {
        screen.confirming = None;
        session.send(ClientCommand::Resign { game_id }, screen);
    } else {
        explain_online_wait(game, screen);
    }
}

pub(crate) fn online_can_move(screen: &Screen, game: &Game) -> bool {
    screen
        .online
        .as_ref()
        .is_some_and(|online| online.can_move(game))
}

pub(crate) fn online_connected_for_actions(screen: &Screen, game: &Game) -> bool {
    outcome(game).is_none()
        && screen.online.as_ref().is_some_and(|online| {
            online.connection == ConnectionDisplay::Connected
                && online.white_connected
                && online.black_connected
        })
}

pub(crate) fn online_draw_available(screen: &Screen, game: &Game, side: Option<Color>) -> bool {
    online_connected_for_actions(screen, game) && game.draw_offer != side
}

pub(crate) fn explain_online_wait(game: &Game, screen: &mut Screen) {
    let message = if outcome(game).is_some() {
        "The game is over. Export the PGN or quit when you are ready."
    } else if screen
        .online
        .as_ref()
        .is_some_and(|online| online.connection != ConnectionDisplay::Connected)
    {
        "Reconnecting — your seat is reserved and moves will resume automatically."
    } else if screen
        .online
        .as_ref()
        .is_some_and(|online| !online.white_connected || !online.black_connected)
    {
        "Waiting for both players to be connected."
    } else {
        "It is your opponent's turn."
    };
    screen.note(screen.theme.dim(message));
}

pub(crate) fn request_online_move(
    session: &OnlineSession,
    game: &Game,
    input: &str,
    screen: &mut Screen,
) {
    match parse_move(&game.pos, input) {
        Ok(movement) => send_online_move(session, game, movement, screen),
        Err(ParseError::Illegal(text)) => {
            if let Some(movement) = promotion_default(&game.pos, input) {
                send_online_move(session, game, movement, screen);
                return;
            }
            let near = nearby_moves(&game.pos, input);
            let hint = if near.is_empty() {
                "type `moves` to list legal moves".to_string()
            } else {
                format!("did you mean {}?", near.join(" or "))
            };
            screen.note(format!(
                "{} {}",
                screen.theme.warn(&format!("`{text}` is not a legal move")),
                screen.theme.dim(&format!("— {hint}"))
            ));
        }
        Err(ParseError::Ambiguous(text, candidates)) => screen.note(format!(
            "{} {}",
            screen.theme.warn(&format!("`{text}` could mean")),
            screen.theme.bold(&candidates.join(" or "))
        )),
    }
}

pub(crate) fn send_online_move(
    session: &OnlineSession,
    game: &Game,
    movement: Move,
    screen: &mut Screen,
) {
    let Some(game_id) = session.game_id.clone() else {
        explain_online_wait(game, screen);
        return;
    };
    if let Some(online) = &mut screen.online {
        online.move_pending = true;
    }
    screen.clear_marks();
    screen.redraw = true;
    session.send(
        ClientCommand::PlayMove {
            game_id,
            expected_ply: game.sans.len() as u32,
            uci: movement.to_uci(),
        },
        screen,
    );
}

pub(crate) fn online_board_click(
    game: &Game,
    screen: &mut Screen,
    square: Option<board::Square>,
) -> Option<Move> {
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
            return None;
        }
    };
    screen.invalid = None;
    if let Some(choice) = screen
        .promotions
        .iter()
        .find(|choice| choice.square == square)
        .copied()
    {
        return Some(choice.movement);
    }
    screen.promotions.clear();

    let legal = generate_legal(&game.pos);
    if let Some(from) = screen.selected {
        if square == from {
            screen.selected = None;
            screen.targets.clear();
            screen.captures.clear();
            screen.redraw = true;
            return None;
        }
        let choices: Vec<Move> = legal
            .iter()
            .copied()
            .filter(|movement| movement.from == from && movement.to == square)
            .collect();
        if let Some(&movement) = choices.first() {
            if choices.iter().any(|movement| movement.promo.is_some()) {
                open_promotion_menu(game, screen, &choices);
                return None;
            }
            return Some(movement);
        }
    }

    match game.pos.at(square) {
        Some(piece) if piece.color == game.pos.side => {
            screen.selected = Some(square);
            let moves: Vec<Move> = legal
                .iter()
                .filter(|movement| movement.from == square)
                .copied()
                .collect();
            screen.targets = moves.iter().map(|movement| movement.to).collect();
            screen.captures = moves
                .iter()
                .filter(|movement| {
                    movement.kind == MoveKind::EnPassant || game.pos.at(movement.to).is_some()
                })
                .map(|movement| movement.to)
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
            screen.message = vec![screen
                .theme
                .dim(&format!("Choose a {} piece.", game.pos.side.name()))];
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
    None
}

pub(crate) fn online_preferences(
    previous: &storage::Preferences,
    screen: &Screen,
    game: &Game,
) -> storage::Preferences {
    let mut preferences = runtime_preferences(screen, game);
    preferences.white_name = previous.white_name.clone();
    preferences.black_name = previous.black_name.clone();
    preferences.flipped = previous.flipped;
    preferences.clock_enabled = previous.clock_enabled;
    preferences.clock_minutes = previous.clock_minutes;
    preferences.increment_seconds = previous.increment_seconds;
    preferences
}

pub(crate) fn online_help_lines(theme: &Theme) -> Vec<String> {
    vec![
        theme.bold("PLAY"),
        "  Click a piece and a highlighted square, or type e4 / Nf3 / e2e4.".to_string(),
        "  The server validates every move and owns both clocks.".to_string(),
        String::new(),
        theme.bold("GAME"),
        "  draw             offer or accept a draw".to_string(),
        "  draw decline     decline the current draw offer".to_string(),
        "  resign           resign the game".to_string(),
        "  history · pgn · export [FILE] · fen".to_string(),
        String::new(),
        theme.bold("VIEW"),
        "  flip · size [small|big] · pieces [auto|art|glyph]".to_string(),
        "  theme [slate|wood|forest|mono] · sound [auto|on|off]".to_string(),
        String::new(),
        theme.dim("If the connection drops, this client reconnects and restores your seat."),
    ]
}

pub(crate) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
