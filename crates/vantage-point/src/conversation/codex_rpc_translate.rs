//! codex app-server notification → [`super::event::ConversationEvent`] の item 層翻訳。
//!
//! 実装は共有 `agent-chat-host`（design 86）。MCP の tool 名は `mcp__<server>__<tool>`、
//! 呼び出し結果で `McpStatus` も出す。lifecycle（SessionInit / TurnCompleted / Error）は host 側。
pub use agent_chat_host::codex_rpc_translate::CodexRpcTranslator;
