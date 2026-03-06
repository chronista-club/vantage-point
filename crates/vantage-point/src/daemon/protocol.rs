//! Daemon IPC プロトコルのメッセージ型定義
//!
//! Session / Terminal / System の3チャネルに対応する
//! リクエスト・レスポンス型を定義する。

use serde::{Deserialize, Serialize};

// =============================================================================
// Unified Channel メッセージ
// =============================================================================

/// Unison Channel 通信用メッセージ型
///
/// Daemon ↔ Console、Process ↔ MCP の双方で使用する共通エンベロープ。
/// 1つのチャネル上で Request/Response/Error/Event を多重化する。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChannelMessage {
    /// クライアント → サーバーのリクエスト
    #[serde(rename = "request")]
    Request {
        id: u64,
        method: String,
        payload: serde_json::Value,
    },
    /// サーバー → クライアントのレスポンス
    #[serde(rename = "response")]
    Response { id: u64, payload: serde_json::Value },
    /// サーバー → クライアントのエラーレスポンス
    #[serde(rename = "error")]
    Error { id: u64, message: String },
    /// サーバー → クライアントの一方向イベント（PTY出力など）
    #[serde(rename = "event")]
    Event {
        method: String,
        payload: serde_json::Value,
    },
}

impl ChannelMessage {
    /// 成功レスポンスを作成
    pub fn ok(id: u64, payload: serde_json::Value) -> Self {
        Self::Response { id, payload }
    }

    /// エラーレスポンスを作成
    pub fn err(id: u64, message: impl Into<String>) -> Self {
        Self::Error {
            id,
            message: message.into(),
        }
    }

    /// イベントを作成
    pub fn event(method: impl Into<String>, payload: serde_json::Value) -> Self {
        Self::Event {
            method: method.into(),
            payload,
        }
    }
}

// =============================================================================
// Session Channel
// =============================================================================

/// セッション作成リクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    /// セッションID（プロジェクト名など）
    pub session_id: String,
}

/// セッション作成レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    /// 作成されたセッションの情報（JSON文字列として受け渡し）
    pub session_id: String,
    pub created_at: u64,
}

/// セッション一覧レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListSessionsResponse {
    /// セッション情報のリスト
    pub sessions: Vec<SessionSummary>,
}

/// セッション概要（一覧表示用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub pane_count: usize,
    pub created_at: u64,
}

/// セッションアタッチリクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachRequest {
    /// アタッチ先のセッションID
    pub session_id: String,
}

/// セッションデタッチリクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachRequest {
    /// デタッチするセッションID
    pub session_id: String,
}

// =============================================================================
// Terminal Channel
// =============================================================================

/// ペイン作成リクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePaneRequest {
    /// 対象セッションID
    pub session_id: String,
    /// 起動するシェルコマンド
    #[serde(default = "default_shell")]
    pub shell_cmd: String,
    /// ターミナル幅（カラム数）
    #[serde(default = "default_cols")]
    pub cols: u16,
    /// ターミナル高さ（行数）
    #[serde(default = "default_rows")]
    pub rows: u16,
}

/// ペイン作成レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePaneResponse {
    /// 作成されたペインID
    pub pane_id: u32,
}

/// PTY入力書き込みリクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRequest {
    /// 対象セッションID
    pub session_id: String,
    /// 対象ペインID
    pub pane_id: u32,
    /// 入力データ（base64エンコード済み）
    pub data: String,
}

/// ペインリサイズリクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeRequest {
    /// 対象セッションID
    pub session_id: String,
    /// 対象ペインID
    pub pane_id: u32,
    /// 新しい幅（カラム数）
    pub cols: u16,
    /// 新しい高さ（行数）
    pub rows: u16,
}

/// ペイン終了リクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillPaneRequest {
    /// 対象セッションID
    pub session_id: String,
    /// 対象ペインID
    pub pane_id: u32,
}

/// PTY出力読み取りリクエスト
///
/// 指定ペインの PTY 出力を読み取る（ポーリング型）。
/// タイムアウト内に出力があればデータを返し、なければ空を返す。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadOutputRequest {
    /// 対象セッションID
    pub session_id: String,
    /// 対象ペインID
    pub pane_id: u32,
    /// 待機タイムアウト（ミリ秒）
    #[serde(default = "default_read_timeout_ms")]
    pub timeout_ms: u64,
}

/// PTY出力読み取りレスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadOutputResponse {
    /// 出力データ（base64エンコード済み、空の場合はタイムアウト）
    pub data: String,
    /// 読み取ったバイト数
    pub bytes_read: usize,
}

// =============================================================================
// System Channel
// =============================================================================

/// ヘルスチェックレスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// ステータス文字列（"ok" など）
    pub status: String,
    /// 管理中のセッション数
    pub sessions_count: usize,
    /// Daemon起動からの経過秒数
    pub uptime_secs: u64,
}

// =============================================================================
// デフォルト値関数
// =============================================================================

/// デフォルトの出力読み取りタイムアウト（ミリ秒）
pub fn default_read_timeout_ms() -> u64 {
    50
}

/// デフォルトのシェルコマンドを返す（$SHELL環境変数 or /bin/zsh）
pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

/// デフォルトのカラム数
pub fn default_cols() -> u16 {
    80
}

/// デフォルトの行数
pub fn default_rows() -> u16 {
    24
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_shell() {
        let shell = default_shell();
        // $SHELL が設定されていれば空でないことを確認
        // 設定されていなければ /bin/zsh がデフォルト
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_default_dimensions() {
        assert_eq!(default_cols(), 80);
        assert_eq!(default_rows(), 24);
    }

    #[test]
    fn test_create_session_request_serialize() {
        let req = CreateSessionRequest {
            session_id: "vantage-point".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("vantage-point"));

        let deserialized: CreateSessionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "vantage-point");
    }

    #[test]
    fn test_create_pane_request_defaults() {
        // デフォルト値付きでデシリアライズ
        let json = r#"{"session_id": "test"}"#;
        let req: CreatePaneRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.session_id, "test");
        assert_eq!(req.cols, 80);
        assert_eq!(req.rows, 24);
        assert!(!req.shell_cmd.is_empty());
    }

    #[test]
    fn test_create_pane_request_custom_values() {
        let json = r#"{"session_id": "test", "shell_cmd": "/bin/bash", "cols": 120, "rows": 40}"#;
        let req: CreatePaneRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.shell_cmd, "/bin/bash");
        assert_eq!(req.cols, 120);
        assert_eq!(req.rows, 40);
    }

    #[test]
    fn test_write_request_serialize() {
        let req = WriteRequest {
            session_id: "test-session".to_string(),
            pane_id: 0,
            data: "bHMgLWxhCg==".to_string(), // "ls -la\n" in base64
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: WriteRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "test-session");
        assert_eq!(deserialized.pane_id, 0);
        assert_eq!(deserialized.data, "bHMgLWxhCg==");
    }

    #[test]
    fn test_resize_request_serialize() {
        let req = ResizeRequest {
            session_id: "test".to_string(),
            pane_id: 1,
            cols: 160,
            rows: 48,
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: ResizeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.cols, 160);
        assert_eq!(deserialized.rows, 48);
    }

    #[test]
    fn test_health_response_serialize() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            sessions_count: 3,
            uptime_secs: 3600,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: HealthResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.status, "ok");
        assert_eq!(deserialized.sessions_count, 3);
        assert_eq!(deserialized.uptime_secs, 3600);
    }

    #[test]
    fn test_list_sessions_response_serialize() {
        let resp = ListSessionsResponse {
            sessions: vec![
                SessionSummary {
                    id: "project-a".to_string(),
                    pane_count: 2,
                    created_at: 1708500000,
                },
                SessionSummary {
                    id: "project-b".to_string(),
                    pane_count: 1,
                    created_at: 1708500100,
                },
            ],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: ListSessionsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.sessions.len(), 2);
        assert_eq!(deserialized.sessions[0].id, "project-a");
        assert_eq!(deserialized.sessions[1].pane_count, 1);
    }

    #[test]
    fn test_kill_pane_request_serialize() {
        let req = KillPaneRequest {
            session_id: "my-session".to_string(),
            pane_id: 42,
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: KillPaneRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, "my-session");
        assert_eq!(deserialized.pane_id, 42);
    }
}
