//! Authoritative, transport-independent state for online guest matches.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::board::{Color, Position};
use crate::game::{outcome, Game, Outcome};
use crate::protocol::{
    ClientCommand, ClientEnvelope, ClockSnapshot, ErrorCode, FinishReason, GameResult,
    GameSnapshot, GameStatus, MoveRejection, PlayerSnapshot, ServerEnvelope, ServerEvent, Side,
    TimeControl, PROTOCOL_VERSION,
};
use crate::san::parse_move;

pub type ConnectionId = u64;

const RECONNECT_GRACE: Duration = Duration::from_secs(60);
const MAX_NAME_CHARS: usize = 32;
const MAX_INITIAL_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_INCREMENT_MS: u64 = 60 * 60 * 1_000;
const STATE_VERSION: u32 = 1;

/// A message ready for the WebSocket layer to send to one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub target: ConnectionId,
    pub message: ServerEnvelope,
}

#[derive(Clone, Debug)]
struct Membership {
    game_id: String,
    side: Color,
}

struct PlayerSlot {
    name: String,
    reconnect_token: String,
    connection: Option<ConnectionId>,
    disconnected_at: Option<Instant>,
}

struct Room {
    game_id: String,
    invite_code: String,
    game: Game,
    white: PlayerSlot,
    black: Option<PlayerSlot>,
    abandoned: Option<Color>,
}

impl Room {
    fn player(&self, side: Color) -> Option<&PlayerSlot> {
        match side {
            Color::White => Some(&self.white),
            Color::Black => self.black.as_ref(),
        }
    }

    fn player_mut(&mut self, side: Color) -> Option<&mut PlayerSlot> {
        match side {
            Color::White => Some(&mut self.white),
            Color::Black => self.black.as_mut(),
        }
    }

    fn targets(&self) -> Vec<ConnectionId> {
        [Some(&self.white), self.black.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|player| player.connection)
            .collect()
    }

    fn is_active(&self) -> bool {
        self.black.is_some() && self.abandoned.is_none() && outcome(&self.game).is_none()
    }
}

/// In-memory lobby and room manager. It contains no sockets, so its game and
/// concurrency behavior can be tested without starting a network listener.
pub struct Hub {
    rooms: HashMap<String, Room>,
    invites: HashMap<String, String>,
    memberships: HashMap<ConnectionId, Membership>,
    next_connection_id: ConnectionId,
    next_event_id: u64,
    dirty: bool,
}

impl Default for Hub {
    fn default() -> Hub {
        Hub::new()
    }
}

impl Hub {
    pub fn new() -> Hub {
        Hub {
            rooms: HashMap::new(),
            invites: HashMap::new(),
            memberships: HashMap::new(),
            next_connection_id: 1,
            next_event_id: 1,
            dirty: false,
        }
    }

    /// Restore durable room state. Network connections are intentionally not
    /// restored; clients prove possession of their reconnect token again.
    pub fn load(path: &Path) -> Result<Hub, String> {
        if !path.exists() {
            return Ok(Hub::new());
        }
        let text = fs::read_to_string(path)
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        let stored: PersistedHub = serde_json::from_str(&text)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if stored.version != STATE_VERSION {
            return Err(format!(
                "{} uses unsupported server state version {}",
                path.display(),
                stored.version
            ));
        }

        let now = Instant::now();
        let downtime_ms = unix_time_ms().saturating_sub(stored.saved_at_ms);
        let mut hub = Hub::new();
        for saved in stored.rooms {
            let initial = (saved.time_control.initial_ms > 0)
                .then(|| Duration::from_millis(saved.time_control.initial_ms));
            let mut game = Game::with_clock(
                Position::startpos(),
                initial,
                Duration::from_millis(saved.time_control.increment_ms),
            );
            for (index, notation) in saved.moves.iter().enumerate() {
                let movement = parse_move(&game.pos, notation).map_err(|_| {
                    format!(
                        "saved online game {} has an illegal move {} (`{notation}`)",
                        saved.game_id,
                        index + 1
                    )
                })?;
                game.play(movement);
            }
            let mut remaining = saved.remaining_ms;
            if let Some(running) = saved.running {
                let index = color(running).index();
                remaining[index] = remaining[index].saturating_sub(downtime_ms);
            }
            let running = saved.running.is_some() && remaining.iter().all(|&time| time > 0);
            game.clock.restore(remaining, game.pos.side, running);
            if let Some(running_side) = saved.running {
                let running_color = color(running_side);
                if remaining[running_color.index()] == 0 && game.clock.initial.is_some() {
                    game.clock.flagged = Some(running_color);
                    game.clock.running = None;
                }
            }
            game.resigned = saved.resigned.map(color);
            game.draw_offer = saved.draw_offer.map(color);
            game.agreed_draw = saved.agreed_draw;
            game.abandoned = saved.abandoned.map(color);
            game.revision = saved.revision;

            let disconnected_at = Some(now);
            let room = Room {
                game_id: saved.game_id.clone(),
                invite_code: saved.invite_code.clone(),
                game,
                white: saved.white.restore(disconnected_at),
                black: saved.black.map(|player| player.restore(disconnected_at)),
                abandoned: saved.abandoned.map(color),
            };
            hub.invites.insert(saved.invite_code, saved.game_id.clone());
            hub.rooms.insert(saved.game_id, room);
        }
        Ok(hub)
    }

    /// Atomically save every room, including reconnect credentials. The file
    /// is private on Unix because possession of a token grants a player's seat.
    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        let saved_at_ms = unix_time_ms();
        let rooms = self
            .rooms
            .values_mut()
            .map(PersistedRoom::capture)
            .collect();
        let text = serde_json::to_vec_pretty(&PersistedHub {
            version: STATE_VERSION,
            saved_at_ms,
            rooms,
        })
        .map_err(|error| format!("could not encode server state: {error}"))?;
        atomic_write(path, &text)?;
        self.dirty = false;
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn connect(&mut self) -> ConnectionId {
        let id = self.next_connection_id;
        self.next_connection_id = self.next_connection_id.wrapping_add(1).max(1);
        id
    }

    pub fn handle(&mut self, connection: ConnectionId, request: ClientEnvelope) -> Vec<Delivery> {
        if request.protocol != PROTOCOL_VERSION {
            return self.direct_error(
                connection,
                Some(request.request_id),
                ErrorCode::UnsupportedProtocol,
                format!(
                    "protocol {} is not supported; this server uses {}",
                    request.protocol, PROTOCOL_VERSION
                ),
            );
        }

        let request_id = request.request_id;
        if !matches!(
            request.command,
            ClientCommand::Hello { .. } | ClientCommand::Ping
        ) {
            self.dirty = true;
        }
        match request.command {
            ClientCommand::Hello { .. } => vec![self.delivery(
                connection,
                Some(request_id),
                ServerEvent::Welcome {
                    server_version: env!("CARGO_PKG_VERSION").to_string(),
                },
            )],
            ClientCommand::CreateGame {
                player_name,
                time_control,
            } => self.create_game(connection, request_id, player_name, time_control),
            ClientCommand::JoinGame {
                invite_code,
                player_name,
            } => self.join_game(connection, request_id, &invite_code, player_name),
            ClientCommand::Reconnect {
                game_id,
                reconnect_token,
            } => self.reconnect(connection, request_id, &game_id, &reconnect_token),
            ClientCommand::PlayMove {
                game_id,
                expected_ply,
                uci,
            } => self.play_move(connection, request_id, &game_id, expected_ply, &uci),
            ClientCommand::OfferDraw { game_id } => {
                self.offer_draw(connection, request_id, &game_id)
            }
            ClientCommand::RespondDraw { game_id, accept } => {
                self.respond_draw(connection, request_id, &game_id, accept)
            }
            ClientCommand::Resign { game_id } => self.resign(connection, request_id, &game_id),
            ClientCommand::Ping => {
                vec![self.delivery(connection, Some(request_id), ServerEvent::Pong)]
            }
        }
    }

    pub fn invalid_request(
        &mut self,
        connection: ConnectionId,
        message: impl Into<String>,
    ) -> Vec<Delivery> {
        self.direct_error(connection, None, ErrorCode::InvalidRequest, message.into())
    }

    pub fn rate_limited(&mut self, connection: ConnectionId) -> Vec<Delivery> {
        self.direct_error(
            connection,
            None,
            ErrorCode::RateLimited,
            "too many requests; wait a moment before trying again",
        )
    }

    pub fn disconnect(&mut self, connection: ConnectionId) -> Vec<Delivery> {
        let Some(membership) = self.memberships.remove(&connection) else {
            return Vec::new();
        };
        self.dirty = true;
        let now = Instant::now();
        let deadline = unix_time_ms().saturating_add(RECONNECT_GRACE.as_millis() as u64);
        let (targets, game_id) = {
            let Some(room) = self.rooms.get_mut(&membership.game_id) else {
                return Vec::new();
            };
            if let Some(player) = room.player_mut(membership.side) {
                if player.connection == Some(connection) {
                    player.connection = None;
                    player.disconnected_at = Some(now);
                }
            }
            (room.targets(), room.game_id.clone())
        };

        targets
            .into_iter()
            .map(|target| {
                self.delivery(
                    target,
                    None,
                    ServerEvent::OpponentDisconnected {
                        game_id: game_id.clone(),
                        reconnect_deadline_ms: deadline,
                    },
                )
            })
            .collect()
    }

    /// Advance clocks and enforce the reconnect grace period.
    pub fn tick(&mut self) -> Vec<Delivery> {
        let now = Instant::now();
        let mut updates = Vec::new();

        for room in self.rooms.values_mut() {
            let mut changed = room.game.clock.tick();
            if room.is_active() {
                for side in [Color::White, Color::Black] {
                    let expired = room
                        .player(side)
                        .and_then(|player| player.disconnected_at)
                        .is_some_and(|since| now.duration_since(since) >= RECONNECT_GRACE);
                    let opponent_online = room
                        .player(side.flip())
                        .is_some_and(|player| player.connection.is_some());
                    if expired && opponent_online {
                        room.abandoned = Some(side);
                        room.game.clock.pause();
                        room.game.changed();
                        changed = true;
                        break;
                    }
                }
            }
            if changed {
                updates.push((room.targets(), snapshot(room)));
            }
        }

        if !updates.is_empty() {
            self.dirty = true;
        }

        let mut deliveries = Vec::new();
        for (targets, game) in updates {
            for target in targets {
                deliveries.push(self.delivery(
                    target,
                    None,
                    ServerEvent::GameUpdated { game: game.clone() },
                ));
            }
        }
        deliveries
    }

    fn create_game(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        player_name: String,
        time_control: TimeControl,
    ) -> Vec<Delivery> {
        if self.memberships.contains_key(&connection) {
            return self.direct_error(
                connection,
                Some(request_id),
                ErrorCode::InvalidRequest,
                "leave the current game before creating another one",
            );
        }
        let player_name = match clean_player_name(&player_name) {
            Ok(name) => name,
            Err(message) => {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::InvalidRequest,
                    message,
                )
            }
        };
        if time_control.initial_ms > MAX_INITIAL_MS || time_control.increment_ms > MAX_INCREMENT_MS
        {
            return self.direct_error(
                connection,
                Some(request_id),
                ErrorCode::InvalidRequest,
                "time control is outside the supported range",
            );
        }

        let game_id = Uuid::new_v4().to_string();
        let invite_code = self.unique_invite_code();
        let reconnect_token = Uuid::new_v4().to_string();
        let initial =
            (time_control.initial_ms > 0).then(|| Duration::from_millis(time_control.initial_ms));
        let mut game = Game::with_clock(
            Position::startpos(),
            initial,
            Duration::from_millis(time_control.increment_ms),
        );
        game.clock.pause();
        let room = Room {
            game_id: game_id.clone(),
            invite_code: invite_code.clone(),
            game,
            white: PlayerSlot {
                name: player_name,
                reconnect_token: reconnect_token.clone(),
                connection: Some(connection),
                disconnected_at: None,
            },
            black: None,
            abandoned: None,
        };
        self.invites.insert(invite_code.clone(), game_id.clone());
        self.memberships.insert(
            connection,
            Membership {
                game_id: game_id.clone(),
                side: Color::White,
            },
        );
        self.rooms.insert(game_id.clone(), room);
        let game = snapshot(self.rooms.get_mut(&game_id).expect("new room exists"));

        vec![self.delivery(
            connection,
            Some(request_id),
            ServerEvent::GameCreated {
                invite_code,
                reconnect_token,
                game,
            },
        )]
    }

    fn join_game(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        invite_code: &str,
        player_name: String,
    ) -> Vec<Delivery> {
        if self.memberships.contains_key(&connection) {
            return self.direct_error(
                connection,
                Some(request_id),
                ErrorCode::InvalidRequest,
                "leave the current game before joining another one",
            );
        }
        let player_name = match clean_player_name(&player_name) {
            Ok(name) => name,
            Err(message) => {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::InvalidRequest,
                    message,
                )
            }
        };
        let code = invite_code.trim().to_ascii_uppercase();
        let Some(game_id) = self.invites.get(&code).cloned() else {
            return self.direct_error(
                connection,
                Some(request_id),
                ErrorCode::GameNotFound,
                "invite code was not found",
            );
        };
        let reconnect_token = Uuid::new_v4().to_string();
        let (creator, game) = {
            let room = self.rooms.get_mut(&game_id).expect("invite points to room");
            if room.black.is_some() {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::GameFull,
                    "this game already has two players",
                );
            }
            room.black = Some(PlayerSlot {
                name: player_name,
                reconnect_token: reconnect_token.clone(),
                connection: Some(connection),
                disconnected_at: None,
            });
            room.game.clock.resume(Color::White);
            room.game.changed();
            let creator = room.white.connection;
            let game = snapshot(room);
            (creator, game)
        };
        self.memberships.insert(
            connection,
            Membership {
                game_id: game_id.clone(),
                side: Color::Black,
            },
        );

        let mut deliveries = vec![self.delivery(
            connection,
            Some(request_id),
            ServerEvent::GameJoined {
                reconnect_token,
                game: game.clone(),
            },
        )];
        if let Some(creator) = creator {
            deliveries.push(self.delivery(creator, None, ServerEvent::GameUpdated { game }));
        }
        deliveries
    }

    fn reconnect(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        game_id: &str,
        reconnect_token: &str,
    ) -> Vec<Delivery> {
        if self.memberships.contains_key(&connection) {
            return self.direct_error(
                connection,
                Some(request_id),
                ErrorCode::InvalidRequest,
                "this connection is already in a game",
            );
        }
        let (side, old_connection) = {
            let Some(room) = self.rooms.get(game_id) else {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::GameNotFound,
                    "game was not found",
                );
            };
            let side = [Color::White, Color::Black].into_iter().find(|&side| {
                room.player(side)
                    .is_some_and(|player| player.reconnect_token == reconnect_token)
            });
            let Some(side) = side else {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::InvalidReconnectToken,
                    "reconnect token is not valid for this game",
                );
            };
            (side, room.player(side).and_then(|player| player.connection))
        };
        if let Some(old_connection) = old_connection {
            self.memberships.remove(&old_connection);
        }

        let (opponent, game) = {
            let room = self.rooms.get_mut(game_id).expect("room checked above");
            let player = room.player_mut(side).expect("token matched a player");
            player.connection = Some(connection);
            player.disconnected_at = None;
            let opponent = room
                .player(side.flip())
                .and_then(|player| player.connection);
            let game = snapshot(room);
            (opponent, game)
        };
        self.memberships.insert(
            connection,
            Membership {
                game_id: game_id.to_string(),
                side,
            },
        );

        let mut deliveries = vec![self.delivery(
            connection,
            Some(request_id),
            ServerEvent::GameJoined {
                reconnect_token: reconnect_token.to_string(),
                game,
            },
        )];
        if let Some(opponent) = opponent {
            deliveries.push(self.delivery(
                opponent,
                None,
                ServerEvent::OpponentReconnected {
                    game_id: game_id.to_string(),
                },
            ));
        }
        deliveries
    }

    fn play_move(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        game_id: &str,
        expected_ply: u32,
        notation: &str,
    ) -> Vec<Delivery> {
        let side = match self.member_side(connection, game_id) {
            Ok(side) => side,
            Err((code, message)) => {
                return self.direct_error(connection, Some(request_id), code, message)
            }
        };

        let (targets, game, rejection) = {
            let room = self
                .rooms
                .get_mut(game_id)
                .expect("membership points to room");
            room.game.clock.tick();
            let rejection = if !room.is_active() {
                Some(MoveRejection::GameNotActive)
            } else if room.game.sans.len() != expected_ply as usize {
                Some(MoveRejection::StalePosition)
            } else if room.game.pos.side != side {
                Some(MoveRejection::NotYourTurn)
            } else {
                match parse_move(&room.game.pos, notation) {
                    Ok(movement) => {
                        room.game.play(movement);
                        if outcome(&room.game).is_some() {
                            room.game.clock.pause();
                        }
                        None
                    }
                    Err(_) => Some(MoveRejection::IllegalMove),
                }
            };
            (room.targets(), snapshot(room), rejection)
        };

        if let Some(reason) = rejection {
            return vec![self.delivery(
                connection,
                Some(request_id),
                ServerEvent::MoveRejected {
                    game_id: game_id.to_string(),
                    reason,
                    game,
                },
            )];
        }
        self.broadcast_update(targets, connection, request_id, game)
    }

    fn offer_draw(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        game_id: &str,
    ) -> Vec<Delivery> {
        let side = match self.member_side(connection, game_id) {
            Ok(side) => side,
            Err((code, message)) => {
                return self.direct_error(connection, Some(request_id), code, message)
            }
        };
        let (targets, game) = {
            let room = self
                .rooms
                .get_mut(game_id)
                .expect("membership points to room");
            if !room.is_active() {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::InvalidRequest,
                    "the game is not active",
                );
            }
            room.game.draw_offer = Some(side);
            room.game.changed();
            (room.targets(), snapshot(room))
        };
        self.broadcast_update(targets, connection, request_id, game)
    }

    fn respond_draw(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        game_id: &str,
        accept: bool,
    ) -> Vec<Delivery> {
        let side = match self.member_side(connection, game_id) {
            Ok(side) => side,
            Err((code, message)) => {
                return self.direct_error(connection, Some(request_id), code, message)
            }
        };
        let (targets, game) = {
            let room = self
                .rooms
                .get_mut(game_id)
                .expect("membership points to room");
            if room.game.draw_offer != Some(side.flip()) || !room.is_active() {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::InvalidRequest,
                    "there is no opponent draw offer to answer",
                );
            }
            room.game.draw_offer = None;
            if accept {
                room.game.agreed_draw = true;
                room.game.clock.pause();
            }
            room.game.changed();
            (room.targets(), snapshot(room))
        };
        self.broadcast_update(targets, connection, request_id, game)
    }

    fn resign(
        &mut self,
        connection: ConnectionId,
        request_id: u64,
        game_id: &str,
    ) -> Vec<Delivery> {
        let side = match self.member_side(connection, game_id) {
            Ok(side) => side,
            Err((code, message)) => {
                return self.direct_error(connection, Some(request_id), code, message)
            }
        };
        let (targets, game) = {
            let room = self
                .rooms
                .get_mut(game_id)
                .expect("membership points to room");
            if !room.is_active() {
                return self.direct_error(
                    connection,
                    Some(request_id),
                    ErrorCode::InvalidRequest,
                    "the game is not active",
                );
            }
            room.game.resigned = Some(side);
            room.game.clock.pause();
            room.game.changed();
            (room.targets(), snapshot(room))
        };
        self.broadcast_update(targets, connection, request_id, game)
    }

    fn member_side(
        &self,
        connection: ConnectionId,
        game_id: &str,
    ) -> Result<Color, (ErrorCode, &'static str)> {
        let membership = self.memberships.get(&connection).ok_or((
            ErrorCode::InvalidRequest,
            "this connection has not joined a game",
        ))?;
        if membership.game_id != game_id {
            return Err((ErrorCode::GameNotFound, "connection is not in that game"));
        }
        Ok(membership.side)
    }

    fn broadcast_update(
        &mut self,
        targets: Vec<ConnectionId>,
        requester: ConnectionId,
        request_id: u64,
        game: GameSnapshot,
    ) -> Vec<Delivery> {
        targets
            .into_iter()
            .map(|target| {
                self.delivery(
                    target,
                    (target == requester).then_some(request_id),
                    ServerEvent::GameUpdated { game: game.clone() },
                )
            })
            .collect()
    }

    fn direct_error(
        &mut self,
        connection: ConnectionId,
        request_id: Option<u64>,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Vec<Delivery> {
        vec![self.delivery(
            connection,
            request_id,
            ServerEvent::Error {
                code,
                message: message.into(),
            },
        )]
    }

    fn delivery(
        &mut self,
        target: ConnectionId,
        request_id: Option<u64>,
        event: ServerEvent,
    ) -> Delivery {
        let event_id = self.next_event_id;
        self.next_event_id = self.next_event_id.wrapping_add(1).max(1);
        Delivery {
            target,
            message: ServerEnvelope::new(event_id, request_id, event),
        }
    }

    fn unique_invite_code(&self) -> String {
        loop {
            let compact = Uuid::new_v4().simple().to_string().to_ascii_uppercase();
            let code = compact[..6].to_string();
            if !self.invites.contains_key(&code) {
                return code;
            }
        }
    }
}

fn clean_player_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    let length = name.chars().count();
    if length == 0 {
        return Err("player name cannot be empty");
    }
    if length > MAX_NAME_CHARS {
        return Err("player name must be at most 32 characters");
    }
    if name.chars().any(char::is_control) {
        return Err("player name cannot contain control characters");
    }
    Ok(name.to_string())
}

fn snapshot(room: &mut Room) -> GameSnapshot {
    let remaining = room.game.clock.snapshot();
    let status = room_status(room);
    GameSnapshot {
        game_id: room.game_id.clone(),
        revision: room.game.revision,
        fen: room.game.pos.to_fen(),
        moves: room
            .game
            .undos
            .iter()
            .map(|undo| undo.mv.to_uci())
            .collect(),
        last_move: room.game.last_move().map(|movement| movement.to_uci()),
        side_to_move: side(room.game.pos.side),
        time_control: TimeControl {
            initial_ms: room
                .game
                .clock
                .initial
                .map(|time| time.as_millis().min(u64::MAX as u128) as u64)
                .unwrap_or(0),
            increment_ms: room.game.clock.increment.as_millis().min(u64::MAX as u128) as u64,
        },
        white: player_snapshot(Some(&room.white)),
        black: player_snapshot(room.black.as_ref()),
        clock: ClockSnapshot {
            white_ms: remaining[Color::White.index()],
            black_ms: remaining[Color::Black.index()],
            running: room.game.clock.running.map(|(color, _)| side(color)),
            server_time_ms: unix_time_ms(),
        },
        draw_offer: room.game.draw_offer.map(side),
        status,
    }
}

fn player_snapshot(player: Option<&PlayerSlot>) -> PlayerSnapshot {
    PlayerSnapshot {
        name: player.map_or_else(String::new, |player| player.name.clone()),
        connected: player.is_some_and(|player| player.connection.is_some()),
    }
}

fn room_status(room: &Room) -> GameStatus {
    if room.black.is_none() {
        return GameStatus::WaitingForOpponent;
    }
    if let Some(loser) = room.abandoned {
        return finished(loser.flip(), FinishReason::Abandonment);
    }
    match outcome(&room.game) {
        Some(Outcome::Checkmate(winner)) => finished(winner, FinishReason::Checkmate),
        Some(Outcome::Resignation(loser)) => finished(loser.flip(), FinishReason::Resignation),
        Some(Outcome::Timeout(loser)) => finished(loser.flip(), FinishReason::Timeout),
        Some(Outcome::Abandonment(loser)) => finished(loser.flip(), FinishReason::Abandonment),
        Some(Outcome::DrawAgreement) => drawn(FinishReason::DrawAgreement),
        Some(Outcome::Stalemate) => drawn(FinishReason::Stalemate),
        Some(Outcome::FiftyMove) => drawn(FinishReason::FiftyMove),
        Some(Outcome::Threefold) => drawn(FinishReason::Threefold),
        Some(Outcome::Insufficient) => drawn(FinishReason::InsufficientMaterial),
        None => GameStatus::Active,
    }
}

fn finished(winner: Color, reason: FinishReason) -> GameStatus {
    GameStatus::Finished {
        result: match winner {
            Color::White => GameResult::WhiteWins,
            Color::Black => GameResult::BlackWins,
        },
        reason,
    }
}

fn drawn(reason: FinishReason) -> GameStatus {
    GameStatus::Finished {
        result: GameResult::Draw,
        reason,
    }
}

fn side(color: Color) -> Side {
    match color {
        Color::White => Side::White,
        Color::Black => Side::Black,
    }
}

fn color(side: Side) -> Color {
    match side {
        Side::White => Color::White,
        Side::Black => Color::Black,
    }
}

#[derive(Serialize, Deserialize)]
struct PersistedHub {
    version: u32,
    saved_at_ms: u64,
    rooms: Vec<PersistedRoom>,
}

#[derive(Serialize, Deserialize)]
struct PersistedRoom {
    game_id: String,
    invite_code: String,
    revision: u64,
    moves: Vec<String>,
    time_control: TimeControl,
    remaining_ms: [u64; 2],
    running: Option<Side>,
    white: PersistedPlayer,
    black: Option<PersistedPlayer>,
    resigned: Option<Side>,
    draw_offer: Option<Side>,
    agreed_draw: bool,
    abandoned: Option<Side>,
}

impl PersistedRoom {
    fn capture(room: &mut Room) -> PersistedRoom {
        let remaining_ms = room.game.clock.snapshot();
        PersistedRoom {
            game_id: room.game_id.clone(),
            invite_code: room.invite_code.clone(),
            revision: room.game.revision,
            moves: room
                .game
                .undos
                .iter()
                .map(|undo| undo.mv.to_uci())
                .collect(),
            time_control: TimeControl {
                initial_ms: room
                    .game
                    .clock
                    .initial
                    .map(|time| time.as_millis().min(u64::MAX as u128) as u64)
                    .unwrap_or(0),
                increment_ms: room.game.clock.increment.as_millis().min(u64::MAX as u128) as u64,
            },
            remaining_ms,
            running: room.game.clock.running.map(|(color, _)| side(color)),
            white: PersistedPlayer::capture(&room.white),
            black: room.black.as_ref().map(PersistedPlayer::capture),
            resigned: room.game.resigned.map(side),
            draw_offer: room.game.draw_offer.map(side),
            agreed_draw: room.game.agreed_draw,
            abandoned: room.abandoned.map(side),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PersistedPlayer {
    name: String,
    reconnect_token: String,
}

impl PersistedPlayer {
    fn capture(player: &PlayerSlot) -> PersistedPlayer {
        PersistedPlayer {
            name: player.name.clone(),
            reconnect_token: player.reconnect_token.clone(),
        }
    }

    fn restore(self, disconnected_at: Option<Instant>) -> PlayerSlot {
        PlayerSlot {
            name: self.name,
            reconnect_token: self.reconnect_token,
            connection: None,
            disconnected_at,
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create server state directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let temp = temporary_path(path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    fs::rename(&temp, path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temp = path.as_os_str().to_os_string();
    temp.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(temp)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(hub: &mut Hub, connection: ConnectionId) -> (String, String, String) {
        let response = hub.handle(
            connection,
            ClientEnvelope::new(
                1,
                ClientCommand::CreateGame {
                    player_name: "White".to_string(),
                    time_control: TimeControl {
                        initial_ms: 300_000,
                        increment_ms: 2_000,
                    },
                },
            ),
        );
        match &response[0].message.event {
            ServerEvent::GameCreated {
                invite_code,
                reconnect_token,
                game,
            } => (
                invite_code.clone(),
                reconnect_token.clone(),
                game.game_id.clone(),
            ),
            event => panic!("unexpected event: {event:?}"),
        }
    }

    fn join(hub: &mut Hub, connection: ConnectionId, invite_code: &str) -> String {
        let response = hub.handle(
            connection,
            ClientEnvelope::new(
                2,
                ClientCommand::JoinGame {
                    invite_code: invite_code.to_string(),
                    player_name: "Black".to_string(),
                },
            ),
        );
        match &response[0].message.event {
            ServerEvent::GameJoined {
                reconnect_token, ..
            } => reconnect_token.clone(),
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn two_guests_create_join_and_play_an_authoritative_move() {
        let mut hub = Hub::new();
        let white = hub.connect();
        let black = hub.connect();
        let (code, _, game_id) = create(&mut hub, white);
        join(&mut hub, black, &code);

        let updates = hub.handle(
            white,
            ClientEnvelope::new(
                3,
                ClientCommand::PlayMove {
                    game_id,
                    expected_ply: 0,
                    uci: "e2e4".to_string(),
                },
            ),
        );

        assert_eq!(updates.len(), 2);
        for update in updates {
            let ServerEvent::GameUpdated { game } = update.message.event else {
                panic!("expected game update");
            };
            assert_eq!(game.moves, ["e2e4"]);
            assert_eq!(game.side_to_move, Side::Black);
            assert_eq!(game.revision, 2);
        }
    }

    #[test]
    fn server_rejects_out_of_turn_and_stale_moves() {
        let mut hub = Hub::new();
        let white = hub.connect();
        let black = hub.connect();
        let (code, _, game_id) = create(&mut hub, white);
        join(&mut hub, black, &code);

        let response = hub.handle(
            black,
            ClientEnvelope::new(
                3,
                ClientCommand::PlayMove {
                    game_id: game_id.clone(),
                    expected_ply: 0,
                    uci: "e7e5".to_string(),
                },
            ),
        );
        assert!(matches!(
            response[0].message.event,
            ServerEvent::MoveRejected {
                reason: MoveRejection::NotYourTurn,
                ..
            }
        ));

        hub.handle(
            white,
            ClientEnvelope::new(
                4,
                ClientCommand::PlayMove {
                    game_id: game_id.clone(),
                    expected_ply: 0,
                    uci: "e2e4".to_string(),
                },
            ),
        );
        let response = hub.handle(
            black,
            ClientEnvelope::new(
                5,
                ClientCommand::PlayMove {
                    game_id,
                    expected_ply: 0,
                    uci: "e7e5".to_string(),
                },
            ),
        );
        assert!(matches!(
            response[0].message.event,
            ServerEvent::MoveRejected {
                reason: MoveRejection::StalePosition,
                ..
            }
        ));
    }

    #[test]
    fn reconnect_token_restores_the_same_seat() {
        let mut hub = Hub::new();
        let original = hub.connect();
        let (code, token, game_id) = create(&mut hub, original);
        let opponent = hub.connect();
        join(&mut hub, opponent, &code);
        assert_eq!(hub.disconnect(original).len(), 1);

        let replacement = hub.connect();
        let response = hub.handle(
            replacement,
            ClientEnvelope::new(
                3,
                ClientCommand::Reconnect {
                    game_id,
                    reconnect_token: token,
                },
            ),
        );

        assert!(matches!(
            response[0].message.event,
            ServerEvent::GameJoined { .. }
        ));
        assert!(matches!(
            response[1].message.event,
            ServerEvent::OpponentReconnected { .. }
        ));
    }

    #[test]
    fn incompatible_protocol_is_rejected_before_processing() {
        let mut hub = Hub::new();
        let connection = hub.connect();
        let mut request = ClientEnvelope::new(1, ClientCommand::Ping);
        request.protocol = PROTOCOL_VERSION + 1;

        let response = hub.handle(connection, request);

        assert!(matches!(
            response[0].message.event,
            ServerEvent::Error {
                code: ErrorCode::UnsupportedProtocol,
                ..
            }
        ));
    }

    #[test]
    fn active_rooms_survive_a_server_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rooms.json");
        let mut hub = Hub::new();
        let white = hub.connect();
        let black = hub.connect();
        let (code, white_token, game_id) = create(&mut hub, white);
        let black_token = join(&mut hub, black, &code);
        hub.handle(
            white,
            ClientEnvelope::new(
                3,
                ClientCommand::PlayMove {
                    game_id: game_id.clone(),
                    expected_ply: 0,
                    uci: "e2e4".to_string(),
                },
            ),
        );
        hub.save(&path).unwrap();

        let mut restored = Hub::load(&path).unwrap();
        let new_white = restored.connect();
        let white_events = restored.handle(
            new_white,
            ClientEnvelope::new(
                4,
                ClientCommand::Reconnect {
                    game_id: game_id.clone(),
                    reconnect_token: white_token,
                },
            ),
        );
        let new_black = restored.connect();
        let black_events = restored.handle(
            new_black,
            ClientEnvelope::new(
                5,
                ClientCommand::Reconnect {
                    game_id,
                    reconnect_token: black_token,
                },
            ),
        );

        let snapshot = white_events
            .iter()
            .chain(&black_events)
            .find_map(|delivery| match &delivery.message.event {
                ServerEvent::GameJoined { game, .. } if game.moves.len() == 1 => Some(game),
                _ => None,
            })
            .expect("reconnected client receives restored position");
        assert_eq!(snapshot.moves, ["e2e4"]);
        assert_eq!(snapshot.side_to_move, Side::Black);
        assert!(snapshot.white.connected || snapshot.black.connected);
    }

    #[test]
    fn two_delayed_clients_can_reconnect_and_finish_by_checkmate() {
        let mut hub = Hub::new();
        let white = hub.connect();
        let black = hub.connect();
        let (code, _, game_id) = create(&mut hub, white);
        let black_token = join(&mut hub, black, &code);

        let play = |hub: &mut Hub, connection, request_id, ply, uci: &str| {
            std::thread::sleep(Duration::from_millis(10));
            hub.handle(
                connection,
                ClientEnvelope::new(
                    request_id,
                    ClientCommand::PlayMove {
                        game_id: game_id.clone(),
                        expected_ply: ply,
                        uci: uci.to_string(),
                    },
                ),
            )
        };

        play(&mut hub, white, 3, 0, "f2f3");
        play(&mut hub, black, 4, 1, "e7e5");
        assert_eq!(hub.disconnect(black).len(), 1);
        std::thread::sleep(Duration::from_millis(20));
        let replacement = hub.connect();
        hub.handle(
            replacement,
            ClientEnvelope::new(
                5,
                ClientCommand::Reconnect {
                    game_id: game_id.clone(),
                    reconnect_token: black_token,
                },
            ),
        );
        play(&mut hub, white, 6, 2, "g2g4");
        let updates = play(&mut hub, replacement, 7, 3, "d8h4");

        for update in updates {
            let ServerEvent::GameUpdated { game } = update.message.event else {
                panic!("expected final game update");
            };
            assert_eq!(game.moves.len(), 4);
            assert!(matches!(
                game.status,
                GameStatus::Finished {
                    result: GameResult::BlackWins,
                    reason: FinishReason::Checkmate,
                }
            ));
        }
    }
}
