//! machine 階層 Stand container (LSCM、 doc 12 §3 / §9 参照)
//!
//! daemon (`run_daemon`、 port 32000) で 1 instance、 machine 全体で共有される
//! machine 階層 Stand 群を host する。 LSCM (Layer-Stand Composition Model) における
//! "Layer がそこに保持されるべき Stand を抱える" の machine layer 実体。
//!
//! ## host する Stand
//!
//! - **daemon 👑** (`ProcessManagerCapability`): VP daemon process manager
//! - **UpdateCapability**: VP self-update (LSCM Open Question Q-12 catalog 拡張候補)
//! - **DeviceRegistry 🧲** (`DeviceRegistry`): multi-device registry + 艦隊 input listener（`with_devices`）
//!
//! ## 実装状態
//!
//! - PR-α-1 (VP-111 ✅): struct 新設、 既存 machine 階層 instance を集約 view、
//!   `AppState.machine_capabilities` field に Some で注入。
//! - 旧 `MidiCapability` hosting（PR-α-2 の single-device monitor）は退役 — 消費者
//!   （`ProtocolCapability`）が本番で実体化されず、enumeration 先頭 device（実機で LPD8）を
//!   無条件 grab して DeviceRegistry listener を沈黙させる害だけが残っていたため（fleet dogfood で発覚）。
//! - 後続 cleanup: AppState 既存 field (`daemon` / `update`) と本 struct の重複保持を整理
//!   (現状は意図的 HACK、 LSCM A6 share-nothing 整合は β 以降で)。
//!
//! wiremsg R5-4: 旧 msgbox の registry サブシステム (旧 External Control stand の registry
//! 登録を含む) は撤去済。 wire の cross-process delivery は daemon の project registry
//! (project → SP port) を使う別経路で、 msgbox registry には依存しない。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §3 Layer + §9 Catalog)、
//! Linear VP-109 (parent epic)、 VP-111/112/113/114 (sub-issue)。

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::capability::{ProcessManagerCapability, UpdateCapability};
#[cfg(feature = "midi")]
use crate::devices::DeviceRegistry;

/// machine 階層 Stand container。
///
/// daemon で 1 instance、 machine 全体で共有。
pub struct MachineCapabilities {
    /// Process Manager (daemon 👑、 LSCM machine 階層 SSOT)
    pub process_manager: Arc<RwLock<ProcessManagerCapability>>,

    /// Self-update Capability (LSCM Open Question Q-12 catalog 拡張候補)
    pub update: Arc<RwLock<UpdateCapability>>,

    /// DeviceRegistry 🧲 — multi-device registry + 艦隊 input listener。
    /// `with_devices` で構築すると起動時 attach（既接続 device の listener）まで済む。
    #[cfg(feature = "midi")]
    pub devices: Option<Arc<RwLock<DeviceRegistry>>>,
}

impl MachineCapabilities {
    /// 既存 instance を集約して新規構築 (midi なし版、 feature gate 無効時 / test 用)。
    ///
    /// PR-α-1 (VP-111): `run_daemon` で散乱していた machine 階層 capability 群の集約 view を提供。
    /// AppState 既存 field (`daemon` / `update`) と本 struct の
    /// 重複保持は意図的 HACK (LSCM A6 share-nothing 整合は β 以降で整理予定)。
    ///
    /// DeviceRegistry を host したい場合は `with_devices` を使う (feature = "midi")。
    pub fn new(
        process_manager: Arc<RwLock<ProcessManagerCapability>>,
        update: Arc<RwLock<UpdateCapability>>,
    ) -> Self {
        Self {
            process_manager,
            update,
            #[cfg(feature = "midi")]
            devices: None,
        }
    }

    /// DeviceRegistry 🧲 を host した状態で構築（feature = "midi"）。
    ///
    /// hot-plug 検知の authority は macOS menu bar agent（Swift `CoreMIDIWatcher`）で、
    /// agent が `device` channel で送る `ReportDevice` を `DeviceRegistry::report_device_*` が
    /// registry に反映する（daemon は midir polling を回さない）。起動前から挿さっている
    /// device は agent 報告が来ない環境があるため、`attach_fleet_inputs` の 1 回
    /// enumeration で input listener を確実に張る（fleet #877/#878）。
    /// ROTO 持続制御は独立経路（`start_roto_control`、process/server.rs）。
    #[cfg(feature = "midi")]
    pub async fn with_devices(
        process_manager: Arc<RwLock<ProcessManagerCapability>>,
        update: Arc<RwLock<UpdateCapability>>,
    ) -> Self {
        let mut wc = Self::new(process_manager, update);

        let event_bus = Arc::new(crate::capability::eventbus::EventBus::new());
        let devices = DeviceRegistry::new(event_bus);
        devices.attach_fleet_inputs().await;
        tracing::info!("devices 🧲 registry ready (hot-plug は Swift agent が報告 / polling 停止)");
        wc.devices = Some(Arc::new(RwLock::new(devices)));

        wc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn daemon_capabilities_new_smoke() {
        let pmc = Arc::new(RwLock::new(ProcessManagerCapability::new()));
        let upd = Arc::new(RwLock::new(UpdateCapability::new()));

        let wc = MachineCapabilities::new(pmc, upd);
        // smoke test: construct succeeds without panic、 各 field が存在
        let _ = wc.process_manager.read().await;
        let _ = wc.update.read().await;

        #[cfg(feature = "midi")]
        assert!(
            wc.devices.is_none(),
            "new() では devices は None (with_devices() を使うと Some)"
        );
    }
}
