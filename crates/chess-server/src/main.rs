//! `chess-server`: read configuration from the environment and serve.

use std::env;
use std::path::PathBuf;

use chess_server::{serve, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_logging();
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
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|error| panic!("could not listen on {address}: {error}"));
    if let Err(error) = serve(listener, config, shutdown_signal()).await {
        eprintln!("chess-server: {error}");
        std::process::exit(1);
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
