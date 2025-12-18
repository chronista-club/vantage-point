//! MIDI Capability
//!
//! MIDI入力をCapabilityシステムに統合。
//! MIDIイベントをCapabilityEventに変換してEventBusに配信。
//!
//! ## 要件
//! - REQ-CAP-001: Capabilityトレイト実装
//! - REQ-CAP-003: EventBus連携
//! - REQ-PROTO-003: Vantage拡張イベント生成
//! - REQ-EVO-001: Evolution System統合

use crate::capability::core::{
    Capability, CapabilityContext, CapabilityError, CapabilityEvent, CapabilityInfo,
    CapabilityResult, CapabilityState,
};
use crate::capability::eventbus::EventBus;
use crate::capability::evolution::{EvolutionCondition, EvolutionLevel, EvolutionState};
use crate::capability::params::MIDI_CAPABILITY_PARAMS;
use crate::midi::{list_ports, parse_midi_message, MidiConfig, MidiEvent, MidiMessage};
use async_trait::async_trait;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

// =============================================================================
// MIDI Capability State
// =============================================================================

/// MIDIの接続状態
#[derive(Debug, Clone)]
pub struct MidiConnectionState {
    /// 接続中のポート名
    pub connected_port: Option<String>,
    /// 利用可能なポート一覧
    pub available_ports: Vec<String>,
    /// 最後に受信したイベント時刻
    pub last_event_time: Option<std::time::Instant>,
    /// 受信イベント数
    pub event_count: u64,
}

impl Default for MidiConnectionState {
    fn default() -> Self {
        Self {
            connected_port: None,
            available_ports: Vec::new(),
            last_event_time: None,
            event_count: 0,
        }
    }
}

// =============================================================================
// MIDI Capability
// =============================================================================

/// MIDI Capability
///
/// MIDI入力を監視し、EventBusにイベントを配信する能力。
/// Evolution Systemにより、使用状況に応じて段階的に成長する。
pub struct MidiCapability {
    /// 能力状態
    state: CapabilityState,
    /// MIDI接続状態
    connection_state: Arc<RwLock<MidiConnectionState>>,
    /// MIDI設定
    config: MidiConfig,
    /// EventBus参照
    event_bus: Option<Arc<EventBus>>,
    /// 現在の監視タスク
    monitor_task: Option<tokio::task::JoinHandle<()>>,
    /// キャンセル用チャンネル
    cancel_tx: Option<mpsc::Sender<()>>,
    /// 進化状態（ACTレベル、使用統計）
    evolution: Arc<RwLock<EvolutionState>>,
    /// レベルアップ条件
    evolution_conditions: HashMap<EvolutionLevel, EvolutionCondition>,
}

impl MidiCapability {
    /// 新しいMidiCapabilityを作成
    pub fn new() -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            connection_state: Arc::new(RwLock::new(MidiConnectionState::default())),
            config: MidiConfig::default(),
            event_bus: None,
            monitor_task: None,
            cancel_tx: None,
            evolution: Arc::new(RwLock::new(EvolutionState::default())),
            evolution_conditions: Self::default_evolution_conditions(),
        }
    }

    /// 設定付きで作成
    pub fn with_config(config: MidiConfig) -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            connection_state: Arc::new(RwLock::new(MidiConnectionState::default())),
            config,
            event_bus: None,
            monitor_task: None,
            cancel_tx: None,
            evolution: Arc::new(RwLock::new(EvolutionState::default())),
            evolution_conditions: Self::default_evolution_conditions(),
        }
    }

    /// デフォルトの進化条件を生成
    fn default_evolution_conditions() -> HashMap<EvolutionLevel, EvolutionCondition> {
        let mut conditions = HashMap::new();

        // ACT1 → ACT2: MIDI出力とLED制御を習得
        conditions.insert(
            EvolutionLevel::ACT2,
            EvolutionCondition {
                min_uses: 50,
                min_success_rate: 0.7,
                min_days: Some(1),
                min_training_score: None,
                custom: HashMap::new(),
            },
        );

        // ACT2 → ACT3: SysEx制御を習得
        conditions.insert(
            EvolutionLevel::ACT3,
            EvolutionCondition {
                min_uses: 200,
                min_success_rate: 0.8,
                min_days: Some(3),
                min_training_score: Some(0.6),
                custom: HashMap::new(),
            },
        );

        // ACT3 → ACT4: 複数デバイス同時制御
        conditions.insert(
            EvolutionLevel::ACT4,
            EvolutionCondition {
                min_uses: 500,
                min_success_rate: 0.9,
                min_days: Some(7),
                min_training_score: Some(0.8),
                custom: HashMap::new(),
            },
        );

        conditions
    }

    /// ポートパターンを設定
    pub fn with_port_pattern(mut self, pattern: String) -> Self {
        self.config.port_pattern = Some(pattern);
        self
    }

    /// 接続状態を取得
    pub async fn connection_state(&self) -> MidiConnectionState {
        self.connection_state.read().await.clone()
    }

    /// EventBusを設定
    pub fn set_event_bus(&mut self, event_bus: Arc<EventBus>) {
        self.event_bus = Some(event_bus);
    }

    /// 設定を取得
    pub fn config(&self) -> &MidiConfig {
        &self.config
    }

    /// 進化状態を取得
    pub async fn evolution_state(&self) -> EvolutionState {
        self.evolution.read().await.clone()
    }

    /// 現在のACTレベルを取得
    pub async fn current_level(&self) -> EvolutionLevel {
        self.evolution.read().await.level
    }

    /// 利用可能なポートを更新
    async fn refresh_ports(&self) {
        let ports = list_ports().unwrap_or_default();
        let mut state = self.connection_state.write().await;
        state.available_ports = ports;
    }

    /// MIDI監視を開始
    pub async fn start_monitoring(&mut self, port_index: Option<usize>) -> CapabilityResult<()> {
        if self.state != CapabilityState::Idle {
            return Err(CapabilityError::Other(format!(
                "Cannot start monitoring in state {:?}",
                self.state
            )));
        }

        // ポート一覧を更新
        self.refresh_ports().await;

        let midi_in = midir::MidiInput::new("vp-midi-capability")
            .map_err(|e| CapabilityError::InitializationFailed(e.to_string()))?;

        let ports = midi_in.ports();
        if ports.is_empty() {
            return Err(CapabilityError::ResourceError("No MIDI ports found".into()));
        }

        // ポートを選択
        let port_idx = if let Some(pattern) = &self.config.port_pattern {
            ports
                .iter()
                .position(|p| {
                    midi_in
                        .port_name(p)
                        .map(|name| name.contains(pattern))
                        .unwrap_or(false)
                })
                .unwrap_or(port_index.unwrap_or(0))
        } else {
            port_index.unwrap_or(0)
        };

        let port = ports
            .get(port_idx)
            .ok_or_else(|| CapabilityError::ResourceError(format!("Port {} not found", port_idx)))?;

        let port_name = midi_in
            .port_name(port)
            .unwrap_or_else(|_| "Unknown".to_string());

        // 接続状態を更新
        {
            let mut state = self.connection_state.write().await;
            state.connected_port = Some(port_name.clone());
        }

        // イベントチャンネル
        let (event_tx, mut event_rx) = mpsc::channel::<MidiEvent>(256);
        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        self.cancel_tx = Some(cancel_tx);

        // MIDI接続
        let port_name_clone = port_name.clone();
        let _connection = midi_in
            .connect(
                port,
                "vp-midi-capability-conn",
                move |_timestamp, message, _| {
                    if let Some(midi_msg) = parse_midi_message(message) {
                        let event = MidiEvent {
                            port_name: port_name_clone.clone(),
                            message: midi_msg,
                            timestamp: std::time::Instant::now(),
                        };
                        let _ = event_tx.blocking_send(event);
                    }
                },
                (),
            )
            .map_err(|e| CapabilityError::InitializationFailed(e.to_string()))?;

        // イベント処理タスク
        let event_bus = self.event_bus.clone();
        let connection_state = self.connection_state.clone();
        let evolution = self.evolution.clone();
        let evolution_conditions = self.evolution_conditions.clone();

        let task = tokio::spawn(async move {
            // 接続を保持（dropすると切断される）
            let _conn = _connection;

            loop {
                tokio::select! {
                    event = event_rx.recv() => {
                        match event {
                            Some(midi_event) => {
                                // 状態を更新
                                {
                                    let mut state = connection_state.write().await;
                                    state.last_event_time = Some(midi_event.timestamp);
                                    state.event_count += 1;
                                }

                                // Evolution: 使用記録を追加
                                let level_up = {
                                    let mut evo = evolution.write().await;
                                    evo.record_use(true); // MIDIイベント受信は成功とみなす

                                    // レベルアップ条件をチェック
                                    let current_level = evo.level;
                                    if let Some(next_level) = current_level.next() {
                                        if let Some(condition) = evolution_conditions.get(&next_level) {
                                            if evo.try_level_up(condition) {
                                                Some((current_level, next_level))
                                            } else {
                                                None
                                            }
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                };

                                // レベルアップした場合はイベント発行
                                if let Some((from, to)) = level_up {
                                    tracing::info!(
                                        "🎉 MidiCapability evolved: {} → {}",
                                        from.display_name(),
                                        to.display_name()
                                    );
                                    if let Some(ref bus) = event_bus {
                                        let evo_event = CapabilityEvent::new(
                                            "evolution.level_up",
                                            "midi-capability",
                                        )
                                        .with_payload(&serde_json::json!({
                                            "from": from.0,
                                            "to": to.0,
                                            "from_name": from.display_name(),
                                            "to_name": to.display_name(),
                                        }));
                                        bus.emit(evo_event).await;
                                    }
                                }

                                // CapabilityEventに変換してEventBusに配信
                                if let Some(ref bus) = event_bus {
                                    let cap_event = midi_event_to_capability_event(&midi_event);
                                    bus.emit(cap_event).await;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = cancel_rx.recv() => {
                        tracing::info!("MIDI monitoring cancelled");
                        break;
                    }
                }
            }
        });

        self.monitor_task = Some(task);
        self.state = CapabilityState::Active;

        // 接続イベントを発行
        if let Some(ref bus) = self.event_bus {
            let event = CapabilityEvent::new("midi.device_connected", "midi-capability")
                .with_payload(&serde_json::json!({
                    "port_name": port_name,
                }));
            bus.emit(event).await;
        }

        tracing::info!("MIDI monitoring started on: {}", port_name);
        Ok(())
    }

    /// MIDI監視を停止
    pub async fn stop_monitoring(&mut self) -> CapabilityResult<()> {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(()).await;
        }

        if let Some(task) = self.monitor_task.take() {
            task.abort();
        }

        // 切断イベントを発行
        let port_name = {
            let mut state = self.connection_state.write().await;
            let name = state.connected_port.take();
            name
        };

        if let Some(ref bus) = self.event_bus {
            if let Some(name) = port_name {
                let event = CapabilityEvent::new("midi.device_disconnected", "midi-capability")
                    .with_payload(&serde_json::json!({
                        "port_name": name,
                    }));
                bus.emit(event).await;
            }
        }

        self.state = CapabilityState::Idle;
        Ok(())
    }
}

impl Default for MidiCapability {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// MidiEventをCapabilityEventに変換
fn midi_event_to_capability_event(event: &MidiEvent) -> CapabilityEvent {
    let (event_type, payload) = match &event.message {
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        } => (
            "midi.note_on",
            serde_json::json!({
                "channel": channel,
                "note": note,
                "velocity": velocity,
                "port_name": event.port_name,
                "data": [0x90 | (channel - 1), *note, *velocity],
            }),
        ),
        MidiMessage::NoteOff {
            channel,
            note,
            velocity,
        } => (
            "midi.note_off",
            serde_json::json!({
                "channel": channel,
                "note": note,
                "velocity": velocity,
                "port_name": event.port_name,
                "data": [0x80 | (channel - 1), *note, *velocity],
            }),
        ),
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        } => (
            "midi.cc",
            serde_json::json!({
                "channel": channel,
                "controller": controller,
                "value": value,
                "port_name": event.port_name,
                "data": [0xB0 | (channel - 1), *controller, *value],
            }),
        ),
        MidiMessage::ProgramChange { channel, program } => (
            "midi.program_change",
            serde_json::json!({
                "channel": channel,
                "program": program,
                "port_name": event.port_name,
            }),
        ),
        MidiMessage::PitchBend { channel, value } => (
            "midi.pitch_bend",
            serde_json::json!({
                "channel": channel,
                "value": value,
                "port_name": event.port_name,
            }),
        ),
        MidiMessage::Other { data } => (
            "midi.other",
            serde_json::json!({
                "data": data,
                "port_name": event.port_name,
            }),
        ),
    };

    CapabilityEvent::new(event_type, "midi-capability").with_payload(&payload)
}

// =============================================================================
// Capability Implementation
// =============================================================================

#[async_trait]
impl Capability for MidiCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "midi-capability",
            env!("CARGO_PKG_VERSION"),
            "MIDI入力を監視し、イベントを配信する能力",
        )
        .with_author("Vantage Point Team")
        .with_params(MIDI_CAPABILITY_PARAMS)
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, ctx: &CapabilityContext) -> CapabilityResult<()> {
        tracing::info!("MidiCapability initializing");

        // ポートパターンを設定から取得
        if self.config.port_pattern.is_none() {
            if let Some(pattern) = ctx.config().get("port_pattern") {
                if let Some(p) = pattern.as_str() {
                    self.config.port_pattern = Some(p.to_string());
                }
            }
        }

        // 利用可能なポートを取得
        self.refresh_ports().await;

        self.state = CapabilityState::Idle;

        // 初期化完了イベント
        if let Some(ref bus) = self.event_bus {
            let state = self.connection_state.read().await;
            let event = CapabilityEvent::new("capability.initialized", "midi-capability")
                .with_payload(&serde_json::json!({
                    "available_ports": state.available_ports,
                    "port_pattern": self.config.port_pattern,
                }));
            bus.emit(event).await;
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        tracing::info!("MidiCapability shutting down");

        // 監視を停止
        self.stop_monitoring().await?;

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
    async fn test_midi_capability_new() {
        let cap = MidiCapability::new();
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
    }

    #[tokio::test]
    async fn test_midi_capability_with_config() {
        let config = MidiConfig {
            port_pattern: Some("LPD8".to_string()),
            ..Default::default()
        };
        let cap = MidiCapability::with_config(config);
        assert_eq!(cap.config.port_pattern, Some("LPD8".to_string()));
    }

    #[tokio::test]
    async fn test_midi_capability_initialize() {
        let mut cap = MidiCapability::new();
        let ctx = CapabilityContext::new();
        cap.initialize(&ctx).await.unwrap();
        assert_eq!(cap.state(), CapabilityState::Idle);
    }

    #[test]
    fn test_midi_event_to_capability_event_note_on() {
        let midi_event = MidiEvent {
            port_name: "Test Port".to_string(),
            message: MidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            timestamp: std::time::Instant::now(),
        };

        let cap_event = midi_event_to_capability_event(&midi_event);
        assert_eq!(cap_event.event_type, "midi.note_on");
        assert_eq!(cap_event.payload.get("note").unwrap(), 60);
        assert_eq!(cap_event.payload.get("velocity").unwrap(), 100);
    }

    #[test]
    fn test_midi_event_to_capability_event_cc() {
        let midi_event = MidiEvent {
            port_name: "Test Port".to_string(),
            message: MidiMessage::ControlChange {
                channel: 1,
                controller: 7,
                value: 64,
            },
            timestamp: std::time::Instant::now(),
        };

        let cap_event = midi_event_to_capability_event(&midi_event);
        assert_eq!(cap_event.event_type, "midi.cc");
        assert_eq!(cap_event.payload.get("controller").unwrap(), 7);
        assert_eq!(cap_event.payload.get("value").unwrap(), 64);
    }

    #[tokio::test]
    async fn test_connection_state_default() {
        let cap = MidiCapability::new();
        let state = cap.connection_state().await;
        assert!(state.connected_port.is_none());
        assert_eq!(state.event_count, 0);
    }
}
