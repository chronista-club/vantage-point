//! Vantage Point Extension Protocol
//!
//! AG-UI/ACP準拠の上で、Vantage Point独自の拡張を提供。
//! MIDI入力、Capability連携、Synergy発動などを扱う。
//!
//! ## 要件
//! - REQ-PROTO-003: Vantage拡張

use serde::{Deserialize, Serialize};
use serde_json::Value;

// =============================================================================
// Vantage Event Types
// =============================================================================

/// Vantage Point独自イベント
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VantageEvent {
    // -------------------------------------------------------------------------
    // MIDI Events
    // -------------------------------------------------------------------------
    /// MIDI入力イベント
    MidiInput {
        /// MIDIチャンネル (0-15)
        channel: u8,
        /// イベント種別
        event_type: MidiEventType,
        /// 生データ
        data: Vec<u8>,
        /// タイムスタンプ（Unix millis）
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// MIDIデバイス接続
    MidiDeviceConnected {
        device_id: String,
        device_name: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// MIDIデバイス切断
    MidiDeviceDisconnected {
        device_id: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Capability Events
    // -------------------------------------------------------------------------
    /// Capability状態変更
    CapabilityStateChanged {
        capability_id: String,
        state: CapabilityStateInfo,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Capability登録
    CapabilityRegistered {
        capability_id: String,
        name: String,
        version: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Capability登録解除
    CapabilityUnregistered {
        capability_id: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Synergy Events
    // -------------------------------------------------------------------------
    /// Synergy発動
    SynergyActivated {
        synergy_id: String,
        synergy_type: SynergyTypeInfo,
        capabilities: Vec<String>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Synergy解除
    SynergyDeactivated {
        synergy_id: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Evolution Events (JoJo-inspired)
    // -------------------------------------------------------------------------
    /// 能力進化
    EvolutionTriggered {
        capability_id: String,
        from_level: String,
        to_level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// 覚醒
    Awakening {
        capability_id: String,
        awakening_kind: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Custom Extension Point
    // -------------------------------------------------------------------------
    /// カスタムイベント（拡張用）
    Custom {
        name: String,
        data: Value,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },
}

// =============================================================================
// MIDI Types
// =============================================================================

/// MIDIイベント種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiEventType {
    NoteOn,
    NoteOff,
    ControlChange,
    ProgramChange,
    PitchBend,
    Aftertouch,
    PolyAftertouch,
    SysEx,
    Clock,
    Start,
    Stop,
    Continue,
    Unknown,
}

impl MidiEventType {
    /// MIDIステータスバイトからイベント種別を判定
    pub fn from_status_byte(status: u8) -> Self {
        match status & 0xF0 {
            0x80 => Self::NoteOff,
            0x90 => Self::NoteOn,
            0xA0 => Self::PolyAftertouch,
            0xB0 => Self::ControlChange,
            0xC0 => Self::ProgramChange,
            0xD0 => Self::Aftertouch,
            0xE0 => Self::PitchBend,
            0xF0 => match status {
                0xF0 => Self::SysEx,
                0xF8 => Self::Clock,
                0xFA => Self::Start,
                0xFB => Self::Continue,
                0xFC => Self::Stop,
                _ => Self::Unknown,
            },
            _ => Self::Unknown,
        }
    }
}

/// MIDI Note情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiNote {
    pub note: u8,
    pub velocity: u8,
    pub channel: u8,
}

/// MIDI CC情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiControlChange {
    pub controller: u8,
    pub value: u8,
    pub channel: u8,
}

// =============================================================================
// Capability Types
// =============================================================================

/// Capability状態情報
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStateInfo {
    Uninitialized,
    Idle,
    Active,
    Processing,
    Paused,
    Error { message: String },
    Stopped,
}

// =============================================================================
// Synergy Types
// =============================================================================

/// Synergy種別情報
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynergyTypeInfo {
    /// 依存関係（一方が他方を必要とする）
    Dependency,
    /// 相互強化（両方あると強くなる）
    Enhancement,
    /// 独立（相互作用なし）
    Independent,
    /// 競合（同時使用で問題が起きる可能性）
    Conflict,
}

// =============================================================================
// Builders
// =============================================================================

impl VantageEvent {
    /// MIDI Note On イベントを作成
    pub fn midi_note_on(channel: u8, note: u8, velocity: u8) -> Self {
        Self::MidiInput {
            channel,
            event_type: MidiEventType::NoteOn,
            data: vec![0x90 | channel, note, velocity],
            timestamp: now_millis(),
        }
    }

    /// MIDI Note Off イベントを作成
    pub fn midi_note_off(channel: u8, note: u8, velocity: u8) -> Self {
        Self::MidiInput {
            channel,
            event_type: MidiEventType::NoteOff,
            data: vec![0x80 | channel, note, velocity],
            timestamp: now_millis(),
        }
    }

    /// MIDI CC イベントを作成
    pub fn midi_cc(channel: u8, controller: u8, value: u8) -> Self {
        Self::MidiInput {
            channel,
            event_type: MidiEventType::ControlChange,
            data: vec![0xB0 | channel, controller, value],
            timestamp: now_millis(),
        }
    }

    /// Capability状態変更イベントを作成
    pub fn capability_state_changed(capability_id: &str, state: CapabilityStateInfo) -> Self {
        Self::CapabilityStateChanged {
            capability_id: capability_id.to_string(),
            state,
            timestamp: now_millis(),
        }
    }

    /// Synergy発動イベントを作成
    pub fn synergy_activated(
        synergy_id: &str,
        synergy_type: SynergyTypeInfo,
        capabilities: Vec<String>,
    ) -> Self {
        Self::SynergyActivated {
            synergy_id: synergy_id.to_string(),
            synergy_type,
            capabilities,
            timestamp: now_millis(),
        }
    }

    /// カスタムイベントを作成
    pub fn custom(name: &str, data: Value) -> Self {
        Self::Custom {
            name: name.to_string(),
            data,
            timestamp: now_millis(),
        }
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_midi_event_type_from_status() {
        assert_eq!(MidiEventType::from_status_byte(0x90), MidiEventType::NoteOn);
        assert_eq!(
            MidiEventType::from_status_byte(0x80),
            MidiEventType::NoteOff
        );
        assert_eq!(
            MidiEventType::from_status_byte(0xB0),
            MidiEventType::ControlChange
        );
        assert_eq!(MidiEventType::from_status_byte(0xF8), MidiEventType::Clock);
    }

    #[test]
    fn test_midi_note_on_serialization() {
        let event = VantageEvent::midi_note_on(0, 60, 100);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"MIDI_INPUT\""));
        assert!(json.contains("\"event_type\":\"note_on\""));
    }

    #[test]
    fn test_capability_state_changed() {
        let event =
            VantageEvent::capability_state_changed("midi-capability", CapabilityStateInfo::Active);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"CAPABILITY_STATE_CHANGED\""));
        assert!(json.contains("\"state\":\"active\""));
    }

    #[test]
    fn test_synergy_activated() {
        let event = VantageEvent::synergy_activated(
            "midi-agent-synergy",
            SynergyTypeInfo::Enhancement,
            vec!["midi".to_string(), "agent".to_string()],
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"SYNERGY_ACTIVATED\""));
        assert!(json.contains("\"synergy_type\":\"enhancement\""));
    }

    #[test]
    fn test_custom_event() {
        let event = VantageEvent::custom("my_custom_event", serde_json::json!({"key": "value"}));
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"CUSTOM\""));
        assert!(json.contains("\"name\":\"my_custom_event\""));
    }
}
