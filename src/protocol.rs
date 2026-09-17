//! Versioned messages exchanged by terminal clients and the game server.

use serde::{Deserialize, Serialize};

/// Increment this only for a breaking wire-format or behavior change.
pub const PROTOCOL_VERSION: u16 = 1;

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
    fn finished_snapshot_round_trips() {
        let snapshot = GameSnapshot {
            game_id: "game-1".to_string(),
            revision: 42,
            fen: "8/8/8/8/8/8/8/8 w - - 0 1".to_string(),
            moves: vec!["e2e4".to_string()],
            last_move: Some("e2e4".to_string()),
            side_to_move: Side::Black,
            white: PlayerSnapshot {
                name: "White".to_string(),
                connected: true,
            },
            black: PlayerSnapshot {
                name: "Black".to_string(),
                connected: false,
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
