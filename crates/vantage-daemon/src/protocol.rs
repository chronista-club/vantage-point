//! Protocol definitions for communication between components

use serde::{Deserialize, Serialize};

/// Content types that can be displayed in the viewer
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// Plain text log
    Log(String),
    /// Markdown content
    Markdown(String),
    /// Base64-encoded image
    ImageBase64 { data: String, mime_type: String },
    /// Raw HTML
    Html(String),
}

/// Message from daemon to browser (WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Show content in a pane
    Show {
        pane_id: String,
        content: Content,
        append: bool,
    },
    /// Clear a pane
    Clear { pane_id: String },
    /// Split a pane
    Split {
        pane_id: String,
        direction: SplitDirection,
        new_pane_id: String,
    },
    /// Close a pane
    Close { pane_id: String },
    /// Ping for keepalive
    Ping,
}

/// Split direction
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// Message from browser to daemon (WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserMessage {
    /// Browser is ready
    Ready,
    /// Pong response
    Pong,
    /// User action (future)
    Action { pane_id: String, action: String },
}

/// Internal message for IPC (Unix Socket or internal channel)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: Option<String>,
    pub payload: DaemonMessage,
}
