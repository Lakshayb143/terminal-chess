//! End-to-end: a real SSH client visits the gateway, which starts the real
//! terminal client for it against an in-process game server.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use chess_server::accounts::Accounts;
use chess_server::hub::Hub;
use chess_server::ssh::{serve_ssh, GatewayConfig};
use chess_server::store::Store;
use chess_server::{serve_with, Config};
use russh::client;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct TrustAnyHost;

impl client::Handler for TrustAnyHost {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// A game server and SSH gateway sharing one account database.
struct Servers {
    ssh_address: std::net::SocketAddr,
    accounts: Accounts,
    _stop: oneshot::Sender<()>,
    _directory: tempfile::TempDir,
}

async fn start() -> Servers {
    let directory = tempfile::tempdir().unwrap();
    let accounts = Accounts::new(Store::open(&directory.path().join("chess.db")).unwrap());
    let websocket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let websocket_address = websocket.local_addr().unwrap();
    let ssh = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ssh_address = ssh.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let stopped = futures_util::FutureExt::shared(async {
        let _ = stopped.await;
    });

    let config = Config {
        state_path: directory.path().join("state.json"),
        requests_per_window: 60,
        database_path: None,
    };
    tokio::spawn(serve_with(
        websocket,
        config,
        Hub::new(),
        Some(accounts.clone()),
        stopped.clone(),
    ));
    let gateway = GatewayConfig {
        host_key_path: directory.path().join("host_key"),
        client_program: env!("CARGO_BIN_EXE_chess").into(),
        server_url: format!("ws://{websocket_address}/ws"),
        max_sessions: 4,
    };
    tokio::spawn(serve_ssh(ssh, gateway, Some(accounts.clone()), stopped));
    Servers {
        ssh_address,
        accounts,
        _stop: stop,
        _directory: directory,
    }
}

/// Connect with `key`, open a terminal, and return everything the game shows
/// until `expected` appears; then leave through the menu.
async fn visit(servers: &Servers, key: PrivateKey, expected: &str) -> String {
    let config = Arc::new(client::Config::default());
    let mut session = client::connect(config, servers.ssh_address, TrustAnyHost)
        .await
        .unwrap();
    let authenticated = session
        .authenticate_publickey("anyone", PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .unwrap();
    assert!(authenticated.success(), "any key is welcome");
    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm-256color", 100, 40, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(true).await.unwrap();

    let mut screen = String::new();
    let seen = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(message) = channel.wait().await {
            if let ChannelMsg::Data { data } = message {
                let text = String::from_utf8_lossy(&data);
                // Answer the terminal queries the client probes with, as a
                // real terminal emulator would.
                if text.contains("\u{1b}[c") {
                    channel.data_bytes(&b"\x1b[?62;22c"[..]).await.unwrap();
                }
                if text.contains("\u{1b}[5n") {
                    channel.data_bytes(&b"\x1b[0n"[..]).await.unwrap();
                }
                if text.contains("\u{1b}[6n") {
                    channel.data_bytes(&b"\x1b[1;1R"[..]).await.unwrap();
                }
                screen.push_str(&text);
                if strip_escapes(&screen).contains(expected) {
                    return true;
                }
            }
        }
        false
    })
    .await;
    assert_eq!(
        seen,
        Ok(true),
        "expected {expected:?} on screen, saw:\n{}",
        strip_escapes(&screen)
    );

    channel.data_bytes(&b"q\r"[..]).await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = channel.wait().await {
            if let ChannelMsg::ExitStatus { exit_status } = message {
                return Some(exit_status);
            }
        }
        None
    })
    .await;
    assert_eq!(exit, Ok(Some(0)), "leaving the menu ends the visit");
    strip_escapes(&screen)
}

fn strip_escapes(text: &str) -> String {
    let mut plain = String::new();
    let mut characters = text.chars().peekable();
    while let Some(c) = characters.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        // Skip a CSI sequence: ESC [ parameters final-byte.
        if characters.peek() == Some(&'[') {
            characters.next();
            for c in characters.by_ref() {
                if c.is_ascii_alphabetic() || c == '~' {
                    break;
                }
            }
        }
    }
    plain
}

#[tokio::test(flavor = "multi_thread")]
async fn a_visitor_plays_as_a_guest_until_their_key_is_linked() {
    let servers = start().await;
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();

    let screen = visit(&servers, key.clone(), "sign in or sign up").await;
    assert!(screen.contains("playing as a guest"));

    // Link the key the way signing in over SSH does, then visit again.
    let user = servers
        .accounts
        .register("carol", "correct horse")
        .await
        .unwrap()
        .user;
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let ticket = servers.accounts.issue_link_ticket(&fingerprint);
    servers
        .accounts
        .link_ssh_key(user.id, &ticket)
        .await
        .unwrap();

    let screen = visit(&servers, key, "your recent games").await;
    assert!(screen.contains("signed in as carol"));
}
