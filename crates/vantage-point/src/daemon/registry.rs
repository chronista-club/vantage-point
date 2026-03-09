//! セッション・ペインの管理レジストリ
//!
//! Daemon が管理するセッション（タブ）とペイン（プロセス）のデータ構造。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// セッション識別子
pub type SessionId = String;
/// ペイン識別子（セッション内で一意）
pub type PaneId = u32;

/// ペインの種類
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PaneKind {
    /// PTYプロセス（shell / claude cli等）
    Pty { pid: u32, shell_cmd: String },
    /// コンテンツ表示（show コマンド用）
    Content { content_type: String, body: String },
}

/// ペインの情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: PaneId,
    pub kind: PaneKind,
    pub cols: u16,
    pub rows: u16,
    pub active: bool,
}

/// セッションの情報（外部公開用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub panes: Vec<PaneInfo>,
    pub created_at: u64,
}

/// セッション（内部状態）
struct Session {
    info: SessionInfo,
    next_pane_id: PaneId,
}

/// セッション・ペインの管理レジストリ
#[derive(Default)]
pub struct SessionRegistry {
    sessions: HashMap<SessionId, Session>,
    default_session: Option<SessionId>,
}

impl SessionRegistry {
    /// 新しいレジストリを作成
    pub fn new() -> Self {
        Self::default()
    }

    /// セッションを作成
    pub fn create_session(&mut self, id: &str) -> &SessionInfo {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let session = Session {
            info: SessionInfo {
                id: id.to_string(),
                panes: Vec::new(),
                created_at: now,
            },
            next_pane_id: 0,
        };
        self.sessions.insert(id.to_string(), session);

        // 最初のセッションをデフォルトに設定
        if self.default_session.is_none() {
            self.default_session = Some(id.to_string());
        }

        &self.sessions[id].info
    }

    /// セッションを削除
    pub fn remove_session(&mut self, id: &str) -> bool {
        let removed = self.sessions.remove(id).is_some();
        if removed {
            // デフォルトセッションが削除された場合、リセット
            if self.default_session.as_deref() == Some(id) {
                self.default_session = self.sessions.keys().next().cloned();
            }
        }
        removed
    }

    /// セッション情報を取得
    pub fn get_session(&self, id: &str) -> Option<&SessionInfo> {
        self.sessions.get(id).map(|s| &s.info)
    }

    /// 全セッションの一覧を取得
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions.values().map(|s| s.info.clone()).collect()
    }

    /// セッションにペインを追加
    pub fn add_pane(
        &mut self,
        session_id: &str,
        kind: PaneKind,
        cols: u16,
        rows: u16,
    ) -> Option<PaneId> {
        let session = self.sessions.get_mut(session_id)?;
        let pane_id = session.next_pane_id;
        session.next_pane_id += 1;

        let is_first = session.info.panes.is_empty();
        let pane = PaneInfo {
            id: pane_id,
            kind,
            cols,
            rows,
            active: is_first, // 最初のペインをアクティブに
        };
        session.info.panes.push(pane);

        Some(pane_id)
    }

    /// セッションからペインを削除
    pub fn remove_pane(&mut self, session_id: &str, pane_id: PaneId) -> bool {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return false;
        };

        let before_len = session.info.panes.len();
        session.info.panes.retain(|p| p.id != pane_id);
        let removed = session.info.panes.len() < before_len;

        // アクティブペインが削除された場合、最初のペインをアクティブに
        if removed
            && !session.info.panes.iter().any(|p| p.active)
            && let Some(first) = session.info.panes.first_mut()
        {
            first.active = true;
        }

        removed
    }

    /// デフォルトセッションIDを取得
    pub fn default_session(&self) -> Option<&str> {
        self.default_session.as_deref()
    }

    /// デフォルトセッションを設定
    pub fn set_default_session(&mut self, id: &str) {
        self.default_session = Some(id.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_list_sessions() {
        let mut registry = SessionRegistry::new();

        // セッション作成
        let info = registry.create_session("session-1");
        assert_eq!(info.id, "session-1");
        assert!(info.panes.is_empty());

        registry.create_session("session-2");

        // 一覧取得
        let sessions = registry.list_sessions();
        assert_eq!(sessions.len(), 2);

        // 個別取得
        let s1 = registry.get_session("session-1");
        assert!(s1.is_some());
        assert_eq!(s1.unwrap().id, "session-1");

        // 存在しないセッション
        assert!(registry.get_session("nonexistent").is_none());
    }

    #[test]
    fn test_add_and_remove_panes() {
        let mut registry = SessionRegistry::new();
        registry.create_session("s1");

        // ペイン追加
        let pane0 = registry.add_pane(
            "s1",
            PaneKind::Pty {
                pid: 1234,
                shell_cmd: "/bin/zsh".to_string(),
            },
            80,
            24,
        );
        assert_eq!(pane0, Some(0));

        let pane1 = registry.add_pane(
            "s1",
            PaneKind::Content {
                content_type: "markdown".to_string(),
                body: "# Hello".to_string(),
            },
            80,
            24,
        );
        assert_eq!(pane1, Some(1));

        // ペイン確認
        let session = registry.get_session("s1").unwrap();
        assert_eq!(session.panes.len(), 2);
        assert!(session.panes[0].active); // 最初のペインがアクティブ
        assert!(!session.panes[1].active);

        // ペイン削除
        assert!(registry.remove_pane("s1", 0));
        let session = registry.get_session("s1").unwrap();
        assert_eq!(session.panes.len(), 1);
        assert!(session.panes[0].active); // 残ったペインがアクティブに

        // 存在しないペインの削除
        assert!(!registry.remove_pane("s1", 99));

        // 存在しないセッションへのペイン操作
        assert!(
            registry
                .add_pane(
                    "nonexistent",
                    PaneKind::Pty {
                        pid: 0,
                        shell_cmd: "sh".to_string()
                    },
                    80,
                    24
                )
                .is_none()
        );
        assert!(!registry.remove_pane("nonexistent", 0));
    }

    #[test]
    fn test_default_session() {
        let mut registry = SessionRegistry::new();

        // 初期状態ではデフォルトなし
        assert!(registry.default_session().is_none());

        // 最初のセッション作成でデフォルトに設定される
        registry.create_session("first");
        assert_eq!(registry.default_session(), Some("first"));

        // 2番目のセッション作成ではデフォルトは変わらない
        registry.create_session("second");
        assert_eq!(registry.default_session(), Some("first"));

        // 手動でデフォルトを変更
        registry.set_default_session("second");
        assert_eq!(registry.default_session(), Some("second"));
    }

    #[test]
    fn test_remove_session() {
        let mut registry = SessionRegistry::new();
        registry.create_session("s1");
        registry.create_session("s2");
        assert_eq!(registry.default_session(), Some("s1"));

        // s1（デフォルト）を削除 → デフォルトが別のセッションに切り替わる
        assert!(registry.remove_session("s1"));
        assert!(registry.get_session("s1").is_none());
        assert!(registry.default_session().is_some()); // s2 がデフォルトに

        // 存在しないセッションの削除
        assert!(!registry.remove_session("nonexistent"));

        // 最後のセッションを削除
        assert!(registry.remove_session("s2"));
        assert!(registry.default_session().is_none());
        assert!(registry.list_sessions().is_empty());
    }
}
