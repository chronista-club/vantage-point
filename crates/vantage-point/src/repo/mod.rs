//! Process module - AI Agent server (HTTP + WebSocket hub)
//!
//! Process はrepoの開発プロセスを表す本体。
//! 各種能力（Capability / Agent）を保持し、ユーザーの開発を支援する。
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
/// wire delivery loop — 未 ack command の lane nudge + 再掲示 (R2-b、 daemon 常駐)
pub(crate) mod delivery_actor;
/// Lane echoes pump — EchoesAgentHost の EchoesEvent を per-lane topic に route (doc 30、gui)
pub(crate) mod echoes_pump;
pub(crate) mod hub;
/// Lane 階層 Agent container (LSCM doc 12 §9 / doc 13 §3、 PR-β-1 受け皿、 VP-119)
pub(crate) mod lane_capabilities;
/// Lane subcommand types (LaneCmd) — Mailbox actor 経由の Lane 操作 Cmd (I-b、 2026-04-30)
pub(crate) mod lane_cmd;
/// lane の実体（PtySlot / chat engine / 代表値）を intent（registry）に合わせる reconcile 本体（doc 53 §12）
pub(crate) mod lane_reconcile;
/// Lane spawn actor — `LaneCmd` を recv して Semaphore で gate しつつ Lane を spawn (I-b、 2026-04-30)
pub(crate) mod lane_spawn_actor;
/// Lane に host される Stand の minimal marker trait + Registry (PR-δ-1、 VP-135)
pub(crate) mod lane_stand;
/// Lane state types (LaneAddress / LanePool 等) — Lane scope の data model
pub(crate) mod lanes_state;
pub mod process_runner;
/// Repo scope の Agent pool (board / runner ほか — 現在は縮退済)
pub(crate) mod repo_registry;
pub(crate) mod repo_stands_state;
pub(crate) mod retained;
// L0 portless B-4 (wire-unison): daemon の "wire" channel handler が
// `routes::wire` / `routes::delegation` の dispatch fn を呼ぶため crate 可視に格上げ。
/// StandSpawner — Stand 名 → slot (login shell) + claude 注入の spawn command 構築 (tmux decoupling PR2)
pub(crate) mod agent_spawner;
pub(crate) mod daemon_wire;
pub(crate) mod routes;
mod server;
pub(crate) mod state;
pub(crate) mod terminal_pump;
pub mod topic;
pub(crate) mod topic_router;
pub(crate) mod unison_server;

pub use capabilities::CapabilityConfig;
// doc 44 P1 (fold-in): `run`（repo プロセスとしての実行）は退役。repo は daemon の
// `run_daemon` が in-process で起こす（`RepoRuntimes::start` → `start_repo`）。
pub use server::run_daemon;
