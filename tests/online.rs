//! End-to-end: the real server binary and two real WebSocket clients.

use std::path::Path;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chess::client::{OnlineClient, TransportEvent};
use chess_protocol::{
    ClientCommand, ErrorCode, FinishReason, GameResult, GameSnapshot, GameStatus, ServerEvent,
    TimeControl,
};
use tokio::sync::oneshot;

/// A real server on an ephemeral port, running on its own runtime thread.
struct Server {
    address: String,
    shutdown: oneshot::Sender<()>,
    thread: JoinHandle<Result<(), String>>,
}

impl Server {
    fn start(state: &Path) -> Server {
        let config = chess_server::Config {
            state_path: state.to_path_buf(),
            requests_per_window: 60,
            database_path: Some(state.with_file_name("chess.db")),
        };
        let (shutdown, stop) = oneshot::channel::<()>();
        let (bound, address) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                bound.send(listener.local_addr().unwrap()).unwrap();
                chess_server::serve(listener, config, async {
                    let _ = stop.await;
                })
                .await
            })
        });
        let address = address.recv_timeout(Duration::from_secs(10)).unwrap();
        Server {
            address: address.to_string(),
            shutdown,
            thread,
        }
    }

    fn url(&self) -> String {
        format!("ws://{}/ws", self.address)
    }

    /// Shut down gracefully, which flushes state to disk.
    fn stop(self) {
        let _ = self.shutdown.send(());
        self.thread.join().unwrap().unwrap();
    }
}

/// Wait for the first server event that `pick` accepts.
fn expect<T>(client: &OnlineClient, mut pick: impl FnMut(ServerEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the server"
        );
        match client.try_recv() {
            Some(TransportEvent::Message(envelope)) => {
                if let Some(found) = pick(envelope.event) {
                    return found;
                }
            }
            Some(TransportEvent::Stopped(reason)) => panic!("client stopped: {reason}"),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn updated_to_ply(client: &OnlineClient, ply: usize) -> GameSnapshot {
    expect(client, |event| match event {
        ServerEvent::GameUpdated { game } if game.moves.len() == ply => Some(game),
        ServerEvent::MoveRejected { reason, .. } => panic!("move rejected: {reason:?}"),
        _ => None,
    })
}

/// Create a game as White and have a second client join it as Black.
fn start_game(white: &OnlineClient, black: &OnlineClient, black_name: &str) -> GameSnapshot {
    white
        .send(ClientCommand::CreateGame {
            player_name: "ignored for accounts".to_string(),
            time_control: TimeControl {
                initial_ms: 60_000,
                increment_ms: 0,
            },
        })
        .unwrap();
    let invite_code = expect(white, |event| match event {
        ServerEvent::GameCreated { invite_code, .. } => Some(invite_code),
        _ => None,
    });
    black
        .send(ClientCommand::JoinGame {
            invite_code,
            player_name: black_name.to_string(),
        })
        .unwrap();
    expect(black, |event| match event {
        ServerEvent::GameJoined { game, .. } => Some(game),
        _ => None,
    })
}

fn play_fools_mate(white: &OnlineClient, black: &OnlineClient, game_id: &str) {
    let moves = [
        (white, "f2f3"),
        (black, "e7e5"),
        (white, "g2g4"),
        (black, "d8h4"),
    ];
    for (ply, (client, uci)) in moves.into_iter().enumerate() {
        client
            .send(ClientCommand::PlayMove {
                game_id: game_id.to_string(),
                expected_ply: ply as u32,
                uci: uci.to_string(),
            })
            .unwrap();
        updated_to_ply(white, ply + 1);
        updated_to_ply(black, ply + 1);
    }
}

fn error_code(client: &OnlineClient) -> ErrorCode {
    expect(client, |event| match event {
        ServerEvent::Error { code, .. } => Some(code),
        _ => None,
    })
}

#[test]
fn an_account_keeps_its_games_across_connections() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::start(&directory.path().join("server-state.json"));

    let white = OnlineClient::connect(server.url());
    white
        .send(ClientCommand::Register {
            username: "Ada".to_string(),
            password: "analytical engine".to_string(),
        })
        .unwrap();
    let token = expect(&white, |event| match event {
        ServerEvent::SignedIn {
            account,
            session_token,
        } => {
            assert_eq!(account.username, "Ada");
            session_token
        }
        _ => None,
    });

    let black = OnlineClient::connect(server.url());
    black
        .send(ClientCommand::Register {
            username: "ada".to_string(),
            password: "someone else".to_string(),
        })
        .unwrap();
    assert_eq!(error_code(&black), ErrorCode::UsernameTaken);
    black
        .send(ClientCommand::LogIn {
            username: "ada".to_string(),
            password: "wrong password".to_string(),
        })
        .unwrap();
    assert_eq!(error_code(&black), ErrorCode::InvalidCredentials);
    black.send(ClientCommand::ListGames { limit: 5 }).unwrap();
    assert_eq!(error_code(&black), ErrorCode::NotSignedIn);

    // Black stays a guest; White plays under the account's username.
    let game = start_game(&white, &black, "Grace");
    assert_eq!(game.white.name, "Ada");
    assert!(game.white.registered);
    assert!(!game.black.registered);
    play_fools_mate(&white, &black, &game.game_id);
    drop(white);
    drop(black);

    // A new connection signs in with the saved token alone.
    let returning = OnlineClient::connect(server.url());
    returning
        .send(ClientCommand::Authenticate {
            session_token: token.clone(),
        })
        .unwrap();
    let username = expect(&returning, |event| match event {
        ServerEvent::SignedIn {
            account,
            session_token: None,
        } => Some(account.username),
        _ => None,
    });
    assert_eq!(username, "Ada");
    // Finished games are recorded on the next clock tick.
    let deadline = Instant::now() + Duration::from_secs(5);
    let games = loop {
        returning
            .send(ClientCommand::ListGames { limit: 5 })
            .unwrap();
        let games = expect(&returning, |event| match event {
            ServerEvent::GameList { games } => Some(games),
            _ => None,
        });
        if !games.is_empty() || Instant::now() > deadline {
            break games;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].game_id, game.game_id);
    assert_eq!(games[0].black.name, "Grace");
    assert_eq!(games[0].result, GameResult::BlackWins);
    assert_eq!(games[0].reason, FinishReason::Checkmate);
    assert_eq!(games[0].moves, ["f2f3", "e7e5", "g2g4", "d8h4"]);

    returning.send(ClientCommand::LogOut).unwrap();
    expect(&returning, |event| {
        matches!(event, ServerEvent::SignedOut).then_some(())
    });
    returning
        .send(ClientCommand::Authenticate {
            session_token: token,
        })
        .unwrap();
    assert_eq!(error_code(&returning), ErrorCode::InvalidSession);

    drop(returning);
    server.stop();
}

#[test]
fn two_clients_finish_a_game_over_websockets() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("server-state.json");
    let server = Server::start(&state);

    let white = OnlineClient::connect(server.url());
    white
        .send(ClientCommand::CreateGame {
            player_name: "Ada".to_string(),
            time_control: TimeControl {
                initial_ms: 60_000,
                increment_ms: 0,
            },
        })
        .unwrap();
    let (invite_code, game_id) = expect(&white, |event| match event {
        ServerEvent::GameCreated {
            invite_code, game, ..
        } => Some((invite_code, game.game_id)),
        _ => None,
    });

    let black = OnlineClient::connect(server.url());
    black
        .send(ClientCommand::JoinGame {
            invite_code: invite_code.to_ascii_lowercase(),
            player_name: "Grace".to_string(),
        })
        .unwrap();
    expect(&black, |event| match event {
        ServerEvent::GameJoined { game, .. } => {
            assert_eq!(game.white.name, "Ada");
            Some(())
        }
        _ => None,
    });

    let moves = [
        (&white, "f2f3"),
        (&black, "e7e5"),
        (&white, "g2g4"),
        (&black, "d8h4"),
    ];
    let mut last = None;
    for (ply, (client, uci)) in moves.into_iter().enumerate() {
        client
            .send(ClientCommand::PlayMove {
                game_id: game_id.clone(),
                expected_ply: ply as u32,
                uci: uci.to_string(),
            })
            .unwrap();
        updated_to_ply(&white, ply + 1);
        last = Some(updated_to_ply(&black, ply + 1));
    }

    assert_eq!(
        last.unwrap().status,
        GameStatus::Finished {
            result: GameResult::BlackWins,
            reason: FinishReason::Checkmate,
        }
    );

    drop(white);
    drop(black);
    server.stop();
    let saved = std::fs::read_to_string(&state).expect("state is flushed on shutdown");
    assert!(saved.contains(&game_id));
    assert!(saved.contains("d8h4"));
}
