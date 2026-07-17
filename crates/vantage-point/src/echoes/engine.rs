//! engine 軸の語彙（[`EngineKind`]）と Act II chat engine の所有型（[`ChatHost`] / [`ChatEngineSlot`]）
//!
//! doc 37 §1: Echoes は **engine 軸 × Act(surface) 軸**の直交格子で、engine = session に束縛される
//! identity、Act = 切替可能な view。本 module は engine 軸の SSOT — stand 名 ↔ engine の対応と
//! 能力表明（chat 対応 / model 切替対応）をここに一元化し、`stand == "cursor"` のような
//! stringly 比較の散在（旧 lanes_state / unison_server / stand_spawner の 4 箇所）を畳む。
//!
//! [`ChatHost`] / [`ChatEngineSlot`] は旧 `lanes_state.rs` から移設（doc 33 の chat engine 所有を
//! echoes module に閉じ、chat スタック全体を他プロジェクト（GFP 等）へ切り出せる形にする）。

use tokio::task::JoinHandle;

use super::codex_host::CodexAgentHost;
use super::event::EchoesEvent;
use super::host::{EchoesAgentHost, InFlight, PermissionDecision};

/// engine 軸の語彙（どの頭脳か）。stand 名から導く。
///
/// - stand は DB / wire を流れる自由文字列（入口 allowlist なし）なので、engine 判定は必ず
///   [`EngineKind::from_stand`] を通す — 対応表をここ 1 箇所に閉じる。
/// - `None` = engine を持たない stand（`"shell"` / 退役 `"tmux"` / 未知名）。床（login shell）のみ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// stand=`"echoes"`（+ 旧名 `"hd"`）— claude。常駐 stream-json host（Act II）+ claude TUI（Act I）。
    Claude,
    /// stand=`"cursor"` — cursor-agent。**Act I のみ**（Act II はオミット: Composer 2.5 の CLI
    /// 進化待ちで再検討 — doc 39 §7。旧 TurnHost は step 4 で撤去済み）。
    Cursor,
    /// stand=`"codex"` — OpenAI Codex CLI。常駐 RpcHost（Act II、`codex app-server` — doc 41）
    /// + codex TUI（Act I）。
    Codex,
    /// stand=`"agy"` — Google Antigravity CLI（Gemini CLI 後継）。**Act I のみ**
    /// （v1.1.2 に構造化出力が無く Act II 翻訳層が作れない、doc 37 §7.5）。
    Agy,
}

impl EngineKind {
    /// 全 engine の列挙（`list_stands` 等の導出元）。
    ///
    /// 新 engine は [`Self::from_stand`] / [`Self::stand_name`] と併せてここにも足す —
    /// roundtrip テストが片側だけの追加（= GUI dropdown からの取りこぼし、moody 指摘）を
    /// コンパイル時 match 網羅性 + テストで検知する。
    pub const ALL: [EngineKind; 4] = [Self::Claude, Self::Cursor, Self::Codex, Self::Agy];

    /// stand 名 → engine。対応表の SSOT（新 engine はここに 1 行足す）。
    pub fn from_stand(stand: &str) -> Option<Self> {
        match stand {
            "echoes" | "hd" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "codex" => Some(Self::Codex),
            "agy" => Some(Self::Agy),
            _ => None,
        }
    }

    /// canonical な stand 名（[`Self::from_stand`] の逆写像。旧名 `"hd"` は含まない）。
    pub fn stand_name(self) -> &'static str {
        match self {
            Self::Claude => "echoes",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }

    /// GUI（sidebar `+ Add Performer` dropdown 等）向けの表示説明。
    pub fn description(self) -> &'static str {
        match self {
            Self::Claude => "VP Stand: Echoes 💬 — login shell の床 + Claude CLI 自動起動",
            Self::Cursor => {
                "VP Stand: Cursor Agent 🖱️ — login shell の床 + cursor-agent 自動起動（console のみ、Act II 非対応）"
            }
            Self::Codex => "VP Stand: Codex 🧮 — login shell の床 + codex (OpenAI) 自動起動",
            Self::Agy => {
                "VP Stand: Antigravity 🚀 — login shell の床 + agy 自動起動（console のみ、Act II 非対応）"
            }
        }
    }

    /// Act II（chat GUI）の host を持つか = console mode Chat を許すか。
    ///
    /// 常駐型のみ（doc 39 §7 の一枚岩: claude / codex。grok=ACP は AcpHost 実装後に追加）。
    /// cursor は Act II オミット（step 4 で TurnHost 系ごと撤去）、agy は翻訳層が作れない。
    pub fn chat_capable(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }

    /// VP からの model 切替（`engine_model` 永続 + `--model` 注入）を受けるか。
    ///
    /// cursor / codex は CLI 側に model 選択があるため VP からは切替えない（cursor は TUI の
    /// `/model`、codex は `-m` を持つが v1 スコープ外 — doc 37 §7）。
    pub fn model_switchable(self) -> bool {
        matches!(self, Self::Claude)
    }
}

/// chat engine の 1 スロット（host と、その EchoesEvent を topic に流す pump）。
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

/// Act II の chat engine host（engine ごとに turn 駆動が違う enum、Pre-MVP は dyn 抽象を作らない）。
///
/// - [`ChatHost::Claude`]: 常駐 stream-json host（stdin 連投、1 プロセスが会話を保持）。
/// - [`ChatHost::Codex`]: 常駐 JSONL JSON-RPC host（`codex app-server`、doc 41）。
///
/// GUI 語彙 [`EchoesEvent`] は全 engine 共通なので、pump / topic 配線・chatview は engine
/// 非依存のまま。variant 名は engine 名（doc 37 の語彙: Echoes = namespace、claude = engine —
/// 旧 `Echoes` variant の二重意味をここで清算）。
pub enum ChatHost {
    Claude(EchoesAgentHost),
    Codex(CodexAgentHost),
}

impl ChatHost {
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EchoesEvent> {
        match self {
            ChatHost::Claude(h) => h.subscribe(),
            ChatHost::Codex(h) => h.subscribe(),
        }
    }

    pub fn in_flight(&self) -> InFlight {
        match self {
            ChatHost::Claude(h) => h.in_flight(),
            ChatHost::Codex(h) => h.in_flight(),
        }
    }

    pub fn commit_seq(&self) -> u64 {
        match self {
            ChatHost::Claude(h) => h.commit_seq(),
            ChatHost::Codex(h) => h.commit_seq(),
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            ChatHost::Claude(h) => h.pid(),
            ChatHost::Codex(h) => h.pid(),
        }
    }

    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.submit(prompt).await,
            ChatHost::Codex(h) => h.submit(prompt).await,
        }
    }

    pub async fn interrupt(&self) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.interrupt().await,
            ChatHost::Codex(h) => h.interrupt().await,
        }
    }

    /// 明示 teardown（[`ChatEngineSlot`] Drop から呼ぶ）。turn-scoped は turn task abort、
    /// claude は Child kill_on_drop に委ねる（host drop 時に停止）。
    pub fn stop(&mut self) {
        match self {
            // EchoesAgentHost の Child は kill_on_drop(true) なので host drop で停止する。
            ChatHost::Claude(_) => {}
            ChatHost::Codex(h) => h.stop(),
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
            ChatHost::Codex(_) => {
                anyhow::bail!("このエンジンは対話承認/permission mode を持ちません")
            }
        }
    }

    /// permission mode の動的切替（claude のみ）。
    pub async fn set_permission_mode(&self, mode: &str) -> anyhow::Result<()> {
        match self {
            ChatHost::Claude(h) => h.set_permission_mode(mode).await,
            ChatHost::Codex(_) => {
                anyhow::bail!("このエンジンは対話承認/permission mode を持ちません")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ALL ⇄ from_stand ⇄ stand_name の roundtrip（片側だけ足した engine を検知する防壁）。
    #[test]
    fn all_engines_roundtrip_through_stand_name() {
        for k in EngineKind::ALL {
            assert_eq!(
                EngineKind::from_stand(k.stand_name()),
                Some(k),
                "stand_name → from_stand が roundtrip しない: {k:?}"
            );
            assert!(!k.description().is_empty());
        }
    }

    /// stand 名 → engine 対応表の固定（drift 検知）。
    #[test]
    fn from_stand_maps_all_known_stands() {
        assert_eq!(EngineKind::from_stand("echoes"), Some(EngineKind::Claude));
        assert_eq!(EngineKind::from_stand("hd"), Some(EngineKind::Claude));
        assert_eq!(EngineKind::from_stand("cursor"), Some(EngineKind::Cursor));
        assert_eq!(EngineKind::from_stand("codex"), Some(EngineKind::Codex));
        assert_eq!(EngineKind::from_stand("agy"), Some(EngineKind::Agy));
        assert_eq!(EngineKind::from_stand("shell"), None);
        assert_eq!(EngineKind::from_stand("tmux"), None, "退役 stand は床のみ");
        assert_eq!(EngineKind::from_stand(""), None);
    }

    /// 能力表: chat = agy 以外、model 切替 = claude のみ。
    #[test]
    fn capability_table() {
        assert!(EngineKind::Claude.chat_capable());
        assert!(
            !EngineKind::Cursor.chat_capable(),
            "cursor は Act II オミット（doc 39 §7 / step 4）"
        );
        assert!(EngineKind::Codex.chat_capable());
        assert!(
            !EngineKind::Agy.chat_capable(),
            "agy は Act I のみ（doc 37 §7.5）"
        );

        assert!(EngineKind::Claude.model_switchable());
        assert!(!EngineKind::Cursor.model_switchable());
        assert!(!EngineKind::Codex.model_switchable());
        assert!(!EngineKind::Agy.model_switchable());
    }
}
