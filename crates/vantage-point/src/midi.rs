//! MIDI input handling for vp
//!
//! Monitors MIDI input devices and triggers actions based on MIDI events.
//! Used for physical controller integration (e.g., LPD8, Launch Control, etc.)

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;

/// MIDI message types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidiMessage {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    PitchBend {
        channel: u8,
        value: u16,
    },
    Other {
        data: Vec<u8>,
    },
}

/// MIDI event with port info
#[derive(Debug, Clone)]
pub struct MidiEvent {
    pub port_name: String,
    pub message: MidiMessage,
    pub timestamp: std::time::Instant,
}

/// List available MIDI input ports
pub fn list_ports() -> Result<Vec<String>> {
    let midi_in = midir::MidiInput::new("vp-midi")?;
    let ports = midi_in.ports();

    let mut port_names = Vec::new();
    for port in ports.iter() {
        if let Ok(name) = midi_in.port_name(port) {
            port_names.push(name);
        }
    }

    Ok(port_names)
}

/// Parse raw MIDI bytes into MidiMessage
pub fn parse_midi_message(message: &[u8]) -> Option<MidiMessage> {
    if message.is_empty() {
        return None;
    }

    let status = message[0];
    let message_type = status & 0xF0;
    let channel = (status & 0x0F) + 1; // Channel is 1-based for display

    match message_type {
        0x90 => {
            // Note On (velocity 0 = Note Off)
            if message.len() >= 3 {
                let velocity = message[2];
                if velocity == 0 {
                    Some(MidiMessage::NoteOff {
                        channel,
                        note: message[1],
                        velocity: 0,
                    })
                } else {
                    Some(MidiMessage::NoteOn {
                        channel,
                        note: message[1],
                        velocity,
                    })
                }
            } else {
                None
            }
        }
        0x80 => {
            // Note Off
            if message.len() >= 3 {
                Some(MidiMessage::NoteOff {
                    channel,
                    note: message[1],
                    velocity: message[2],
                })
            } else {
                None
            }
        }
        0xB0 => {
            // Control Change
            if message.len() >= 3 {
                Some(MidiMessage::ControlChange {
                    channel,
                    controller: message[1],
                    value: message[2],
                })
            } else {
                None
            }
        }
        0xC0 => {
            // Program Change
            if message.len() >= 2 {
                Some(MidiMessage::ProgramChange {
                    channel,
                    program: message[1],
                })
            } else {
                None
            }
        }
        0xE0 => {
            // Pitch Bend
            if message.len() >= 3 {
                let value = ((message[2] as u16) << 7) | (message[1] as u16);
                Some(MidiMessage::PitchBend { channel, value })
            } else {
                None
            }
        }
        _ => Some(MidiMessage::Other {
            data: message.to_vec(),
        }),
    }
}

/// MIDI action to execute
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MidiAction {
    /// Switch to a repo by index (1-based, opens WebUI for that repo)
    SwitchProject { index: usize },
    /// Open WebUI for current/specified instance
    OpenWebUI { port: Option<u16> },
    /// Stop an instance
    StopInstance { port: u16 },
    /// Send chat message
    SendChat { message: String },
    /// Cancel current chat (sends to active Process)
    CancelChat { port: Option<u16> },
    /// Reset session (sends to active Process)
    ResetSession { port: Option<u16> },
    /// Custom HTTP request to Process API
    ApiCall {
        endpoint: String,
        method: String,
        body: Option<String>,
    },
}

/// MIDI mapping configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MidiConfig {
    /// Port index to connect to (0-based)
    pub port_index: Option<usize>,
    /// Port name pattern to connect to (substring match)
    pub port_pattern: Option<String>,
    /// Note mappings: note number -> action
    pub note_actions: std::collections::HashMap<u8, MidiAction>,
    /// CC mappings: controller number -> action
    pub cc_actions: std::collections::HashMap<u8, MidiAction>,
    /// Program change mappings: program number -> action
    pub program_actions: std::collections::HashMap<u8, MidiAction>,
}

/// MIDI event handler that executes actions
pub struct MidiHandler {
    config: MidiConfig,
    daemon_port: u16,
}

impl MidiHandler {
    pub fn new(config: MidiConfig, daemon_port: u16) -> Self {
        Self {
            config,
            daemon_port,
        }
    }

    /// Handle incoming MIDI event
    pub async fn handle_event(&self, event: &MidiEvent) {
        match &event.message {
            MidiMessage::NoteOn { note, velocity, .. } if *velocity > 0 => {
                if let Some(action) = self.config.note_actions.get(note) {
                    tracing::info!("MIDI Note {} -> {:?}", note, action);
                    self.execute_action(action).await;
                }
            }
            MidiMessage::ControlChange {
                controller, value, ..
            } => {
                // Only trigger on value > 64 (like a button press)
                if *value > 64
                    && let Some(action) = self.config.cc_actions.get(controller)
                {
                    tracing::info!("MIDI CC {} -> {:?}", controller, action);
                    self.execute_action(action).await;
                }
            }
            MidiMessage::ProgramChange { program, .. } => {
                if let Some(action) = self.config.program_actions.get(program) {
                    tracing::info!("MIDI PC {} -> {:?}", program, action);
                    self.execute_action(action).await;
                }
            }
            _ => {}
        }
    }

    /// Execute a MIDI action
    async fn execute_action(&self, action: &MidiAction) {
        let client = reqwest::Client::new();
        let base_url = format!("http://[::1]:{}", self.daemon_port);

        match action {
            MidiAction::OpenWebUI { port } => {
                let url = format!("http://[::1]:{}", port.unwrap_or(self.daemon_port));
                if let Err(e) = open::that(&url) {
                    tracing::error!("Failed to open browser: {}", e);
                }
            }
            MidiAction::StopInstance { port } => {
                let url = format!("http://[::1]:{}/api/shutdown", port);
                let _ = client.post(&url).send().await;
                tracing::info!("Sent shutdown to port {}", port);
            }
            MidiAction::CancelChat { port } => {
                let target_port = port.unwrap_or(self.daemon_port);
                // TODO: Implement cancel via WebSocket or API
                tracing::info!("Cancel chat on port {} (not yet implemented)", target_port);
            }
            MidiAction::ResetSession { port } => {
                let target_port = port.unwrap_or(self.daemon_port);
                // TODO: Implement reset via WebSocket or API
                tracing::info!(
                    "Reset session on port {} (not yet implemented)",
                    target_port
                );
            }
            MidiAction::ApiCall {
                endpoint,
                method,
                body,
            } => {
                let url = format!("{}{}", base_url, endpoint);
                let request = match method.to_uppercase().as_str() {
                    "POST" => client.post(&url),
                    "GET" => client.get(&url),
                    "PUT" => client.put(&url),
                    "DELETE" => client.delete(&url),
                    _ => {
                        tracing::error!("Unknown HTTP method: {}", method);
                        return;
                    }
                };

                let request = if let Some(body) = body {
                    request
                        .header("Content-Type", "application/json")
                        .body(body.clone())
                } else {
                    request
                };

                match request.send().await {
                    Ok(resp) => tracing::info!("API call {} -> {}", endpoint, resp.status()),
                    Err(e) => tracing::error!("API call failed: {}", e),
                }
            }
            MidiAction::SwitchProject { index } => {
                tracing::info!("Switch to repo {} (requires restart)", index);
            }
            MidiAction::SendChat { message } => {
                tracing::info!("Send chat: {} (not yet implemented)", message);
            }
        }
    }
}

/// Start MIDI input monitoring
pub async fn run_midi(
    port_index: Option<usize>,
    config: MidiConfig,
    daemon_port: u16,
) -> Result<()> {
    let midi_in = midir::MidiInput::new("vp-midi")?;
    let ports = midi_in.ports();

    if ports.is_empty() {
        anyhow::bail!("No MIDI input ports found");
    }

    // Find port by pattern or index
    let port_idx = if let Some(pattern) = &config.port_pattern {
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
        .ok_or_else(|| anyhow::anyhow!("MIDI port {} not found", port_idx))?;

    let port_name = midi_in
        .port_name(port)
        .unwrap_or_else(|_| "Unknown".to_string());
    tracing::info!("Connecting to MIDI port: {}", port_name);

    let (tx, mut rx) = broadcast::channel::<MidiEvent>(100);
    let handler = Arc::new(MidiHandler::new(config, daemon_port));

    // Spawn event handler task
    let handler_clone = handler.clone();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            handler_clone.handle_event(&event).await;
        }
    });

    // Connect to MIDI port
    let port_name_clone = port_name.clone();
    let _connection = midi_in.connect(
        port,
        "vp-midi-connection",
        move |_timestamp, message, _| {
            if let Some(midi_msg) = parse_midi_message(message) {
                let event = MidiEvent {
                    port_name: port_name_clone.clone(),
                    message: midi_msg,
                    timestamp: std::time::Instant::now(),
                };
                let _ = tx.send(event);
            }
        },
        (),
    )?;

    tracing::info!("MIDI monitoring started on: {}", port_name);

    // Keep connection alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

/// Print available MIDI ports
pub fn print_ports() {
    match list_ports() {
        Ok(ports) => {
            if ports.is_empty() {
                println!("No MIDI input ports found.");
            } else {
                println!("Available MIDI input ports:");
                for (i, name) in ports.iter().enumerate() {
                    println!("  {}: {}", i, name);
                }
            }
        }
        Err(e) => {
            println!("Error listing MIDI ports: {}", e);
        }
    }
}

// =============================================================================
// LPD8 SysEx Protocol
// =============================================================================

/// LPD8 SysEx constants
pub mod lpd8 {
    /// Akai Manufacturer ID
    pub const MANUFACTURER_ID: u8 = 0x47;
    /// LPD8 Model ID bytes
    pub const MODEL_ID: [u8; 2] = [0x7F, 0x75];

    /// SysEx commands
    pub mod cmd {
        /// Get program data from device
        pub const GET_PROGRAM: u8 = 0x63;
        /// Send program data to device
        pub const SEND_PROGRAM: u8 = 0x61;
        /// Get active program number
        pub const GET_ACTIVE: u8 = 0x64;
        /// Set active program number
        pub const SET_ACTIVE: u8 = 0x62;
    }

    /// Pad toggle mode
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub enum PadToggle {
        Momentary = 0,
        Toggle = 1,
    }

    /// Pad configuration (11 bytes per pad)
    #[derive(Debug, Clone)]
    pub struct PadConfig {
        pub note: u8,
        pub pc: u8, // Program Change number
        pub cc: u8, // Control Change number
        pub toggle: PadToggle,
        pub pad_off_color: u8, // Mk2 only: LED off color (0-3)
        pub pad_on_color: u8,  // Mk2 only: LED on color (0-3)
    }

    impl Default for PadConfig {
        fn default() -> Self {
            Self {
                note: 36,
                pc: 0,
                cc: 1,
                toggle: PadToggle::Momentary,
                pad_off_color: 0,
                pad_on_color: 3, // Red
            }
        }
    }

    impl PadConfig {
        /// Convert to 11-byte SysEx format
        pub fn to_bytes(&self) -> [u8; 11] {
            [
                self.note,
                self.pc,
                self.cc,
                self.toggle as u8,
                0,
                0,
                0,
                0,
                0, // Reserved
                self.pad_off_color,
                self.pad_on_color,
            ]
        }

        /// Parse from 11-byte SysEx format
        pub fn from_bytes(data: &[u8]) -> Option<Self> {
            if data.len() < 11 {
                return None;
            }
            Some(Self {
                note: data[0],
                pc: data[1],
                cc: data[2],
                toggle: if data[3] == 1 {
                    PadToggle::Toggle
                } else {
                    PadToggle::Momentary
                },
                pad_off_color: data[9],
                pad_on_color: data[10],
            })
        }
    }

    /// Knob configuration (5 bytes per knob)
    #[derive(Debug, Clone)]
    pub struct KnobConfig {
        pub cc: u8,
        pub low: u8,
        pub high: u8,
    }

    impl Default for KnobConfig {
        fn default() -> Self {
            Self {
                cc: 1,
                low: 0,
                high: 127,
            }
        }
    }

    impl KnobConfig {
        /// Convert to 5-byte SysEx format
        pub fn to_bytes(&self) -> [u8; 5] {
            [self.cc, self.low, self.high, 0, 0]
        }

        /// Parse from 5-byte SysEx format
        pub fn from_bytes(data: &[u8]) -> Option<Self> {
            if data.len() < 5 {
                return None;
            }
            Some(Self {
                cc: data[0],
                low: data[1],
                high: data[2],
            })
        }
    }

    /// Full LPD8 program configuration
    #[derive(Debug, Clone)]
    pub struct Program {
        pub channel: u8, // 1-16
        pub pads: [PadConfig; 8],
        pub knobs: [KnobConfig; 8],
    }

    impl Default for Program {
        fn default() -> Self {
            Self {
                channel: 1,
                pads: std::array::from_fn(|i| PadConfig {
                    note: 36 + i as u8,
                    ..Default::default()
                }),
                knobs: std::array::from_fn(|i| KnobConfig {
                    cc: 1 + i as u8,
                    ..Default::default()
                }),
            }
        }
    }

    impl Program {
        /// Build SysEx message to send this program to device
        pub fn to_sysex(&self, program_num: u8) -> Vec<u8> {
            let mut msg = vec![
                0xF0, // SysEx start
                MANUFACTURER_ID,
                MODEL_ID[0],
                MODEL_ID[1],
                cmd::SEND_PROGRAM,
                program_num,
                self.channel.saturating_sub(1), // 0-indexed in protocol
            ];

            // 8 pads × 11 bytes
            for pad in &self.pads {
                msg.extend_from_slice(&pad.to_bytes());
            }

            // 8 knobs × 5 bytes
            for knob in &self.knobs {
                msg.extend_from_slice(&knob.to_bytes());
            }

            msg.push(0xF7); // SysEx end
            msg
        }

        /// Parse program from SysEx response
        pub fn from_sysex(data: &[u8]) -> Option<Self> {
            // Expected: F0 47 7F 75 63 <prog> <chan> <88 pad bytes> <40 knob bytes> F7
            if data.len() < 136 || data[0] != 0xF0 || data[data.len() - 1] != 0xF7 {
                return None;
            }

            let channel = data[6] + 1; // Convert to 1-indexed
            let pad_data = &data[7..95]; // 88 bytes for 8 pads
            let knob_data = &data[95..135]; // 40 bytes for 8 knobs

            let mut pads: [PadConfig; 8] = std::array::from_fn(|_| PadConfig::default());
            let mut knobs: [KnobConfig; 8] = std::array::from_fn(|_| KnobConfig::default());

            for i in 0..8 {
                pads[i] = PadConfig::from_bytes(&pad_data[i * 11..(i + 1) * 11])?;
            }
            for i in 0..8 {
                knobs[i] = KnobConfig::from_bytes(&knob_data[i * 5..(i + 1) * 5])?;
            }

            Some(Self {
                channel,
                pads,
                knobs,
            })
        }

        /// Create VP default program (PAD 1-4: Notes 36-39, PAD 5-8: Notes 40-43)
        pub fn vp_default() -> Self {
            Self {
                channel: 1,
                pads: std::array::from_fn(|i| PadConfig {
                    note: 36 + i as u8,
                    pc: i as u8,
                    cc: 1 + i as u8,
                    toggle: PadToggle::Momentary,
                    pad_off_color: 0,
                    pad_on_color: match i {
                        0..=3 => 1, // Repos: Green
                        4 => 3,     // Cancel: Red
                        5 => 2,     // Reset: Yellow/Orange
                        _ => 0,     // Unassigned: Off
                    },
                }),
                knobs: std::array::from_fn(|i| KnobConfig {
                    cc: 70 + i as u8, // CC 70-77 for knobs
                    low: 0,
                    high: 127,
                }),
            }
        }
    }

    /// Build SysEx to request program data
    pub fn request_program(program_num: u8) -> Vec<u8> {
        vec![
            0xF0,
            MANUFACTURER_ID,
            MODEL_ID[0],
            MODEL_ID[1],
            cmd::GET_PROGRAM,
            program_num,
            0xF7,
        ]
    }

    /// Build SysEx to get active program number
    pub fn request_active_program() -> Vec<u8> {
        vec![
            0xF0,
            MANUFACTURER_ID,
            MODEL_ID[0],
            MODEL_ID[1],
            cmd::GET_ACTIVE,
            0xF7,
        ]
    }

    /// Build SysEx to set active program
    pub fn set_active_program(program_num: u8) -> Vec<u8> {
        vec![
            0xF0,
            MANUFACTURER_ID,
            MODEL_ID[0],
            MODEL_ID[1],
            cmd::SET_ACTIVE,
            program_num,
            0xF7,
        ]
    }
}

/// LPD8 の機種判定（identity request / reply）
///
/// 初代（model `0x75`）と mk2（model `0x4C`）は program 設定の SysEx が別物なので、
/// 書き込み・切替の前に必ずここで経路を選ぶ。
pub mod lpd8_identity {
    /// Universal identity request（device ID `7F` = broadcast）
    pub const REQUEST: [u8; 6] = [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7];

    /// LPD8 の機種
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Lpd8Model {
        /// 初代 LPD8（model `0x75`）
        Original,
        /// LPD8 mk2（model `0x4C`）
        Mk2,
    }

    /// identity reply（`F0 7E <dev> 06 02 <manufacturer> <model> ...`）から機種を読む。
    /// Akai 以外・LPD8 以外・identity reply でないものは `None`
    pub fn parse_reply(data: &[u8]) -> Option<Lpd8Model> {
        if data.len() < 8 || data[0] != 0xF0 || data[1] != 0x7E || data[3..5] != [0x06, 0x02] {
            return None;
        }
        if data[5] != super::lpd8::MANUFACTURER_ID {
            return None;
        }
        match data[6] {
            0x75 => Some(Lpd8Model::Original),
            0x4C => Some(Lpd8Model::Mk2),
            _ => None,
        }
    }
}

/// LPD8 mk2 の program SysEx（model `0x4C`）
///
/// 2026-10-12 に実機で probe して確定した形（design 81「mk2 の program と SysEx」）:
/// - 読み出し要求: `F0 47 7F 4C 03 00 01 <prog 1-4> F7`
/// - 応答（173 byte）: `F0 47 7F 4C 03 01 29 <prog> <body 164 byte> F7`
/// - 書き込み: `F0 47 7F 4C 01 01 29 <prog> <body 164 byte> F7`（応答は無い → 読み戻して確かめる）
///
/// body は head 4 byte（未解読 → 触らず保持）+ pad 8 × 16 byte + knob 8 × 4 byte。
/// 解読済みの field だけを型に出し、それ以外の byte は持ち回して build で元の位置に戻す。
pub mod lpd8_mk2 {
    /// mk2 SysEx ヘッダ（Akai / broadcast / model `4C`）
    pub const HDR: [u8; 4] = [0xF0, 0x47, 0x7F, 0x4C];
    /// 書き込み command
    pub const CMD_WRITE: u8 = 0x01;
    /// 読み出し command（要求・応答とも）
    pub const CMD_READ: u8 = 0x03;
    /// 応答 / 書き込みの長さ field（`01 29`）
    const LEN_FIELD: [u8; 2] = [0x01, 0x29];
    /// program body の長さ
    pub const BODY_LEN: usize = 164;
    /// 読み出し応答の全長（HDR 4 + cmd 1 + len 2 + prog 1 + body + F7）
    pub const RESPONSE_LEN: usize = 8 + BODY_LEN + 1;
    const HEAD_LEN: usize = 4;
    const PAD_LEN: usize = 16;
    const KNOB_LEN: usize = 4;
    const PADS: usize = 8;
    const KNOBS: usize = 8;
    /// VP の入力 profile が読む pad の先頭 note（pad i = 36 + i）
    pub const VP_PAD_NOTE_BASE: u8 = 36;
    /// VP の入力 profile が読む knob の先頭 CC（knob i = 70 + i）
    pub const VP_KNOB_CC_BASE: u8 = 70;

    /// VP が書く program の土台（body 164 byte）。VP の割り当てはこの上に重ね、
    /// 4 program のどれに書いても同じ body にする（機材に今入っている中身には依存させない）。
    ///
    /// 出どころ: 2026-10-12 時点で機材の Program 1 に入っていた中身（mako 裁定で土台に採用）。
    pub const BASE_PROGRAM: [u8; BODY_LEN] = [
        0x07, 0x02, 0x00, 0x00, 0x2C, 0x25, 0x00, 0x10, 0x01, 0x7F, 0x01, 0x7F, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x7F, 0x01, 0x7F, 0x2D, 0x26, 0x01, 0x10, 0x00, 0x33, 0x00, 0x33, 0x00, 0x33,
        0x00, 0x0B, 0x00, 0x64, 0x01, 0x34, 0x2E, 0x27, 0x02, 0x10, 0x00, 0x33, 0x00, 0x33, 0x00,
        0x33, 0x01, 0x7F, 0x00, 0x69, 0x01, 0x34, 0x2F, 0x28, 0x03, 0x10, 0x00, 0x33, 0x00, 0x33,
        0x00, 0x33, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x28, 0x21, 0x04, 0x10, 0x01, 0x45, 0x01,
        0x34, 0x01, 0x63, 0x00, 0x00, 0x01, 0x7F, 0x01, 0x7F, 0x29, 0x22, 0x05, 0x10, 0x01, 0x45,
        0x01, 0x34, 0x01, 0x63, 0x00, 0x0B, 0x00, 0x64, 0x01, 0x34, 0x2A, 0x23, 0x06, 0x10, 0x01,
        0x45, 0x01, 0x34, 0x01, 0x63, 0x01, 0x7F, 0x00, 0x69, 0x01, 0x34, 0x2B, 0x24, 0x07, 0x10,
        0x01, 0x45, 0x01, 0x34, 0x01, 0x63, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x4F, 0x10, 0x00,
        0x7F, 0x50, 0x10, 0x00, 0x7F, 0x51, 0x10, 0x00, 0x7F, 0x52, 0x10, 0x00, 0x7F, 0x53, 0x10,
        0x00, 0x7F, 0x54, 0x10, 0x00, 0x7F, 0x55, 0x10, 0x00, 0x7F, 0x56, 0x10, 0x00, 0x7F,
    ];

    /// pad 1 枚（16 byte）
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Pad {
        pub note: u8,
        pub cc: u8,
        pub pc: u8,
        /// 実機では `0x10`（意味は未確定。保持するだけ）
        pub channel: u8,
        /// on 色（R/G/B 各 2 byte = 上位 1 bit + 下位 7 bit）
        pub on_rgb: [u8; 6],
        /// off 色（同上）
        pub off_rgb: [u8; 6],
    }

    /// knob 1 本（4 byte）
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Knob {
        pub cc: u8,
        /// 実機では `0x10`（pad と同じく保持するだけ）
        pub channel: u8,
        pub min: u8,
        pub max: u8,
    }

    /// program 1 本分の body
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Program {
        /// 未解読の先頭 4 byte（先頭は channel と思われる）。触らずに保持する
        pub head: [u8; HEAD_LEN],
        pub pads: [Pad; PADS],
        pub knobs: [Knob; KNOBS],
    }

    impl Program {
        /// body 164 byte を読む。長さが違えば `None`
        pub fn parse_body(body: &[u8]) -> Option<Self> {
            if body.len() != BODY_LEN {
                return None;
            }
            let head = body[..HEAD_LEN].try_into().ok()?;
            let pad_area = &body[HEAD_LEN..HEAD_LEN + PADS * PAD_LEN];
            let knob_area = &body[HEAD_LEN + PADS * PAD_LEN..];
            let pads = std::array::from_fn(|i| {
                let p = &pad_area[i * PAD_LEN..(i + 1) * PAD_LEN];
                Pad {
                    note: p[0],
                    cc: p[1],
                    pc: p[2],
                    channel: p[3],
                    on_rgb: p[4..10].try_into().expect("6 byte"),
                    off_rgb: p[10..16].try_into().expect("6 byte"),
                }
            });
            let knobs = std::array::from_fn(|i| {
                let k = &knob_area[i * KNOB_LEN..(i + 1) * KNOB_LEN];
                Knob {
                    cc: k[0],
                    channel: k[1],
                    min: k[2],
                    max: k[3],
                }
            });
            Some(Self { head, pads, knobs })
        }

        /// body 164 byte に戻す（`parse_body` の逆）
        pub fn to_body(&self) -> Vec<u8> {
            let mut body = Vec::with_capacity(BODY_LEN);
            body.extend_from_slice(&self.head);
            for pad in &self.pads {
                body.extend_from_slice(&[pad.note, pad.cc, pad.pc, pad.channel]);
                body.extend_from_slice(&pad.on_rgb);
                body.extend_from_slice(&pad.off_rgb);
            }
            for knob in &self.knobs {
                body.extend_from_slice(&[knob.cc, knob.channel, knob.min, knob.max]);
            }
            body
        }

        /// VP が要る所だけを書き換えた program を返す: pad i の note = 36 + i、knob i の cc = 70 + i。
        /// head・pad の cc/pc/ch・色・knob の ch/min/max は元のまま
        pub fn with_vp_layout(&self) -> Self {
            let mut next = self.clone();
            for (i, pad) in next.pads.iter_mut().enumerate() {
                pad.note = VP_PAD_NOTE_BASE + i as u8;
            }
            for (i, knob) in next.knobs.iter_mut().enumerate() {
                knob.cc = VP_KNOB_CC_BASE + i as u8;
            }
            next
        }
    }

    /// VP が書く program の body: `BASE_PROGRAM` に VP の割り当てを重ねたもの
    pub fn vp_body() -> Vec<u8> {
        vp_body_from(&BASE_PROGRAM)
    }

    /// `base`（164 byte の body）に VP の割り当てを重ねた body。土台の値に依存しない検証用に分けてある
    pub fn vp_body_from(base: &[u8; BODY_LEN]) -> Vec<u8> {
        Program::parse_body(base)
            .expect("長さは型で保証")
            .with_vp_layout()
            .to_body()
    }

    /// 読み出し要求（`prog` は 1-4）
    pub fn read_request(prog: u8) -> Vec<u8> {
        let mut msg = HDR.to_vec();
        msg.extend_from_slice(&[CMD_READ, 0x00, 0x01, prog, 0xF7]);
        msg
    }

    /// 読み出し応答なら（program 番号, body）を返す。形が違えば `None`
    pub fn parse_read_response(data: &[u8]) -> Option<(u8, &[u8])> {
        if data.len() != RESPONSE_LEN
            || data[..4] != HDR
            || data[4] != CMD_READ
            || data[5..7] != LEN_FIELD
            || data[RESPONSE_LEN - 1] != 0xF7
        {
            return None;
        }
        Some((data[7], &data[8..8 + BODY_LEN]))
    }

    /// 書き込み message（`prog` は 1-4、body は 164 byte）
    pub fn write_message(prog: u8, body: &[u8]) -> Vec<u8> {
        debug_assert_eq!(body.len(), BODY_LEN);
        let mut msg = HDR.to_vec();
        msg.push(CMD_WRITE);
        msg.extend_from_slice(&LEN_FIELD);
        msg.push(prog);
        msg.extend_from_slice(body);
        msg.push(0xF7);
        msg
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 実機 dump（2026-10-12、lead の probe。~/.local/state/vp/backup/lpd8-mk2-programs-20261012.txt）
        const DUMPS: [&str; 4] = [
            "F0 47 7F 4C 03 01 29 01 07 02 00 00 2C 25 00 10 01 7F 01 7F 00 00 00 00 01 7F 01 7F 2D 26 01 10 00 33 00 33 00 33 00 0B 00 64 01 34 2E 27 02 10 00 33 00 33 00 33 01 7F 00 69 01 34 2F 28 03 10 00 33 00 33 00 33 01 00 00 00 01 00 28 21 04 10 01 45 01 34 01 63 00 00 01 7F 01 7F 29 22 05 10 01 45 01 34 01 63 00 0B 00 64 01 34 2A 23 06 10 01 45 01 34 01 63 01 7F 00 69 01 34 2B 24 07 10 01 45 01 34 01 63 01 00 00 00 01 00 4F 10 00 7F 50 10 00 7F 51 10 00 7F 52 10 00 7F 53 10 00 7F 54 10 00 7F 55 10 00 7F 56 10 00 7F F7",
            "F0 47 7F 4C 03 01 29 02 07 02 01 00 34 25 00 10 00 00 01 00 00 00 00 00 01 7F 01 7F 35 26 01 10 00 00 01 00 00 00 00 0B 00 64 01 34 36 27 02 10 00 00 01 00 00 00 01 7F 00 69 01 34 37 28 03 10 00 00 01 7F 01 7F 01 00 00 00 01 00 30 21 04 10 01 10 01 6E 01 10 00 00 01 7F 01 7F 31 22 05 10 01 10 01 6E 01 10 00 0B 00 64 01 34 32 23 06 10 01 10 01 6E 01 10 01 7F 00 69 01 34 33 24 07 10 00 00 01 7F 01 7F 01 00 00 00 01 00 57 10 00 7F 58 10 00 7F 59 10 00 7F 5A 10 00 7F 5B 10 00 7F 5C 10 00 7F 5D 10 00 7F 5E 10 00 7F F7",
            "F0 47 7F 4C 03 01 29 03 07 02 01 00 24 35 00 10 01 00 00 00 01 00 01 7F 00 00 00 00 25 36 01 10 01 00 00 00 01 00 01 7F 00 00 00 00 26 37 02 10 01 00 00 00 01 00 01 7F 00 00 00 00 27 38 03 10 00 00 01 00 00 00 01 7F 00 00 00 00 28 31 04 10 01 7F 01 7F 00 00 01 7F 00 00 00 00 29 32 05 10 01 7F 01 7F 00 00 01 7F 00 00 00 00 2A 33 06 10 01 7F 01 7F 00 00 01 7F 00 00 00 00 2B 34 07 10 00 00 01 00 00 00 01 7F 00 00 00 00 39 10 00 7F 3A 10 00 7F 3B 10 00 7F 3C 10 00 7F 3D 10 00 7F 3E 10 00 7F 3F 10 00 7F 40 10 00 7F F7",
            "F0 47 7F 4C 03 01 29 04 07 02 00 00 2C 25 00 10 01 45 01 34 01 63 00 00 01 7F 01 7F 2D 26 01 10 00 00 01 00 00 00 00 0B 00 64 01 34 2E 27 02 10 00 00 01 00 00 00 01 7F 00 69 01 34 2F 28 03 10 00 00 01 7F 01 7F 01 00 00 00 01 00 28 21 04 10 01 10 01 6E 01 10 00 00 01 7F 01 7F 29 22 05 10 01 10 01 6E 01 10 00 0B 00 64 01 34 2A 23 06 10 01 10 01 6E 01 10 01 7F 00 69 01 34 2B 24 07 10 00 00 01 7F 01 7F 01 00 00 00 01 00 4F 10 00 7F 50 10 00 7F 51 10 00 7F 52 10 00 7F 53 10 00 7F 54 10 00 7F 55 10 00 7F 56 10 00 7F F7",
        ];

        fn hex(s: &str) -> Vec<u8> {
            s.split_whitespace()
                .map(|b| u8::from_str_radix(b, 16).unwrap())
                .collect()
        }

        #[test]
        fn 実機_dump_4_本が_parse_から_build_で_byte_一致する() {
            for (i, dump) in DUMPS.iter().enumerate() {
                let raw = hex(dump);
                let (prog, body) = parse_read_response(&raw).expect("読み出し応答として読める");
                assert_eq!(prog as usize, i + 1);
                let program = Program::parse_body(body).expect("body として読める");
                assert_eq!(program.to_body(), body, "Program {} の round trip", i + 1);
            }
        }

        #[test]
        fn 実機_dump_の_note_と_cc_を読める() {
            let raw = hex(DUMPS[0]);
            let program = Program::parse_body(parse_read_response(&raw).unwrap().1).unwrap();
            let notes: Vec<u8> = program.pads.iter().map(|p| p.note).collect();
            let ccs: Vec<u8> = program.knobs.iter().map(|k| k.cc).collect();
            assert_eq!(notes, [44, 45, 46, 47, 40, 41, 42, 43]);
            assert_eq!(ccs, (79..=86).collect::<Vec<u8>>());

            let raw = hex(DUMPS[2]);
            let program = Program::parse_body(parse_read_response(&raw).unwrap().1).unwrap();
            let notes: Vec<u8> = program.pads.iter().map(|p| p.note).collect();
            assert_eq!(notes, (36..=43).collect::<Vec<u8>>());
        }

        #[test]
        fn vp_配置は_pad_の_note_と_knob_の_cc_だけを書き換える() {
            let raw = hex(DUMPS[0]);
            let body = parse_read_response(&raw).unwrap().1;
            let vp = Program::parse_body(body)
                .unwrap()
                .with_vp_layout()
                .to_body();

            // 変わってよい位置 = pad i の先頭 byte と knob i の先頭 byte だけ
            let allowed: Vec<usize> = (0..PADS)
                .map(|i| HEAD_LEN + i * PAD_LEN)
                .chain((0..KNOBS).map(|i| HEAD_LEN + PADS * PAD_LEN + i * KNOB_LEN))
                .collect();
            for (pos, (before, after)) in body.iter().zip(&vp).enumerate() {
                if !allowed.contains(&pos) {
                    assert_eq!(before, after, "byte {pos} は保持されるべき");
                }
            }
            let program = Program::parse_body(&vp).unwrap();
            for i in 0..8 {
                assert_eq!(program.pads[i].note, 36 + i as u8);
                assert_eq!(program.knobs[i].cc, 70 + i as u8);
            }
        }

        #[test]
        fn 書き込み_message_は_読み出し応答と_command_byte_だけが違う() {
            let raw = hex(DUMPS[3]);
            let (prog, body) = parse_read_response(&raw).unwrap();
            let msg = write_message(prog, body);
            assert_eq!(msg.len(), RESPONSE_LEN);
            assert_eq!(msg[4], CMD_WRITE);
            assert_eq!(msg[..4], raw[..4]);
            assert_eq!(msg[5..], raw[5..]);
        }

        #[test]
        fn vp_body_は土台に_vp_の割り当てだけを重ねる() {
            // 土台の値に依存しない: 実機 dump 4 本どれを土台にしても同じ性質を満たす
            for dump in DUMPS {
                let raw = hex(dump);
                let base: [u8; BODY_LEN] = parse_read_response(&raw).unwrap().1.try_into().unwrap();
                let expected = Program::parse_body(&base)
                    .unwrap()
                    .with_vp_layout()
                    .to_body();
                assert_eq!(vp_body_from(&base), expected);
            }
        }

        #[test]
        fn base_program_は_body_として読める() {
            assert!(Program::parse_body(&BASE_PROGRAM).is_some());
            assert_eq!(vp_body().len(), BODY_LEN);
        }

        #[test]
        fn 読み出し要求の形() {
            assert_eq!(
                read_request(1),
                [0xF0, 0x47, 0x7F, 0x4C, 0x03, 0x00, 0x01, 0x01, 0xF7]
            );
        }

        #[test]
        fn 読み出し応答でないものは読まない() {
            let mut raw = hex(DUMPS[0]);
            assert!(parse_read_response(&raw[..raw.len() - 1]).is_none());
            raw[4] = CMD_WRITE;
            assert!(parse_read_response(&raw).is_none());
            assert!(Program::parse_body(&[0; BODY_LEN - 1]).is_none());
        }

        #[test]
        fn identity_reply_で機種を見分ける() {
            use super::super::lpd8_identity::{Lpd8Model, parse_reply};
            let mk2 = hex(
                "F0 7E 00 06 02 47 4C 00 19 02 49 00 00 7F 00 00 00 00 0A 04 32 34 31 30 32 35 35 36 32 38 35 31 36 00 F7",
            );
            assert_eq!(parse_reply(&mk2), Some(Lpd8Model::Mk2));
            let mut original = mk2.clone();
            original[6] = 0x75;
            assert_eq!(parse_reply(&original), Some(Lpd8Model::Original));
            let mut other = mk2.clone();
            other[5] = 0x00;
            assert_eq!(parse_reply(&other), None);
            assert_eq!(parse_reply(&read_request(1)), None);
        }
    }
}

/// SysEx を 1 通送り、`accept` が受け入れた最初の応答を返す（`timeout` 内に来なければ error）。
///
/// 入力 port は出力と同じ pattern で開く。SysEx を受け取るので、backend ごとの既定に
/// 頼らず `Ignore::None` を明示してから接続する。daemon が MIDI ON（LPD8 を握っている）の
/// ままでも identity reply を読めることを 2026-10-12 に実機で確かめた。
pub fn sysex_request(
    port_pattern: &str,
    request: &[u8],
    accept: impl Fn(&[u8]) -> bool + Send + 'static,
    timeout: std::time::Duration,
) -> Result<Vec<u8>> {
    let mut midi_in = midir::MidiInput::new("vp-midi-sysex-in")?;
    midi_in.ignore(midir::Ignore::None);
    let port = midi_in
        .ports()
        .into_iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .map(|name| name.contains(port_pattern))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("No MIDI input port matching '{}'", port_pattern))?;

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let _input = midi_in
        .connect(
            &port,
            "vp-midi-sysex-in",
            move |_ts, message, _| {
                if accept(message) {
                    let _ = tx.send(message.to_vec());
                }
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("MIDI input connect failed: {}", e))?;

    send_sysex(Some(port_pattern), request)?;
    rx.recv_timeout(timeout)
        .map_err(|_| anyhow::anyhow!("{:?} 以内に SysEx の応答がありませんでした", timeout))
}

/// identity request で LPD8 の機種を判定する
pub fn detect_lpd8_model(port_pattern: &str) -> Result<lpd8_identity::Lpd8Model> {
    let reply = sysex_request(
        port_pattern,
        &lpd8_identity::REQUEST,
        |m| lpd8_identity::parse_reply(m).is_some(),
        std::time::Duration::from_secs(2),
    )
    .map_err(|e| {
        anyhow::anyhow!("LPD8 の機種を判定できませんでした（identity reply 無し）: {e}")
    })?;
    Ok(lpd8_identity::parse_reply(&reply).expect("accept 済み"))
}

/// mk2 の program を読み出し、読み出し応答の全体（173 byte）を返す
pub fn read_lpd8_mk2_program(port_pattern: &str, prog: u8) -> Result<Vec<u8>> {
    sysex_request(
        port_pattern,
        &lpd8_mk2::read_request(prog),
        move |m| lpd8_mk2::parse_read_response(m).is_some_and(|(p, _)| p == prog),
        std::time::Duration::from_secs(2),
    )
}

/// List available MIDI output ports
pub fn list_output_ports() -> Result<Vec<String>> {
    let midi_out = midir::MidiOutput::new("vp-midi-out")?;
    let ports = midi_out.ports();

    let mut port_names = Vec::new();
    for port in ports.iter() {
        if let Ok(name) = midi_out.port_name(port) {
            port_names.push(name);
        }
    }

    Ok(port_names)
}

/// Send SysEx message to a MIDI output port
pub fn send_sysex(port_pattern: Option<&str>, data: &[u8]) -> Result<()> {
    send_batch(port_pattern, std::slice::from_ref(&data.to_vec()))
}

/// 出力 port を pattern で解決して接続を開く。
/// 連続送信（wave 等）で接続を保持したい caller 向け。戻り値は（接続, port 名）。
pub fn open_output(port_pattern: Option<&str>) -> Result<(midir::MidiOutputConnection, String)> {
    let midi_out = midir::MidiOutput::new("vp-midi-out")?;
    let ports = midi_out.ports();

    if ports.is_empty() {
        anyhow::bail!("No MIDI output ports found");
    }

    // Find port by pattern
    let port_idx = if let Some(pattern) = port_pattern {
        ports
            .iter()
            .position(|p| {
                midi_out
                    .port_name(p)
                    .map(|name| name.contains(pattern))
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow::anyhow!("No MIDI port matching '{}'", pattern))?
    } else {
        0
    };

    let port = &ports[port_idx];
    let port_name = midi_out
        .port_name(port)
        .unwrap_or_else(|_| "Unknown".to_string());

    let conn = midi_out
        .connect(port, "vp-midi-sysex")
        .map_err(|e| anyhow::anyhow!("MIDI connect failed: {}", e))?;
    Ok((conn, port_name))
}

/// 複数の MIDI メッセージを 1 接続でまとめて送る。
/// `DeviceProfile` が返すメッセージバッチ（`Vec<Vec<u8>>`）の送出用
/// （メッセージごとに接続を張り直すのを避ける）。
pub fn send_batch(port_pattern: Option<&str>, messages: &[Vec<u8>]) -> Result<()> {
    let (mut conn, port_name) = open_output(port_pattern)?;
    // バッチ送出時の取りこぼし対策（X-Touch 実機検証、doc 21 §5）:
    // 無間隔で 41 連射すると実機が末尾側を取りこぼすため per-message 1ms pacing。
    // pacing のみで十分（送信後の flush 待ちは不要）を抜き差しビルドの比較で確認済み。
    // 単発送信（LPD8 write 等、len == 1）は sleep なしの従来挙動で実績があるため skip
    let pacing = messages.len() > 1;
    let mut total_bytes = 0usize;
    for message in messages {
        conn.send(message)?;
        total_bytes += message.len();
        if pacing {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    tracing::info!(
        "Sent {} MIDI messages ({} bytes) to {}",
        messages.len(),
        total_bytes,
        port_name
    );
    Ok(())
}

/// Print MIDI output ports
pub fn print_output_ports() {
    match list_output_ports() {
        Ok(ports) => {
            if ports.is_empty() {
                println!("No MIDI output ports found.");
            } else {
                println!("Available MIDI output ports:");
                for (i, name) in ports.iter().enumerate() {
                    println!("  {}: {}", i, name);
                }
            }
        }
        Err(e) => {
            println!("Error listing MIDI output ports: {}", e);
        }
    }
}

/// Start MIDI monitoring with console output
pub async fn run_midi_interactive(
    port_index: Option<usize>,
    config: MidiConfig,
    daemon_port: u16,
) -> Result<()> {
    let midi_in = midir::MidiInput::new("vp-midi")?;
    let ports = midi_in.ports();

    if ports.is_empty() {
        println!("No MIDI input ports found.");
        return Ok(());
    }

    // Find port by pattern or index
    let port_idx = if let Some(pattern) = &config.port_pattern {
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
        .ok_or_else(|| anyhow::anyhow!("MIDI port {} not found", port_idx))?;

    let port_name = midi_in
        .port_name(port)
        .unwrap_or_else(|_| "Unknown".to_string());

    println!("Connecting to MIDI port: {}", port_name);
    println!("daemon port: {}", daemon_port);
    println!("Press Ctrl+C to stop.\n");
    println!("Waiting for MIDI events...");

    let (tx, mut rx) = broadcast::channel::<MidiEvent>(100);
    let handler = Arc::new(MidiHandler::new(config, daemon_port));

    // Spawn event handler task
    let handler_clone = handler.clone();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            // Print event to console
            println!("  {:?}", event.message);
            handler_clone.handle_event(&event).await;
        }
    });

    // Connect to MIDI port
    let port_name_clone = port_name.clone();
    let _connection = midi_in.connect(
        port,
        "vp-midi-connection",
        move |_timestamp, message, _| {
            if let Some(midi_msg) = parse_midi_message(message) {
                let event = MidiEvent {
                    port_name: port_name_clone.clone(),
                    message: midi_msg,
                    timestamp: std::time::Instant::now(),
                };
                let _ = tx.send(event);
            }
        },
        (),
    )?;

    println!("Connected! Monitoring: {}\n", port_name);

    // Keep connection alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}
