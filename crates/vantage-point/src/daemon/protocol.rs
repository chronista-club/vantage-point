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
// world-process Channel (VP-154 PR-2)
// =============================================================================

/// VP-154 PR-2: Process lifecycle event (= World が SP の register/unregister を broadcast)
///
/// World 内側 hub の data plane を Unison Topic 経由で expose するための event 型。
/// Daemon の registry channel handler が SP register/unregister を受信したタイミングで、
/// `DaemonState.process_lifecycle_tx` に publish。 "world-process" channel の
/// subscribe handler が broadcast::Receiver から取り出して client に send_event で push。
///
/// ## Topic semantics (= 将来 TopicRouter migration の予定)
///
/// 現状は wire 上の serde tag (= `kind: "add" | "remove"`) と project_path で区別。
/// 将来的には `world/process/state/<project>` (retained) + `world/process/event` (transient)
/// の Topic 名で SP の TopicRouter と対称化予定 (= 別 PR scope)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessLifecycleEvent {
    /// SP が register された (= 新 Process が World 配下に加わった)
    Add {
        /// 正規化済 project path (= HashMap key と同じ form)
        project_path: String,
        /// 表示用 project name (例: `creo-memories`)
        project_name: String,
        /// SP の HTTP/QUIC port (例: 33000)
        port: u16,
        /// SP プロセス PID
        pid: u32,
    },
    /// SP が unregister された (= QUIC 切断 or 明示 unregister)
    Remove {
        /// 正規化済 project path
        project_path: String,
    },
}

/// Bastet 🧲 — device 接続/切断/操作イベント (= "world-device" Unison channel の data plane)
///
/// EventBus の `bastet.*` event を Unison wire に変換した公開型。 `ProcessLifecycleEvent` と
/// 同じ `serde(tag = "kind")` 規約で serialize する。 daemon の world-device bridge が
/// `from_capability_event` で変換し、 vp-app が QUIC subscribe して受ける。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceEvent {
    /// 物理 MIDI device が接続された (hot-plug discovery 検出)
    DeviceConnected {
        /// CoreMIDI port の displayName (registry HashMap key と同値)
        port_name: String,
        has_input: bool,
        has_output: bool,
    },
    /// 物理 MIDI device が切断された
    DeviceDisconnected {
        /// 切断された device の port name
        port_name: String,
    },
    /// device からの操作入力 (Knob/Button/Fader/Pad、 0.0–1.0 正規化済)
    ControlEvent {
        /// 送信元 device の port name
        port_name: String,
        /// `ControlEvent` の serde 値 (variant tag + fields)
        event: serde_json::Value,
    },
}

impl DeviceEvent {
    /// `CapabilityEvent` の event_type + payload から `DeviceEvent` を構築する。
    ///
    /// bridge task が EventBus から受け取った `bastet.*` event を wire 型へ変換する。
    /// 対象外の event_type / payload 不足 (= 必須 field 欠落) は `None` (= bridge が skip)。
    pub fn from_capability_event(event_type: &str, payload: &serde_json::Value) -> Option<Self> {
        match event_type {
            "bastet.device_connected" => Some(DeviceEvent::DeviceConnected {
                port_name: payload.get("port_name")?.as_str()?.to_string(),
                has_input: payload
                    .get("has_input")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                has_output: payload
                    .get("has_output")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }),
            "bastet.device_disconnected" => Some(DeviceEvent::DeviceDisconnected {
                port_name: payload.get("port_name")?.as_str()?.to_string(),
            }),
            "bastet.control_event" => Some(DeviceEvent::ControlEvent {
                port_name: payload.get("port_name")?.as_str()?.to_string(),
                event: payload
                    .get("event")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            }),
            _ => None,
        }
    }
}

/// VP-154 PR-2: Process snapshot 1 entry (= "world-process" list method の応答 payload)
///
/// 既存 `RunningProcess` の wire 公開版。 内部 path 型 (PathBuf) を String 化して serde_json
/// で safe に network 越しに送れる形にする。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessSnapshot {
    pub project_path: String,
    pub project_name: String,
    pub port: u16,
    pub pid: u32,
    /// tmux session 名 (= SP register 時に渡された場合のみ Some)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
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

    #[test]
    fn test_process_lifecycle_event_add_serialize() {
        // VP-154 PR-2: Add variant の wire 形 (= snake_case tag "add" + project_path/name/port/pid)
        let event = ProcessLifecycleEvent::Add {
            project_path: "/Users/x/projects/creo".to_string(),
            project_name: "creo-memories".to_string(),
            port: 33000,
            pid: 12345,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"add\""), "got: {}", json);
        assert!(
            json.contains("\"project_name\":\"creo-memories\""),
            "got: {}",
            json
        );
        assert!(json.contains("\"port\":33000"), "got: {}", json);

        let restored: ProcessLifecycleEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, event);
    }

    #[test]
    fn test_process_lifecycle_event_remove_serialize() {
        // VP-154 PR-2: Remove variant は project_path のみ (= 切断検出経路でも publish される)
        let event = ProcessLifecycleEvent::Remove {
            project_path: "/Users/x/projects/creo".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"remove\""), "got: {}", json);
        assert!(!json.contains("project_name"), "got: {}", json);

        let restored: ProcessLifecycleEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, event);
    }

    #[test]
    fn test_process_snapshot_serialize_with_tmux() {
        let snap = ProcessSnapshot {
            project_path: "/x".to_string(),
            project_name: "vp".to_string(),
            port: 33002,
            pid: 99,
            tmux_session: Some("vp-session".to_string()),
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("tmux_session"), "got: {}", json);
        let restored: ProcessSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, snap);
    }

    #[test]
    fn test_process_snapshot_omits_tmux_when_none() {
        // skip_serializing_if で None 時は wire に出さない (= tmux_session field 不在)
        let snap = ProcessSnapshot {
            project_path: "/x".to_string(),
            project_name: "vp".to_string(),
            port: 33002,
            pid: 99,
            tmux_session: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("tmux_session"), "got: {}", json);
    }

    #[test]
    fn device_event_from_capability_event_variants() {
        use serde_json::json;
        // device_connected: 全 field を拾う
        assert_eq!(
            DeviceEvent::from_capability_event(
                "bastet.device_connected",
                &json!({"port_name": "ROTO-CONTROL", "has_input": true, "has_output": true}),
            ),
            Some(DeviceEvent::DeviceConnected {
                port_name: "ROTO-CONTROL".into(),
                has_input: true,
                has_output: true,
            })
        );
        // device_disconnected: port_name のみ
        assert_eq!(
            DeviceEvent::from_capability_event(
                "bastet.device_disconnected",
                &json!({"port_name": "ROTO-CONTROL"}),
            ),
            Some(DeviceEvent::DeviceDisconnected {
                port_name: "ROTO-CONTROL".into(),
            })
        );
        // control_event: event を生 JSON で運ぶ
        assert_eq!(
            DeviceEvent::from_capability_event(
                "bastet.control_event",
                &json!({"port_name": "ROTO-CONTROL", "event": {"Knob": {"index": 0, "value": 0.5}}}),
            ),
            Some(DeviceEvent::ControlEvent {
                port_name: "ROTO-CONTROL".into(),
                event: json!({"Knob": {"index": 0, "value": 0.5}}),
            })
        );
        // 未知 event_type → None
        assert_eq!(
            DeviceEvent::from_capability_event("other.event", &json!({})),
            None
        );
        // 必須 port_name 欠落 → None
        assert_eq!(
            DeviceEvent::from_capability_event(
                "bastet.device_connected",
                &json!({"has_input": true}),
            ),
            None
        );
    }
}
