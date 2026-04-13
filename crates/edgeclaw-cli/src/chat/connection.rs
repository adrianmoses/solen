use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use edgeclaw_server::session::ServerMessage;
use edgeclaw_server::startup::{run_server, RunOptions};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type WsWrite = SplitSink<WsStream, Message>;
pub type WsRead = SplitStream<WsStream>;

/// Result of a successful connection, including an optional handle to a
/// server that was spawned in-process.
pub struct Connection {
    pub session_id: String,
    pub write: WsWrite,
    pub read: WsRead,
    /// If we spawned the server ourselves, this handle lets us shut it down.
    pub server_handle: Option<JoinHandle<()>>,
}

/// Connect to the server, spawning one in-process if none is running.
///
/// 1. Try connecting to `url`.
/// 2. If the connection is refused, parse host/port from the URL, spawn
///    `run_server` in a background task, wait for it to accept connections,
///    then connect.
/// 3. Complete the handshake and return the split stream.
pub async fn connect_and_handshake(url: &str) -> Result<Connection> {
    match connect_async(url).await {
        Ok((ws, _)) => {
            let (session_id, write, read) = handshake(ws).await?;
            Ok(Connection {
                session_id,
                write,
                read,
                server_handle: None,
            })
        }
        Err(_) => {
            // Connection failed — spawn server in-process
            let (host, port) = parse_host_port(url)?;
            eprintln!("No server found at {url}, starting one in-process...");

            let server_handle = tokio::spawn(async move {
                if let Err(e) = run_server(RunOptions {
                    host: Some(host),
                    port: Some(port),
                })
                .await
                {
                    tracing::error!("In-process server error: {e}");
                }
            });

            // Poll until the server is accepting connections
            wait_for_server(url, Duration::from_secs(10)).await?;

            let (ws, _) = connect_async(url)
                .await
                .context("Failed to connect to in-process server")?;

            let (session_id, write, read) = handshake(ws).await?;
            Ok(Connection {
                session_id,
                write,
                read,
                server_handle: Some(server_handle),
            })
        }
    }
}

/// Complete the WebSocket handshake: send user_id, receive session_started.
async fn handshake(ws: WsStream) -> Result<(String, WsWrite, WsRead)> {
    let (mut write, mut read) = ws.split();

    let handshake = serde_json::json!({"user_id": "default"});
    write
        .send(Message::Text(handshake.to_string().into()))
        .await
        .context("Failed to send handshake")?;

    let msg = read
        .next()
        .await
        .context("Server closed connection during handshake")?
        .context("WebSocket error during handshake")?;

    let text = match msg {
        Message::Text(t) => t,
        other => bail!("Expected text message during handshake, got: {other:?}"),
    };

    let server_msg: ServerMessage =
        serde_json::from_str(&text).context("Failed to parse handshake response")?;

    let session_id = match server_msg {
        ServerMessage::SessionStarted { session_id } => session_id,
        other => bail!("Expected session_started, got: {other:?}"),
    };

    Ok((session_id, write, read))
}

/// Parse host and port from a WebSocket URL like `ws://127.0.0.1:7100/ws`.
fn parse_host_port(url: &str) -> Result<(String, u16)> {
    let parsed = url::Url::parse(url).context("Invalid WebSocket URL")?;
    let host = parsed.host_str().context("No host in URL")?.to_string();
    let port = parsed.port().unwrap_or(80);
    Ok((host, port))
}

/// Poll until the server accepts a TCP connection, or time out.
async fn wait_for_server(url: &str, timeout: Duration) -> Result<()> {
    let (host, port) = parse_host_port(url)?;
    let addr = format!("{host}:{port}");
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("Timed out waiting for in-process server to start");
        }
        if TcpStream::connect(&addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
