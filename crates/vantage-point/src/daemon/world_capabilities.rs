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
//!   PR-α-2 で `ProcessCapabilities` から移管予定、 現状 (PR-α-1) は None placeholder。
//!
//! ## PR 段階
//!
//! - **PR-α-1** (本 commit、 VP-111): struct 新設、 既存 World 階層 instance を集約 view。
//!   `AppState.world_capabilities` field に Some で注入 (既存散乱 field は keep、 重複許容)。
//! - PR-α-2: `MidiCapability` を `ProcessCapabilities` から取り外し、 本 struct の `midi`
//!   field に host。 mailbox address `midi@{project}` → `hp@world` 移行。
//! - PR-α-3: caller migration (CLI `vp midi` / vp-app sidebar / 既存 AppState 重複 field 整理)。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §3 Layer + §9 Catalog)、
//! Linear VP-109 (parent epic) / VP-111 (本 PR)。

use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(feature = "midi")]
use crate::capability::MidiCapability;
use crate::capability::{
    MsgboxRegistry, ProcessManagerCapability, UpdateCapability, Whitesnake,
};

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
    /// PR-α-2 で `ProcessCapabilities.midi` から移管予定、 現状 None placeholder。
    #[cfg(feature = "midi")]
    pub midi: Option<Arc<RwLock<MidiCapability>>>,
}

impl WorldCapabilities {
    /// 既存 instance を集約して新規構築。
    ///
    /// PR-α-1: `run_world` で散乱していた World 階層 capability 群の集約 view を提供。
    /// 既存挙動への影響なし (AppState 既存 field は keep、 本 struct は parallel に保持される)。
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn world_capabilities_new_smoke() {
        let pmc = Arc::new(RwLock::new(ProcessManagerCapability::new()));
        let upd = Arc::new(RwLock::new(UpdateCapability::new()));
        let registry = Arc::new(MsgboxRegistry::new());
        let ws = Whitesnake::file_backed_for_port(32099);

        let wc = WorldCapabilities::new(pmc, upd, registry, ws);
        // smoke test: construct succeeds without panic、 各 field が存在
        let _ = wc.process_manager.read().await;
        let _ = wc.update.read().await;
        let _ = &wc.msgbox_registry;
        let _ = &wc.whitesnake;

        #[cfg(feature = "midi")]
        assert!(wc.midi.is_none(), "PR-α-1 では midi は None placeholder");
    }
}
