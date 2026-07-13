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

    /// transcript replay の開始マーカー（attach 時に 1 回、以降の event 列が過去会話）。
    ///
    /// GUI は受信時に当該 lane の会話表示を **クリア**してから後続を fold する。
    /// これで replay は冪等になる: backend は「新規 attach」と「reconnect / demand 再発火」を
    /// 区別できないため（terminal replay の clear-prefix と同じ問題）、単純追記だと再接続の
    /// たび会話が二重化する。 reset してから描き直すことで、cold-start でも reconnect でも
    /// 同一の最終状態に収束する。
    ReplayStart,

    /// user 自身の発話（transcript replay 専用）。
    ///
    /// live 経路では ChatView が submit 時に optimistic に user bubble を足すため発火しない。
    /// replay では過去の user turn を再現する手段が他に無いので本 variant で運ぶ。
    /// engine 非依存（どの engine にも user turn がある）なので語彙原則には反しない。
    UserMessage { text: String },

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

    /// turn 終了。diff 集計トリガ + コスト表示 + context ゲージ更新。
    ///
    /// context_* は Act I statusline（cc-status の `bar :context`）と同じ意味論:
    /// tokens = turn 最後の assistant usage の合算（input + cache_read + cache_creation）、
    /// window = `result.modelUsage[*].contextWindow`。engine / 版が値を運ばなければ None
    /// （GUI はゲージを出さないだけ）。凍結語彙への additive optional なので後方互換。
    TurnCompleted {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// 現在の会話が占める context tokens（ゲージの分子）。
        #[serde(skip_serializing_if = "Option::is_none")]
        context_tokens: Option<u64>,
        /// モデルの context window 総量（ゲージの分母）。
        #[serde(skip_serializing_if = "Option::is_none")]
        context_window: Option<u64>,
    },

    /// engine / 翻訳層のエラー。
    Error { message: String },

    /// clarifying question（AskUserQuestion の逆方向 `can_use_tool` 横取り、doc 35 PR1）。
    ///
    /// engine が turn を pause して人間の選択を待つ HITL 面。GUI は PromptCard（選択肢）で描き、
    /// 回答を `request_id` 付きで戻すと host が `control_response` を stdin へ書いて turn が継続する。
    /// **transcript には commit されない**（control 面 = 会話本文ではない）ため in-flight tail には
    /// 積まない（[`super::host`] module doc の ⚠️）。
    Question {
        /// `control_response` の request_id マッチング用（回答時に GUI から戻す）。
        request_id: String,
        /// AskUserQuestion input の questions（1〜4 質問 × 2〜4 択、multiSelect 含む）。
        questions: Vec<QuestionSpec>,
    },

    /// tool 承認要求（非 AskUserQuestion の逆方向 `can_use_tool`、doc 35 PR3）。
    ///
    /// `permission-mode=default`（GUI から opt-in）に切替えた時、Write/Bash 等の tool 実行前に
    /// engine が turn を pause してここへ来る。GUI は PromptCard（allow/deny）で描き、`request_id`
    /// 付きで behavior を戻すと host が `control_response` を書いて実行 or 回避する。Question と同じ
    /// レール（pending + respond_permission）、種別だけ違う。
    PermissionRequest {
        /// `control_response` の request_id マッチング用。
        request_id: String,
        /// 承認対象 tool 名（Write / Bash / …）。
        tool_name: String,
        /// tool の原 input（GUI が要約表示、allow 時は verbatim echo）。
        input: serde_json::Value,
    },
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

/// AskUserQuestion の 1 質問（doc 35 §3 / §8 の生 wire を型化）。
///
/// 由来は claude の逆方向 `can_use_tool` input（`{questions:[{question,header,options,multiSelect}]}`、
/// camelCase）。[`super::host`] の control frame 翻訳が本型（GUI 語彙 = snake_case）へ写す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionSpec {
    /// 質問文（answers map のキーにもなる）。
    pub question: String,
    /// 短い見出し（PromptCard のラベル）。
    pub header: String,
    /// 選択肢（2〜4 択）。
    pub options: Vec<QuestionOption>,
    /// true = 複数選択（GUI は確定ボタンで束ねる）。
    #[serde(default)]
    pub multi_select: bool,
}

/// 質問の 1 選択肢。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// 回答値（answers に詰まる label）。
    pub label: String,
    /// 補足説明（無い場合は空）。
    #[serde(default)]
    pub description: String,
}
