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
/// Lane subcommand types (LaneCmd) — Mailbox actor 経由の Lane 操作 Cmd (I-b、 2026-04-30)
pub(crate) mod lane_cmd;
/// Lane spawn actor — `LaneCmd` を recv して Semaphore で gate しつつ Lane を spawn (I-b、 2026-04-30)
pub(crate) mod lane_spawn_actor;
/// Lane state types (LaneAddress / LaneStand / LanePool 等) — Lane scope の data model
pub(crate) mod lanes_state;
pub mod process_runner;
/// Project scope の Stand pool (PP / GE / HP)
pub(crate) mod project_stands_state;
pub mod pty;
pub(crate) mod retained;
mod routes;
mod server;
mod session;
/// StandSpawner — LaneStand 別の spawn command 構築 (Architecture v4 A5-1)
pub(crate) mod stand_spawner;
/// LaneStandSpec trait — 舞台-役者-演目 metaphor の Layer 2 (Phase 6-E、 VP-107)
pub(crate) mod stand_spec;
pub(crate) mod state;
pub(crate) mod tmux_actor;
pub mod topic;
pub(crate) mod topic_router;
pub(crate) mod unison_server;

pub use capabilities::CapabilityConfig;
pub use server::{run, run_world};
