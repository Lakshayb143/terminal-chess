//! Versioned messages exchanged by terminal clients and the game server.

use std::time::Duration;

use chess_core::board::Color;
use chess_core::game::Outcome;
use serde::{Deserialize, Serialize};

/// The version this build speaks. Version 3 added accounts and game history.
pub const PROTOCOL_VERSION: u16 = 3;
/// The oldest version a server still accepts. Version 3 only added commands
/// and optional fields, so version 2 guests keep working unchanged.
pub const MIN_PROTOCOL_VERSION: u16 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEnvelope {
    pub protocol: u16,
    /// Chosen by the client and echoed by direct responses.
    pub request_id: u64,
    pub command: ClientCommand,
}

impl ClientEnvelope {
    pub fn new(request_id: u64, command: ClientCommand) -> ClientEnvelope {
        ClientEnvelope {
            protocol: PROTOCOL_VERSION,
            request_id,
            command,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientCommand {
    Hello {
        client_version: String,
    },
    CreateGame {
        player_name: String,
        time_control: TimeControl,
    },
    JoinGame {
        invite_code: String,
        player_name: String,
    },
    Reconnect {
        game_id: String,
        reconnect_token: String,
    },
    PlayMove {
        game_id: String,
        /// Rejects a delayed or duplicated move after the position advances.
        expected_ply: u32,
        uci: String,
    },
    OfferDraw {
        game_id: String,
    },
    RespondDraw {
        game_id: String,
        accept: bool,
    },
    Resign {
        game_id: String,
    },
    Ping,
    /// Create an account and sign this connection in to it.
    Register {
        username: String,
        password: String,
    },
    LogIn {
        username: String,
        password: String,
    },
    /// Sign in with a session token from an earlier `SignedIn`.
    Authenticate {
        session_token: String,
    },
    /// Sign out and revoke this connection's session token.
    LogOut,
    /// The signed-in player's most recent finished games, newest first.
    ListGames {
        limit: u32,
    },
    /// Let the SSH key behind `ticket` sign in to the current account.
    LinkSshKey {
        ticket: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEnvelope {
    pub protocol: u16,
    /// Monotonically increasing within one connection.
    pub event_id: u64,
    /// Set when this event directly answers a client request.
    pub request_id: Option<u64>,
    pub event: ServerEvent,
}

impl ServerEnvelope {
    pub fn new(event_id: u64, request_id: Option<u64>, event: ServerEvent) -> ServerEnvelope {
        ServerEnvelope {
            protocol: PROTOCOL_VERSION,
            event_id,
            request_id,
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Welcome {
        server_version: String,
    },
    GameCreated {
        invite_code: String,
        reconnect_token: String,
        game: GameSnapshot,
    },
    GameJoined {
        reconnect_token: String,
        game: GameSnapshot,
    },
    GameUpdated {
        game: GameSnapshot,
    },
    MoveRejected {
        game_id: String,
        reason: MoveRejection,
        game: GameSnapshot,
    },
    OpponentDisconnected {
        game_id: String,
        reconnect_deadline_ms: u64,
    },
    OpponentReconnected {
        game_id: String,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    Pong,
    /// The connection is signed in. `session_token` is only sent when a new
    /// session was created, after registering or logging in.
    SignedIn {
        account: Account,
        session_token: Option<String>,
    },
    SignedOut,
    GameList {
        games: Vec<GameRecord>,
    },
    SshKeyLinked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub username: String,
    pub created_at_ms: u64,
}

/// A finished game as it is kept in the player's history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRecord {
    pub game_id: String,
    pub white: RecordedPlayer,
    pub black: RecordedPlayer,
    pub time_control: TimeControl,
    /// Every move in coordinate notation, from the starting position.
    pub moves: Vec<String>,
    pub result: GameResult,
    pub reason: FinishReason,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedPlayer {
    pub name: String,
    /// Whether `name` is an account's username rather than a guest's choice.
    pub registered: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeControl {
    pub initial_ms: u64,
    pub increment_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSnapshot {
    pub game_id: String,
    pub revision: u64,
    pub fen: String,
    pub moves: Vec<String>,
    pub last_move: Option<String>,
    pub side_to_move: Side,
    pub time_control: TimeControl,
    pub white: PlayerSnapshot,
    pub black: PlayerSnapshot,
    pub clock: ClockSnapshot,
    pub draw_offer: Option<Side>,
    pub status: GameStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub name: String,
    pub connected: bool,
    /// Whether `name` is an account's username rather than a guest's choice.
    #[serde(default)]
    pub registered: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockSnapshot {
    pub white_ms: u64,
    pub black_ms: u64,
    pub running: Option<Side>,
    /// Server Unix time used by clients only to estimate display drift.
    pub server_time_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GameStatus {
    WaitingForOpponent,
    Active,
    Finished {
        result: GameResult,
        reason: FinishReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    White,
    Black,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameResult {
    WhiteWins,
    BlackWins,
    Draw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Checkmate,
    Resignation,
    Timeout,
    DrawAgreement,
    Stalemate,
    FiftyMove,
    Threefold,
    InsufficientMaterial,
    Abandonment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveRejection {
    NotYourTurn,
    IllegalMove,
    StalePosition,
    GameNotActive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedProtocol,
    InvalidRequest,
    GameNotFound,
    GameFull,
    InvalidReconnectToken,
    RateLimited,
    Internal,
    UsernameTaken,
    /// The username or password was wrong. Which one is never said.
    InvalidCredentials,
    /// The session token has expired or been revoked; sign in again.
    InvalidSession,
    /// The command needs a signed-in account.
    NotSignedIn,
    /// Accounts are not enabled on this server.
    AccountsUnavailable,
}

// ---------------------------------------------------------------------------
// Conversions between wire types and the shared game model
// ---------------------------------------------------------------------------

impl From<Color> for Side {
    fn from(color: Color) -> Side {
        match color {
            Color::White => Side::White,
            Color::Black => Side::Black,
        }
    }
}

impl From<Side> for Color {
    fn from(side: Side) -> Color {
        match side {
            Side::White => Color::White,
            Side::Black => Color::Black,
        }
    }
}

impl TimeControl {
    /// An untimed game is sent as `initial_ms: 0`.
    pub fn new(initial: Option<Duration>, increment: Duration) -> TimeControl {
        TimeControl {
            initial_ms: initial.map_or(0, duration_ms),
            increment_ms: duration_ms(increment),
        }
    }

    pub fn initial(&self) -> Option<Duration> {
        (self.initial_ms > 0).then(|| Duration::from_millis(self.initial_ms))
    }

    pub fn increment(&self) -> Duration {
        Duration::from_millis(self.increment_ms)
    }
}

/// Milliseconds as sent on the wire, saturating rather than wrapping.
pub fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

impl From<Outcome> for GameStatus {
    fn from(outcome: Outcome) -> GameStatus {
        let won = |winner: Color, reason| GameStatus::Finished {
            result: match winner {
                Color::White => GameResult::WhiteWins,
                Color::Black => GameResult::BlackWins,
            },
            reason,
        };
        let drawn = |reason| GameStatus::Finished {
            result: GameResult::Draw,
            reason,
        };
        match outcome {
            Outcome::Checkmate(winner) => won(winner, FinishReason::Checkmate),
            Outcome::Resignation(loser) => won(loser.flip(), FinishReason::Resignation),
            Outcome::Timeout(loser) => won(loser.flip(), FinishReason::Timeout),
            Outcome::Abandonment(loser) => won(loser.flip(), FinishReason::Abandonment),
            Outcome::DrawAgreement => drawn(FinishReason::DrawAgreement),
            Outcome::Stalemate => drawn(FinishReason::Stalemate),
            Outcome::FiftyMove => drawn(FinishReason::FiftyMove),
            Outcome::Threefold => drawn(FinishReason::Threefold),
            Outcome::Insufficient => drawn(FinishReason::InsufficientMaterial),
        }
    }
}

impl GameResult {
    /// The side that lost, or `None` for a draw.
    pub fn loser(self) -> Option<Color> {
        match self {
            GameResult::WhiteWins => Some(Color::Black),
            GameResult::BlackWins => Some(Color::White),
            GameResult::Draw => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_have_stable_tagged_json() {
        let message = ClientEnvelope::new(
            7,
            ClientCommand::PlayMove {
                game_id: "game-1".to_string(),
                expected_ply: 12,
                uci: "e2e4".to_string(),
            },
        );

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["protocol"], PROTOCOL_VERSION);
        assert_eq!(json["request_id"], 7);
        assert_eq!(json["command"]["type"], "play_move");
        assert_eq!(json["command"]["expected_ply"], 12);
        assert_eq!(
            serde_json::from_value::<ClientEnvelope>(json).unwrap(),
            message
        );
    }

    #[test]
    fn version_2_snapshots_without_the_registered_flag_still_parse() {
        let json = r#"{"name":"Guest","connected":true}"#;
        let player: PlayerSnapshot = serde_json::from_str(json).unwrap();
        assert!(!player.registered);
    }

    #[test]
    fn account_commands_use_snake_case_tags() {
        let json = serde_json::to_value(ClientCommand::LogIn {
            username: "magnus".to_string(),
            password: "hunter22".to_string(),
        })
        .unwrap();
        assert_eq!(json["type"], "log_in");
        let json = serde_json::to_value(ClientCommand::LogOut).unwrap();
        assert_eq!(json["type"], "log_out");
    }

    #[test]
    fn every_outcome_maps_to_the_matching_result() {
        use chess_core::board::Color::{Black, White};
        let cases = [
            (
                Outcome::Checkmate(White),
                GameResult::WhiteWins,
                FinishReason::Checkmate,
            ),
            (
                Outcome::Resignation(White),
                GameResult::BlackWins,
                FinishReason::Resignation,
            ),
            (
                Outcome::Timeout(Black),
                GameResult::WhiteWins,
                FinishReason::Timeout,
            ),
            (
                Outcome::Abandonment(Black),
                GameResult::WhiteWins,
                FinishReason::Abandonment,
            ),
            (
                Outcome::DrawAgreement,
                GameResult::Draw,
                FinishReason::DrawAgreement,
            ),
            (
                Outcome::Stalemate,
                GameResult::Draw,
                FinishReason::Stalemate,
            ),
            (
                Outcome::FiftyMove,
                GameResult::Draw,
                FinishReason::FiftyMove,
            ),
            (
                Outcome::Threefold,
                GameResult::Draw,
                FinishReason::Threefold,
            ),
            (
                Outcome::Insufficient,
                GameResult::Draw,
                FinishReason::InsufficientMaterial,
            ),
        ];
        for (outcome, result, reason) in cases {
            assert_eq!(
                GameStatus::from(outcome),
                GameStatus::Finished { result, reason }
            );
        }
        assert_eq!(GameResult::BlackWins.loser(), Some(White));
        assert_eq!(GameResult::Draw.loser(), None);
    }

    #[test]
    fn untimed_games_round_trip_as_zero_initial_time() {
        let untimed = TimeControl::new(None, Duration::from_secs(2));
        assert_eq!(untimed.initial_ms, 0);
        assert_eq!(untimed.initial(), None);
        assert_eq!(untimed.increment(), Duration::from_secs(2));
        let timed = TimeControl::new(Some(Duration::from_secs(300)), Duration::ZERO);
        assert_eq!(timed.initial(), Some(Duration::from_secs(300)));
    }

    #[test]
    fn finished_snapshot_round_trips() {
        let snapshot = GameSnapshot {
            game_id: "game-1".to_string(),
            revision: 42,
            fen: "8/8/8/8/8/8/8/8 w - - 0 1".to_string(),
            moves: vec!["e2e4".to_string()],
            last_move: Some("e2e4".to_string()),
            side_to_move: Side::Black,
            time_control: TimeControl {
                initial_ms: 300_000,
                increment_ms: 2_000,
            },
            white: PlayerSnapshot {
                name: "White".to_string(),
                connected: true,
                registered: true,
            },
            black: PlayerSnapshot {
                name: "Black".to_string(),
                connected: false,
                registered: false,
            },
            clock: ClockSnapshot {
                white_ms: 299_000,
                black_ms: 300_000,
                running: None,
                server_time_ms: 1_000,
            },
            draw_offer: None,
            status: GameStatus::Finished {
                result: GameResult::WhiteWins,
                reason: FinishReason::Checkmate,
            },
        };
        let message = ServerEnvelope::new(9, None, ServerEvent::GameUpdated { game: snapshot });

        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerEnvelope>(&json).unwrap(),
            message
        );
    }
}
