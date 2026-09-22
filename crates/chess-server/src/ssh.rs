//! SSH gateway: `ssh chess.example.com` opens Terminal Chess with nothing to
//! install.
//!
//! Each visitor gets the ordinary terminal client, started in a pseudo-terminal
//! of its own and connected to this server's WebSocket like any other client.
//! The gateway only carries keystrokes, output, and window sizes between SSH
//! and that terminal, and tells the client who is visiting:
//!
//! - a key already linked to an account signs straight in, with a session
//!   that is revoked when the visitor leaves;
//! - any other key comes with a one-time link ticket, so signing in or
//!   creating an account from the menu links the key and the next visit
//!   needs no password;
//! - visitors without a key are let in as guests.
//!
//! No SSH password is ever asked for: the chess account is the only account.

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, Server as _, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

use crate::accounts::Accounts;

/// Environment a visitor's SSH client may pass on to the chess client. These
/// describe the visitor's terminal, which is what the client needs to draw
/// the board well; nothing else is forwarded.
const FORWARDED_ENV: &[&str] = &[
    "COLORTERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
];
const MAX_ENV_VALUE: usize = 128;
const OUTPUT_CHUNK: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// The server's SSH host key, created on first start if missing.
    pub host_key_path: PathBuf,
    /// The terminal client started for each visitor.
    pub client_program: PathBuf,
    /// The WebSocket URL those clients connect to, normally this server's own.
    pub server_url: String,
    /// Visitors allowed at once; later arrivals are asked to come back.
    pub max_sessions: usize,
    /// Games one address may have open at once, so a single visitor cannot
    /// take every seat.
    pub max_per_address: usize,
}

/// Accept SSH visitors on `listener` until `shutdown` resolves.
pub async fn serve_ssh(
    listener: TcpListener,
    config: GatewayConfig,
    accounts: Option<Accounts>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let host_key = load_or_create_host_key(&config.host_key_path)?;
    let fingerprint = host_key.public_key().fingerprint(HashAlg::Sha256);
    let ssh_config = russh::server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey, MethodKind::KeyboardInteractive][..]),
        keys: vec![host_key],
        // Nothing is secret here, so there is no reason to slow anyone down.
        auth_rejection_time: Duration::from_millis(50),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(2 * 60 * 60)),
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        nodelay: true,
        ..Default::default()
    };
    if let Ok(address) = listener.local_addr() {
        info!(%address, host_key = %fingerprint, "ssh gateway listening");
    }
    let mut gateway = Gateway {
        shared: Arc::new(Shared {
            sessions: Arc::new(Semaphore::new(config.max_sessions)),
            addresses: Arc::default(),
            config,
            accounts,
        }),
    };
    let running = gateway.run_on_socket(Arc::new(ssh_config), &listener);
    let handle = running.handle();
    tokio::select! {
        result = running => result.map_err(|error| format!("ssh gateway failed: {error}")),
        () = shutdown => {
            handle.shutdown("the chess server is restarting; please reconnect in a moment".to_string());
            Ok(())
        }
    }
}

/// Read the host key, or create an Ed25519 key readable only by the server.
/// Keeping it stable matters: a new key makes every visitor's `ssh` warn that
/// the server may be an impostor.
pub fn load_or_create_host_key(path: &Path) -> Result<PrivateKey, String> {
    if path.exists() {
        return PrivateKey::read_openssh_file(path)
            .map_err(|error| format!("could not read SSH host key {}: {error}", path.display()));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .map_err(|error| format!("could not generate an SSH host key: {error}"))?;
    let pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|error| format!("could not encode the SSH host key: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(pem.as_bytes()))
        .map_err(|error| format!("could not write SSH host key {}: {error}", path.display()))?;
    info!(path = %path.display(), "created a new SSH host key");
    Ok(key)
}

struct Shared {
    config: GatewayConfig,
    accounts: Option<Accounts>,
    sessions: Arc<Semaphore>,
    addresses: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

/// One of the games an address has open, given back when the game ends.
struct AddressSlot {
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    address: IpAddr,
}

impl AddressSlot {
    /// `None` when `address` already has `limit` games open.
    fn take(
        counts: &Arc<Mutex<HashMap<IpAddr, usize>>>,
        address: IpAddr,
        limit: usize,
    ) -> Option<AddressSlot> {
        // An IPv4 visitor reached through an IPv6 socket is the same visitor.
        let address = address.to_canonical();
        let mut open = counts.lock().unwrap_or_else(PoisonError::into_inner);
        let count = open.entry(address).or_insert(0);
        if *count >= limit {
            return None;
        }
        *count += 1;
        Some(AddressSlot {
            counts: Arc::clone(counts),
            address,
        })
    }
}

impl Drop for AddressSlot {
    fn drop(&mut self) {
        let mut open = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.address) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.address);
            }
        }
    }
}

struct Gateway {
    shared: Arc<Shared>,
}

impl russh::server::Server for Gateway {
    type Handler = Visitor;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> Visitor {
        Visitor {
            shared: Arc::clone(&self.shared),
            peer,
            fingerprint: None,
            channel: None,
            term: "xterm-256color".to_string(),
            size: PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
            env: Vec::new(),
            running: None,
        }
    }
}

/// One SSH connection.
struct Visitor {
    shared: Arc<Shared>,
    peer: Option<SocketAddr>,
    /// The SHA-256 fingerprint of the key the visitor proved they hold.
    fingerprint: Option<String>,
    channel: Option<ChannelId>,
    term: String,
    size: PtySize,
    env: Vec<(String, String)>,
    running: Option<Running>,
}

/// The chess client running for a visitor, and what to undo when they leave.
struct Running {
    /// Only used to resize; the mutex makes the visitor shareable across
    /// awaits, which a bare terminal handle is not.
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: std_mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Holds the visitor's configuration and saves; deleted on drop.
    _home: tempfile::TempDir,
    _permit: OwnedSemaphorePermit,
    _address: Option<AddressSlot>,
    session_token: Option<String>,
    link_ticket: Option<String>,
}

impl Drop for Visitor {
    fn drop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        let _ = running.killer.kill();
        let Some(accounts) = self.shared.accounts.clone() else {
            return;
        };
        if let Some(ticket) = running.link_ticket.take() {
            accounts.revoke_link_ticket(&ticket);
        }
        if let Some(token) = running.session_token.take() {
            // The session only existed for this visit.
            tokio::spawn(async move {
                let _ = accounts.log_out(&token).await;
            });
        }
        info!(peer = ?self.peer, "ssh visitor left");
    }
}

impl russh::server::Handler for Visitor {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        // Ask for a key first, so a visitor who has one is recognised.
        Ok(Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(
                &[MethodKind::PublicKey, MethodKind::KeyboardInteractive][..],
            )),
            partial_success: false,
        })
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        self.fingerprint = Some(key.fingerprint(HashAlg::Sha256).to_string());
        Ok(Auth::Accept)
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        _response: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        // No key: welcome them as a guest without asking anything.
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel.is_none() {
            self.channel = Some(channel.id());
            reply.accept().await;
        }
        // A second channel on the same connection is rejected on drop.
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pixel_width: u32,
        _pixel_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !term.is_empty()
            && term.len() <= MAX_ENV_VALUE
            && term.chars().all(|c| c.is_ascii_graphic())
        {
            self.term = term.to_string();
        }
        self.size = pty_size(cols, rows);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let allowed = FORWARDED_ENV.contains(&name)
            && value.len() <= MAX_ENV_VALUE
            && !value.chars().any(char::is_control);
        if allowed {
            self.env.push((name.to_string(), value.to_string()));
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.running.is_some() || self.channel != Some(channel) {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let handle = session.handle();
        match self.start(channel, handle.clone()).await {
            Ok(running) => self.running = Some(running),
            Err(message) => {
                say_goodbye(&handle, channel, &message).await;
            }
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let handle = session.handle();
        say_goodbye(
            &handle,
            channel,
            "Terminal Chess runs no commands. Connect with plain `ssh` and the game opens.",
        )
        .await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // `sftp`, and `scp` in recent OpenSSH, ask for a file-transfer
        // subsystem. Left unanswered, they wait forever, so say no at once;
        // OpenSSH then reports "subsystem request failed" and exits.
        info!(peer = ?self.peer, subsystem = name, "ssh visitor asked for a subsystem");
        session.channel_failure(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(running) = &self.running {
            let _ = running.input.send(data.to_vec());
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        cols: u32,
        rows: u32,
        _pixel_width: u32,
        _pixel_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.size = pty_size(cols, rows);
        if let Some(running) = &self.running {
            if let Ok(master) = running.master.lock() {
                let _ = master.resize(self.size);
            }
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(running) = &mut self.running {
            let _ = running.killer.kill();
        }
        Ok(())
    }
}

impl Visitor {
    /// Start the chess client for this visitor and connect its terminal to
    /// the SSH channel.
    async fn start(&mut self, channel: ChannelId, handle: Handle) -> Result<Running, String> {
        let limit = self.shared.config.max_per_address;
        let address = match self.peer {
            Some(peer) => Some(
                AddressSlot::take(&self.shared.addresses, peer.ip(), limit).ok_or_else(|| {
                    info!(peer = ?self.peer, "ssh visitor has too many games open");
                    if limit == 1 {
                        "You already have a game open from this address. \
                         Close it, then connect again."
                            .to_string()
                    } else {
                        format!(
                            "You already have {limit} games open from this address. \
                             Close one, then connect again."
                        )
                    }
                })?,
            ),
            None => None,
        };
        let permit = Arc::clone(&self.shared.sessions)
            .try_acquire_owned()
            .map_err(|_| {
                "The chess server is full right now. Please try again in a few minutes."
            })?;
        let home = tempfile::Builder::new()
            .prefix("chess-visitor-")
            .tempdir()
            .map_err(|error| internal("could not prepare a session", error))?;

        let (session_token, link_ticket, username) = self.identify().await;
        let config = &self.shared.config;
        let mut command = CommandBuilder::new(&config.client_program);
        command.env_clear();
        command.cwd(home.path());
        command.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        command.env("HOME", home.path());
        command.env("TERM", &self.term);
        command.env("TERMINAL_CHESS_CONFIG", home.path().join("config.toml"));
        command.env("CHESS_SERVER_URL", &config.server_url);
        command.env("CHESS_HOSTED", "1");
        // Tells the client it is remote, which keeps sound off.
        let peer = self.peer.map_or_else(
            || "unknown 0".to_string(),
            |peer| format!("{} {}", peer.ip(), peer.port()),
        );
        command.env("SSH_CONNECTION", format!("{peer} 0 22"));
        for (name, value) in &self.env {
            command.env(name, value);
        }
        if let (Some(token), Some(username)) = (&session_token, &username) {
            command.env("CHESS_SESSION_TOKEN", token);
            command.env("CHESS_SESSION_USERNAME", username);
        }
        if let Some(ticket) = &link_ticket {
            command.env("CHESS_SSH_LINK_TICKET", ticket);
        }
        // So the account page can say which linked key is this visitor's.
        if let Some(fingerprint) = &self.fingerprint {
            command.env("CHESS_SSH_KEY_FINGERPRINT", fingerprint);
        }

        let pair = native_pty_system()
            .openpty(self.size)
            .map_err(|error| internal("could not open a terminal", error))?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| internal("could not start the game", error))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| internal("could not read the game's terminal", error))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| internal("could not write to the game's terminal", error))?;

        let input = spawn_input_writer(writer);
        let (output_tx, output_rx) = mpsc::channel(32);
        std::thread::Builder::new()
            .name("ssh-visitor-output".to_string())
            .spawn(move || {
                pump_output(reader, &output_tx);
                let status = child.wait().map_or(1, |status| status.exit_code());
                let _ = output_tx.blocking_send(Output::Exited(status));
            })
            .map_err(|error| internal("could not start the output reader", error))?;
        tokio::spawn(forward_output(output_rx, handle, channel));

        info!(
            peer = ?self.peer,
            username = username.as_deref().unwrap_or("guest"),
            has_key = self.fingerprint.is_some(),
            "ssh visitor started a game"
        );
        Ok(Running {
            master: Mutex::new(pair.master),
            input,
            killer,
            _home: home,
            _permit: permit,
            _address: address,
            session_token,
            link_ticket,
        })
    }

    /// A session for the account this visitor's key is linked to, or a
    /// ticket to link the key once they sign in.
    async fn identify(&self) -> (Option<String>, Option<String>, Option<String>) {
        let (Some(accounts), Some(fingerprint)) = (&self.shared.accounts, &self.fingerprint) else {
            return (None, None, None);
        };
        match accounts.user_for_ssh_key(fingerprint).await {
            Ok(Some(user)) => match accounts.start_session(user.id).await {
                Ok(token) => (Some(token), None, Some(user.username)),
                Err(error) => {
                    warn!(error = ?error, "could not start a session for a linked key");
                    (None, None, None)
                }
            },
            Ok(None) => (None, Some(accounts.issue_link_ticket(fingerprint)), None),
            Err(error) => {
                warn!(error = ?error, "could not look up an SSH key");
                (None, None, None)
            }
        }
    }
}

enum Output {
    Bytes(Vec<u8>),
    Exited(u32),
}

/// Copy the game's terminal output until the game exits.
fn pump_output(mut reader: Box<dyn Read + Send>, output: &mpsc::Sender<Output>) {
    let mut buffer = vec![0u8; OUTPUT_CHUNK];
    loop {
        match reader.read(&mut buffer) {
            // Linux reports EIO once the last process on the terminal exits.
            Ok(0) | Err(_) => return,
            Ok(count) => {
                if output
                    .blocking_send(Output::Bytes(buffer[..count].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Keystrokes go through a thread of their own, because writing to a
/// terminal blocks when the program on it is not reading.
fn spawn_input_writer(mut writer: Box<dyn Write + Send>) -> std_mpsc::Sender<Vec<u8>> {
    let (input, keystrokes) = std_mpsc::channel::<Vec<u8>>();
    let _ = std::thread::Builder::new()
        .name("ssh-visitor-input".to_string())
        .spawn(move || {
            for bytes in keystrokes {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    return;
                }
            }
        });
    input
}

async fn forward_output(mut output: mpsc::Receiver<Output>, handle: Handle, channel: ChannelId) {
    while let Some(message) = output.recv().await {
        match message {
            Output::Bytes(bytes) => {
                if handle.data(channel, bytes).await.is_err() {
                    return;
                }
            }
            Output::Exited(status) => {
                let _ = handle.exit_status_request(channel, status).await;
                let _ = handle.eof(channel).await;
                let _ = handle.close(channel).await;
                return;
            }
        }
    }
}

async fn say_goodbye(handle: &Handle, channel: ChannelId, message: &str) {
    let _ = handle
        .data(channel, format!("{message}\r\n").into_bytes())
        .await;
    let _ = handle.exit_status_request(channel, 1).await;
    let _ = handle.eof(channel).await;
    let _ = handle.close(channel).await;
}

fn internal(what: &str, error: impl std::fmt::Display) -> String {
    warn!(%error, "{what}");
    "Something went wrong on the chess server. Please try again.".to_string()
}

fn pty_size(cols: u32, rows: u32) -> PtySize {
    let clamp = |value: u32, fallback: u16| {
        if value == 0 {
            fallback
        } else {
            value.min(1_000) as u16
        }
    };
    PtySize {
        rows: clamp(rows, 24),
        cols: clamp(cols, 80),
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_key_is_created_once_and_kept_private() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keys/ssh_host_ed25519_key");
        let created = load_or_create_host_key(&path).unwrap();
        let loaded = load_or_create_host_key(&path).unwrap();
        assert_eq!(created.public_key(), loaded.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    #[test]
    fn each_address_gets_a_few_games_and_gets_them_back() {
        let counts = Arc::default();
        let home: IpAddr = "203.0.113.7".parse().unwrap();
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        let other: IpAddr = "198.51.100.1".parse().unwrap();

        let first = AddressSlot::take(&counts, home, 2).unwrap();
        // The same visitor, seen through an IPv6 socket.
        let second = AddressSlot::take(&counts, mapped, 2).unwrap();
        assert!(AddressSlot::take(&counts, home, 2).is_none());
        assert!(AddressSlot::take(&counts, other, 2).is_some());

        drop(first);
        let third = AddressSlot::take(&counts, home, 2).unwrap();
        drop((second, third));
        assert!(counts.lock().unwrap().is_empty());
    }

    #[test]
    fn window_sizes_are_clamped_to_something_drawable() {
        assert_eq!(pty_size(0, 0).cols, 80);
        assert_eq!(pty_size(0, 0).rows, 24);
        assert_eq!(pty_size(5_000, 50).cols, 1_000);
        assert_eq!(pty_size(120, 40).rows, 40);
    }
}
