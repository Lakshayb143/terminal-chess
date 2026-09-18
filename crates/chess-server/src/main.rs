//! WebSocket transport for authoritative online guest games.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;
use chess_protocol::{ClientCommand, ClientEnvelope, ServerEnvelope};
use chess_server::hub::{ConnectionId, Delivery, Hub};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const MAX_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Clone)]
struct AppState {
    hub: Arc<Mutex<Hub>>,
    peers: Arc<Mutex<HashMap<ConnectionId, mpsc::UnboundedSender<ServerEnvelope>>>>,
    state_path: Arc<PathBuf>,
    requests_per_window: u32,
}

#[tokio::main]
async fn main() {
    init_logging();
    let address = env::var("CHESS_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let state_path = PathBuf::from(
        env::var("CHESS_SERVER_STATE").unwrap_or_else(|_| "data/server-state.json".to_string()),
    );
    let requests_per_window = env::var("CHESS_RATE_LIMIT_PER_10S")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&value| value > 0)
        .unwrap_or(60);
    let hub = Hub::load(&state_path).unwrap_or_else(|error| {
        panic!("could not restore server state: {error}");
    });
    let state = AppState {
        hub: Arc::new(Mutex::new(hub)),
        peers: Arc::new(Mutex::new(HashMap::new())),
        state_path: Arc::new(state_path),
        requests_per_window,
    };

    spawn_ticker(state.clone());
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", any(websocket))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|error| panic!("could not listen on {address}: {error}"));
    info!(%address, state = %state.state_path.display(), "server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
    persist(&state).await;
    info!("server stopped");
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| serve_socket(socket, state))
}

async fn serve_socket(socket: WebSocket, state: AppState) {
    let connection = state.hub.lock().await.connect();
    info!(connection, "client connected");
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

    let mut rate = RequestRate::new(state.requests_per_window);
    while let Some(message) = reader.next().await {
        let deliveries = match message {
            Ok(Message::Text(text)) if text.len() > MAX_MESSAGE_BYTES => state
                .hub
                .lock()
                .await
                .invalid_request(connection, "request is too large"),
            Ok(Message::Text(_)) if !rate.allow() => {
                warn!(connection, "client request rate limited");
                state.hub.lock().await.rate_limited(connection)
            }
            Ok(Message::Text(text)) => match serde_json::from_str::<ClientEnvelope>(&text) {
                Ok(request) => {
                    info!(
                        connection,
                        command = command_name(&request.command),
                        "request"
                    );
                    state.hub.lock().await.handle(connection, request)
                }
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
    info!(connection, "client disconnected");
}

fn spawn_ticker(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        loop {
            interval.tick().await;
            let deliveries = state.hub.lock().await.tick();
            dispatch(&state, deliveries).await;
            persist(&state).await;
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
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("could not listen for SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn persist(state: &AppState) {
    let mut hub = state.hub.lock().await;
    if hub.is_dirty() {
        if let Err(error) = hub.save(&state.state_path) {
            error!(%error, path = %state.state_path.display(), "could not persist server state");
        }
    }
}

struct RequestRate {
    limit: u32,
    count: u32,
    window_started: Instant,
}

impl RequestRate {
    fn new(limit: u32) -> RequestRate {
        RequestRate {
            limit,
            count: 0,
            window_started: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        if self.window_started.elapsed() >= Duration::from_secs(10) {
            self.window_started = Instant::now();
            self.count = 0;
        }
        self.count = self.count.saturating_add(1);
        self.count <= self.limit
    }
}

fn command_name(command: &ClientCommand) -> &'static str {
    match command {
        ClientCommand::Hello { .. } => "hello",
        ClientCommand::CreateGame { .. } => "create_game",
        ClientCommand::JoinGame { .. } => "join_game",
        ClientCommand::Reconnect { .. } => "reconnect",
        ClientCommand::PlayMove { .. } => "play_move",
        ClientCommand::OfferDraw { .. } => "offer_draw",
        ClientCommand::RespondDraw { .. } => "respond_draw",
        ClientCommand::Resign { .. } => "resign",
        ClientCommand::Ping => "ping",
    }
}

fn init_logging() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("chess_server=info"));
    if env::var("CHESS_LOG_FORMAT").is_ok_and(|value| value.eq_ignore_ascii_case("json")) {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}
