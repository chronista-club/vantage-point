//! Echoes 💬 — コーディングアシスタント Stand（engine 軸 × Act(surface) 軸の直交格子）
//!
//! doc 37: Echoes は「コーディングアシスタント」という能力の namespace。その中に
//! - **engine 軸**（どの頭脳か: claude / cursor / codex / agy …）= session に束縛される identity
//! - **Act(surface) 軸**（どう視るか: Act I 端末 / Act II chat GUI）= 切替可能な view
//!
//! の直交 2 軸がある。本 module は Act II のバックエンド（SP 側）+ engine 軸の語彙を持つ。
//! Act I は raw PTY（`process::stand_spawner` の床 + CLI 注入）で、本 module を通らない。
//!
//! 設計 SSOT: `docs/design/37-echoes-two-axes.md`（2 軸）/ `32-echoes-act2-gui.md`（Act II）。
//!
//! ## モジュール構成
//! - [`event`]: GUI 語彙 [`EchoesEvent`]（engine 非依存 — 全 engine をこの共通面に翻訳する）
//! - [`engine`]: engine 軸の語彙 [`EngineKind`] と chat engine 所有型 [`ChatHost`] / [`ChatEngineSlot`]
//! - [`host`]: [`EchoesAgentHost`] — headless claude を lane 単位で**常駐**駆動（stream-json stdin 連投）
//! - [`codex_rpc_translate`] + [`codex_host`]: codex — **常駐 RpcHost**（`codex app-server`
//!   JSONL JSON-RPC、doc 41）
//! - [`acp_translate`] + [`acp_host`]: grok — **常駐 AcpAgentHost**（`grok agent stdio` = ACP、doc 42）
//! - [`translate`] / [`transcript`]: claude stream / transcript → [`EchoesEvent`] 翻訳層
//! - [`replay_log`]: transcript を持たない engine（codex / grok）の per-session 会話ログ（disk 永続、
//!   Act II の replay 源）。claude は transcript が SSOT なので使わない
//!
//! 対応 engine は**常駐型のみの一枚岩**（doc 39 §7: claude / codex / grok — doc 41・42）。
//! 旧 turn-scoped 系（TurnHost / cursor_host / cursor_translate）は step 4 で撤去済み —
//! cursor は Act I（床）のみ、Act II はオミット（Composer 2.5 の CLI 進化待ちで再検討）。
//!
//! Unison 配信は `process::echoes_pump`（terminal_pump と同型）が担う。GUI 語彙 [`EchoesEvent`] は
//! engine 非依存なので、新 engine は「翻訳器 + 常駐 host」を足すだけで乗る
//! （chatview / topic 配線は無改修）。

pub mod acp_host;
pub mod acp_translate;
pub mod codex_host;
pub mod codex_rpc_translate;
pub mod engine;
pub mod event;
pub mod host;
pub mod replay_log;
pub mod transcript;
pub mod translate;

pub use acp_host::{AcpAgentHost, AcpHostConfig};
pub use codex_host::{CodexAgentHost, CodexRpcHostConfig};
pub use engine::{ChatEngineSlot, ChatHost, EngineKind};
pub use event::{EchoesEvent, PlanEntry, QuestionOption, QuestionSpec};
pub use host::{EchoesAgentHost, EchoesHostConfig, InFlight, PermissionDecision};
pub use translate::EchoesTranslator;
