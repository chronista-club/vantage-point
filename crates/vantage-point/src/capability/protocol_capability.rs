//! Protocol Capability
//!
//! EventBusとプロトコル層（AG-UI/ACP/Vantage）を橋渡しする能力。
//! CapabilityEventを各プロトコル形式に変換し、WebSocket/stdioで配信する。
//!
//! ## 要件
//! - REQ-PROTO-004: EventBus連携
//! - REQ-PROTO-005: Transport抽象化

use crate::capability::core::{
    Capability, CapabilityContext, CapabilityError, CapabilityEvent, CapabilityInfo,
    CapabilityResult, CapabilityState,
};
use crate::capability::eventbus::{EventBus, FilteredSubscription};
use crate::protocol::{AcpMessage, ProtocolMessage, ToAcp, ToAgUi, VantageEvent};
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc};

// =============================================================================
// Protocol Capability
// =============================================================================

/// プロトコル能力
///
/// EventBusからのイベントを各プロトコル形式に変換し、
/// 登録されたトランスポートに配信する。
pub struct ProtocolCapability {
    /// 能力状態
    state: CapabilityState,
    /// 現在のrun_id（AG-UIイベント用）
    run_id: Arc<RwLock<Option<String>>>,
    /// 現在のsession_id（ACPメッセージ用）
    session_id: Arc<RwLock<Option<String>>>,
    /// プロトコルメッセージ送信用チャンネル
    protocol_tx: broadcast::Sender<ProtocolMessage>,
    /// EventBus購読解除用
    subscription_id: String,
}

impl ProtocolCapability {
    /// 新しいProtocolCapabilityを作成
    pub fn new() -> Self {
        let (protocol_tx, _) = broadcast::channel(1024);
        Self {
            state: CapabilityState::Uninitialized,
            run_id: Arc::new(RwLock::new(None)),
            session_id: Arc::new(RwLock::new(None)),
            protocol_tx,
            subscription_id: format!("protocol-capability-{}", uuid::Uuid::new_v4()),
        }
    }

    /// プロトコルメッセージの受信チャンネルを取得
    pub fn subscribe(&self) -> broadcast::Receiver<ProtocolMessage> {
        self.protocol_tx.subscribe()
    }

    /// run_idを設定
    pub async fn set_run_id(&self, run_id: Option<String>) {
        let mut guard = self.run_id.write().await;
        *guard = run_id;
    }

    /// session_idを設定
    pub async fn set_session_id(&self, session_id: Option<String>) {
        let mut guard = self.session_id.write().await;
        *guard = session_id;
    }

    /// CapabilityEventをプロトコルメッセージに変換して送信
    async fn process_event(&self, event: CapabilityEvent) {
        let run_id = self.run_id.read().await.clone().unwrap_or_default();
        let session_id = self.session_id.read().await.clone().unwrap_or_default();

        // AG-UIに変換
        if let Some(agui_event) = event.to_agui(&run_id) {
            let msg = ProtocolMessage::agui(agui_event);
            let _ = self.protocol_tx.send(msg);
        }

        // ACPに変換
        if let Some(acp_msg) = event.to_acp(&session_id) {
            let msg = ProtocolMessage::acp(acp_msg);
            let _ = self.protocol_tx.send(msg);
        }

        // Vantage独自イベントとして送信
        if let Some(vantage_event) = Self::to_vantage(&event) {
            let msg = ProtocolMessage::vantage(vantage_event);
            let _ = self.protocol_tx.send(msg);
        }
    }

    /// CapabilityEventをVantageEventに変換
    fn to_vantage(event: &CapabilityEvent) -> Option<VantageEvent> {
        match event.event_type.as_str() {
            // MIDI関連
            t if t.starts_with("midi.") => {
                // MIDIイベントはpayloadから詳細を取得
                let channel = event
                    .payload
                    .get("channel")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u8;

                let data: Vec<u8> = event
                    .payload
                    .get("data")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_u64().map(|n| n as u8))
                            .collect()
                    })
                    .unwrap_or_default();

                let event_type = match event.event_type.as_str() {
                    "midi.note_on" => crate::protocol::MidiEventType::NoteOn,
                    "midi.note_off" => crate::protocol::MidiEventType::NoteOff,
                    "midi.cc" => crate::protocol::MidiEventType::ControlChange,
                    _ => crate::protocol::MidiEventType::Unknown,
                };

                Some(VantageEvent::MidiInput {
                    channel,
                    event_type,
                    data,
                    timestamp: event.timestamp,
                })
            }

            // Capability状態変更
            "capability.state_changed" => {
                let capability_id = event.source.clone();
                let state_str = event
                    .payload
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                let state = match state_str {
                    "uninitialized" => crate::protocol::CapabilityStateInfo::Uninitialized,
                    "idle" => crate::protocol::CapabilityStateInfo::Idle,
                    "active" => crate::protocol::CapabilityStateInfo::Active,
                    "processing" => crate::protocol::CapabilityStateInfo::Processing,
                    "paused" => crate::protocol::CapabilityStateInfo::Paused,
                    "stopped" => crate::protocol::CapabilityStateInfo::Stopped,
                    _ => crate::protocol::CapabilityStateInfo::Idle,
                };

                Some(VantageEvent::capability_state_changed(
                    &capability_id,
                    state,
                ))
            }

            // Synergy発動
            "synergy.activated" => {
                let synergy_id = event
                    .payload
                    .get("synergy_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let capabilities: Vec<String> = event
                    .payload
                    .get("capabilities")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                Some(VantageEvent::synergy_activated(
                    &synergy_id,
                    crate::protocol::SynergyTypeInfo::Enhancement,
                    capabilities,
                ))
            }

            // その他は変換しない
            _ => None,
        }
    }

    /// EventBusからのイベントを処理するタスクを起動
    pub fn start_event_processing(
        &self,
        mut subscription: FilteredSubscription,
    ) -> tokio::task::JoinHandle<()> {
        let protocol_tx = self.protocol_tx.clone();
        let run_id = self.run_id.clone();
        let session_id = self.session_id.clone();

        tokio::spawn(async move {
            while let Some(event) = subscription.recv().await {
                let run_id_val = run_id.read().await.clone().unwrap_or_default();
                let session_id_val = session_id.read().await.clone().unwrap_or_default();

                // AG-UIに変換
                if let Some(agui_event) = event.to_agui(&run_id_val) {
                    let msg = ProtocolMessage::agui(agui_event);
                    let _ = protocol_tx.send(msg);
                }

                // ACPに変換
                if let Some(acp_msg) = event.to_acp(&session_id_val) {
                    let msg = ProtocolMessage::acp(acp_msg);
                    let _ = protocol_tx.send(msg);
                }

                // Vantageに変換
                if let Some(vantage_event) = Self::to_vantage(&event) {
                    let msg = ProtocolMessage::vantage(vantage_event);
                    let _ = protocol_tx.send(msg);
                }
            }
        })
    }
}

impl Default for ProtocolCapability {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Capability Implementation
// =============================================================================

#[async_trait]
impl Capability for ProtocolCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "protocol-capability",
            env!("CARGO_PKG_VERSION"),
            "EventBusとプロトコル層を橋渡しする能力",
        )
        .with_author("Vantage Point Team")
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
        tracing::info!("ProtocolCapability initializing");
        self.state = CapabilityState::Idle;
        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        tracing::info!("ProtocolCapability shutting down");
        self.state = CapabilityState::Stopped;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// =============================================================================
// Protocol Router
// =============================================================================

/// プロトコルルーター
///
/// 複数のトランスポートにプロトコルメッセージを配信する
pub struct ProtocolRouter {
    /// プロトコル能力
    capability: Arc<RwLock<ProtocolCapability>>,
    /// 配信先トランスポート
    transports: Vec<mpsc::Sender<ProtocolMessage>>,
}

impl ProtocolRouter {
    /// 新しいルーターを作成
    pub fn new(capability: Arc<RwLock<ProtocolCapability>>) -> Self {
        Self {
            capability,
            transports: Vec::new(),
        }
    }

    /// トランスポートを追加
    pub fn add_transport(&mut self, tx: mpsc::Sender<ProtocolMessage>) {
        self.transports.push(tx);
    }

    /// ルーティングを開始
    pub async fn start(&self) -> tokio::task::JoinHandle<()> {
        let cap = self.capability.read().await;
        let mut rx = cap.subscribe();
        let transports = self.transports.clone();

        tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                for tx in &transports {
                    let _ = tx.send(msg.clone()).await;
                }
            }
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_protocol_capability_new() {
        let cap = ProtocolCapability::new();
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
    }

    #[tokio::test]
    async fn test_protocol_capability_initialize() {
        let mut cap = ProtocolCapability::new();
        let ctx = CapabilityContext::new();
        cap.initialize(&ctx).await.unwrap();
        assert_eq!(cap.state(), CapabilityState::Idle);
    }

    #[tokio::test]
    async fn test_set_run_id() {
        let cap = ProtocolCapability::new();
        cap.set_run_id(Some("run-123".to_string())).await;

        let run_id = cap.run_id.read().await;
        assert_eq!(run_id.as_deref(), Some("run-123"));
    }

    #[tokio::test]
    async fn test_set_session_id() {
        let cap = ProtocolCapability::new();
        cap.set_session_id(Some("session-456".to_string())).await;

        let session_id = cap.session_id.read().await;
        assert_eq!(session_id.as_deref(), Some("session-456"));
    }

    #[tokio::test]
    async fn test_subscribe() {
        let cap = ProtocolCapability::new();
        let _rx = cap.subscribe();
        // 購読できることを確認
    }

    #[test]
    fn test_to_vantage_midi() {
        let event = CapabilityEvent::new("midi.note_on", "midi-capability").with_payload(
            &serde_json::json!({
                "channel": 0,
                "data": [144, 60, 100]
            }),
        );

        let vantage = ProtocolCapability::to_vantage(&event);
        assert!(vantage.is_some());

        match vantage.unwrap() {
            VantageEvent::MidiInput {
                channel,
                event_type,
                ..
            } => {
                assert_eq!(channel, 0);
                assert_eq!(event_type, crate::protocol::MidiEventType::NoteOn);
            }
            _ => panic!("Expected MidiInput event"),
        }
    }

    #[test]
    fn test_to_vantage_capability_state() {
        let event = CapabilityEvent::new("capability.state_changed", "midi-capability")
            .with_payload(&serde_json::json!({
                "state": "active"
            }));

        let vantage = ProtocolCapability::to_vantage(&event);
        assert!(vantage.is_some());

        match vantage.unwrap() {
            VantageEvent::CapabilityStateChanged { state, .. } => {
                assert!(matches!(
                    state,
                    crate::protocol::CapabilityStateInfo::Active
                ));
            }
            _ => panic!("Expected CapabilityStateChanged event"),
        }
    }

    #[test]
    fn test_to_vantage_synergy() {
        let event = CapabilityEvent::new("synergy.activated", "synergy-engine").with_payload(
            &serde_json::json!({
                "synergy_id": "midi-agent",
                "capabilities": ["midi", "agent"]
            }),
        );

        let vantage = ProtocolCapability::to_vantage(&event);
        assert!(vantage.is_some());

        match vantage.unwrap() {
            VantageEvent::SynergyActivated { capabilities, .. } => {
                assert_eq!(capabilities, vec!["midi", "agent"]);
            }
            _ => panic!("Expected SynergyActivated event"),
        }
    }
}
