//! WebSocket transport for authoritative online games and player accounts.

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
use chess_protocol::{
    Account, ClientCommand, ClientEnvelope, ErrorCode, ServerEnvelope, ServerEvent,
    MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::accounts::{AccountError, Accounts, SignIn};
use crate::hub::{write_state, ConnectionId, Delivery, Hub, Identity};
use crate::store::{Store, User};

const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// Events queued for one client before it is treated as stalled and dropped.
const OUTBOX_CAPACITY: usize = 256;
const TICK_INTERVAL: Duration = Duration::from_millis(200);
const PERSIST_INTERVAL: Duration = Duration::from_millis(500);
const SESSION_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Password checks one connection may ask for per minute.
const PASSWORD_ATTEMPTS_PER_MINUTE: u32 = 10;
const MAX_HISTORY_GAMES: u32 = 100;

#[derive(Clone)]
struct AppState {
    hub: Arc<Mutex<Hub>>,
    peers: Arc<Mutex<HashMap<ConnectionId, mpsc::Sender<ServerEnvelope>>>>,
    state_path: Arc<PathBuf>,
    /// Serializes state writes so an older snapshot never replaces a newer one.
    persist_lock: Arc<Mutex<()>>,
    requests_per_window: u32,
    /// `None` when the server runs without a database: guests only.
    accounts: Option<Accounts>,
}

/// Server settings, independent of where they were read from.
#[derive(Clone, Debug)]
pub struct Config {
    /// Durable room state; created on first save.
    pub state_path: PathBuf,
    /// Requests one connection may send in each 10-second window.
    pub requests_per_window: u32,
    /// The SQLite database for accounts and game history. Without one the
    /// server only offers guest games.
    pub database_path: Option<PathBuf>,
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
    let accounts = match &config.database_path {
        Some(path) => {
            Some(Accounts::new(Store::open(path).map_err(|error| {
                format!("could not open the database: {error}")
            })?))
        }
        None => None,
    };
    serve_with(listener, config, hub, accounts, shutdown).await
}

/// [`serve`] with an account service the caller already opened, so the SSH
/// gateway and the WebSocket server can share it.
pub async fn serve_with(
    listener: TcpListener,
    config: Config,
    hub: Hub,
    accounts: Option<Accounts>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let state = AppState {
        hub: Arc::new(Mutex::new(hub)),
        peers: Arc::new(Mutex::new(HashMap::new())),
        state_path: Arc::new(config.state_path),
        persist_lock: Arc::new(Mutex::new(())),
        requests_per_window: config.requests_per_window,
        accounts,
    };

    let ticker = spawn_ticker(state.clone());
    let persister = spawn_persister(state.clone());
    let cleaner = spawn_session_cleaner(state.clone());
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", any(websocket))
        .with_state(state.clone());
    if let Ok(address) = listener.local_addr() {
        info!(
            %address,
            state = %state.state_path.display(),
            accounts = state.accounts.is_some(),
            "server listening"
        );
    }
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|error| format!("server failed: {error}"));
    ticker.abort();
    persister.abort();
    cleaner.abort();
    persist(&state).await;
    record_finished_games(&state).await;
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
    let mut account = ConnectionAccount::default();
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
                    match &state.accounts {
                        Some(accounts) if answers_account_command(&request) => {
                            handle_account(&state, accounts, connection, &mut account, request)
                                .await
                        }
                        _ => state.hub.lock().await.handle(connection, request),
                    }
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
            record_finished_games(&state).await;
        }
    })
}

/// Hand games that just ended to the history database.
async fn record_finished_games(state: &AppState) {
    let finished = state.hub.lock().await.take_finished_games();
    let Some(accounts) = &state.accounts else {
        return;
    };
    let count = finished.len();
    if let Err(error) = accounts.record_games(finished).await {
        error!(error = ?error, count, "could not record finished games");
    }
}

fn spawn_session_cleaner(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(accounts) = state.accounts else {
            return;
        };
        let mut interval = tokio::time::interval(SESSION_CLEANUP_INTERVAL);
        loop {
            interval.tick().await;
            match accounts.delete_idle_sessions().await {
                Ok(0) => {}
                Ok(removed) => info!(removed, "removed idle sessions"),
                Err(error) => error!(error = ?error, "could not remove idle sessions"),
            }
        }
    })
}

/// What one connection has proved about who is using it.
#[derive(Default)]
struct ConnectionAccount {
    /// The session behind the sign-in, revoked by `LogOut`.
    session_token: Option<String>,
    password_attempts: Option<RequestRate>,
}

fn answers_account_command(request: &ClientEnvelope) -> bool {
    (MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&request.protocol)
        && matches!(
            request.command,
            ClientCommand::Register { .. }
                | ClientCommand::LogIn { .. }
                | ClientCommand::Authenticate { .. }
                | ClientCommand::LogOut
                | ClientCommand::ListGames { .. }
                | ClientCommand::LinkSshKey { .. }
        )
}

/// Answer an account command. These wait on the database and on password
/// hashing, so they run here rather than under the hub lock.
async fn handle_account(
    state: &AppState,
    accounts: &Accounts,
    connection: ConnectionId,
    account: &mut ConnectionAccount,
    request: ClientEnvelope,
) -> Vec<Delivery> {
    let request_id = request.request_id;
    let signed_in = || async { state.hub.lock().await.identity(connection).cloned() };
    let outcome: Result<ServerEvent, (ErrorCode, String)> = match request.command {
        ClientCommand::Register { username, password } => {
            if !account.allow_password_attempt() {
                Err(too_many_attempts())
            } else {
                match accounts.register(&username, &password).await {
                    Ok(sign_in) => {
                        info!(connection, username = %sign_in.user.username, "account created");
                        Ok(adopt_sign_in(state, connection, account, sign_in).await)
                    }
                    Err(error) => Err(account_error(connection, error)),
                }
            }
        }
        ClientCommand::LogIn { username, password } => {
            if !account.allow_password_attempt() {
                Err(too_many_attempts())
            } else {
                match accounts.log_in(&username, &password).await {
                    Ok(sign_in) => {
                        info!(connection, username = %sign_in.user.username, "logged in");
                        Ok(adopt_sign_in(state, connection, account, sign_in).await)
                    }
                    Err(error) => Err(account_error(connection, error)),
                }
            }
        }
        ClientCommand::Authenticate { session_token } => {
            match accounts.authenticate(&session_token).await {
                Ok(user) => {
                    sign_in_connection(state, connection, &user).await;
                    account.session_token = Some(session_token);
                    Ok(ServerEvent::SignedIn {
                        account: Account::from(&user),
                        session_token: None,
                    })
                }
                Err(error) => Err(account_error(connection, error)),
            }
        }
        ClientCommand::LogOut => {
            state.hub.lock().await.sign_out(connection);
            match account.session_token.take() {
                Some(token) => accounts
                    .log_out(&token)
                    .await
                    .map(|()| ServerEvent::SignedOut)
                    .map_err(|error| account_error(connection, error)),
                None => Ok(ServerEvent::SignedOut),
            }
        }
        ClientCommand::ListGames { limit } => match signed_in().await {
            Some(identity) => accounts
                .games_for(identity.user, limit.clamp(1, MAX_HISTORY_GAMES))
                .await
                .map(|games| ServerEvent::GameList { games })
                .map_err(|error| account_error(connection, error)),
            None => Err(not_signed_in()),
        },
        ClientCommand::LinkSshKey { ticket } => match signed_in().await {
            Some(identity) => accounts
                .link_ssh_key(identity.user, &ticket)
                .await
                .map(|()| {
                    info!(connection, username = %identity.username, "linked an SSH key");
                    ServerEvent::SshKeyLinked
                })
                .map_err(|error| account_error(connection, error)),
            None => Err(not_signed_in()),
        },
        _ => unreachable!("only account commands are routed here"),
    };
    let mut hub = state.hub.lock().await;
    match outcome {
        Ok(event) => hub.reply(connection, request_id, event),
        Err((code, message)) => hub.reply_error(connection, request_id, code, message),
    }
}

async fn adopt_sign_in(
    state: &AppState,
    connection: ConnectionId,
    account: &mut ConnectionAccount,
    sign_in: SignIn,
) -> ServerEvent {
    sign_in_connection(state, connection, &sign_in.user).await;
    account.session_token = Some(sign_in.token.clone());
    ServerEvent::SignedIn {
        account: Account::from(&sign_in.user),
        session_token: Some(sign_in.token),
    }
}

async fn sign_in_connection(state: &AppState, connection: ConnectionId, user: &User) {
    state.hub.lock().await.sign_in(
        connection,
        Identity {
            user: user.id,
            username: user.username.clone(),
        },
    );
}

impl ConnectionAccount {
    fn allow_password_attempt(&mut self) -> bool {
        self.password_attempts
            .get_or_insert_with(|| {
                RequestRate::with_window(PASSWORD_ATTEMPTS_PER_MINUTE, Duration::from_secs(60))
            })
            .allow()
    }
}

fn account_error(connection: ConnectionId, error: AccountError) -> (ErrorCode, String) {
    if let AccountError::Storage(detail) = &error {
        error!(connection, %detail, "account storage failed");
    }
    (error.code(), error.message())
}

fn too_many_attempts() -> (ErrorCode, String) {
    (
        ErrorCode::RateLimited,
        "too many password attempts; wait a minute and try again".to_string(),
    )
}

fn not_signed_in() -> (ErrorCode, String) {
    (ErrorCode::NotSignedIn, "sign in to see this".to_string())
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
    window: Duration,
    count: u32,
    window_started: Instant,
}

impl RequestRate {
    fn new(limit: u32) -> RequestRate {
        RequestRate::with_window(limit, Duration::from_secs(10))
    }

    fn with_window(limit: u32, window: Duration) -> RequestRate {
        RequestRate {
            limit,
            window,
            count: 0,
            window_started: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        if self.window_started.elapsed() >= self.window {
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
        ClientCommand::Register { .. } => "register",
        ClientCommand::LogIn { .. } => "log_in",
        ClientCommand::Authenticate { .. } => "authenticate",
        ClientCommand::LogOut => "log_out",
        ClientCommand::ListGames { .. } => "list_games",
        ClientCommand::LinkSshKey { .. } => "link_ssh_key",
    }
}
