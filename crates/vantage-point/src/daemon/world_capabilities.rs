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
//! - **Whitesnake 🐍** (`Whitesnake`): file-backed persistence wrapper
//! - **Bastet 🧲** (`MidiCapability`、 Option): external IF (MIDI/MCP/tmux)。
//!   PR-α-2 で `ProcessCapabilities` から移管完了。 `with_midi` 経由で `Some(...)` host、
//!   `new` のみだと None。
//!
//! ## 実装状態 (PR-α 完了後)
//!
//! - PR-α-1 (VP-111 ✅): struct 新設、 既存 World 階層 instance を集約 view、
//!   `AppState.world_capabilities` field に Some で注入。
//! - PR-α-2 (VP-112 ✅): `MidiCapability` を `ProcessCapabilities` から取り外し、 本 struct の
//!   `midi` field に host。
//! - PR-α-4 (VP-114): `vp daemon start --midi` flag 追加で MidiConfig CLI 経路復活 (planned)。
//! - 後続 cleanup: AppState 既存 field (`world` / `update` / `whitesnake`)
//!   と本 struct の重複保持を整理 (現状は意図的 HACK、 LSCM A6 share-nothing 整合は β 以降で)。
//!
//! wiremsg R5-4: 旧 msgbox の registry サブシステム (`hermit_purple@world` の registry
//! 登録を含む) は撤去済。 wire の cross-process delivery は TheWorld の project registry
//! (project → SP port) を使う別経路で、 msgbox registry には依存しない。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §3 Layer + §9 Catalog)、
//! Linear VP-109 (parent epic)、 VP-111/112/113/114 (sub-issue)。

use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(feature = "midi")]
use crate::capability::MidiCapability;
use crate::capability::{ProcessManagerCapability, UpdateCapability, Whitesnake};

/// World 階層 Stand container。
///
/// TheWorld daemon で 1 instance、 machine 全体で共有。
pub struct WorldCapabilities {
    /// Process Manager (TheWorld 👑、 LSCM World 階層 SSOT)
    pub process_manager: Arc<RwLock<ProcessManagerCapability>>,

    /// Self-update Capability (LSCM Open Question Q-12 catalog 拡張候補)
    pub update: Arc<RwLock<UpdateCapability>>,

    /// Whitesnake 🐍 — 汎用永続化レイヤー (file-backed per port)
    pub whitesnake: Whitesnake,

    /// Bastet 🧲 — external IF (MIDI/MCP/tmux)。
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
    /// AppState 既存 field (`world` / `update` / `whitesnake`) と本 struct の
    /// 重複保持は意図的 HACK (LSCM A6 share-nothing 整合は β 以降で整理予定)。
    ///
    /// midi を host したい場合は `with_midi` を使う (feature = "midi")。
    pub fn new(
        process_manager: Arc<RwLock<ProcessManagerCapability>>,
        update: Arc<RwLock<UpdateCapability>>,
        whitesnake: Whitesnake,
    ) -> Self {
        Self {
            process_manager,
            update,
            whitesnake,
            #[cfg(feature = "midi")]
            midi: None,
        }
    }

    /// MidiCapability を host した状態で構築 (PR-α-2、 feature = "midi")。
    ///
    /// LSCM doc 12 §9 の Bastet 🧲 = World 階層 target を実現。 旧 `ProcessCapabilities.midi`
    /// (Project 階層) の経路を World daemon (`run_world`) に移管。
    ///
    /// 内部で `MidiCapability::with_config` → `initialize` → `start_monitoring` を実行し、
    /// `midi: Some(...)` 状態で構築する。 監視 start に失敗した場合は warning log して
    /// graceful degrade (構築自体は成功、 midi 監視タスクなしで継続)。
    #[cfg(feature = "midi")]
    pub async fn with_midi(
        process_manager: Arc<RwLock<ProcessManagerCapability>>,
        update: Arc<RwLock<UpdateCapability>>,
        whitesnake: Whitesnake,
        midi_config: crate::midi::MidiConfig,
    ) -> anyhow::Result<Self> {
        use crate::capability::core::{Capability, CapabilityContext};

        let mut wc = Self::new(process_manager, update, whitesnake);

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
        let ws = Whitesnake::in_memory();

        let wc = WorldCapabilities::new(pmc, upd, ws);
        // smoke test: construct succeeds without panic、 各 field が存在
        let _ = wc.process_manager.read().await;
        let _ = wc.update.read().await;
        let _ = &wc.whitesnake;

        #[cfg(feature = "midi")]
        assert!(
            wc.midi.is_none(),
            "new() では midi は None (with_midi() を使うと Some)"
        );
    }
}
