//! StandSpawner — LaneStand selection に応じた process command 構築
//!
//! 関連 memory:
//! - `mem_1CaTpCQH8iLJ2PasRcPjHv` (Architecture v4: Process recursive、9 component minimum)
//! - `mem_1CaSmvKgsX2AQxRYFYgNM3` (Lead pane shell — TheHand path)
//!
//! ## 役割
//!
//! Lane (Session kind の Process) を起動する時、内部の Worker (Stand) を spawn するための
//! command を LaneStand 別に構築する。
//!
//! - `HeavensDoor` (HD): `claude --continue` (fallback `claude`)
//! - `TheHand` (TH): `$SHELL -l` (login shell、 mise / dev tooling PATH を rc 経由で取り込み)
//!
//! ## Phase
//!
//! - A5-1 (今): command 構築 fn のみ (この module)
//! - A5-2: `PtySlot::spawn` 連動 (`LanePool::with_lead` を実 PTY 化)
//! - A5-3: spawn 結果から pid を `LaneInfo` に書き戻し

use std::path::Path;

use super::lanes_state::LaneStand;

/// Stand spawn 用 command (binary + args)
#[derive(Debug, Clone)]
pub struct StandCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// LaneStand に応じた spawn command を構築
///
/// `cwd` は将来 cwd-aware command (例: project_dir に応じた custom env) のため
/// 受け取るが、A5-1 では未使用。
pub fn build_stand_command(stand: LaneStand, _cwd: &Path) -> StandCommand {
    match stand {
        LaneStand::HeavensDoor => build_heavens_door_command(),
        LaneStand::TheHand => build_the_hand_command(),
    }
}

/// HD (Heaven's Door) = Claude CLI を起動
///
/// `claude --continue` を default、前 session が無ければ Claude CLI 側が新規 session に fall back。
/// 関連 memory: feedback_hd_input_newline_on_restart で `\n` 混入 bug あり、続けて調査予定。
fn build_heavens_door_command() -> StandCommand {
    StandCommand {
        program: "claude".to_string(),
        args: vec!["--continue".to_string()],
    }
}

/// TH (The Hand) = 素 shell を login mode で起動
///
/// `$SHELL` (default `/bin/zsh`) + `-l` で `~/.zprofile`/`~/.zshrc` 連鎖を読み、
/// mise / volta / nvm 等の PATH を rc 経由で取り込む。
fn build_the_hand_command() -> StandCommand {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/bin/zsh".to_string()
        } else {
            "/bin/bash".to_string()
        }
    });
    StandCommand {
        program: shell,
        args: vec!["-l".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavens_door_uses_claude_continue() {
        let cmd = build_stand_command(LaneStand::HeavensDoor, Path::new("/tmp"));
        assert_eq!(cmd.program, "claude");
        assert_eq!(cmd.args, vec!["--continue".to_string()]);
    }

    #[test]
    fn the_hand_uses_shell_login() {
        let cmd = build_stand_command(LaneStand::TheHand, Path::new("/tmp"));
        assert!(
            cmd.program.contains("zsh") || cmd.program.contains("bash"),
            "shell expected zsh/bash, got {}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-l".to_string()]);
    }
}
