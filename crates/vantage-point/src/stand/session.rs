//! セッション管理モジュール
//!
//! 複数のClaude CLIセッションを管理し、状態の永続化を行う。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::protocol::SessionInfo;

/// Chat message for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredMessage {
    pub role: String,
    pub content: String,
    pub timestamp: u64,
}

/// Session entry with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionEntry {
    pub id: String,
    pub name: String,
    pub message_count: usize,
    pub model: Option<String>,
    /// Session creation timestamp (Unix millis)
    #[serde(default = "default_created_at")]
    pub created_at: u64,
    /// Chat history for this session
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
}

fn default_created_at() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Persisted state for hot reload
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    /// Active session ID
    active_id: Option<String>,
    /// All known sessions
    sessions: HashMap<String, SessionEntry>,
    /// Counter for generating session names
    session_counter: usize,
    /// Project directory
    project_dir: String,
}

/// Session manager for multiple Claude sessions
#[derive(Debug, Default)]
pub(crate) struct SessionManager {
    /// Active session ID
    pub active_id: Option<String>,
    /// All known sessions: session_id -> SessionEntry
    sessions: HashMap<String, SessionEntry>,
    /// Counter for generating session names
    session_counter: usize,
    /// Port number for state file path
    port: u16,
    /// Project directory
    project_dir: String,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// セッション数を返す
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Create with port and project_dir, attempting to restore from saved state
    pub fn with_config(port: u16, project_dir: String) -> Self {
        let state_path = Self::state_path(port);

        // Try to load existing state
        if let Ok(data) = std::fs::read_to_string(&state_path)
            && let Ok(state) = serde_json::from_str::<PersistedState>(&data)
        {
            // Only restore if same project directory
            if state.project_dir == project_dir {
                tracing::info!("Restored session state from {:?}", state_path);
                return Self {
                    active_id: state.active_id,
                    sessions: state.sessions,
                    session_counter: state.session_counter,
                    port,
                    project_dir,
                };
            } else {
                tracing::info!("Project dir changed, starting fresh session");
            }
        }

        Self {
            port,
            project_dir,
            ..Default::default()
        }
    }

    /// Get state file path for a port
    fn state_path(port: u16) -> PathBuf {
        crate::config::config_dir()
            .join("state")
            .join(format!("{}.json", port))
    }

    /// Save state to file
    fn save(&self) {
        let state = PersistedState {
            active_id: self.active_id.clone(),
            sessions: self.sessions.clone(),
            session_counter: self.session_counter,
            project_dir: self.project_dir.clone(),
        };

        let state_path = Self::state_path(self.port);

        // Ensure directory exists
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(&state) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&state_path, json) {
                    tracing::warn!("Failed to save session state: {}", e);
                } else {
                    tracing::debug!("Saved session state to {:?}", state_path);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize session state: {}", e);
            }
        }
    }

    /// Get or create active session for chat
    /// Returns (session_id, is_continue) where is_continue=true means use --continue
    pub fn get_active_session(&self) -> (Option<String>, bool) {
        if let Some(ref id) = self.active_id {
            (Some(id.clone()), false) // Explicit --resume <id>
        } else {
            // No active session - use --continue for most recent
            (None, true)
        }
    }

    /// Set the active session ID
    pub fn set_active_session(&mut self, id: String) {
        self.active_id = Some(id);
        self.save();
    }

    /// Register a session from Claude CLI init event
    pub fn register_session(&mut self, id: String, model: Option<String>) {
        if !self.sessions.contains_key(&id) {
            self.session_counter += 1;
            let name = format!("Session {}", self.session_counter);
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            self.sessions.insert(
                id.clone(),
                SessionEntry {
                    id: id.clone(),
                    name,
                    message_count: 0,
                    model,
                    created_at,
                    messages: Vec::new(),
                },
            );
        }
        self.active_id = Some(id);
        self.save();
    }

    /// Add a message to the active session
    pub fn add_message(&mut self, role: &str, content: String) {
        if let Some(ref id) = self.active_id
            && let Some(entry) = self.sessions.get_mut(id)
        {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            entry.messages.push(StoredMessage {
                role: role.to_string(),
                content,
                timestamp,
            });
            self.save();
        }
    }

    /// Get messages for a session
    pub fn get_messages(&self, session_id: &str) -> Vec<StoredMessage> {
        self.sessions
            .get(session_id)
            .map(|e| e.messages.clone())
            .unwrap_or_default()
    }

    /// Increment message count for active session
    pub fn increment_message_count(&mut self) {
        if let Some(ref id) = self.active_id
            && let Some(entry) = self.sessions.get_mut(id)
        {
            entry.message_count += 1;
            self.save();
        }
    }

    /// Switch to a different session
    pub fn switch_to(&mut self, session_id: &str) -> Option<&SessionEntry> {
        if self.sessions.contains_key(session_id) {
            self.active_id = Some(session_id.to_string());
            self.save();
            self.sessions.get(session_id)
        } else {
            None
        }
    }

    /// Create a new session (will be registered when Claude CLI responds)
    pub fn prepare_new_session(&mut self) {
        self.active_id = None;
        self.save();
    }

    /// Rename a session
    pub fn rename(&mut self, session_id: &str, new_name: String) -> bool {
        if let Some(entry) = self.sessions.get_mut(session_id) {
            entry.name = new_name;
            self.save();
            true
        } else {
            false
        }
    }

    /// Close/remove a session
    pub fn close(&mut self, session_id: &str) -> bool {
        if self.sessions.remove(session_id).is_some() {
            if self.active_id.as_deref() == Some(session_id) {
                // Switch to another session or none
                self.active_id = self.sessions.keys().next().cloned();
            }
            self.save();
            true
        } else {
            false
        }
    }

    /// Get all sessions as SessionInfo for UI
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut sessions: Vec<_> = self
            .sessions
            .values()
            .map(|e| SessionInfo {
                id: e.id.clone(),
                name: e.name.clone(),
                is_active: self.active_id.as_deref() == Some(&e.id),
                message_count: e.message_count,
                model: e.model.clone(),
                created_at: e.created_at,
            })
            .collect();
        // Sort by created_at descending (newest first)
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        sessions
    }
}
