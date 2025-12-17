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
fn parse_midi_message(message: &[u8]) -> Option<MidiMessage> {
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
    /// Switch to a project by index
    SwitchProject { index: usize },
    /// Open WebUI for current/specified instance
    OpenWebUI { port: Option<u16> },
    /// Stop an instance
    StopInstance { port: u16 },
    /// Send chat message
    SendChat { message: String },
    /// Cancel current chat
    CancelChat,
    /// Reset session
    ResetSession,
    /// Custom HTTP request to daemon API
    ApiCall {
        endpoint: String,
        method: String,
        body: Option<String>,
    },
}

/// MIDI mapping configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MidiConfig {
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
                    && let Some(action) = self.config.cc_actions.get(controller) {
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
        let base_url = format!("http://localhost:{}", self.daemon_port);

        match action {
            MidiAction::OpenWebUI { port } => {
                let url = format!("http://localhost:{}", port.unwrap_or(self.daemon_port));
                if let Err(e) = open::that(&url) {
                    tracing::error!("Failed to open browser: {}", e);
                }
            }
            MidiAction::StopInstance { port } => {
                let url = format!("http://localhost:{}/api/shutdown", port);
                let _ = client.post(&url).send().await;
                tracing::info!("Sent shutdown to port {}", port);
            }
            MidiAction::CancelChat => {
                // Would need WebSocket connection or API endpoint
                tracing::info!("Cancel chat requested (not yet implemented)");
            }
            MidiAction::ResetSession => {
                // Would need WebSocket connection or API endpoint
                tracing::info!("Reset session requested (not yet implemented)");
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
    println!("Daemon port: {}", daemon_port);
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
