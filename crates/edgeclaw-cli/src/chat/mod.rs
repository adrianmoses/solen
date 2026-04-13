mod connection;

use std::io::{self, Write};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use edgeclaw_server::session::{ClientMessage, ServerMessage};

use crate::ChatArgs;

/// RAII guard that disables raw mode on drop.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

pub async fn run_chat(args: ChatArgs) -> Result<()> {
    let conn = connection::connect_and_handshake(&args.connect).await?;
    let mut ws_write = conn.write;
    let ws_read = conn.read;
    let server_handle = conn.server_handle;

    println!("Connected. Session: {}\r", conn.session_id);

    let _raw_guard = RawModeGuard::enable()?;

    // Channel for server messages from the WS reader task
    let (server_tx, mut server_rx) = mpsc::channel::<ServerMessage>(32);

    // Spawn WS reader task
    let reader_handle = tokio::spawn(async move {
        let mut ws_read = ws_read;
        while let Some(result) = ws_read.next().await {
            match result {
                Ok(Message::Text(text)) => {
                    if let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) {
                        if server_tx.send(msg).await.is_err() {
                            break;
                        }
                    } else {
                        tracing::warn!("Could not parse server message: {text}");
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    tracing::debug!("WebSocket read error: {e}");
                    break;
                }
                _ => {}
            }
        }
    });

    let mut event_stream = EventStream::new();
    let mut input_buffer = String::new();
    let mut pending_approval: Option<String> = None;

    draw_prompt(&input_buffer, pending_approval.is_some());

    loop {
        tokio::select! {
            Some(Ok(event)) = event_stream.next() => {
                match event {
                    Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL, .. })
                    | Event::Key(KeyEvent { code: KeyCode::Char('d'), modifiers: KeyModifiers::CONTROL, .. }) => {
                        break;
                    }
                    Event::Key(KeyEvent { code: KeyCode::Char(ch), modifiers, .. }) => {
                        // Handle y/n for pending approval
                        if pending_approval.is_some() && modifiers.is_empty() && (ch == 'y' || ch == 'n') {
                            let request_id = pending_approval.take().unwrap();
                            let approved = ch == 'y';
                            let label = if approved { "approved" } else { "denied" };
                            clear_line();
                            print_line(&format!("[{label}]\r\n"));
                            let msg = ClientMessage::ApprovalResponse { request_id, approved };
                            let json = serde_json::to_string(&msg)?;
                            ws_write.send(Message::Text(json.into())).await?;
                            draw_prompt(&input_buffer, false);
                        } else {
                            input_buffer.push(ch);
                            draw_prompt(&input_buffer, false);
                        }
                    }
                    Event::Key(KeyEvent { code: KeyCode::Backspace, .. }) => {
                        input_buffer.pop();
                        draw_prompt(&input_buffer, pending_approval.is_some());
                    }
                    Event::Key(KeyEvent { code: KeyCode::Enter, .. }) => {
                        if !input_buffer.is_empty() {
                            let message = input_buffer.clone();
                            input_buffer.clear();
                            clear_line();
                            print_line(&format!("you> {message}\r\n"));
                            let msg = ClientMessage::UserMessage { message };
                            let json = serde_json::to_string(&msg)?;
                            ws_write.send(Message::Text(json.into())).await?;
                            draw_prompt(&input_buffer, false);
                        }
                    }
                    _ => {}
                }
            }
            Some(msg) = server_rx.recv() => {
                clear_line();
                render_server_message(&msg);
                if let ServerMessage::ConfirmationPrompt { request_id, .. } = msg {
                    pending_approval = Some(request_id);
                }
                draw_prompt(&input_buffer, pending_approval.is_some());
            }
            else => break,
        }
    }

    // Cleanup: close WebSocket and stop in-process server if we spawned one
    let _ = ws_write.send(Message::Close(None)).await;
    reader_handle.abort();
    if let Some(handle) = server_handle {
        handle.abort();
    }
    clear_line();
    print_line("Disconnected.\r\n");

    Ok(())
}

/// Clear the current line and move cursor to the beginning.
fn clear_line() {
    print!("\r\x1b[2K");
    let _ = io::stdout().flush();
}

/// Print a line (must include `\r\n` for raw mode).
fn print_line(s: &str) {
    print!("{s}");
    let _ = io::stdout().flush();
}

/// Draw the input prompt.
fn draw_prompt(buffer: &str, approval_mode: bool) {
    clear_line();
    if approval_mode {
        print!("[y] approve  [n] deny > ");
    } else {
        print!("you> {buffer}");
    }
    let _ = io::stdout().flush();
}

/// Render a server message to stdout.
fn render_server_message(msg: &ServerMessage) {
    match msg {
        ServerMessage::SessionStarted { .. } => {}
        ServerMessage::AgentResponse { answer } => {
            if let Some(text) = answer {
                for line in text.lines() {
                    print_line(&format!("\x1b[32magent>\x1b[0m {line}\r\n"));
                }
            }
        }
        ServerMessage::ConfirmationPrompt {
            tool_calls,
            reasons,
            ..
        } => {
            print_line("\r\n\x1b[33m┌─ Tool approval ─────────────────────────────\x1b[0m\r\n");
            for (i, tc) in tool_calls.iter().enumerate() {
                print_line(&format!("\x1b[33m│\x1b[0m  \x1b[1m{}\x1b[0m", tc.name));
                if !reasons.is_empty() {
                    if let Some(reason) = reasons.get(i) {
                        print_line(&format!("  ({reason})"));
                    }
                }
                print_line("\r\n");
                // Show a compact summary of the tool input
                let input_str = serde_json::to_string_pretty(&tc.input).unwrap_or_default();
                for (j, line) in input_str.lines().enumerate() {
                    if j >= 5 {
                        print_line("\x1b[33m│\x1b[0m    \x1b[2m...\x1b[0m\r\n");
                        break;
                    }
                    print_line(&format!("\x1b[33m│\x1b[0m    \x1b[2m{line}\x1b[0m\r\n"));
                }
            }
            print_line("\x1b[33m└──────────────────────────────────────────────\x1b[0m\r\n");
        }
        ServerMessage::ToolExecuted { tool_name, success } => {
            let status = if *success { "ok" } else { "failed" };
            print_line(&format!("\x1b[2m[tool] {tool_name}: {status}\x1b[0m\r\n"));
        }
        ServerMessage::AgentError { error } => {
            print_line(&format!("\x1b[31merror>\x1b[0m {error}\r\n"));
        }
    }
}
