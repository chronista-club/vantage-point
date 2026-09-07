//! Protocol module — GUI / CLI と repo runtime の間を流れる wire 型。
//!
//! 現行の中身は `messages`（`RepoMessage` と board / chat の部品型）のみ。
//! 旧 AG-UI / ACP / Vantage 拡張（`ProtocolMessage` / `ToAgUi` / `ToAcp` / `VantageEvent`）は
//! 消費者が実体化されないまま dead だったため 2026-09 に撤去（docs/archive/03, 04 が
//! AG-UI 未採用を記録）。会話の wire 型は `crate::conversation::ConversationEvent`。

pub mod messages;

pub use messages::{
    BoardItem, BrowserMessage, ChatComponent, ChatMessage, ChatRole, ComponentAction, Content,
    HistoryMessage, RepoMessage, SessionInfo, SplitDirection,
};
