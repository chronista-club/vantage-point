//! EchoesEvent — Echoes Act II GUI が話す唯一の言葉（PR1 で凍結）
//!
//! vp-app（GUI）はこの語彙だけを描画する。engine（現状 claude）ごとの
//! stream 形式は SP 側の翻訳層（[`super::translate`]）で吸収し、engine を
//! 足すときは翻訳層を 1 個追加するだけで GUI は無改修 — これが多 engine 方針の支え。
//!
//! 語彙は ACP `session/update` の実績あるサブセットを借用。
//! 由来のマッピングは design doc 32 §4 / §10（Step 0 実測スキーマ）を参照。

use serde::{Deserialize, Serialize};

/// GUI へ配信する構造化イベント（1 engine turn = 複数 EchoesEvent の列）。
///
/// serde 表現は `{"kind":"message_chunk","text":"..."}` の形（`tag = "kind"`）。
/// vp-app 側はこの `kind` で分岐して描画する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EchoesEvent {
    /// セッション初期化。engine プロセス起動直後に 1 回。
    /// session_id は cc_session への記録に使う（Act I ⇄ II の resume 共有）。
    SessionInit {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mcp_servers: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        slash_commands: Vec<String>,
    },

    /// 本文テキストの増分（1 token 前後）。GUI は末尾に append。
    MessageChunk { text: String },

    /// thinking の増分。GUI は折りたたみ領域に append。
    ThoughtChunk { text: String },

    /// tool 呼び出し（完全 input 確定時に発火）。
    /// `input` は tool ごとに形が違う生 JSON（Bash なら `{command,description}` 等）。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// tool の実行結果（`tool_use_id` で [`EchoesEvent::ToolCall`] と対応）。
    ToolCallUpdate {
        tool_use_id: String,
        /// 結果本文（text 化。大きい場合は翻訳層で切り詰め得る）。
        content: String,
        #[serde(default)]
        is_error: bool,
    },

    /// plan（TodoWrite の input から導出）。plan ウィジェット用。
    Plan { entries: Vec<PlanEntry> },

    /// turn 終了。diff 集計トリガ + コスト表示。
    TurnCompleted {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },

    /// engine / 翻訳層のエラー。
    Error { message: String },
    // NOTE: permission_request（control protocol / can_use_tool）は MVP 非対象。
    //       acceptEdits で回避する（doc 32 §10.1）。将来 control protocol ごと実装。
}

/// plan の 1 項目（TodoWrite の todo に対応）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    /// "pending" | "in_progress" | "completed"（claude の status をそのまま運ぶ）。
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}
