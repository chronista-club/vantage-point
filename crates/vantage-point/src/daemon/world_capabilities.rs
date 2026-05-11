//! World 階層 Stand container (LSCM、 doc 12 §3 / §9 参照)
//!
//! TheWorld daemon (`run_world`、 port 32000) で 1 instance、 machine 全体で共有される
//! World 階層 Stand 群を host する。 LSCM (Layer-Stand Composition Model) における
//! "Layer がそこに保持されるべき Stand を抱える" の World Layer 実体。
//!
//! ## host する Stand
//!
//! - **TheWorld 👑** (`ProcessManagerCapability`): VP world process manager
//! - **UpdateCapability**: VP self-update (LSCM Open Question Q-12 catalog 拡張候補)
//! - **MsgboxRegistry**: cross-Process actor mailbox routing (Phase 3)
//! - **Whitesnake 🐍** (`Whitesnake`): file-backed persistence wrapper
//! - **Hermit Purple 🍇** (`MidiCapability`、 Option): external IF (MIDI/MCP/tmux)。
//!   PR-α-2 で `ProcessCapabilities` から移管完了、 PR-α-3 で mailbox `hermit_purple@world`
//!   register 完了。 `with_midi` 経由で `Some(...)` host、 `new` のみだと None。
//!
//! ## 実装状態 (PR-α 完了後)
//!
//! - PR-α-1 (VP-111 ✅): struct 新設、 既存 World 階層 instance を集約 view、
//!   `AppState.world_capabilities` field に Some で注入。
//! - PR-α-2 (VP-112 ✅): `MidiCapability` を `ProcessCapabilities` から取り外し、 本 struct の
//!   `midi` field に host。 mailbox 移管 prep 完了 (`hp@world` register 含む)。
//! - PR-α-3 (VP-113 ✅): mailbox `hermit_purple@world` を `msgbox_registry` に register、
//!   CLI `vp midi monitor` を World daemon (port 32000) 経由に rewire。
//! - PR-α-4 (VP-114): `vp daemon start --midi` flag 追加で MidiConfig CLI 経路復活 (planned)。
//! - 後続 cleanup: AppState 既存 field (`world` / `msgbox_registry` / `update` / `whitesnake`)
//!   と本 struct の重複保持を整理 (現状は意図的 HACK、 LSCM A6 share-nothing 整合は β 以降で)。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §3 Layer + §9 Catalog)、
//! Linear VP-109 (parent epic)、 VP-111/112/113/114 (sub-issue)。

use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(feature = "midi")]
use crate::capability::MidiCapability;
use crate::capability::{MsgboxRegistry, ProcessManagerCapability, UpdateCapability, Whitesnake};

/// World 階層 Stand container。
///
/// TheWorld daemon で 1 instance、 machine 全体で共有。 SP (Project) からは Phase 3
/// cross-Process forward 経由で reach (mailbox address `*@world`)。
pub struct WorldCapabilities {
    /// Process Manager (TheWorld 👑、 LSCM World 階層 SSOT)
    pub process_manager: Arc<RwLock<ProcessManagerCapability>>,

    /// Self-update Capability (LSCM Open Question Q-12 catalog 拡張候補)
    pub update: Arc<RwLock<UpdateCapability>>,

    /// Msgbox actor registry (cross-Process routing 用)
    pub msgbox_registry: Arc<MsgboxRegistry>,

    /// Whitesnake 🐍 — 汎用永続化レイヤー (file-backed per port)
    pub whitesnake: Whitesnake,

    /// Hermit Purple 🍇 — external IF (MIDI/MCP/tmux)。
    /// PR-α-2 (VP-112) で `ProcessCapabilities.midi` から移管完了。
    /// `WorldCapabilities::with_midi` 経由で構築すると `Some(...)` で host、
    /// `WorldCapabilities::new` だけだと None placeholder。
    #[cfg(feature = "midi")]
    pub midi: Option<Arc<RwLock<MidiCapability>>>,
}

impl WorldCapabilities {
    /// 既存 instance を集約して新規構築 (midi なし版、 feature gate 無効時 / test 用)。
    ///
    /// PR-α-1 (VP-111): `run_world` で散乱していた World 階層 capability 群の集約 view を提供。
    /// AppState 既存 field (`world` / `msgbox_registry` / `update` / `whitesnake`) と本 struct の
    /// 重複保持は意図的 HACK (LSCM A6 share-nothing 整合は β 以降で整理予定)。
    ///
    /// midi を host したい場合は `with_midi` を使う (feature = "midi")。
    pub fn new(
        process_manager: Arc<RwLock<ProcessManagerCapability>>,
        update: Arc<RwLock<UpdateCapability>>,
        msgbox_registry: Arc<MsgboxRegistry>,
        whitesnake: Whitesnake,
    ) -> Self {
        Self {
            process_manager,
            update,
            msgbox_registry,
            whitesnake,
            #[cfg(feature = "midi")]
            midi: None,
        }
    }

    /// MidiCapability を host した状態で構築 (PR-α-2、 feature = "midi")。
    ///
    /// LSCM doc 12 §9 の Hermit Purple 🍇 = World 階層 target を実現。 旧 `ProcessCapabilities.midi`
    /// (Project 階層) の経路を World daemon (`run_world`) に移管。
    ///
    /// 内部で `MidiCapability::with_config` → `initialize` → `start_monitoring` を実行し、
    /// `midi: Some(...)` 状態で構築する。 監視 start に失敗した場合は warning log して
    /// graceful degrade (構築自体は成功、 midi 監視タスクなしで継続)。
    ///
    /// PR-α-3 (VP-113): mailbox address `hermit_purple@world` を `msgbox_registry` に
    /// register する。 cross-process forward で SP から `hp@world` (or `hermit_purple@world`)
    /// に reach 可能になる。 `world_port` 引数は World daemon (TheWorld) の port 番号、
    /// register entry の port field に使う。
    ///
    /// 注: 現実装の `MsgboxRegistry` は `(project_name, actor)` key で管理。 World scope を
    /// 表現するため pseudo project name `"world"` を使う (LSCM Open Question Q-7
    /// `(layer_path, actor)` 拡張までの暫定 HACK)。
    #[cfg(feature = "midi")]
    pub async fn with_midi(
        process_manager: Arc<RwLock<ProcessManagerCapability>>,
        update: Arc<RwLock<UpdateCapability>>,
        msgbox_registry: Arc<MsgboxRegistry>,
        whitesnake: Whitesnake,
        midi_config: crate::midi::MidiConfig,
        world_port: u16,
    ) -> anyhow::Result<Self> {
        use crate::capability::core::{Capability, CapabilityContext};

        let mut wc = Self::new(process_manager, update, msgbox_registry.clone(), whitesnake);

        // MidiCapability を host (PR-α-2)
        let mut midi_cap = MidiCapability::with_config(midi_config);
        let ctx = CapabilityContext::new();
        midi_cap
            .initialize(&ctx)
            .await
            .map_err(|e| anyhow::anyhow!("MidiCapability initialize failed: {}", e))?;

        // 監視開始 (port_index は config から)
        let port_index = midi_cap.config().port_index;
        if let Err(e) = midi_cap.start_monitoring(port_index).await {
            tracing::warn!(
                "MidiCapability start_monitoring failed (graceful degrade): {}",
                e
            );
        }

        // PR-α-3 (VP-113): mailbox `hermit_purple@world` を register
        // cross-process forward で SP から reach 可能にする (LSCM doc 12 §5)
        if let Err(e) = msgbox_registry
            .register("hermit_purple", "world", world_port)
            .await
        {
            tracing::warn!(
                "hermit_purple@world register failed (graceful degrade): {}",
                e
            );
        } else {
            tracing::info!(
                "Mailbox registered: hermit_purple@world (port={})",
                world_port
            );
        }

        wc.midi = Some(Arc::new(RwLock::new(midi_cap)));
        Ok(wc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn world_capabilities_new_smoke() {
        let pmc = Arc::new(RwLock::new(ProcessManagerCapability::new()));
        let upd = Arc::new(RwLock::new(UpdateCapability::new()));
        let registry = Arc::new(MsgboxRegistry::new());
        let ws = Whitesnake::in_memory();

        let wc = WorldCapabilities::new(pmc, upd, registry, ws);
        // smoke test: construct succeeds without panic、 各 field が存在
        let _ = wc.process_manager.read().await;
        let _ = wc.update.read().await;
        let _ = &wc.msgbox_registry;
        let _ = &wc.whitesnake;

        #[cfg(feature = "midi")]
        assert!(
            wc.midi.is_none(),
            "new() では midi は None (with_midi() を使うと Some)"
        );
    }
}
