//! The World MIDI 連携
//!
//! MIDI コントローラーからのイベントを処理し、View 操作にマッピング。
//! 設計: docs/design/12-midi-mapping.md

use tokio::sync::broadcast;

use super::server::BroadcastMessage;
use crate::midi::{MidiEvent, MidiMessage};

/// MIDI アクション（マッピング結果）
#[derive(Debug, Clone)]
pub enum MidiAction {
    /// ワークスペース切り替え
    WorkspaceSwitch { workspace_id: String },
    /// タイルフォーカス
    TileFocus { tile_index: usize },
    /// パネルトグル
    PanelToggle { panel_id: String },
    /// パネルリサイズ (0-127 → min-max)
    PanelResize { panel_id: String, value: u8 },
    /// タイル分割
    TileSplit { direction: String },
    /// カスタムコマンド
    Custom { command: String },
    /// 無視
    Ignore,
}

/// LPD8 プリセット (Program 1 - Workspace Mode)
///
/// Pad Layout (Note numbers):
/// ```
/// ┌─────┬─────┬─────┬─────┐
/// │ 36  │ 37  │ 38  │ 39  │  Pad 1-4: Workspace 1-4
/// ├─────┼─────┼─────┼─────┤
/// │ 40  │ 41  │ 42  │ 43  │  Pad 5-8: Tile Focus 1-4
/// └─────┴─────┴─────┴─────┘
/// ```
pub fn lpd8_default_mapping(event: &MidiEvent) -> MidiAction {
    match &event.message {
        MidiMessage::NoteOn { note, velocity, .. } if *velocity > 0 => {
            match *note {
                // Pad 1-4: ワークスペース切り替え
                36 => MidiAction::WorkspaceSwitch {
                    workspace_id: "1".to_string(),
                },
                37 => MidiAction::WorkspaceSwitch {
                    workspace_id: "2".to_string(),
                },
                38 => MidiAction::WorkspaceSwitch {
                    workspace_id: "3".to_string(),
                },
                39 => MidiAction::WorkspaceSwitch {
                    workspace_id: "4".to_string(),
                },
                // Pad 5-8: タイルフォーカス
                40 => MidiAction::TileFocus { tile_index: 0 },
                41 => MidiAction::TileFocus { tile_index: 1 },
                42 => MidiAction::TileFocus { tile_index: 2 },
                43 => MidiAction::TileFocus { tile_index: 3 },
                _ => MidiAction::Ignore,
            }
        }
        MidiMessage::ControlChange {
            controller, value, ..
        } => {
            match *controller {
                // Knob 1: Left Panel 幅
                1 => MidiAction::PanelResize {
                    panel_id: "left".to_string(),
                    value: *value,
                },
                // Knob 2: Right Panel 幅
                2 => MidiAction::PanelResize {
                    panel_id: "right".to_string(),
                    value: *value,
                },
                // Knob 3: 分割比率 (future)
                3 => MidiAction::Ignore,
                // Knob 4-8: 未割り当て
                _ => MidiAction::Ignore,
            }
        }
        _ => MidiAction::Ignore,
    }
}

/// MIDI アクションを BroadcastMessage に変換
pub fn action_to_broadcast(action: MidiAction) -> Option<BroadcastMessage> {
    match action {
        MidiAction::WorkspaceSwitch { workspace_id } => Some(BroadcastMessage::WorkspaceUpdate {
            workspace_id: workspace_id.clone(),
            name: format!("Workspace {}", workspace_id),
        }),
        MidiAction::TileFocus { tile_index } => Some(BroadcastMessage::Show {
            pane_id: format!("tile-{}", tile_index),
            content_type: "focus".to_string(),
            content: "".to_string(),
            append: false,
        }),
        MidiAction::PanelToggle { panel_id } => Some(BroadcastMessage::Show {
            pane_id: panel_id,
            content_type: "toggle".to_string(),
            content: "".to_string(),
            append: false,
        }),
        MidiAction::PanelResize { panel_id, value } => {
            // 0-127 を 100-400px にマッピング
            let width = 100 + ((value as u32) * 300 / 127);
            Some(BroadcastMessage::Show {
                pane_id: panel_id,
                content_type: "resize".to_string(),
                content: format!("{}px", width),
                append: false,
            })
        }
        MidiAction::TileSplit { direction } => Some(BroadcastMessage::Show {
            pane_id: "center".to_string(),
            content_type: "split".to_string(),
            content: direction,
            append: false,
        }),
        MidiAction::Custom { command } => Some(BroadcastMessage::Show {
            pane_id: "command".to_string(),
            content_type: "custom".to_string(),
            content: command,
            append: false,
        }),
        MidiAction::Ignore => None,
    }
}

/// MIDI イベントを処理して View に配信
pub fn handle_midi_event(event: &MidiEvent, broadcast_tx: &broadcast::Sender<BroadcastMessage>) {
    let action = lpd8_default_mapping(event);

    if let Some(msg) = action_to_broadcast(action) {
        tracing::debug!("MIDI → View: {:?}", msg);
        let _ = broadcast_tx.send(msg);
    }
}

/// MIDI 入力監視を開始し、View に配信
///
/// `port_pattern` で MIDI ポートを指定（例: "LPD8"）
pub async fn start_midi_listener(
    port_pattern: Option<&str>,
    broadcast_tx: broadcast::Sender<BroadcastMessage>,
) -> anyhow::Result<()> {
    use crate::midi::{MidiEvent, parse_midi_message};

    let midi_in = midir::MidiInput::new("vp-world-midi")?;
    let ports = midi_in.ports();

    if ports.is_empty() {
        anyhow::bail!("MIDI ポートが見つかりません");
    }

    // パターンでポートを検索、なければ最初のポート
    let port_idx = if let Some(pattern) = port_pattern {
        ports
            .iter()
            .position(|p| {
                midi_in
                    .port_name(p)
                    .map(|name| name.contains(pattern))
                    .unwrap_or(false)
            })
            .unwrap_or(0)
    } else {
        0
    };

    let port = ports
        .get(port_idx)
        .ok_or_else(|| anyhow::anyhow!("MIDI ポート {} が見つかりません", port_idx))?;

    let port_name = midi_in
        .port_name(port)
        .unwrap_or_else(|_| "Unknown".to_string());

    tracing::info!("MIDI 接続中: {}", port_name);

    let port_name_clone = port_name.clone();
    let _connection = midi_in.connect(
        port,
        "vp-world-midi-connection",
        move |_timestamp, message, tx: &mut broadcast::Sender<BroadcastMessage>| {
            if let Some(midi_msg) = parse_midi_message(message) {
                let event = MidiEvent {
                    port_name: port_name_clone.clone(),
                    message: midi_msg,
                    timestamp: std::time::Instant::now(),
                };
                handle_midi_event(&event, tx);
            }
        },
        broadcast_tx,
    )?;

    tracing::info!("MIDI 監視開始: {}", port_name);

    // 接続を維持
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}
