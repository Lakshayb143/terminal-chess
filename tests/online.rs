//! End-to-end: the real server binary and two real WebSocket clients.

use std::path::Path;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chess::client::{OnlineClient, TransportEvent};
use chess_protocol::{
    ClientCommand, FinishReason, GameResult, GameSnapshot, GameStatus, ServerEvent, TimeControl,
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
