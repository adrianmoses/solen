use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::ToolCall;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, RwLock};

/// Opaque session identifier. A user can have multiple concurrent sessions.
pub type SessionId = String;

/// Messages sent from the server (agent loop) to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Session established.
    SessionStarted { session_id: String },
    /// Final agent response text.
    AgentResponse { answer: Option<String> },
    /// Agent is requesting approval for tool calls.
    ConfirmationPrompt {
        request_id: String,
        tool_calls: Vec<ToolCall>,
        reasons: Vec<String>,
    },
    /// A tool was executed (progress notification).
    ToolExecuted { tool_name: String, success: bool },
    /// The agent turn errored out.
    AgentError { error: String },
}

/// Messages sent from the client to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// A new user message to start an agent turn.
    UserMessage { message: String },
    /// Response to a ConfirmationPrompt.
    ApprovalResponse { request_id: String, approved: bool },
}

/// Per-session handle stored in the registry.
pub struct SessionHandle {
    /// Send server messages to the client (agent -> websocket).
    pub server_tx: mpsc::Sender<ServerMessage>,
    /// The user_id associated with this session.
    pub user_id: String,
    /// Pending approval oneshots, keyed by request_id.
    pub pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

/// Global session registry, stored in AppState.
pub type SessionRegistry = Arc<RwLock<HashMap<SessionId, SessionHandle>>>;

pub fn new_registry() -> SessionRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}
