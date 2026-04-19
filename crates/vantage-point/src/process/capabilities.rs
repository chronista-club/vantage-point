//! Process Capability Integration
//!
//! Capability システムを Process に統合するモジュール。
//! EventBus、Registry、各Capabilityの初期化と連携を担当。

use crate::capability::core::Capability;
use crate::capability::mailbox_remote::RemoteRoutingClient;
use crate::capability::{
    AgentCapability, CapabilityContext, CapabilityRegistry, EventBus, MailboxRouter,
    MidiCapability, ProtocolCapability, Whitesnake,
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
    /// メールボックスルーター（1:1 ポイントツーポイント通信）
    pub mailbox_router: Arc<MailboxRouter>,
    /// Capability レジストリ
    pub registry: Arc<RwLock<CapabilityRegistry>>,
    /// Protocol Capability（WebSocket/stdio配信用）
    pub protocol: Arc<RwLock<ProtocolCapability>>,
    /// Agent Capability（Claude Agent統合）
    pub agent: Arc<RwLock<AgentCapability>>,
    /// MIDI Capability（オプション）
    pub midi: Option<Arc<RwLock<MidiCapability>>>,
}

/// Capability 初期化設定
pub struct CapabilityConfig {
    /// プロジェクトディレクトリ
    pub project_dir: String,
    /// MIDI設定（有効な場合）
    pub midi_config: Option<MidiConfig>,
    /// 永続化バックエンド（Mailbox persistent メッセージ用）
    ///
    /// Some の場合、MailboxRouter が persistent メッセージを DISC に保存し、
    /// Process 再起動後に `restore_pending()` で復元する。
    pub whitesnake: Option<Whitesnake>,
    /// Remote routing client（Mailbox Phase 3: cross-Process actor messaging）
    ///
    /// Some の場合、`@{port}` / `@{project}` 形式のアドレスを TheWorld registry で
    /// 解決し、target Process に forward する。None の場合は Process-local 配信のみ。
    pub remote_routing: Option<RemoteRoutingClient>,
}

impl ProcessCapabilities {
    /// 新しい ProcessCapabilities を作成・初期化
    pub async fn new(config: CapabilityConfig) -> Self {
        // EventBus を作成
        let event_bus = Arc::new(EventBus::new());

        // MailboxRouter を作成
        // - Whitesnake 注入: persistent メッセージ対応
        // - RemoteRoutingClient 注入: cross-Process forward 対応（Phase 3 Step 2）
        let mailbox_router = Arc::new(
            match (config.whitesnake.clone(), config.remote_routing.clone()) {
                (Some(ws), Some(remote)) => MailboxRouter::with_persistence_and_remote(ws, remote),
                (Some(ws), None) => MailboxRouter::with_persistence(ws),
                (None, Some(remote)) => MailboxRouter::with_remote(remote),
                (None, None) => MailboxRouter::new(),
            },
        );

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

        Self {
            event_bus,
            mailbox_router,
            registry,
            protocol,
            agent,
            midi,
        }
    }

    /// 全 Capability を初期化
    pub async fn initialize(&self) -> anyhow::Result<()> {
        // 各 Capability に Mailbox を登録
        let protocol_mailbox = self.mailbox_router.register("protocol").await;
        let agent_mailbox = self.mailbox_router.register("agent").await;

        // Protocol Capability 初期化
        {
            let ctx = CapabilityContext::new().with_mailbox(protocol_mailbox);
            let mut protocol = self.protocol.write().await;
            protocol.initialize(&ctx).await?;
        }

        // Agent Capability 初期化
        {
            let ctx = CapabilityContext::new().with_mailbox(agent_mailbox);
            let mut agent = self.agent.write().await;
            agent.initialize(&ctx).await?;
        }

        // MIDI Capability 初期化と監視開始（存在する場合）
        if let Some(ref midi) = self.midi {
            let midi_mailbox = self.mailbox_router.register("midi").await;
            let ctx = CapabilityContext::new().with_mailbox(midi_mailbox);
            let mut midi = midi.write().await;
            midi.initialize(&ctx).await?;
            // MidiConfigからport_indexを取得して監視開始
            let port_index = midi.config().port_index;
            if let Err(e) = midi.start_monitoring(port_index).await {
                tracing::warn!("Failed to start MIDI monitoring: {}", e);
            }
        }

        tracing::info!(
            "All capabilities initialized (mailbox addresses: {:?})",
            self.mailbox_router.addresses().await
        );

        // 永続化メッセージを復元（Whitesnake 有効時のみ）
        match self.mailbox_router.restore_pending().await {
            Ok(0) => {}
            Ok(n) => tracing::info!("Mailbox: {} 件の永続メッセージを復元", n),
            Err(e) => tracing::warn!("Mailbox: 永続メッセージ復元失敗: {}", e),
        }

        Ok(())
    }

    /// 全 Capability をシャットダウン
    pub async fn shutdown(&self) -> anyhow::Result<()> {
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

        // MailboxRouter シャットダウン
        self.mailbox_router.shutdown();

        tracing::info!("All capabilities shut down");
        Ok(())
    }

    /// EventBus からのイベントを Hub にブリッジするタスクを開始
    pub fn start_event_bridge(
        &self,
        hub_sender: tokio::sync::broadcast::Sender<crate::protocol::ProcessMessage>,
        shutdown_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let event_bus = self.event_bus.clone();

        tokio::spawn(async move {
            // EventBus を購読
            let mut subscription = event_bus.subscribe("process-bridge", "*").await;

            loop {
                tokio::select! {
                    _ = shutdown_token.cancelled() => {
                        tracing::info!("EventBus bridge: shutdown");
                        break;
                    }
                    event = subscription.recv() => {
                        match event {
                            Some(event) => {
                                let process_msg = capability_event_to_process_message(&event);
                                let _ = hub_sender.send(process_msg);
                            }
                            None => break,
                        }
                    }
                }
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
            whitesnake: None,
            remote_routing: None,
        };

        let caps = ProcessCapabilities::new(config).await;
        assert!(caps.midi.is_none());
    }

    #[tokio::test]
    async fn test_process_capabilities_with_midi() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
            midi_config: Some(MidiConfig::default()),
            whitesnake: None,
            remote_routing: None,
        };

        let caps = ProcessCapabilities::new(config).await;
        assert!(caps.midi.is_some());
    }

    #[tokio::test]
    async fn test_process_capabilities_initialize() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
            midi_config: None,
            whitesnake: None,
            remote_routing: None,
        };

        let caps = ProcessCapabilities::new(config).await;
        let result = caps.initialize().await;
        assert!(result.is_ok());
    }
}
