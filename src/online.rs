//! Authoritative, transport-independent state for online guest matches.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
        }
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

    pub fn disconnect(&mut self, connection: ConnectionId) -> Vec<Delivery> {
        let Some(membership) = self.memberships.remove(&connection) else {
            return Vec::new();
        };
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
                    if expired {
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
}
