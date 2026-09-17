//! Resilient WebSocket transport used by the interactive terminal client.
//!
//! The worker owns its Tokio runtime on a background thread so the terminal's
//! synchronous crossterm loop stays small and predictable. Reconnection is a
//! transport concern; restoring a game seat remains an explicit protocol
//! command from the UI once a socket reconnects.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{ClientCommand, ClientEnvelope, ServerEnvelope};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub enum TransportEvent {
    Connecting { attempt: u32 },
    Connected,
    Message(ServerEnvelope),
    Disconnected { reason: String, retry_in: Duration },
    Stopped(String),
}

enum WorkerCommand {
    Send(ClientCommand),
    Stop,
}

pub struct OnlineClient {
    commands: tokio_mpsc::UnboundedSender<WorkerCommand>,
    events: Receiver<TransportEvent>,
}

impl OnlineClient {
    pub fn connect(url: String) -> OnlineClient {
        let (commands, command_rx) = tokio_mpsc::unbounded_channel();
        let (event_tx, events) = mpsc::channel();
        thread::Builder::new()
            .name("terminal-chess-network".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(worker(url, command_rx, event_tx)),
                    Err(error) => {
                        let _ = event_tx.send(TransportEvent::Stopped(format!(
                            "could not start network runtime: {error}"
                        )));
                    }
                }
            })
            .expect("could not start network thread");
        OnlineClient { commands, events }
    }

    pub fn send(&self, command: ClientCommand) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::Send(command))
            .map_err(|_| "the network worker has stopped".to_string())
    }

    pub fn try_recv(&self) -> Option<TransportEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for OnlineClient {
    fn drop(&mut self) {
        let _ = self.commands.send(WorkerCommand::Stop);
    }
}

async fn worker(
    url: String,
    mut commands: tokio_mpsc::UnboundedReceiver<WorkerCommand>,
    events: mpsc::Sender<TransportEvent>,
) {
    let mut attempt = 0u32;
    let mut next_request_id = 1u64;

    loop {
        attempt = attempt.saturating_add(1);
        if events.send(TransportEvent::Connecting { attempt }).is_err() {
            return;
        }
        let connected = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&url)).await;
        let (mut socket, _) = match connected {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                if !retry(&mut commands, &events, attempt, error.to_string()).await {
                    return;
                }
                continue;
            }
            Err(_) => {
                if !retry(
                    &mut commands,
                    &events,
                    attempt,
                    format!(
                        "connection attempt timed out after {} seconds",
                        CONNECT_TIMEOUT.as_secs()
                    ),
                )
                .await
                {
                    return;
                }
                continue;
            }
        };

        attempt = 0;
        if events.send(TransportEvent::Connected).is_err() {
            return;
        }
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let disconnect_reason = loop {
            tokio::select! {
                command = commands.recv() => {
                    match command {
                        Some(WorkerCommand::Send(command)) => {
                            let envelope = ClientEnvelope::new(next_request_id, command);
                            next_request_id = next_request_id.wrapping_add(1).max(1);
                            let json = match serde_json::to_string(&envelope) {
                                Ok(json) => json,
                                Err(error) => {
                                    let _ = events.send(TransportEvent::Stopped(format!(
                                        "could not encode network request: {error}"
                                    )));
                                    return;
                                }
                            };
                            if let Err(error) = socket.send(Message::Text(json.into())).await {
                                break format!("send failed: {error}");
                            }
                        }
                        Some(WorkerCommand::Stop) | None => {
                            let _ = socket.close(None).await;
                            return;
                        }
                    }
                }
                incoming = socket.next() => {
                    match incoming {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<ServerEnvelope>(&text) {
                                Ok(message) => {
                                    if events.send(TransportEvent::Message(message)).is_err() {
                                        return;
                                    }
                                }
                                Err(error) => break format!("server sent an invalid message: {error}"),
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if let Err(error) = socket.send(Message::Pong(payload)).await {
                                break format!("heartbeat reply failed: {error}");
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(frame))) => {
                            break frame.map_or_else(
                                || "server closed the connection".to_string(),
                                |frame| format!("server closed the connection: {}", frame.reason),
                            );
                        }
                        Some(Ok(Message::Binary(_) | Message::Frame(_))) => {}
                        Some(Err(error)) => break format!("connection failed: {error}"),
                        None => break "server closed the connection".to_string(),
                    }
                }
                _ = heartbeat.tick() => {
                    if let Err(error) = socket.send(Message::Ping(Vec::new().into())).await {
                        break format!("heartbeat failed: {error}");
                    }
                }
            }
        };

        if !retry(&mut commands, &events, 1, disconnect_reason).await {
            return;
        }
    }
}

async fn retry(
    commands: &mut tokio_mpsc::UnboundedReceiver<WorkerCommand>,
    events: &mpsc::Sender<TransportEvent>,
    attempt: u32,
    reason: String,
) -> bool {
    let exponent = attempt.saturating_sub(1).min(4);
    let retry_in = Duration::from_millis(500 * 2u64.pow(exponent)).min(MAX_RETRY_DELAY);
    if events
        .send(TransportEvent::Disconnected { reason, retry_in })
        .is_err()
    {
        return false;
    }
    tokio::select! {
        _ = tokio::time::sleep(retry_in) => true,
        command = commands.recv() => !matches!(command, Some(WorkerCommand::Stop) | None),
    }
}
