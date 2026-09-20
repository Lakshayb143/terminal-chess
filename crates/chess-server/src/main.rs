//! `chess-server`: read configuration from the environment and serve.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use chess_server::accounts::Accounts;
use chess_server::hub::Hub;
use chess_server::ssh::{serve_ssh, GatewayConfig};
use chess_server::store::Store;
use chess_server::{serve_with, Config};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_logging();
    if let Err(error) = run().await {
        eprintln!("chess-server: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let address = env::var("CHESS_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let config = Config {
        state_path: PathBuf::from(
            env::var("CHESS_SERVER_STATE").unwrap_or_else(|_| "data/server-state.json".to_string()),
        ),
        requests_per_window: env::var("CHESS_RATE_LIMIT_PER_10S")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&value| value > 0)
            .unwrap_or(60),
        database_path: match env::var("CHESS_SERVER_DB") {
            // An empty value runs a guests-only server with no database.
            Ok(path) if path.is_empty() => None,
            Ok(path) => Some(PathBuf::from(path)),
            Err(_) => Some(PathBuf::from("data/chess.db")),
        },
    };
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
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .map_err(|error| format!("could not listen on {address}: {error}"))?;
    let websocket_address = listener
        .local_addr()
        .map_err(|error| format!("could not read the listening address: {error}"))?;

    let (stop, stopped) = watch::channel(false);
    let shutdown = move || {
        let mut stopped = stopped.clone();
        async move {
            let _ = stopped.wait_for(|&stop| stop).await;
        }
    };
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop.send(true);
    });

    let gateway = match env::var("CHESS_SSH_ADDR")
        .ok()
        .filter(|value| !value.is_empty())
    {
        Some(ssh_address) => {
            let gateway = gateway_config(websocket_address)?;
            let ssh_listener = tokio::net::TcpListener::bind(&ssh_address)
                .await
                .map_err(|error| format!("could not listen for SSH on {ssh_address}: {error}"))?;
            Some(tokio::spawn(serve_ssh(
                ssh_listener,
                gateway,
                accounts.clone(),
                shutdown(),
            )))
        }
        None => None,
    };
    let served = serve_with(listener, config, hub, accounts, shutdown()).await;
    if let Some(gateway) = gateway {
        match gateway.await {
            Ok(result) => result?,
            Err(error) => return Err(format!("ssh gateway stopped unexpectedly: {error}")),
        }
    }
    served
}

fn gateway_config(websocket_address: SocketAddr) -> Result<GatewayConfig, String> {
    let client_program = match env::var("CHESS_CLIENT_BIN") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        // Installed side by side, as the container image does.
        _ => env::current_exe()
            .map_err(|error| format!("could not locate chess-server: {error}"))?
            .with_file_name("chess"),
    };
    if !client_program.is_file() {
        return Err(format!(
            "the SSH gateway needs the terminal client, but {} does not exist; set CHESS_CLIENT_BIN",
            client_program.display()
        ));
    }
    // Visitors' clients run on this machine, so they reach the WebSocket
    // server on loopback even when it listens on every interface.
    let port = websocket_address.port();
    let local = if websocket_address.ip().is_unspecified() {
        format!("127.0.0.1:{port}")
    } else {
        websocket_address.to_string()
    };
    Ok(GatewayConfig {
        host_key_path: PathBuf::from(
            env::var("CHESS_SSH_HOST_KEY")
                .unwrap_or_else(|_| "data/ssh_host_ed25519_key".to_string()),
        ),
        client_program,
        server_url: env::var("CHESS_SSH_SERVER_URL").unwrap_or_else(|_| format!("ws://{local}/ws")),
        max_sessions: env::var("CHESS_SSH_MAX_SESSIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|&value| value > 0)
            .unwrap_or(100),
    })
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
