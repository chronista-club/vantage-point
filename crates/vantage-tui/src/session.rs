use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<SessionMessage>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

impl Session {
    pub fn new() -> Self {
        let now = chrono_lite::now();
        let id = format!("session_{}", now.replace(['-', ':', ' '], ""));
        Self {
            id,
            created_at: now.clone(),
            updated_at: now,
            messages: Vec::new(),
            summary: None,
        }
    }

    pub fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push(SessionMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: chrono_lite::now(),
        });
        self.updated_at = chrono_lite::now();
    }

    pub fn set_summary(&mut self, summary: &str) {
        self.summary = Some(summary.to_string());
    }
}

/// Simple datetime helper (avoid heavy chrono dependency)
mod chrono_lite {
    use std::time::{SystemTime, UNIX_EPOCH};

    pub fn now() -> String {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let secs = duration.as_secs();

        // Simple UTC timestamp format
        let days = secs / 86400;
        let remaining = secs % 86400;
        let hours = remaining / 3600;
        let minutes = (remaining % 3600) / 60;
        let seconds = remaining % 60;

        // Approximate date calculation (good enough for session IDs)
        let year = 1970 + (days / 365);
        let day_of_year = days % 365;
        let month = day_of_year / 30 + 1;
        let day = day_of_year % 30 + 1;

        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            year, month, day, hours, minutes, seconds
        )
    }
}

/// Session storage
pub struct SessionStore {
    base_path: PathBuf,
}

impl SessionStore {
    pub fn new() -> Self {
        let base_path = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("vantage-point")
            .join("sessions");
        Self { base_path }
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.base_path)?;
        Ok(())
    }

    pub fn save(&self, session: &Session) -> Result<PathBuf> {
        self.ensure_dir()?;
        let path = self.base_path.join(format!("{}.json", session.id));
        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }

    pub fn load(&self, session_id: &str) -> Result<Session> {
        let path = self.base_path.join(format!("{}.json", session_id));
        let json = std::fs::read_to_string(path)?;
        let session: Session = serde_json::from_str(&json)?;
        Ok(session)
    }

    pub fn list_sessions(&self) -> Result<Vec<(String, String, Option<String>)>> {
        self.ensure_dir()?;
        let mut sessions = Vec::new();

        for entry in std::fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<Session>(&json) {
                        sessions.push((
                            session.id,
                            session.updated_at,
                            session.summary,
                        ));
                    }
                }
            }
        }

        // Sort by updated_at descending
        sessions.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(sessions)
    }

    pub fn get_latest(&self) -> Option<Session> {
        self.list_sessions().ok().and_then(|sessions| {
            sessions.first().and_then(|(id, _, _)| self.load(id).ok())
        })
    }
}
