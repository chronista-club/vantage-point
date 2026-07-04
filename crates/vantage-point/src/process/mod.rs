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
/// CC activity poll — `claude agents --json` の LaneActivity 供給 (R3-a / Phase A)
pub(crate) mod cc_activity;
/// Agent 委譲 (delegation) — durable cross-agent future の v1 ローカル atom (doc 28 §4)
pub(crate) mod delegation;
/// wire delivery loop — 未 ack command の lane nudge + 再掲示 (R2-b、 TheWorld 常駐)
pub(crate) mod delivery_actor;
pub(crate) mod hub;
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
pub mod process_runner;
/// Project scope の Stand pool (PP / GE / HP)
pub(crate) mod project_stands_state;
pub mod pty;
pub(crate) mod retained;
// L0 portless B-4 (wire-unison): daemon の "wire" channel handler が
// `routes::wire` / `routes::delegation` の dispatch fn を呼ぶため crate 可視に格上げ。
pub(crate) mod routes;
mod server;
mod session;
/// StandSpawner — Stand 名 → 床 (login shell) + claude 注入の spawn command 構築 (tmux decoupling PR2)
pub(crate) mod stand_spawner;
pub(crate) mod state;
pub(crate) mod terminal_pump;
pub mod topic;
pub(crate) mod topic_router;
pub(crate) mod unison_server;
pub(crate) mod world_wire;

pub use capabilities::CapabilityConfig;
pub use server::{run, run_world};
