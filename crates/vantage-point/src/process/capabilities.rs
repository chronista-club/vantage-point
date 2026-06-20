//! Process Capability Integration
//!
//! Capability システムを Process に統合するモジュール。
//! EventBus、Registry、各Capabilityの初期化と連携を担当。
//!
//! ## LSCM 境界 (PR-α-2 / VP-112、 doc 12 §9)
//!
//! - **Project 階層 Stand**: 本 module の `ProcessCapabilities` が host (Protocol / Agent / 等)
//! - **World 階層 Stand**: `crate::daemon::world_capabilities::WorldCapabilities` が host
//!   - Bastet 🧲 (`MidiCapability`) は **PR-α-2 で本 module から World に移管完了**
//!     （epic v3.1 E2-0 で旧 Hermit Purple 🍇 から rename）
//!   - 旧 `ProcessCapabilities.midi` field / `CapabilityConfig.midi_config` field は削除済
//!   - mailbox address `midi@{project}` (旧) → `bastet@world` (新、 旧称 `hp@world`)

use crate::capability::core::Capability;
use crate::capability::{AgentCapability, CapabilityContext, EventBus, ProtocolCapability};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Process Capability Manager
///
/// Process (= LSCM Project Layer) で使用する Capability を管理する。 LSCM doc 12 §9 catalog の
/// Project 階層 Stand のみ host。 World 階層 Stand (HP / Update / TheWorld) は
/// `crate::daemon::world_capabilities::WorldCapabilities` 側に移管 (PR-α-2 完了)。
///
/// VP-179 (Phase 5): `msgbox_router` field 撤去。 wiremsg R5-3 で旧 msgbox store も
/// 撤去済、 msg messaging は `AppState.wiremsg_store` に一本化。
pub struct ProcessCapabilities {
    /// イベントバス（全Capability共有）
    pub event_bus: Arc<EventBus>,
    /// Protocol Capability（WebSocket/stdio配信用）
    pub protocol: Arc<RwLock<ProtocolCapability>>,
    /// Agent Capability（Claude Agent統合）
    pub agent: Arc<RwLock<AgentCapability>>,
}

/// Capability 初期化設定
///
/// wiremsg R5-3: 旧 `remote_routing` field (msgbox forward) は撤去済。
/// Whitesnake 退役 (永続は SurrealDB 一本化): 旧 `whitesnake` field も撤去。
pub struct CapabilityConfig {
    /// プロジェクトディレクトリ
    pub project_dir: String,
}

impl ProcessCapabilities {
    /// 新しい ProcessCapabilities を作成・初期化
    pub async fn new(config: CapabilityConfig) -> Self {
        // EventBus を作成
        let event_bus = Arc::new(EventBus::new());

        // Protocol Capability
        let protocol = Arc::new(RwLock::new(ProtocolCapability::new()));

        // Agent Capability
        let mut agent = AgentCapability::new().with_working_dir(config.project_dir.clone());
        agent.set_event_bus(event_bus.clone());
        let agent = Arc::new(RwLock::new(agent));

        Self {
            event_bus,
            protocol,
            agent,
        }
    }

    /// 全 Capability を初期化
    pub async fn initialize(&self) -> anyhow::Result<()> {
        // VP-178 (Phase 4) / VP-179 (Phase 5): msgbox 経路を持たない empty context で
        // 各 Capability を初期化。 Protocol / Agent は observer 化済 (= subscription
        // 経路なし、 EventBus のみ使う)。
        {
            let ctx = CapabilityContext::new();
            let mut protocol = self.protocol.write().await;
            protocol.initialize(&ctx).await?;
        }
        {
            let ctx = CapabilityContext::new();
            let mut agent = self.agent.write().await;
            agent.initialize(&ctx).await?;
        }

        tracing::info!("All capabilities initialized");
        Ok(())
    }

    /// 全 Capability をシャットダウン
    pub async fn shutdown(&self) -> anyhow::Result<()> {
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
}

/// CapabilityEvent を ProcessMessage に変換
///
/// 注: PR-α-2 (VP-112) で MidiCapability を World daemon に移管したため、 SP (Project) の
/// EventBus は MIDI event を受け取らない。 旧 `t if t.starts_with("midi.")` 分岐は不要なので削除。
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
        };

        let _caps = ProcessCapabilities::new(config).await;
    }

    #[tokio::test]
    async fn test_process_capabilities_initialize() {
        let config = CapabilityConfig {
            project_dir: "/tmp/test".to_string(),
        };

        let caps = ProcessCapabilities::new(config).await;
        let result = caps.initialize().await;
        assert!(result.is_ok());
    }
}
