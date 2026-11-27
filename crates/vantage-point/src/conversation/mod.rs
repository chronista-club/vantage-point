//! conversation — AI との会話層（engine 軸 × Mode(surface) 軸の直交格子）
//!
//! doc 37（当時の名: Echoes）: 「コーディングアシスタント」という能力の namespace。その中に
//! - **engine 軸**（どの頭脳か: claude / codex / grok / opencode …）= session に束縛される identity
//! - **Mode(surface) 軸**（どう視るか: tui 端末 / gui chat GUI）= 切替可能な view
//!
//! の直交 2 軸がある。本 module は gui のバックエンド（repo 側）+ engine 軸の語彙を持つ。
//! tui は raw PTY（`process::agent_spawner` の slot + CLI 注入）で、本 module を通らない。
//!
//! 設計 SSOT: `docs/design/37-echoes-two-axes.md`（2 軸）/ `32-echoes-act2-gui.md`（gui）。
//!
//! ## モジュール構成
//! - [`event`]: GUI 語彙 [`ConversationEvent`]（engine 非依存 — 全 engine をこの共通面に翻訳する）
//! - [`engine`]: engine 軸の語彙 [`EngineKind`] と chat engine 所有型 [`ChatHost`] / [`ChatEngineSlot`]
//! - [`host`]: [`ClaudeHost`] — headless claude を lane 単位で**常駐**駆動（stream-json stdin 連投）
//! - [`codex_rpc_translate`] + [`codex_host`]: codex — **常駐 RpcHost**（`codex app-server`
//!   JSONL JSON-RPC、doc 41）
//! - [`acp_host`]: grok / opencode / agy / muse — **常駐 AcpAgentHost**
//!   （`grok agent stdio` = ACP、doc 42 / `opencode acp` = 同 ACP、doc 43 / `agy acp` = 同 ACP、
//!   doc 88 / `muse acp` = 同 ACP、doc 91。engine 差分は [`AcpEngine`] に集約）。接続・翻訳・設定面・履歴の控えは共有 crate
//!   `agent-chat-acp` が持ち、ここは VP 固有の注入だけ
//! - [`replay_source`]: 履歴を host から受け取って配り直す口（ACP engine — pump が使う）
//! - [`translate`] / [`transcript`]: claude stream / transcript → [`ConversationEvent`] 翻訳層
//! - [`replay_log`]: 履歴を自分で持たない engine（vpcode）の per-session 会話ログ
//!   （disk 永続、gui の replay 源）。claude は transcript、codex は app-server 履歴、
//!   ACP engine（grok / opencode / agy / muse）は engine の `session/load` が SSOT なので使わない
//!
//! 対応 engine は**常駐型のみの一枚岩**（doc 39 §7: claude / codex / grok / opencode / agy / muse — doc 41・42・43・88・91）。
//! 旧 turn-scoped 系（TurnHost / cursor_host / cursor_translate）は step 4 で撤去済み、
//! cursor は sweep 6.5 で agent ごと完全撤去（再導入時は新規実装 — 旧実装は
//! git history #773/#776。Composer の CLI 進化待ちの再検討方針は doc 39 §7）。
//!
//! Unison 配信は `process::conversation_pump`（terminal_pump と同型）が担う。GUI 語彙 [`ConversationEvent`] は
//! engine 非依存なので、新 engine は「翻訳器 + 常駐 host」を足すだけで乗る
//! （chatview / topic 配線は無改修）。

pub mod acp_config;
pub mod acp_host;
pub mod agy_settings;
pub mod claude_settings;
pub mod claude_translate;
mod codex_async_questions;
mod codex_question_session;
pub mod muse_settings;
pub(crate) use codex_question_session::CodexQuestionSession;
mod codex_history;
pub mod codex_host;
mod codex_interactions;
pub mod codex_rpc_translate;
pub mod codex_settings;
pub mod engine;
pub mod event;
pub mod grok_settings;
pub mod host;
pub mod jsonl_store;
pub mod progress;
pub mod replay_log;
pub mod replay_source;
pub mod settings;
pub mod skill_docs;
pub mod transcript;
pub mod vpcode_catalog;
pub mod vpcode_host;
pub mod vpcode_transcript;

pub use acp_host::{AcpAgentHost, AcpConfigHandle, AcpEngine, AcpHostConfig};
pub use claude_translate::ClaudeTranslator;
pub use codex_host::{CodexAgentHost, CodexRpcHostConfig};
pub use engine::{ChatEngineSlot, ChatHost, EngineKind};
pub use event::{ConversationEvent, PlanEntry, QuestionOption, QuestionSpec};
pub use host::{ClaudeHost, ClaudeHostConfig, ImageInput, InFlight, PermissionDecision};
pub use settings::EngineSettings;
pub use vpcode_host::{VpcodeHost, VpcodeHostConfig};
