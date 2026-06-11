//! Process module - AI Agent server (HTTP + WebSocket hub)
//!
//! Process はプロジェクトの開発プロセスを表す本体。
//! JoJo の Stand（能力）を保持し、ユーザーの開発を支援する。
//!
//! ## 構成
//! - **Process**: サーバー（開発プロセス本体）
//! - **Point**: WebView（視点/観測点）
//! - **Capability**: Process が持つ能力（Agent, MIDI, Protocol等）

pub mod capabilities;
pub(crate) mod hub;
/// VP install root の runtime 解決 (doc 11 PR-D / Z 系統)
/// wire delivery loop — 未 ack command の tmux nudge + 再掲示 (R2-b、 TheWorld 常駐)
pub(crate) mod delivery_actor;
pub(crate) mod install_root;
/// Lane 階層 Stand container (LSCM doc 12 §9 / doc 13 §3、 PR-β-1 受け皿、 VP-119)
pub(crate) mod lane_capabilities;
/// Lane subcommand types (LaneCmd) — Mailbox actor 経由の Lane 操作 Cmd (I-b、 2026-04-30)
pub(crate) mod lane_cmd;
/// Lane spawn actor — `LaneCmd` を recv して Semaphore で gate しつつ Lane を spawn (I-b、 2026-04-30)
pub(crate) mod lane_spawn_actor;
/// Lane に host される Stand の minimal marker trait + Registry (PR-δ-1、 VP-135)
pub(crate) mod lane_stand;
/// Lane state types (LaneAddress / LanePool 等) — Lane scope の data model
pub(crate) mod lanes_state;
/// Notification bridge actor — `notify` mailbox から DistributedNotification 配信 (VP-159 PR-3、 VP-24 inline 実装 を struct 化)
pub(crate) mod notification_actor;
pub mod process_runner;
/// Project scope の Stand pool (PP / GE / HP)
pub(crate) mod project_stands_state;
pub mod pty;
pub(crate) mod retained;
mod routes;
mod server;
mod session;
/// Stand metadata reader — `.mise/tasks/vp/stand/{name}` 冒頭の `#VP key=value` を parse (VP-108)
pub(crate) mod stand_metadata;
/// StandSpawner — Stand 名 → mise task spawn command 構築 (doc 11 PR-B)
pub(crate) mod stand_spawner;
// stand_spec module は doc 11 PR-B で削除 (LaneStandSpec trait / TheHand / LlmStand 全廃、
// `mise run vp:stand:{name}` 1 経路に集約)。
pub(crate) mod state;
pub(crate) mod tmux_actor;
pub mod topic;
pub(crate) mod topic_router;
pub(crate) mod unison_server;
pub(crate) mod world_wire;

pub use capabilities::CapabilityConfig;
pub use server::{run, run_world};
