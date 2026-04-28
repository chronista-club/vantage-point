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
            // 素 shell は user 入力待ちのまま。 auto-launch なし。
            initial_input: None,
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

    /// Phase 6-E (Slice 2): shell に流す invocation 文字列を組立。
    ///
    /// `fallback_args` がある場合、 shell の `||` chain で auto-retry を表現:
    /// `"claude --continue || claude\n"` ─ `--continue` 失敗時に空 args で再起動、
    /// retry 制御を shell に委譲することで Rust 側 PTY 出力検知を回避。
    /// 失敗時も shell は alive のまま、 user は `/exit` 同様 shell prompt に着地。
    ///
    /// 末尾 `\n` は PTY 入力として行確定する (== Enter)。
    pub fn cli_invocation(&self) -> String {
        let primary = self.format_invocation(&self.args);
        match &self.fallback_args {
            Some(fb) => {
                let fallback = self.format_invocation(fb);
                format!("{} || {}\n", primary, fallback)
            }
            None => format!("{}\n", primary),
        }
    }

    /// `cli` + 引数の連結 (空 args は cli のみ)。
    fn format_invocation(&self, args: &[String]) -> String {
        if args.is_empty() {
            self.cli.clone()
        } else {
            format!("{} {}", self.cli, args.join(" "))
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
        // Phase 6-E (Slice 2, shell-hosted): TH 借用で `$SHELL -l` を立て、
        // initial_input で LLM CLI を auto-launch。 fallback は profile.cli_invocation()
        // が `||` chain として shell に流す。 `/exit` で claude を抜けると shell prompt に
        // 戻る (Lane death ではない) ─ memory mental model の「役者が降りても舞台は残る」。
        let mut cmd = TheHand.build();
        cmd.initial_input = Some(self.profile.cli_invocation());
        // fallback_args は shell-hosted では使わない (shell が retry する)。
        // shell 自体の spawn 失敗 fallback は TheHand と同じく None。
        cmd.fallback_args = None;
        cmd
    }
}

impl LaneStand {
    /// `LaneStand` enum (wire format) → `Box<dyn LaneStandSpec>` adapter。
    ///
    /// HTTP API の `"heavens_door"` / `"the_hand"` 文字列は LaneStand enum で
    /// parse され続ける (互換性維持)。 内部処理が trait object 経由になる橋渡し。
    /// Phase 6-F で wire format を `{name, profile?}` object に拡張する時、
    /// この adapter を通じて新 path も同 trait に流せる。
    ///
    /// Copy 型なので `self` by value (clippy::wrong_self_convention)。
    pub fn to_spec(self) -> Box<dyn LaneStandSpec> {
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
    fn llm_stand_heavens_door_is_shell_hosted() {
        let stand = LlmStand::heavens_door();
        assert_eq!(stand.name(), "anthropic-claude-continue");
        let cmd = stand.build();
        // Slice 2: shell-hosted ─ program は shell、 LLM CLI は initial_input に
        assert!(
            cmd.program.contains("zsh") || cmd.program.contains("bash"),
            "shell-hosted: program=zsh/bash 想定、 got {}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-l".to_string()]);
        // shell-hosted では shell 自体の fallback はなし (shell が retry を担当)
        assert!(cmd.fallback_args.is_none());
        let input = cmd.initial_input.expect("HD は initial_input 必須");
        assert_eq!(input, "claude --continue || claude\n");
    }

    #[test]
    fn lane_stand_adapter_dispatches_correctly() {
        // wire format LaneStand から trait object への adapter が機能する
        let hd_spec = LaneStand::HeavensDoor.to_spec();
        assert_eq!(hd_spec.name(), "anthropic-claude-continue");
        let hd_cmd = hd_spec.build();
        // Slice 2: HD は shell-hosted
        assert!(hd_cmd.program.contains("zsh") || hd_cmd.program.contains("bash"));
        assert!(hd_cmd.initial_input.is_some());

        let th_spec = LaneStand::TheHand.to_spec();
        assert_eq!(th_spec.name(), "the_hand");
        let th_cmd = th_spec.build();
        assert!(th_cmd.program.contains("zsh") || th_cmd.program.contains("bash"));
        // TH は initial_input なし (素 shell)
        assert!(th_cmd.initial_input.is_none());
    }

    #[test]
    fn cli_invocation_with_fallback_chains_via_shell_or() {
        // anthropic_continue: args=["--continue"], fallback_args=Some([])
        // → "claude --continue || claude\n"
        let p = LlmProfile::anthropic_continue();
        assert_eq!(p.cli_invocation(), "claude --continue || claude\n");
    }

    #[test]
    fn cli_invocation_without_fallback_no_chain() {
        let p = LlmProfile {
            name: "test".to_string(),
            provider: "test".to_string(),
            cli: "foo".to_string(),
            args: vec!["--bar".to_string(), "baz".to_string()],
            fallback_args: None,
        };
        assert_eq!(p.cli_invocation(), "foo --bar baz\n");
    }

    #[test]
    fn cli_invocation_empty_args_renders_cli_only() {
        let p = LlmProfile {
            name: "bare".to_string(),
            provider: "test".to_string(),
            cli: "foo".to_string(),
            args: vec![],
            fallback_args: None,
        };
        assert_eq!(p.cli_invocation(), "foo\n");
    }
}
