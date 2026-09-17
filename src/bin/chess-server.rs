//! WebSocket transport for authoritative online guest games.

use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;
use chess::online::{ConnectionId, Delivery, Hub};
use chess::protocol::{ClientEnvelope, ServerEnvelope};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};

#[derive(Clone)]
struct AppState {
    hub: Arc<Mutex<Hub>>,
    peers: Arc<Mutex<HashMap<ConnectionId, mpsc::UnboundedSender<ServerEnvelope>>>>,
}

#[tokio::main]
async fn main() {
    let address = env::var("CHESS_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let state = AppState {
        hub: Arc::new(Mutex::new(Hub::new())),
        peers: Arc::new(Mutex::new(HashMap::new())),
    };

    spawn_ticker(state.clone());
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", any(websocket))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|error| panic!("could not listen on {address}: {error}"));
    eprintln!("terminal-chess server listening on {address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| serve_socket(socket, state))
}

async fn serve_socket(socket: WebSocket, state: AppState) {
    let connection = state.hub.lock().await.connect();
    let (outgoing, mut receiver) = mpsc::unbounded_channel::<ServerEnvelope>();
    state.peers.lock().await.insert(connection, outgoing);

    let (mut writer, mut reader) = socket.split();
    let write_task = tokio::spawn(async move {
        while let Some(envelope) = receiver.recv().await {
            let Ok(json) = serde_json::to_string(&envelope) else {
                continue;
            };
            if writer.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(message) = reader.next().await {
        let deliveries = match message {
            Ok(Message::Text(text)) => match serde_json::from_str::<ClientEnvelope>(&text) {
                Ok(request) => state.hub.lock().await.handle(connection, request),
                Err(error) => state
                    .hub
                    .lock()
                    .await
                    .invalid_request(connection, format!("invalid JSON request: {error}")),
            },
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Binary(_)) => state
                .hub
                .lock()
                .await
                .invalid_request(connection, "binary messages are not supported"),
        };
        dispatch(&state, deliveries).await;
    }

    state.peers.lock().await.remove(&connection);
    let deliveries = state.hub.lock().await.disconnect(connection);
    dispatch(&state, deliveries).await;
    write_task.abort();
}

fn spawn_ticker(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        loop {
            interval.tick().await;
            let deliveries = state.hub.lock().await.tick();
            dispatch(&state, deliveries).await;
        }
    });
}

async fn dispatch(state: &AppState, deliveries: Vec<Delivery>) {
    let peers = state.peers.lock().await;
    for delivery in deliveries {
        if let Some(sender) = peers.get(&delivery.target) {
            let _ = sender.send(delivery.message);
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
