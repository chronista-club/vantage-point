//! engine 軸の語彙（[`EngineKind`]）と gui chat engine の所有型（[`ChatHost`] / [`ChatEngineSlot`]）
//!
//! doc 37 §1（当時の名: Echoes）: **engine 軸 × Mode(surface) 軸**の直交格子で、engine = session に束縛される
//! identity、Mode = 切替可能な view。本 module は engine 軸の SSOT — agent 名 ↔ engine の対応と
//! 能力表明（chat 対応 / model 切替対応）をここに一元化し、`agent == "cursor"` のような
//! stringly 比較の散在（旧 lanes_state / unison_server / agent_spawner の 4 箇所）を畳む。
//!
//! [`ChatHost`] / [`ChatEngineSlot`] は旧 `lanes_state.rs` から移設（doc 33 の chat engine 所有を
//! conversation module に閉じ、chat スタック全体を他repo（GFP 等）へ切り出せる形にする）。

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use super::acp_host::AcpAgentHost;
use super::codex_host::CodexAgentHost;
use super::event::ConversationEvent;
use super::host::{ClaudeHost, InFlight, PermissionDecision};

/// engine 軸の語彙（どの頭脳か）。agent 名から導く。
///
/// - agent は DB / wire を流れる自由文字列（入口 allowlist なし）なので、engine 判定は必ず
///   [`EngineKind::from_agent`] を通す — 対応表をここ 1 箇所に閉じる。
/// - `None` = engine を持たない agent（`"shell"` / 退役 `"tmux"` / 未知名）。shell（login shell）のみ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// agent=`"claude"` — claude。常駐 stream-json host（gui）+ claude TUI（tui）。
    Claude,
    /// agent=`"codex"` — OpenAI Codex CLI。常駐 RpcHost（gui、`codex app-server` — doc 41）
    /// + codex TUI（tui）。
    Codex,
    /// agent=`"grok"` — xAI Grok CLI。常駐 AcpAgentHost（gui、`grok agent stdio` = ACP —
    /// doc 42）+ grok TUI（tui、`-r` resume）。
    Grok,
    /// agent=`"opencode"` — opencode。常駐 AcpAgentHost（gui、`opencode acp` = grok と同 ACP —
    /// doc 43）+ opencode TUI（tui、`-s <id>` resume）。model / provider は opencode config が
    /// SSOT（local LLM 等 — VP は注入しない、doc 43 §3）。
    OpenCode,
}

impl EngineKind {
    /// 全 engine の列挙（`list_agents` 等の導出元）。
    ///
    /// 新 engine は [`Self::from_agent`] / [`Self::agent_name`] と併せてここにも足す —
    /// roundtrip テストが片側だけの追加（= GUI dropdown からの取りこぼし、moody 指摘）を
    /// コンパイル時 match 網羅性 + テストで検知する。
    pub const ALL: [EngineKind; 4] = [Self::Claude, Self::Codex, Self::Grok, Self::OpenCode];

    /// agent 名 → engine。対応表の SSOT（新 engine はここに 1 行足す）。
    ///
    /// 撤去済み engine（`"cursor"` / `"agy"` — sweep 6.5、doc 39 §7）は `None` に倒れる。
    /// disk / wire に残る旧 agent 文字列は shell（login shell）のみで graceful に受ける。
    pub fn from_agent(agent: &str) -> Option<Self> {
        match agent {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }

    /// canonical な agent 名（[`Self::from_agent`] の逆写像）。
    pub fn agent_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
        }
    }

    /// GUI（sidebar `+ Add Performer` dropdown 等）向けの表示説明。
    pub fn description(self) -> &'static str {
        match self {
            Self::Claude => "VP Agent: claude 💬 — tui slot（login shell）+ Claude CLI 自動起動",
            Self::Codex => "VP Agent: Codex 🧮 — tui slot（login shell）+ codex (OpenAI) 自動起動",
            Self::Grok => "VP Agent: Grok ⚡ — tui slot（login shell）+ grok (xAI) 自動起動",
            Self::OpenCode => {
                "VP Agent: OpenCode 🧩 — tui slot（login shell）+ opencode 自動起動（model は opencode config）"
            }
        }
    }

    /// gui（chat GUI）の host を持つか = console mode Chat を許すか。
    ///
    /// 常駐型のみ（doc 39 §7 の一枚岩: claude / codex / grok / opencode — doc 41・42・43）。
    /// 全 engine が chat 対応だが、engine を持たない agent（shell / 未知）は host を持たない。
    pub fn chat_capable(self) -> bool {
        matches!(
            self,
            Self::Claude | Self::Codex | Self::Grok | Self::OpenCode
        )
    }

    /// VP の model picker に出す選択肢（engine ごとの catalog — server が SSOT、client は
    /// 並べるだけ。mako 裁定 2026-07-27: model は成長し続け多 engine で頻度も増えるため、
    /// client の hardcode を廃してここに一元化）。
    ///
    /// **空 = VP からの model 切替なし**（client は picker を出さず、実測 model があれば
    /// read-only 表示に落とす）。切替可否の述語は別に持たない — この catalog の空/非空が
    /// 唯一の真実（旧 `model_switchable` は撤去）。
    ///
    /// codex は CLI 側に model 選択があるため v1 は空（`-m` 解禁 = catalog を足すだけ、PR-2）。
    /// grok も同様。opencode は model / provider を opencode config が管理するため注入しない
    /// （doc 43 §3 — 二重管理で SSOT が割れるのを避ける）。
    pub fn model_choices(self) -> Vec<Choice> {
        match self {
            // value = `--model` に渡る id。空文字 = 「engine 既定」（`--model` を注入しない）。
            // 各系列の最新のみを載せる（2026-07-27 に claude-api skill の model catalog と照合。
            // Opus 4.8→5 は mako 裁定）。Haiku は date suffix 付き full id でなく **alias** —
            // alias は系列の最新を指し続けるので catalog が古びにくい。
            Self::Claude => Choice::list(&[
                ("", "Default"),
                ("claude-fable-5", "Fable 5"),
                ("claude-opus-5", "Opus 5"),
                ("claude-sonnet-5", "Sonnet 5"),
                ("claude-haiku-4-5", "Haiku 4.5"),
            ]),
            Self::Codex | Self::Grok | Self::OpenCode => Vec::new(),
        }
    }

    /// permission picker の選択肢（表記は claude TUI と同一の英語 — mako 裁定 2026-07-27）。
    ///
    /// **空 = 対話承認の概念なし**（[`ChatHost::set_permission_mode`] が bail する engine。
    /// client は picker を出さない）。codex の approval_policy / sandbox のような別語彙の
    /// 承認体系を将来 VP から触る場合も、その engine の catalog を足すだけで受かる。
    ///
    /// claude の集合と表記は TUI の shift+tab cycle 準拠（CC v2.1.200 で `default` の表示名は
    /// `manual` へ改名 — wire 値は `default` のまま両受理なので互換側を送る。順序も cycle と
    /// 同じ manual → accept edits → plan）。cycle 外の `dontAsk` と account 条件付きの `auto` は
    /// 出さない（押しても効かない選択肢を並べない — 解禁は catalog に足すだけ）。
    pub fn permission_choices(self) -> Vec<Choice> {
        match self {
            Self::Claude => Choice::list(&[
                ("default", "manual"),
                ("acceptEdits", "accept edits"),
                ("plan", "plan mode"),
                ("bypassPermissions", "bypass permissions"),
            ]),
            Self::Codex | Self::Grok | Self::OpenCode => Vec::new(),
        }
    }
}

/// picker の選択肢 1 件（server が能力として表明する wire 型 — client は並べるだけ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// engine に渡る値（model id / permission mode。model の空文字 = engine 既定）。
    pub value: String,
    /// 表示ラベル。
    pub label: String,
}

impl Choice {
    /// (value, label) の静的表から catalog を組む。
    fn list(pairs: &[(&str, &str)]) -> Vec<Choice> {
        pairs
            .iter()
            .map(|(v, l)| Choice {
                value: (*v).to_string(),
                label: (*l).to_string(),
            })
            .collect()
    }
}

/// chat engine の 1 スロット（host と、その ConversationEvent を topic に流す pump）。
///
/// drop = engine 停止（host teardown + pump abort）。
pub struct ChatEngineSlot {
    pub host: ChatHost,
    pub pump: JoinHandle<()>,
}

impl Drop for ChatEngineSlot {
    fn drop(&mut self) {
        // engine 停止（codex は app-server kill、claude は Child kill_on_drop に委ねる）。
        self.host.stop();
        // pump は broadcast Closed で自然終了するが、即時性のため明示 abort する。
        self.pump.abort();
    }
}

/// gui の chat engine host（engine ごとに turn 駆動が違う enum、Pre-MVP は dyn 抽象を作らない）。
///
/// - [`ChatHost::Claude`]: 常駐 stream-json host（stdin 連投、1 プロセスが会話を保持）。
/// - [`ChatHost::Codex`]: 常駐 JSONL JSON-RPC host（`codex app-server`、doc 41）。
/// - [`ChatHost::Grok`]: 常駐 ACP host（`grok agent stdio`、doc 42）。
/// - [`ChatHost::OpenCode`]: 常駐 ACP host（`opencode acp`、doc 43。grok と同じ AcpAgentHost で、
///   [`AcpEngine`] パラメタだけが違う）。
///
/// GUI 語彙 [`ConversationEvent`] は全 engine 共通なので、pump / topic 配線・chatview は engine
/// 非依存のまま。variant 名は engine 名（doc 37 当時の語彙: Echoes = namespace、claude = engine —
/// 旧 `Echoes` variant の二重意味をここで清算）。
pub enum ChatHost {
    Claude(ClaudeHost),
    Codex(CodexAgentHost),
    Grok(AcpAgentHost),
    OpenCode(AcpAgentHost),
}

impl ChatHost {
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ConversationEvent> {
        match self {
            ChatHost::Claude(h) => h.subscribe(),
            ChatHost::Codex(h) => h.subscribe(),
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.subscribe(),
        }
    }

    pub fn in_flight(&self) -> InFlight {
        match self {
            ChatHost::Claude(h) => h.in_flight(),
            ChatHost::Codex(h) => h.in_flight(),
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.in_flight(),
        }
    }

    pub fn commit_seq(&self) -> u64 {
        match self {
            ChatHost::Claude(h) => h.commit_seq(),
            ChatHost::Codex(h) => h.commit_seq(),
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.commit_seq(),
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            ChatHost::Claude(h) => h.pid(),
            ChatHost::Codex(h) => h.pid(),
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.pid(),
        }
    }

    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.submit(prompt).await,
            ChatHost::Codex(h) => h.submit(prompt).await,
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.submit(prompt).await,
        }
    }

    pub async fn interrupt(&self) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.interrupt().await,
            ChatHost::Codex(h) => h.interrupt().await,
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.interrupt().await,
        }
    }

    /// 明示 teardown（[`ChatEngineSlot`] Drop から呼ぶ）。codex / grok / opencode は host kill、
    /// claude は Child kill_on_drop に委ねる（host drop 時に停止）。
    pub fn stop(&mut self) {
        match self {
            // ClaudeHost の Child は kill_on_drop(true) なので host drop で停止する。
            ChatHost::Claude(_) => {}
            ChatHost::Codex(h) => h.stop(),
            ChatHost::Grok(h) | ChatHost::OpenCode(h) => h.stop(),
        }
    }

    /// 逆方向 permission への回答（control channel を持つのは claude のみ）。
    pub async fn respond_permission(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.respond_permission(request_id, decision).await,
            ChatHost::Codex(_) | ChatHost::Grok(_) | ChatHost::OpenCode(_) => {
                anyhow::bail!("このエンジンは対話承認/permission mode を持ちません")
            }
        }
    }

    /// permission mode の動的切替（claude のみ）。
    pub async fn set_permission_mode(&self, mode: &str) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.set_permission_mode(mode).await,
            ChatHost::Codex(_) | ChatHost::Grok(_) | ChatHost::OpenCode(_) => {
                anyhow::bail!("このエンジンは対話承認/permission mode を持ちません")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ALL ⇄ from_agent ⇄ agent_name の roundtrip（片側だけ足した engine を検知する防壁）。
    #[test]
    fn all_engines_roundtrip_through_stand_name() {
        for k in EngineKind::ALL {
            assert_eq!(
                EngineKind::from_agent(k.agent_name()),
                Some(k),
                "agent_name → from_agent が roundtrip しない: {k:?}"
            );
            assert!(!k.description().is_empty());
        }
    }

    /// agent 名 → engine 対応表の固定（drift 検知）。
    #[test]
    fn from_stand_maps_all_known_stands() {
        assert_eq!(EngineKind::from_agent("claude"), Some(EngineKind::Claude));
        // 旧名 "hd" の alias は命名エピック 6/9 で撤去（未知値として graceful に落ちる）
        assert_eq!(EngineKind::from_agent("hd"), None);
        assert_eq!(EngineKind::from_agent("codex"), Some(EngineKind::Codex));
        assert_eq!(EngineKind::from_agent("grok"), Some(EngineKind::Grok));
        assert_eq!(
            EngineKind::from_agent("opencode"),
            Some(EngineKind::OpenCode)
        );
        assert_eq!(EngineKind::from_agent("shell"), None);
        assert_eq!(
            EngineKind::from_agent("tmux"),
            None,
            "退役 agent は shell のみ"
        );
        assert_eq!(EngineKind::from_agent(""), None);
    }

    /// graceful degradation: 撤去済み engine（cursor / agy — sweep 6.5）の旧 agent 文字列は
    /// `None` に倒れる（disk / wire に残っても shell のみで受け、chat 不可・中立 chip）。
    #[test]
    fn removed_engines_degrade_to_none() {
        assert_eq!(EngineKind::from_agent("cursor"), None, "cursor は撤去済み");
        assert_eq!(EngineKind::from_agent("agy"), None, "agy は撤去済み");
    }

    /// 能力表: 全 engine が chat 対応。model / permission の catalog は claude のみ非空
    /// （空/非空が切替可否の唯一の真実 — 旧 `model_switchable` 述語は catalog に畳んだ）。
    #[test]
    fn capability_table() {
        assert!(EngineKind::Claude.chat_capable());
        assert!(EngineKind::Codex.chat_capable());
        assert!(EngineKind::Grok.chat_capable());
        assert!(EngineKind::OpenCode.chat_capable());

        assert!(!EngineKind::Claude.model_choices().is_empty());
        assert!(EngineKind::Grok.model_choices().is_empty());
        assert!(EngineKind::Codex.model_choices().is_empty());
        // opencode の model は opencode config が SSOT（VP は注入しない、doc 43 §3）。
        assert!(EngineKind::OpenCode.model_choices().is_empty());

        // permission mode は claude のみ（他 engine は ChatHost::set_permission_mode が bail）。
        // 表記は TUI と同一（v2.1.200 の manual 改名を反映）、wire 値は互換の "default"。
        let perms = EngineKind::Claude.permission_choices();
        assert_eq!(
            perms
                .iter()
                .map(|c| (c.value.as_str(), c.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("default", "manual"),
                ("acceptEdits", "accept edits"),
                ("plan", "plan mode"),
                ("bypassPermissions", "bypass permissions"),
            ],
        );
        assert!(EngineKind::Codex.permission_choices().is_empty());
        assert!(EngineKind::Grok.permission_choices().is_empty());
        assert!(EngineKind::OpenCode.permission_choices().is_empty());
    }
}
