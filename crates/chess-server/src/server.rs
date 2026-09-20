//! WebSocket transport for authoritative online guest games.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;
use chess_protocol::{ClientCommand, ClientEnvelope, ServerEnvelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::hub::{write_state, ConnectionId, Delivery, Hub};

const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// Events queued for one client before it is treated as stalled and dropped.
const OUTBOX_CAPACITY: usize = 256;
const TICK_INTERVAL: Duration = Duration::from_millis(200);
const PERSIST_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct AppState {
    hub: Arc<Mutex<Hub>>,
    peers: Arc<Mutex<HashMap<ConnectionId, mpsc::Sender<ServerEnvelope>>>>,
    state_path: Arc<PathBuf>,
    /// Serializes state writes so an older snapshot never replaces a newer one.
    persist_lock: Arc<Mutex<()>>,
    requests_per_window: u32,
}

/// Server settings, independent of where they were read from.
#[derive(Clone, Debug)]
pub struct Config {
    /// Durable room state; created on first save.
    pub state_path: PathBuf,
    /// Requests one connection may send in each 10-second window.
    pub requests_per_window: u32,
}

/// Serve `/health` and `/ws` on `listener` until `shutdown` resolves, then
/// flush state to disk.
pub async fn serve(
    listener: TcpListener,
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let hub = Hub::load(&config.state_path)
        .map_err(|error| format!("could not restore server state: {error}"))?;
    let state = AppState {
        hub: Arc::new(Mutex::new(hub)),
        peers: Arc::new(Mutex::new(HashMap::new())),
        state_path: Arc::new(config.state_path),
        persist_lock: Arc::new(Mutex::new(())),
        requests_per_window: config.requests_per_window,
    };

    let ticker = spawn_ticker(state.clone());
    let persister = spawn_persister(state.clone());
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", any(websocket))
        .with_state(state.clone());
    if let Ok(address) = listener.local_addr() {
        info!(%address, state = %state.state_path.display(), "server listening");
    }
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|error| format!("server failed: {error}"));
    ticker.abort();
    persister.abort();
    persist(&state).await;
    info!("server stopped");
    served
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| serve_socket(socket, state))
}

async fn serve_socket(socket: WebSocket, state: AppState) {
    let connection = state.hub.lock().await.connect();
    info!(connection, "client connected");
    let (outgoing, mut receiver) = mpsc::channel::<ServerEnvelope>(OUTBOX_CAPACITY);
    state.peers.lock().await.insert(connection, outgoing);

    let (mut writer, mut reader) = socket.split();
    let mut write_task = tokio::spawn(async move {
        while let Some(envelope) = receiver.recv().await {
            let Ok(json) = serde_json::to_string(&envelope) else {
                continue;
            };
            if writer.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
        // The sender was dropped because this client fell too far behind.
        let _ = writer.send(Message::Close(None)).await;
    });

    let mut rate = RequestRate::new(state.requests_per_window);
    loop {
        let message = tokio::select! {
            message = reader.next() => message,
            // The socket failed or the client stalled; stop reading from it.
            _ = &mut write_task => break,
        };
        let Some(message) = message else {
            break;
        };
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

fn spawn_ticker(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let deliveries = state.hub.lock().await.tick();
            dispatch(&state, deliveries).await;
        }
    })
}

/// Disk writes run on their own schedule so a slow disk never delays clocks.
fn spawn_persister(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PERSIST_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            persist(&state).await;
        }
    })
}

async fn dispatch(state: &AppState, deliveries: Vec<Delivery>) {
    let mut peers = state.peers.lock().await;
    for delivery in deliveries {
        let target = delivery.target;
        let Some(sender) = peers.get(&target) else {
            continue;
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = sender.try_send(delivery.message) {
            // Dropping the sender ends the writer task, which closes the socket.
            warn!(connection = target, "client outbox full; disconnecting");
            peers.remove(&target);
        }
    }
}

async fn persist(state: &AppState) {
    let _writer = state.persist_lock.lock().await;
    // Only encoding holds the hub lock; the write and fsync happen after it
    // is released so game traffic never waits on the disk.
    let encoded = state.hub.lock().await.encode_if_dirty();
    let bytes = match encoded {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return,
        Err(error) => {
            error!(%error, "could not encode server state");
            return;
        }
    };
    let path = Arc::clone(&state.state_path);
    let written = tokio::task::spawn_blocking(move || write_state(&path, &bytes))
        .await
        .unwrap_or_else(|error| Err(format!("state writer panicked: {error}")));
    if let Err(error) = written {
        error!(%error, path = %state.state_path.display(), "could not persist server state");
        state.hub.lock().await.mark_dirty();
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
