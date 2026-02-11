//! Agent Capability
//!
//! ClaudeAgentをCapabilityシステムに統合。
//! AgentEventをCapabilityEventに変換してEventBusに配信。
//!
//! ## 要件
//! - REQ-CAP-001: Capabilityトレイト実装
//! - REQ-CAP-003: EventBus連携
//! - REQ-PROTO-001: AG-UI準拠イベント生成

use crate::agent::{AgentConfig, AgentEvent, ClaudeAgent};
use crate::capability::core::{
    Capability, CapabilityContext, CapabilityError, CapabilityEvent, CapabilityInfo,
    CapabilityResult, CapabilityState,
};
use crate::capability::eventbus::EventBus;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

// =============================================================================
// Agent Capability
// =============================================================================

/// Agentの実行状態
#[derive(Debug, Clone)]
pub struct AgentRunState {
    /// 現在のセッションID
    pub session_id: Option<String>,
    /// 現在のrun_id（AG-UI用）
    pub run_id: String,
    /// 使用中のモデル
    pub model: Option<String>,
    /// 利用可能なツール
    pub tools: Vec<String>,
    /// 接続中のMCPサーバー
    pub mcp_servers: Vec<String>,
    /// 累積コスト
    pub total_cost: f64,
}

impl Default for AgentRunState {
    fn default() -> Self {
        Self {
            session_id: None,
            run_id: uuid::Uuid::new_v4().to_string(),
            model: None,
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            total_cost: 0.0,
        }
    }
}

/// Agent Capability
///
/// ClaudeAgentをCapabilityとしてラップし、EventBusに統合。
pub struct AgentCapability {
    /// 能力状態
    state: CapabilityState,
    /// 実行状態
    run_state: Arc<RwLock<AgentRunState>>,
    /// Claude Agent設定
    config: AgentConfig,
    /// EventBus参照（初期化時に設定）
    event_bus: Option<Arc<EventBus>>,
    /// 現在の実行タスク
    current_task: Option<tokio::task::JoinHandle<()>>,
    /// キャンセル用チャンネル
    cancel_tx: Option<mpsc::Sender<()>>,
}

impl AgentCapability {
    /// 新しいAgentCapabilityを作成
    pub fn new() -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            run_state: Arc::new(RwLock::new(AgentRunState::default())),
            config: AgentConfig::default(),
            event_bus: None,
            current_task: None,
            cancel_tx: None,
        }
    }

    /// 設定付きで作成
    pub fn with_config(config: AgentConfig) -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            run_state: Arc::new(RwLock::new(AgentRunState::default())),
            config,
            event_bus: None,
            current_task: None,
            cancel_tx: None,
        }
    }

    /// ワーキングディレクトリを設定
    pub fn with_working_dir(mut self, dir: String) -> Self {
        self.config.working_dir = Some(dir);
        self
    }

    /// セッションIDを設定（会話継続用）
    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.config.session_id = Some(session_id);
        self
    }

    /// モデルを設定
    pub fn with_model(mut self, model: String) -> Self {
        self.config.model = Some(model);
        self
    }

    /// 現在の実行状態を取得
    pub async fn run_state(&self) -> AgentRunState {
        self.run_state.read().await.clone()
    }

    /// EventBusを設定
    pub fn set_event_bus(&mut self, event_bus: Arc<EventBus>) {
        self.event_bus = Some(event_bus);
    }

    /// EventBusを取得（初期化後に使用可能）
    pub fn event_bus(&self) -> Option<Arc<EventBus>> {
        self.event_bus.clone()
    }

    /// プロンプトを送信してレスポンスをストリーミング
    pub async fn prompt(&mut self, message: &str) -> CapabilityResult<mpsc::Receiver<AgentEvent>> {
        if self.state != CapabilityState::Idle && self.state != CapabilityState::Active {
            return Err(CapabilityError::Other(format!(
                "Cannot prompt in state {:?}",
                self.state
            )));
        }

        // 新しいrun_idを生成
        {
            let mut state = self.run_state.write().await;
            state.run_id = uuid::Uuid::new_v4().to_string();
        }

        let run_state = self.run_state.clone();
        let event_bus = self.event_bus.clone();
        let config = self.config.clone();

        // 状態をActiveに
        self.state = CapabilityState::Active;

        // run_started イベントを発行
        if let Some(ref bus) = event_bus {
            let run_id = run_state.read().await.run_id.clone();
            let event = CapabilityEvent::new("agent.run_started", "agent-capability").with_payload(
                &serde_json::json!({
                    "run_id": run_id,
                    "message": message,
                }),
            );
            bus.emit(event).await;
        }

        // ClaudeAgentを作成して実行
        let agent = ClaudeAgent::with_config(config);
        let mut rx = agent.chat(message).await;

        // イベント変換用チャンネル
        let (tx, result_rx) = mpsc::channel(100);
        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        self.cancel_tx = Some(cancel_tx);

        // バックグラウンドタスクでイベントを処理
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = rx.recv() => {
                        match event {
                            Some(agent_event) => {
                                // EventBusにCapabilityEventを発行
                                if let Some(ref bus) = event_bus {
                                    let run_id = run_state.read().await.run_id.clone();
                                    let cap_event = agent_event_to_capability_event(&agent_event, &run_id);
                                    bus.emit(cap_event).await;
                                }

                                // run_stateを更新
                                update_run_state(&run_state, &agent_event).await;

                                // 結果を転送
                                if tx.send(agent_event.clone()).await.is_err() {
                                    break;
                                }

                                // Doneイベントなら終了
                                if matches!(agent_event, AgentEvent::Done { .. } | AgentEvent::Error(_)) {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = cancel_rx.recv() => {
                        // キャンセルされた
                        if let Some(ref bus) = event_bus {
                            let run_id = run_state.read().await.run_id.clone();
                            let event = CapabilityEvent::new("agent.run_cancelled", "agent-capability")
                                .with_payload(&serde_json::json!({
                                    "run_id": run_id,
                                }));
                            bus.emit(event).await;
                        }
                        break;
                    }
                }
            }

            // run_ended イベント
            if let Some(ref bus) = event_bus {
                let state = run_state.read().await;
                let event = CapabilityEvent::new("agent.run_ended", "agent-capability")
                    .with_payload(&serde_json::json!({
                        "run_id": state.run_id,
                        "session_id": state.session_id,
                        "total_cost": state.total_cost,
                    }));
                bus.emit(event).await;
            }
        });

        self.current_task = Some(task);
        Ok(result_rx)
    }

    /// 現在の実行をキャンセル
    pub async fn cancel(&mut self) -> CapabilityResult<()> {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(()).await;
        }

        if let Some(task) = self.current_task.take() {
            task.abort();
        }

        self.state = CapabilityState::Idle;
        Ok(())
    }
}

impl Default for AgentCapability {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// AgentEventをCapabilityEventに変換
fn agent_event_to_capability_event(event: &AgentEvent, run_id: &str) -> CapabilityEvent {
    match event {
        AgentEvent::SessionInit {
            session_id,
            model,
            tools,
            mcp_servers,
        } => CapabilityEvent::new("agent.session_init", "agent-capability").with_payload(
            &serde_json::json!({
                "run_id": run_id,
                "session_id": session_id,
                "model": model,
                "tools": tools,
                "mcp_servers": mcp_servers,
            }),
        ),

        AgentEvent::TextChunk(text) => CapabilityEvent::new("agent.text_chunk", "agent-capability")
            .with_payload(&serde_json::json!({
                "run_id": run_id,
                "content": text,
            })),

        AgentEvent::ToolExecuting { name } => {
            CapabilityEvent::new("agent.tool_executing", "agent-capability").with_payload(
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": name,
                    "tool_call_id": format!("tool-{}", uuid::Uuid::new_v4()),
                }),
            )
        }

        AgentEvent::ToolResult { name, preview } => {
            CapabilityEvent::new("agent.tool_result", "agent-capability").with_payload(
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": name,
                    "preview": preview,
                }),
            )
        }

        AgentEvent::Done { result, cost } => CapabilityEvent::new("agent.done", "agent-capability")
            .with_payload(&serde_json::json!({
                "run_id": run_id,
                "result": result,
                "cost": cost,
            })),

        AgentEvent::Error(msg) => CapabilityEvent::new("agent.error", "agent-capability")
            .with_payload(&serde_json::json!({
                "run_id": run_id,
                "error": msg,
            })),

        AgentEvent::UserInputRequest {
            request_id,
            request_type,
            prompt,
            options,
        } => CapabilityEvent::new("agent.user_input_request", "agent-capability").with_payload(
            &serde_json::json!({
                "run_id": run_id,
                "request_id": request_id,
                "request_type": request_type,
                "prompt": prompt,
                "options": options.iter().map(|o| serde_json::json!({
                    "value": o.value,
                    "label": o.label,
                    "description": o.description,
                })).collect::<Vec<_>>(),
            }),
        ),
    }
}

/// AgentEventからrun_stateを更新
async fn update_run_state(run_state: &Arc<RwLock<AgentRunState>>, event: &AgentEvent) {
    let mut state = run_state.write().await;

    match event {
        AgentEvent::SessionInit {
            session_id,
            model,
            tools,
            mcp_servers,
        } => {
            state.session_id = Some(session_id.clone());
            state.model = model.clone();
            state.tools = tools.clone();
            state.mcp_servers = mcp_servers.clone();
        }
        AgentEvent::Done { cost: Some(c), .. } => {
            state.total_cost += c;
        }
        _ => {}
    }
}

// =============================================================================
// Capability Implementation
// =============================================================================

#[async_trait]
impl Capability for AgentCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "agent-capability",
            env!("CARGO_PKG_VERSION"),
            "Claude Agent統合能力",
        )
        .with_author("Vantage Point Team")
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, ctx: &CapabilityContext) -> CapabilityResult<()> {
        tracing::info!("AgentCapability initializing");

        // ワーキングディレクトリを設定から取得
        if self.config.working_dir.is_none()
            && let Some(cwd) = ctx.config().get("working_dir")
            && let Some(dir) = cwd.as_str()
        {
            self.config.working_dir = Some(dir.to_string());
        }

        self.state = CapabilityState::Idle;

        // 初期化完了イベント
        if let Some(ref bus) = self.event_bus {
            let event = CapabilityEvent::new("capability.initialized", "agent-capability")
                .with_payload(&serde_json::json!({
                    "working_dir": self.config.working_dir,
                    "model": self.config.model,
                }));
            bus.emit(event).await;
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        tracing::info!("AgentCapability shutting down");

        // 実行中のタスクをキャンセル
        self.cancel().await?;

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
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_agent_capability_new() {
        let cap = AgentCapability::new();
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
    }

    #[tokio::test]
    async fn test_agent_capability_with_config() {
        let config = AgentConfig {
            working_dir: Some("/tmp".to_string()),
            model: Some("sonnet".to_string()),
            ..Default::default()
        };
        let cap = AgentCapability::with_config(config);
        assert_eq!(cap.config.working_dir, Some("/tmp".to_string()));
        assert_eq!(cap.config.model, Some("sonnet".to_string()));
    }

    #[tokio::test]
    async fn test_agent_capability_initialize() {
        let mut cap = AgentCapability::new();
        let ctx = CapabilityContext::new();
        cap.initialize(&ctx).await.unwrap();
        assert_eq!(cap.state(), CapabilityState::Idle);
    }

    #[tokio::test]
    async fn test_agent_capability_with_event_bus() {
        let mut cap = AgentCapability::new();
        let event_bus = Arc::new(EventBus::new());
        cap.set_event_bus(event_bus.clone());

        let ctx = CapabilityContext::new();
        cap.initialize(&ctx).await.unwrap();

        assert!(cap.event_bus.is_some());
        assert!(cap.event_bus().is_some());
    }

    #[test]
    fn test_agent_event_to_capability_event() {
        let run_id = "run-123";

        // TextChunk
        let event = AgentEvent::TextChunk("Hello".to_string());
        let cap_event = agent_event_to_capability_event(&event, run_id);
        assert_eq!(cap_event.event_type, "agent.text_chunk");
        assert_eq!(cap_event.payload.get("content").unwrap(), "Hello");

        // ToolExecuting
        let event = AgentEvent::ToolExecuting {
            name: "read_file".to_string(),
        };
        let cap_event = agent_event_to_capability_event(&event, run_id);
        assert_eq!(cap_event.event_type, "agent.tool_executing");
        assert_eq!(cap_event.payload.get("tool_name").unwrap(), "read_file");
    }

    #[tokio::test]
    async fn test_run_state_default() {
        let cap = AgentCapability::new();
        let state = cap.run_state().await;
        assert!(state.session_id.is_none());
        assert!(!state.run_id.is_empty());
    }
}
