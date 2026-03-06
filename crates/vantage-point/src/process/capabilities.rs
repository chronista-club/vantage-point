//\! Process Capability Integration
//!
//! Capability システムを Process に統合するモジュール。
//! EventBus、Registry、各Capabilityの初期化と連携を担当。

use crate::capability::core::Capability;
use crate::capability::{
    AgentCapability, BonjourCapability, CapabilityContext, CapabilityRegistry, EventBus,
    MidiCapability, ProtocolCapability,
};
use crate::midi::MidiConfig;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Process Capability Manager
///
/// Process で使用する全ての Capability を管理する。
pub struct ProcessCapabilities {
    /// イベントバス（全Capability共有）
    pub event_bus: Arc<EventBus>,
    /// Capability レジストリ
    pub registry: Arc<RwLock<CapabilityRegistry>>,
    /// Protocol Capability（WebSocket/stdio配信用）
    pub protocol: Arc<RwLock<ProtocolCapability>>,
    /// Agent Capability（Claude Agent統合）
    pub agent: Arc<RwLock<AgentCapability>>,
    /// MIDI Capability（オプション）
    pub midi: Option<Arc<RwLock<MidiCapability>>>,
    /// Bonjour Capability（mDNS広告）
    pub bonjour: Option<Arc<RwLock<BonjourCapability>>>,
}

/// Capability 初期化設定
pub struct CapabilityConfig {
    /// プロジェクトディレクトリ
    pub project_dir: String,
    /// MIDI設定（有効な場合）
    pub midi_config: Option<MidiConfig>,
    /// Bonjour設定（ポート番号、Noneで無効）
    pub bonjour_port: Option<u16>,
}

impl ProcessCapabilities {
    /// 新しい ProcessCapabilities を作成・初期化
    pub async fn new(config: CapabilityConfig) -> Self {
        // EventBus を作成
        let event_bus = Arc::new(EventBus::new());

        // Registry を作成
        let ctx = CapabilityContext::new().with_config(serde_json::json!({
            "working_dir": config.project_dir,
        }));
        let registry = Arc::new(RwLock::new(CapabilityRegistry::with_context(ctx)));

        // Protocol Capability
        let protocol = Arc::new(RwLock::new(ProtocolCapability::new()));

        // Agent Capability
        let mut agent = AgentCapability::new().with_working_dir(config.project_dir.clone());
        agent.set_event_bus(event_bus.clone());
        let agent = Arc::new(RwLock::new(agent));

        // MIDI Capability（オプション）
        let midi = if let Some(midi_config) = config.midi_config {
            let mut midi_cap = MidiCapability::with_config(midi_config);
            midi_cap.set_event_bus(event_bus.clone());
            Some(Arc::new(RwLock::new(midi_cap)))
        } else {
            None
        };

        // Bonjour Capability（オプション）
        let bonjour = if let Some(port) = config.bonjour_port {
            // プロジェクト名をディレクトリから取得
            let project_name = config
                .project_dir
                .rsplit('/')
                .next()
                .unwrap_or("vantage-point")
                .to_string();
            let mut bonjour_cap = BonjourCapability::new(port, project_name);
            bonjour_cap.set_event_bus(event_bus.clone());
            Some(Arc::new(RwLock::new(bonjour_cap)))
        } else {
            None
        };

        Self {
            event_bus,
            registry,
            protocol,
            agent,
            midi,
            bonjour,
        }
    }

    /// 全 Capability を初期化
    pub async fn initialize(&self) -> anyhow::Result<()> {
        let ctx = CapabilityContext::new();

        // Protocol Capability 初期化
        {
            let mut protocol = self.protocol.write().await;
            protocol.initialize(&ctx).await?;
        }

        // Agent Capability 初期化
        {
            let mut agent = self.agent.write().await;
            agent.initialize(&ctx).await?;
        }

        // MIDI Capability 初期化と監視開始（存在する場合）
        if let Some(ref midi) = self.midi {
            let mut midi = midi.write().await;
            midi.initialize(&ctx).await?;
            // MidiConfigからport_indexを取得して監視開始
            let port_index = midi.config().port_index;
            if let Err(e) = midi.start_monitoring(port_index).await {
                tracing::warn!("Failed to start MIDI monitoring: {}", e);
            }
        }

        // Bonjour Capability 初期化（存在する場合）
        if let Some(ref bonjour) = self.bonjour {
            let mut bonjour = bonjour.write().await;
            if let Err(e) = bonjour.initialize(&ctx).await {
                tracing::warn!("Failed to initialize Bonjour: {}", e);
            }
        }

        tracing::info!("All capabilities initialized");
        Ok(())
    }

    /// 全 Capability をシャットダウン
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        // Bonjour Capability シャットダウン（広告停止）
        if let Some(ref bonjour) = self.bonjour {
            let mut bonjour = bonjour.write().await;
            let _ = bonjour.shutdown().await;
        }

        // MIDI Capability シャットダウン
        if let Some(ref midi) = self.midi {
            let mut midi = midi.write().await;
            let _ = midi.shutdown().await;
        }

        // Agent Capability シャットダウン
        {
            let mut agent = self.agent.write().await;
            let _ = agent.shutdown().await;
        }

        // Protocol Capability シャットダウン
        {
            let mut protocol = self.protocol.write().await;
            let _ = protocol.shutdown().await;
        }

        tracing::info!("All capabilities shut down");
        Ok(())
    }

    /// EventBus からのイベントを Hub にブリッジするタスクを開始
    pub fn start_event_bridge(
        &self,
        hub_sender: tokio::sync::broadcast::Sender<crate::protocol::ProcessMessage>,
    ) -> tokio::task::JoinHandle<()> {
        let event_bus = self.event_bus.clone();

        tokio::spawn(async move {
            // EventBus を購読
            let mut subscription = event_bus.subscribe("process-bridge", "*").await;

            while let Some(event) = subscription.recv().await {
                // CapabilityEvent を ProcessMessage に変換
                let process_msg = capability_event_to_process_message(&event);

                // Hub にブロードキャスト
                let _ = hub_sender.send(process_msg);
            }
        })
    }

    /// MIDI 監視を開始（MIDIが有効な場合）
    pub async fn start_midi_monitoring(&self, port_index: Option<usize>) -> anyhow::Result<()> {
        if let Some(ref midi) = self.midi {
            let mut midi = midi.write().await;
            midi.start_monitoring(port_index).await?;
            tracing::info!("MIDI monitoring started");
        }
        Ok(())
    }
}

/// CapabilityEvent を ProcessMessage に変換
fn capability_event_to_process_message(
    event: &crate::capability::CapabilityEvent,
) -> crate::protocol::ProcessMessage {
    use crate::protocol::ProcessMessage;

    match event.event_type.as_str() {
        // Agent イベント
        "agent.text_chunk" => {
            let content = event
                .payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            ProcessMessage::ChatChunk {
                content,
                done: false,
            }
        }

        "agent.done" => ProcessMessage::ChatChunk {
            content: String::new(),
            done: true,
        },

        "agent.error" => {
            let error = event
                .payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            ProcessMessage::DebugInfo {
                level: crate::protocol::DebugMode::Simple,
                category: "error".to_string(),
                message: error,
                data: Some(event.payload.clone()),
                tags: vec!["agent".to_string(), "error".to_string()],
            }
        }

        // MIDI イベント
        t if t.starts_with("midi.") => {
            // MIDI イベントはデバッグ情報として送信
            ProcessMessage::DebugInfo {
                level: crate::protocol::DebugMode::Detail,
                category: "midi".to_string(),
                message: event.event_type.clone(),
                data: Some(event.payload.clone()),
                tags: vec!["midi".to_string(), "capability".to_string()],
            }
        }

        // Capability 状態変更
        "capability.initialized" | "capability.state_changed" => ProcessMessage::DebugInfo {
            level: crate::protocol::DebugMode::Simple,
            category: "capability".to_string(),
            message: format!("{}: {}", event.source, event.event_type),
            data: Some(event.payload.clone()),
            tags: vec!["capability".to_string(), "state".to_string()],
        },

        // その他のイベント
        _ => ProcessMessage::DebugInfo {
            level: crate::protocol::DebugMode::Detail,
            category: "event".to_string(),
            message: event.event_type.clone(),
            data: Some(event.payload.clone()),
            tags: vec!["event".to_string()],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_process_capabilities_new() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
            midi_config: None,
            bonjour_port: None,
        };

        let caps = ProcessCapabilities::new(config).await;
        assert!(caps.midi.is_none());
        assert!(caps.bonjour.is_none());
    }

    #[tokio::test]
    async fn test_process_capabilities_with_midi() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
            midi_config: Some(MidiConfig::default()),
            bonjour_port: None,
        };

        let caps = ProcessCapabilities::new(config).await;
        assert!(caps.midi.is_some());
    }

    #[tokio::test]
    async fn test_process_capabilities_with_bonjour() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
            midi_config: None,
            bonjour_port: Some(33000),
        };

        let caps = ProcessCapabilities::new(config).await;
        assert!(caps.bonjour.is_some());
    }

    #[tokio::test]
    async fn test_process_capabilities_initialize() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
            midi_config: None,
            bonjour_port: None,
        };

        let caps = ProcessCapabilities::new(config).await;
        let result = caps.initialize().await;
        assert!(result.is_ok());
    }
}
