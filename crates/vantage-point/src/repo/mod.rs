//! Process module - AI Agent server (HTTP + WebSocket hub)
//!
//! Process はrepoの開発プロセスを表す本体。
//! 各種能力（Capability / Agent）を保持し、ユーザーの開発を支援する。
//!
//! ## 構成
//! - **Process**: サーバー（開発プロセス本体）
//! - **Point**: WebView（視点/観測点）
//! - **Capability**: repo が持つ能力（現行は repo_manager / update。旧 Agent / Protocol は 2026-09 撤去、MIDI は daemon の DeviceRegistry）

/// AgentSpawner — agent 名 → slot (login shell) + claude 注入の spawn command 構築 (tmux decoupling PR2)
pub(crate) mod agent_spawner;
/// agent discovery — built-in の agent 静的 table + `agents_list` handler（7b で routes/ から）
pub(crate) mod agents;
/// board — scope 別の永続 board（show / board_* の owner、doc 52 / doc 61）
pub(crate) mod board;
/// CC activity poll — `claude agents --json` の LaneActivity 供給 (R3-a / Phase A)
pub(crate) mod cc_activity;
/// conversation ops — 会話系 Unison method の handler（owner は lane::state / conversation::engine、doc 61）
pub(crate) mod conversation_ops;
/// Lane conversation pump — ClaudeHost の ConversationEvent を per-lane topic に route (doc 30、gui)
pub(crate) mod conversation_pump;
/// conversation replay — attach 時の会話配り直しと demand の合流（doc 32 §3、doc 61）
pub(crate) mod conversation_replay;
pub(crate) mod daemon_wire;
/// Agent 委譲 (delegation) — durable cross-agent future の v1 ローカル atom (doc 28 §4)
pub(crate) mod delegation;
/// wire delivery loop — 未 ack command の lane nudge + 再掲示 (R2-b、 daemon 常駐)
pub(crate) mod delivery_actor;
/// Editor bridge — MCP → GUI Editor Mode / layout の request-response（doc 48 Phase 2 / doc 49 LE-15、doc 61）
pub(crate) mod editor_bridge;
/// HTTP（axum）の route handler — health / shutdown / update だけ。Router は `server.rs`（7b で routes/ から）
pub(crate) mod http;
pub(crate) mod hub;
/// lane — repo 側の lane runtime（値型 / LanePool / reconcile / lifecycle / spawn actor / Unison handler）。identity と
/// registry の SSOT は `crate::lane`（doc 61 §1、項目 9-1）
pub(crate) mod lane;
/// process ops — file watch / process runner / ruby の Unison method handler（doc 61）
pub(crate) mod process_ops;
pub mod process_runner;
/// Repo scope の Agent pool (board / runner ほか — 現在は縮退済)
pub(crate) mod repo_registry;
pub(crate) mod retained;
mod server;
pub(crate) mod state;
/// terminal ops — terminal demand / write / resize の Unison method handler + reconcile の収束点（doc 61）
pub(crate) mod terminal_ops;
pub(crate) mod terminal_pump;
pub mod topic;
pub(crate) mod topic_router;
pub(crate) mod unison_server;
/// wire relay — wiremsg の repo 側 proxy（アドレス正規化 → daemon relay、R2-a / doc 61）
pub(crate) mod wire_relay;

// doc 44 P1 (fold-in): `run`（repo プロセスとしての実行）は退役。repo は daemon の
// `run_daemon` が in-process で起こす（`RepoRuntimes::start` → `start_repo`）。
pub use server::run_daemon;
