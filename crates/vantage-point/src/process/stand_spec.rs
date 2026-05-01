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

use super::lanes_state::{LaneAddress, LaneStand};
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
    ///
    /// Phase 1e: `addr` を受け取って tmux session 名を導出 (HD は副舞台 = tmux session を立てる)。
    /// addr 不要な spec (TH 等) は `_addr` で受け流せる。
    fn build(&self, addr: &LaneAddress) -> StandCommand;
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

    fn build(&self, _addr: &LaneAddress) -> StandCommand {
        // TH は素 shell なので addr (tmux session 名) は使わない。 Phase 2 で TH 用
        // init_script (vp_lane_init_script Phase 1) を扱う時に addr を活用予定。
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
        format!("{}\n", self.invocation_chain())
    }

    /// Phase 1e: tmux session でラップした invocation を組立。
    ///
    /// shell の中で更に tmux session を立てて、 その内側で LLM CLI を起動する形:
    /// ```text
    /// tmux new-session -A -s {session} 'claude --continue || claude' || (claude --continue || claude)
    /// ```
    ///
    /// - **副舞台 (tmux session)**: agent が `tmux send-keys -t {session}` で他 Lane の HD に
    ///   入力をリレーできる。 `LaneInfo.tmux` 経由で session 名が引ける。
    /// - **外側 `||`**: tmux 不在環境では shell 直で `claude --continue || claude` に降格 (PtySlotFallback mode)。
    /// - **内側 `'...'`**: tmux session 内 pane の起動 cmd。 `||` chain は shell に委譲済の既存挙動を維持。
    ///
    /// 関連 memory: `mem_1Cac2YvnAhaVRCJemidtkx` (Phase 1 milestone) の design 確定軸。
    pub fn cli_invocation_tmux_wrapped(&self, tmux_session: &str) -> String {
        let inner = self.invocation_chain();
        // 副舞台 (tmux) 起動成功 → 内側 cmd で LLM auto-launch、 失敗 → 外側 fallback
        format!(
            "tmux new-session -A -s {session} '{inner}' || ({inner})\n",
            session = tmux_session,
            inner = inner
        )
    }

    /// 内部 helper: `cli + args` を `||` chain で結合した shell 式 (末尾 `\n` なし)。
    ///
    /// 例: `args=["--continue"], fallback_args=Some([])` → `"claude --continue || claude"`。
    /// `cli_invocation` (素 shell) と `cli_invocation_tmux_wrapped` (副舞台あり) で共有。
    fn invocation_chain(&self) -> String {
        let primary = self.format_invocation(&self.args);
        match &self.fallback_args {
            Some(fb) => {
                let fallback = self.format_invocation(fb);
                format!("{} || {}", primary, fallback)
            }
            None => primary,
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

    fn build(&self, addr: &LaneAddress) -> StandCommand {
        // Phase 1e (副舞台付き shell-hosted): TH 借用で shell を立て、 その中で
        // tmux session を立てる二段構造。
        //
        // - 舞台 (shell) ←→ 副舞台 (tmux session) ←→ 役者 (claude)
        // - tmux 不在 → 舞台 ←→ 役者 (副舞台なし、 send-keys 不可、 PtySlotFallback mode)
        //
        // 副舞台 (tmux) があれば agent が `tmux send-keys` で他 Lane の HD に入力を
        // リレーできる。 tmux 不在環境では shell 内 `||` で素 claude にフォールバック。
        // session 名 derivation は HD prefix を仮定 (将来 Gemini Stand 等で別 prefix 化)。
        let mut cmd = TheHand.build(addr);
        let session = addr.tmux_session_name(LaneStand::HeavensDoor);
        cmd.initial_input = Some(self.profile.cli_invocation_tmux_wrapped(&session));
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

    /// テスト用 LaneAddress factory。 build(&addr) signature に渡す。
    fn test_addr() -> LaneAddress {
        LaneAddress::lead("vp")
    }

    #[test]
    fn the_hand_name_is_wire_compat() {
        // HTTP `/api/lanes` JSON 互換: "the_hand" 文字列を維持。
        assert_eq!(TheHand.name(), "the_hand");
    }

    #[test]
    fn the_hand_builds_shell_login() {
        let cmd = TheHand.build(&test_addr());
        assert!(
            cmd.program.contains("zsh") || cmd.program.contains("bash"),
            "shell expected zsh/bash, got {}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-l".to_string()]);
        assert!(cmd.fallback_args.is_none());
        // TH は addr (tmux session 名) を使わない、 initial_input なし
        assert!(cmd.initial_input.is_none());
    }

    #[test]
    fn llm_stand_heavens_door_uses_tmux_wrapped_invocation() {
        // Phase 1e: HD initial_input は tmux session でラップされた invocation
        let stand = LlmStand::heavens_door();
        assert_eq!(stand.name(), "anthropic-claude-continue");
        let cmd = stand.build(&test_addr());
        // shell-hosted (program は shell)
        assert!(
            cmd.program.contains("zsh") || cmd.program.contains("bash"),
            "shell-hosted: program=zsh/bash 想定、 got {}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-l".to_string()]);
        // shell-hosted では shell 自体の fallback はなし (shell が retry 担当)
        assert!(cmd.fallback_args.is_none());

        let input = cmd.initial_input.expect("HD は initial_input 必須");
        // Phase 1e: tmux new-session で副舞台を立てる + 不在環境では `||` で素 claude
        assert!(
            input.starts_with("tmux new-session -A -s hd-lead-vp"),
            "Phase 1e: tmux session ラッパ必須、 got: {}",
            input
        );
        assert!(
            input.contains("'claude --continue || claude'"),
            "内側 LLM 起動 cmd (tmux pane の cmd) 必須、 got: {}",
            input
        );
        assert!(
            input.contains("|| (claude --continue || claude)"),
            "外側 fallback (tmux 不在環境用) 必須、 got: {}",
            input
        );
        assert!(input.ends_with('\n'), "PTY 入力は改行で行確定");
    }

    #[test]
    fn lane_stand_adapter_dispatches_correctly() {
        // wire format LaneStand から trait object への adapter が機能する
        let addr = test_addr();
        let hd_spec = LaneStand::HeavensDoor.to_spec();
        assert_eq!(hd_spec.name(), "anthropic-claude-continue");
        let hd_cmd = hd_spec.build(&addr);
        // HD は shell-hosted + tmux wrapped (Phase 1e)
        assert!(hd_cmd.program.contains("zsh") || hd_cmd.program.contains("bash"));
        assert!(hd_cmd.initial_input.is_some());

        let th_spec = LaneStand::TheHand.to_spec();
        assert_eq!(th_spec.name(), "the_hand");
        let th_cmd = th_spec.build(&addr);
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

    #[test]
    fn cli_invocation_tmux_wrapped_with_fallback() {
        // Phase 1e: tmux session でラップした invocation が正しく組立される
        let p = LlmProfile::anthropic_continue();
        let wrapped = p.cli_invocation_tmux_wrapped("hd-lead-vantage-point");
        assert_eq!(
            wrapped,
            "tmux new-session -A -s hd-lead-vantage-point 'claude --continue || claude' || (claude --continue || claude)\n"
        );
    }

    #[test]
    fn cli_invocation_tmux_wrapped_without_fallback() {
        // fallback 無し profile も tmux でラップされ、 内側は cli only に
        let p = LlmProfile {
            name: "bare".to_string(),
            provider: "test".to_string(),
            cli: "foo".to_string(),
            args: vec![],
            fallback_args: None,
        };
        assert_eq!(
            p.cli_invocation_tmux_wrapped("hd-lead-vp"),
            "tmux new-session -A -s hd-lead-vp 'foo' || (foo)\n"
        );
    }
}
