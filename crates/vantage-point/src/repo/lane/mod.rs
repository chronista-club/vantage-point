//! repo 側の lane runtime（棚卸し 項目 9-1、doc 61 §1）。
//!
//! identity（`LaneId` の永続）と session registry の SSOT は `crate::lane`（disk）。ここは
//! それを読んで実体（PtySlot / chat engine / pump）を持つ側。9-1a で旧 `repo/lane_*.rs` と
//! `lanes_state.rs` を directory に集め、9-1b で `state.rs` を値（address / info）/ runtime（pool）に分けた。
//! 9-1c で値 module から disk / engine を呼ぶ 3 箇所（今は pool.rs に同居）を切り離す。
//!
//! 依存 rule（9-1c 完了時の形）: 値（address / info）← 何も呼ばない / enrich → info + registry +
//! engine catalog / pool → 値 + enrich + registry + engine / reconcile → pool / lifecycle → pool +
//! reconcile + enrich / ops → lifecycle + pool。`crate::lane` → `repo::lane` の辺は 0 本にする。

/// lane の名前（値型）: LaneId / LaneAddress / ROOT_LANE_NAME
pub(crate) mod address;
/// LaneCmd — spawn actor に渡す Cmd 型
pub(crate) mod cmd;
/// lane の帳簿値（値型）: LaneInfo / LaneState / LaneLifecycle / Diff / SystemEvent / LaneSessionsView
pub(crate) mod info;
/// create / delete / restart / reset の orchestration + lanes snapshot + emit_lane_update（旧 lane_lifecycle）
pub(crate) mod lifecycle;
/// lane 系 Unison method の handler（受付の続き、旧 lane_ops）
pub(crate) mod ops;
/// LanePool = runtime の実体（PtySlot / chat engine / pump / lock）
pub(crate) mod pool;
/// intent（registry）→ 実体の reconcile 本体（doc 53 §12、旧 lane_reconcile）
pub(crate) mod reconcile;
/// LaneCmd を recv して Semaphore で gate しつつ spawn（旧 lane_spawn_actor）
pub(crate) mod spawn_actor;

// facade: 呼び手は `crate::repo::lane::LaneAddress` の形（`conversation/mod.rs` と同じ）。外から使う item だけ。
// 残り（LaneSessionsView / Diff は `lane::info::`、SlotInfo 等は `lane::pool::`）は module path で引く。
pub use address::{LaneAddress, LaneId, ROOT_LANE_NAME};
pub use info::{LaneInfo, LaneLifecycle, LaneState, SystemEvent};
pub use pool::{LanePool, ResolvedSession, deliver_nudge, idle_teardown_after_minutes};
