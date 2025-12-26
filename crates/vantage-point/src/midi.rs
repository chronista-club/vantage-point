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
    /// Switch to a project by index (1-based, opens WebUI for that project)
    SwitchProject { index: usize },
    /// Open WebUI for current/specified instance
    OpenWebUI { port: Option<u16> },
    /// Stop an instance
    StopInstance { port: u16 },
    /// Send chat message
    SendChat { message: String },
    /// Cancel current chat (sends to active Stand)
    CancelChat { port: Option<u16> },
    /// Reset session (sends to active Stand)
    ResetSession { port: Option<u16> },
    /// Custom HTTP request to Stand API
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
    stand_port: u16,
}

impl MidiHandler {
    pub fn new(config: MidiConfig, stand_port: u16) -> Self {
        Self { config, stand_port }
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
        let base_url = format!("http://localhost:{}", self.stand_port);

        match action {
            MidiAction::OpenWebUI { port } => {
                let url = format!("http://localhost:{}", port.unwrap_or(self.stand_port));
                if let Err(e) = open::that(&url) {
                    tracing::error!("Failed to open browser: {}", e);
                }
            }
            MidiAction::StopInstance { port } => {
                let url = format!("http://localhost:{}/api/shutdown", port);
                let _ = client.post(&url).send().await;
                tracing::info!("Sent shutdown to port {}", port);
            }
            MidiAction::CancelChat { port } => {
                let target_port = port.unwrap_or(self.stand_port);
                // TODO: Implement cancel via WebSocket or API
                tracing::info!("Cancel chat on port {} (not yet implemented)", target_port);
            }
            MidiAction::ResetSession { port } => {
                let target_port = port.unwrap_or(self.stand_port);
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
                tracing::info!("Switch to project {} (requires restart)", index);
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
    stand_port: u16,
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
    let handler = Arc::new(MidiHandler::new(config, stand_port));

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
                        0..=3 => 1, // Projects: Green
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

/// Send SysEx message to LPD8
pub fn send_sysex(port_pattern: Option<&str>, data: &[u8]) -> Result<()> {
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

    let mut conn = midi_out.connect(port, "vp-midi-sysex")?;
    conn.send(data)?;

    tracing::info!("Sent {} bytes SysEx to {}", data.len(), port_name);
    Ok(())
}

/// Write VP default config to LPD8 Program 1
pub fn write_vp_config_to_lpd8(port_pattern: Option<&str>) -> Result<()> {
    let program = lpd8::Program::vp_default();
    let sysex = program.to_sysex(0); // Program 1 = index 0
    send_sysex(port_pattern, &sysex)?;
    println!("VP config written to LPD8 Program 1");
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
    stand_port: u16,
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
    println!("Stand port: {}", stand_port);
    println!("Press Ctrl+C to stop.\n");
    println!("Waiting for MIDI events...");

    let (tx, mut rx) = broadcast::channel::<MidiEvent>(100);
    let handler = Arc::new(MidiHandler::new(config, stand_port));

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
