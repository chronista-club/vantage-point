//! Machine-scoped connection to midistaged. A disabled VP observes without taking leases.
use crate::devices::DeviceRegistry;
use crate::roto_setup_sync::Status;
use crate::roto_surface::{RotoSurface, leds_for_pins, ready};
use midistage_client::{
    Client, Endpoint, Hello, Quiesced, SetEnabled,
    protocol::{PROTOCOL_VERSION, Phase},
};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

// The SDK bounds transport (5s), channel open (3s), and Hello response wait (3s),
// but not the entire connect future (request send and failure cleanup can also wait).
// This is VP's independent recovery deadline. Let Hello return its client when possible.
const TASK_STOP_TIMEOUT: Duration = Duration::from_secs(12);
const CLIENT_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct MidiUseTask {
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Option<Arc<Client>>>,
    registry: Weak<RwLock<DeviceRegistry>>,
}

impl MidiUseTask {
    pub(crate) async fn shutdown(mut self) {
        self.shutdown.cancel();
        let client = match tokio::time::timeout(TASK_STOP_TIMEOUT, &mut self.task).await {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => {
                tracing::warn!(%error, "midistage task failed during shutdown");
                None
            }
            Err(_) => {
                tracing::warn!("midistage task shutdown timed out; aborting");
                self.task.abort();
                let _ = self.task.await;
                None
            }
        };
        shutdown_connection(&self.registry, client).await;
    }
}

pub(crate) fn start(registry: Weak<RwLock<DeviceRegistry>>) -> MidiUseTask {
    let path = std::env::var_os("HOME").map(|home| {
        PathBuf::from(home).join("Library/Application Support/Midistage/endpoint.json")
    });
    start_with_path(registry, path)
}

fn start_with_path(registry: Weak<RwLock<DeviceRegistry>>, path: Option<PathBuf>) -> MidiUseTask {
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let weak = registry.clone();
    let task = tokio::spawn(async move {
        while !stop.is_cancelled() {
            let Some(devices) = weak.upgrade() else { break };
            let enabled = devices.read().await.midi_enabled();
            drop(devices);
            let result = run(&weak, enabled, path.as_deref(), &stop).await;
            if stop.is_cancelled() {
                return result.ok().flatten();
            }
            let Some(devices) = weak.upgrade() else {
                break;
            };
            {
                let mut state = devices.write().await;
                state.midi_client = None;
                state.midi_error = result.err().map(|e| e.to_string());
                state.apply_midi_snapshot(None).await;
            }
            drop(devices);
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
        None
    });
    MidiUseTask {
        shutdown,
        task,
        registry,
    }
}

/// ROTO surface を実機に合わせる 1 tick。望む姿は daemon の lane_registry（全 repo の lane）から、
/// 送り先は lease 中に service が作った仮想 port（`midi_access.outputs_for`）から引く。port が無い
/// （lease 外）なら ready でない扱いにして、写した姿を忘れる。
async fn project_needs_you(
    registry: &Weak<RwLock<DeviceRegistry>>,
    surfaces: &mut BTreeMap<String, RotoSurface>,
    statuses: &[Status],
    addresses: &[Option<String>; 8],
) {
    surfaces.retain(|id, _| statuses.iter().any(|s| s.device == *id));
    let Some(devices) = registry.upgrade() else {
        return;
    };
    let lanes = devices.read().await.lane_registry();
    let desired = match lanes {
        Some(lanes) => {
            let lanes = lanes.read().await;
            leds_for_pins(addresses, lanes.values().flatten())
        }
        None => [false; 8],
    };
    for status in statuses {
        let port = devices
            .read()
            .await
            .midi_access
            .read()
            .expect("MIDI access poisoned")
            .outputs_for(&status.device)
            .into_iter()
            .next();
        let surface = surfaces.entry(status.device.clone()).or_default();
        let msgs = surface.reconcile(ready(status) && port.is_some(), desired);
        if let Some(port) = port
            && !msgs.is_empty()
        {
            let msgs: Vec<Vec<u8>> = msgs.iter().map(|m| m.to_vec()).collect();
            devices.read().await.send_output(&port, &msgs).await;
        }
    }
}

async fn run(
    registry: &Weak<RwLock<DeviceRegistry>>,
    enabled: bool,
    path: Option<&std::path::Path>,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<Arc<Client>>> {
    if shutdown.is_cancelled() {
        return Ok(None);
    }
    let path = path.ok_or_else(|| anyhow::anyhow!("HOME unavailable"))?;
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
    let mut setups = crate::roto_setup_sync::Sync::new(client.clone(), &vp_paths::vp_state_dir());
    // ROTO surface（design 84 §LED）: device id → 実機に写した LED 姿
    let mut surfaces: BTreeMap<String, RotoSurface> = BTreeMap::new();
    let mut acknowledged = HashSet::new();
    let result: anyhow::Result<()> = async {
        'session: while !shutdown.is_cancelled() {
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
                    let request = Quiesced {
                        device_id: device.state.device_id.clone(),
                        lease_token: lease.token.clone(),
                    };
                    tokio::select! {
                        _ = shutdown.cancelled() => break 'session,
                        result = client.quiesced(&request) => { result?; }
                    }
                    acknowledged.insert(lease.token.clone());
                }
            }
            // Setup requests run separately so a slow configuration RPC never delays
            // revoking input listeners or acknowledging another device's handoff.
            if enabled {
                let statuses = crate::roto_setup_midistage::statuses(&snapshot);
                setups.tick(&statuses).await;
                // ROTO surface: needs-you を LED に写す（design 84 §LED）。setup 同期と同じ tick に相乗り
                project_needs_you(registry, &mut surfaces, &statuses, setups.pin_addresses()).await;
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
            snapshot = tokio::select! {
                _ = shutdown.cancelled() => break,
                result = client.snapshot() => result?,
            };
        }
        Ok(())
    }
    .await;
    drop(setups);
    if shutdown.is_cancelled() {
        // The owner joins this task before releasing local ports and closing the session.
        return Ok(Some(client));
    }
    // Release local work before closing the session; disconnect authorizes the service to drain.
    if let Some(devices) = registry.upgrade() {
        let mut state = devices.write().await;
        state.midi_client = None;
        state.apply_midi_snapshot(None).await;
    }
    match close_with_timeout(client.close()).await {
        Ok(()) => tracing::info!("midistage client closed"),
        Err(error) => tracing::warn!(%error, "midistage client close failed"),
    }
    result.map(|()| None)
}

async fn shutdown_connection(registry: &Weak<RwLock<DeviceRegistry>>, client: Option<Arc<Client>>) {
    let registered = if let Some(devices) = registry.upgrade() {
        let mut state = devices.write().await;
        state.release_fleet().await;
        state.midi_snapshot = None;
        state.midi_client.take()
    } else {
        None
    };
    // A just-completed Hello may not yet have published its client in the registry.
    if let Some(client) = client.or(registered) {
        match close_with_timeout(client.close()).await {
            Ok(()) => tracing::info!("midistage client closed"),
            Err(error) => tracing::warn!(%error, "midistage client close failed during shutdown"),
        }
    }
}

async fn close_with_timeout(
    close: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tokio::time::timeout(CLIENT_CLOSE_TIMEOUT, close)
        .await
        .map_err(|_| anyhow::anyhow!("midistage client close timed out after 2s"))?
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

#[cfg(test)]
mod tests;
