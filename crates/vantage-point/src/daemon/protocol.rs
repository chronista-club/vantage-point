//! Daemon IPC プロトコルのメッセージ型定義
//!
//! World daemon の live channel（world-process / events / device 等）の
//! リクエスト・レスポンス・イベント型を定義する。

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

/// M2 / doc 26 §2: `device` channel の `ReportDevice` request payload。
///
/// macOS menu bar agent (Swift `CoreMIDIWatcher`) が CoreMIDI hot-plug を daemon に報告する。
/// daemon の `handle_device_report` が `state` で接続/切断を分岐し Bastet registry に反映する。
/// wire は既定 JSON codec のため、 Swift 側は CodingKeys で snake_case にマップする。
#[cfg(feature = "midi")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportDeviceRequest {
    /// CoreMIDI port の displayName (registry HashMap key と同値)
    pub port_name: String,
    /// "connected" | "disconnected"
    pub state: String,
    /// input port (device → VP) が存在するか
    #[serde(default)]
    pub has_input: bool,
    /// output port (VP → device) が存在するか
    #[serde(default)]
    pub has_output: bool,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // M2 / doc 26 §2: Swift agent が送る snake_case JSON が ReportDeviceRequest に decode できる。
    #[cfg(feature = "midi")]
    #[test]
    fn test_report_device_request_deserialize_from_wire() {
        // 接続報告（agent が CodingKeys で snake_case 化して送る形）
        let wire = r#"{"port_name":"X-Touch Compact","state":"connected","has_input":true,"has_output":true}"#;
        let req: ReportDeviceRequest = serde_json::from_str(wire).unwrap();
        assert_eq!(req.port_name, "X-Touch Compact");
        assert_eq!(req.state, "connected");
        assert!(req.has_input);
        assert!(req.has_output);

        // 切断報告は has_input/has_output 省略 → default false
        let wire = r#"{"port_name":"ROTO","state":"disconnected"}"#;
        let req: ReportDeviceRequest = serde_json::from_str(wire).unwrap();
        assert_eq!(req.state, "disconnected");
        assert!(!req.has_input);
        assert!(!req.has_output);
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

    /// tmux decoupling PR2: 旧 wire payload (tmux_session 入り、旧 binary の SP/daemon) が
    /// 新 ProcessSnapshot に decode できる（unknown field は serde が無視 = 後方互換）。
    #[test]
    fn test_process_snapshot_decodes_legacy_payload_with_tmux_session() {
        let legacy = r#"{"project_path":"/x","project_name":"vp","port":33002,"pid":99,"tmux_session":"vp-session"}"#;
        let snap: ProcessSnapshot = serde_json::from_str(legacy).unwrap();
        assert_eq!(snap.project_name, "vp");
        assert_eq!(snap.port, 33002);
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
