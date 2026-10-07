//! Machine-scoped connection to midistaged. A disabled VP observes without taking leases.
use crate::devices::DeviceRegistry;
use midistage_client::{
    Client, Endpoint, Hello, Quiesced, SetEnabled,
    protocol::{PROTOCOL_VERSION, Phase},
};
use std::{
    collections::HashSet,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::RwLock;

pub fn start(registry: Weak<RwLock<DeviceRegistry>>) {
    tokio::spawn(async move {
        while let Some(devices) = registry.upgrade() {
            let enabled = devices.read().await.midi_enabled();
            drop(devices);
            let result = run(&registry, enabled).await;
            let Some(devices) = registry.upgrade() else {
                break;
            };
            {
                let mut state = devices.write().await;
                state.midi_client = None;
                state.midi_error = result.err().map(|e| e.to_string());
                state.apply_midi_snapshot(None).await;
            }
            drop(devices);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}
async fn run(registry: &Weak<RwLock<DeviceRegistry>>, enabled: bool) -> anyhow::Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME unavailable"))?;
    let path =
        std::path::PathBuf::from(home).join("Library/Application Support/Midistage/endpoint.json");
    let endpoint: Endpoint = serde_json::from_slice(&std::fs::read(path)?)?;
    let (client, mut snapshot) = Client::connect(
        &endpoint,
        Hello {
            protocol_version: PROTOCOL_VERSION,
            client_id: if enabled { "vp" } else { "vp-monitor" }.into(),
            display_name: "Vantage Point".into(),
            auth_token: String::new(),
            native_midi: enabled,
            initial_enabled_profiles: vec![],
        },
    )
    .await?;
    let client = Arc::new(client);
    let mut acknowledged = HashSet::new();
    let result: anyhow::Result<()> = async {
        loop {
            let Some(devices) = registry.upgrade() else {
                break;
            };
            {
                let mut state = devices.write().await;
                if state.midi_enabled() != enabled {
                    break;
                }
                state.midi_client = Some(client.clone());
                state.midi_error = None;
                state.apply_midi_snapshot(Some(snapshot.clone())).await;
            }
            drop(devices);
            for device in &snapshot.devices {
                if device.state.phase == Phase::Releasing
                    && let Some(lease) = &device.state.lease
                    && lease.session_id == snapshot.session_id
                    && !acknowledged.contains(&lease.token)
                {
                    client
                        .quiesced(&Quiesced {
                            device_id: device.state.device_id.clone(),
                            lease_token: lease.token.clone(),
                        })
                        .await?;
                    acknowledged.insert(lease.token.clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            snapshot = client.snapshot().await?;
        }
        Ok(())
    }
    .await;
    // Release local work before closing the session; disconnect authorizes the service to drain.
    if let Some(devices) = registry.upgrade() {
        let mut state = devices.write().await;
        state.midi_client = None;
        state.apply_midi_snapshot(None).await;
    }
    let _ = client.close().await;
    result
}

/// Called by daemon commands. Capture the connection without holding the registry across the RPC.
pub async fn set_device(
    registry: &Arc<RwLock<DeviceRegistry>>,
    request: SetEnabled,
) -> anyhow::Result<()> {
    let client = {
        let state = registry.read().await;
        anyhow::ensure!(
            state.midi_enabled(),
            "VP の MIDI 全体スイッチが OFF です。先に ON にしてください。"
        );
        state
            .midi_client
            .clone()
            .ok_or_else(|| anyhow::anyhow!("MIDI サービスに接続していません"))?
    };
    client.set_enabled(&request).await?;
    Ok(())
}
