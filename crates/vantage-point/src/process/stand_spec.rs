//! LaneStandSpec trait — Lane (PTY) 上で発動する Stand 能力の抽象 (Phase 6-E、 VP-107)
//!
//! 関連 memory:
//! - `mem_1CaVnfJRgWtuRgZD9yQSoV` (VP Lane Stand mental model — 舞台-役者-演目 metaphor)
//! - `mem_1CaVeQEKXd8U2XHn75RD4M` (VP Roadmap Phase 5→9)
//!
//! ## 舞台-役者-演目 metaphor
//!
//! - **Lane (PTY)** = 舞台
//! - **TheHand 🤚** = 舞台を立てる能力 (素 shell の base)
//! - **LlmStand 📖 (= HD)** = 役者を呼ぶ能力 (LLM auto-launch)
//! - **LlmProfile** = 演目台本 (誰を、 どんな設定で呼ぶか)
//! - **LLM (Claude/Gemini/...)** = 役者
//!
//! ## Phase 6-E 範囲 (= Slice 1: trait skeleton、 behavior-preserving)
//!
//! 既存 enum `LaneStand` + `build_stand_command(stand)` の二段 dispatch を
//! trait object 化、 wire format (HTTP `/api/lanes` の `"heavens_door"` 等) は維持。
//! HD は **既存挙動を保つため直接 LLM CLI を spawn** (TH 借用 = shell-hosted は Slice 2)。
//!
//! Phase 6-F で `LlmProfile` preset を増やすだけで複数 LLM 対応できる shape を確立。

use super::lanes_state::LaneStand;
use super::stand_spawner::StandCommand;

/// Lane (PTY) 上で発動する Stand 能力。
///
/// 各実装 (`TheHand` / `LlmStand`) が `build()` で `StandCommand` を返し、
/// `spawn_with_fallback` が PTY を起動する流れ。 wire format (`name()`) は
/// HTTP API の `LaneInfo.stand` JSON 値と一致させる。
pub trait LaneStandSpec {
    /// Stand の wire 名 ─ HTTP API の `stand` field と互換。
    fn name(&self) -> &str;

    /// PtySlot 起動用 command を構築。
    fn build(&self) -> StandCommand;
}

/// TH 🤚 = 舞台を立てる能力 ─ 素 shell PTY (login mode)。
///
/// `$SHELL -l` で `~/.zprofile` / `~/.zshrc` 連鎖を読み、
/// mise / volta / nvm 等の PATH を rc 経由で取り込む。
pub struct TheHand;

impl LaneStandSpec for TheHand {
    fn name(&self) -> &str {
        "the_hand"
    }

    fn build(&self) -> StandCommand {
        StandCommand {
            program: shell_path(),
            args: vec!["-l".to_string()],
            // shell 起動失敗は何 fallback しても無理 (PATH / OS issue) なので None。
            fallback_args: None,
        }
    }
}

/// LLM Profile = 演目台本 ─ 「どの LLM をどんな設定で呼ぶか」 を struct で表現。
///
/// Phase 6-E では `anthropic_continue()` 1 種のみ (HD default)。
/// Phase 6-F で `anthropic_opus_47_xhigh()` / `google_gemini_pro_3()` 等の
/// preset constructor を追加していく。
#[derive(Debug, Clone)]
pub struct LlmProfile {
    /// 一意識別子 (e.g., `"anthropic-claude-continue"`、 `"google-gemini-pro-3"`)
    pub name: String,
    /// LLM provider (e.g., `"anthropic"` / `"google"` / `"openai"`)
    pub provider: String,
    /// 起動 CLI binary (e.g., `"claude"` / `"gemini"`)
    pub cli: String,
    /// CLI 引数 (model 指定 / thinking-effort / `--continue` 等)
    pub args: Vec<String>,
    /// primary spawn 早期 exit 時の fallback 引数 (None なら fallback なし)
    pub fallback_args: Option<Vec<String>>,
}

impl LlmProfile {
    /// HD default = `claude --continue` (fallback: 空 args = 新規 session)。
    ///
    /// Phase 5-D で導入した `--continue` 失敗時 (session corrupt 等) の
    /// 空 args fallback を維持。
    pub fn anthropic_continue() -> Self {
        Self {
            name: "anthropic-claude-continue".to_string(),
            provider: "anthropic".to_string(),
            cli: "claude".to_string(),
            args: vec!["--continue".to_string()],
            fallback_args: Some(vec![]),
        }
    }
}

/// LlmStand 📖 (= HD) = 役者を呼ぶ能力。
///
/// `LlmProfile` を保持して、 build() 時に profile から `StandCommand` を生成する。
/// 「HD = LLM を呼ぶ能力 (一般)、 Claude は default profile」 という memory rule を
/// 体現する struct ─ HD が Claude 専用ではないことを型で示す。
pub struct LlmStand {
    pub profile: LlmProfile,
}

impl LlmStand {
    /// HD preset = `LlmProfile::anthropic_continue()` を持つ default 役者。
    ///
    /// 既存の `LaneStand::HeavensDoor` enum variant が解決される時に使われる。
    pub fn heavens_door() -> Self {
        Self {
            profile: LlmProfile::anthropic_continue(),
        }
    }
}

impl LaneStandSpec for LlmStand {
    fn name(&self) -> &str {
        &self.profile.name
    }

    fn build(&self) -> StandCommand {
        // Phase 6-E (Slice 1, behavior-preserving): 既存の HD 挙動 (= 直接 `claude` spawn)
        // を維持。 Slice 2 で TH 借用 (shell-hosted、 `$SHELL -l` 経由で initial_input に
        // `claude --continue\n` を送る) に切り替える予定 ─ それまでは wire-compat 重視。
        StandCommand {
            program: self.profile.cli.clone(),
            args: self.profile.args.clone(),
            fallback_args: self.profile.fallback_args.clone(),
        }
    }
}

impl LaneStand {
    /// `LaneStand` enum (wire format) → `Box<dyn LaneStandSpec>` adapter。
    ///
    /// HTTP API の `"heavens_door"` / `"the_hand"` 文字列は LaneStand enum で
    /// parse され続ける (互換性維持)。 内部処理が trait object 経由になる橋渡し。
    /// Phase 6-F で wire format を `{name, profile?}` object に拡張する時、
    /// この adapter を通じて新 path も同 trait に流せる。
    pub fn to_spec(&self) -> Box<dyn LaneStandSpec> {
        match self {
            LaneStand::HeavensDoor => Box::new(LlmStand::heavens_door()),
            LaneStand::TheHand => Box::new(TheHand),
        }
    }
}

/// `$SHELL` env var の解決 (TH と LlmStand の TH 借用で共有)。
fn shell_path() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/bin/zsh".to_string()
        } else {
            "/bin/bash".to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hand_name_is_wire_compat() {
        // HTTP `/api/lanes` JSON 互換: "the_hand" 文字列を維持。
        assert_eq!(TheHand.name(), "the_hand");
    }

    #[test]
    fn the_hand_builds_shell_login() {
        let cmd = TheHand.build();
        assert!(
            cmd.program.contains("zsh") || cmd.program.contains("bash"),
            "shell expected zsh/bash, got {}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-l".to_string()]);
        assert!(cmd.fallback_args.is_none());
    }

    #[test]
    fn llm_stand_heavens_door_uses_claude_continue() {
        let stand = LlmStand::heavens_door();
        assert_eq!(stand.name(), "anthropic-claude-continue");
        let cmd = stand.build();
        assert_eq!(cmd.program, "claude");
        assert_eq!(cmd.args, vec!["--continue".to_string()]);
        // Phase 5-D fallback (空 args) を保持
        assert_eq!(cmd.fallback_args, Some(vec![]));
    }

    #[test]
    fn lane_stand_adapter_dispatches_correctly() {
        // wire format LaneStand から trait object への adapter が機能する
        let hd_spec = LaneStand::HeavensDoor.to_spec();
        assert_eq!(hd_spec.name(), "anthropic-claude-continue");
        let hd_cmd = hd_spec.build();
        assert_eq!(hd_cmd.program, "claude");

        let th_spec = LaneStand::TheHand.to_spec();
        assert_eq!(th_spec.name(), "the_hand");
        let th_cmd = th_spec.build();
        assert!(th_cmd.program.contains("zsh") || th_cmd.program.contains("bash"));
    }
}
