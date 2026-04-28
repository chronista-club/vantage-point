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

use anyhow::Result;
use tokio::sync::broadcast;

use super::lanes_state::LaneStand;
use crate::daemon::pty_slot::PtySlot;

/// Stand spawn 用 command (binary + args)
#[derive(Debug, Clone)]
pub struct StandCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Phase 5-D: primary spawn が早期 exit した時に試す fallback args。
    ///  HD の `--continue` が前 session corrupt 等で起動しない時に空 args (= 新規 session) で再試行。
    ///  `None` なら fallback 無し (= 失敗 = error 返却)。
    pub fallback_args: Option<Vec<String>>,
}

/// 早期 exit 検知の wait 時間 (ms)。 観測 (gfp-cad で `claude --continue` が 2ms で exit) より十分な値。
///  小さすぎると検知漏れ、 大きすぎると spawn 全体の latency 悪化。 800ms は経験則的 sweet spot。
const EARLY_EXIT_CHECK_MS: u64 = 800;

/// `StandCommand` を spawn、 primary が `EARLY_EXIT_CHECK_MS` 以内に死んだら fallback で retry。
///
/// Phase 5-D: `claude --continue` failure (例: session corrupt) → `claude` (新規 session) で
///  Lane を救済する。 caller (lanes.rs / lanes_state.rs) は通常の `PtySlot::spawn` 同様に使える。
pub fn spawn_with_fallback(
    cwd: &str,
    cmd: &StandCommand,
    cols: u16,
    rows: u16,
) -> Result<(PtySlot, broadcast::Receiver<Vec<u8>>)> {
    let (mut slot, rx) = PtySlot::spawn(cwd, &cmd.program, &cmd.args, cols, rows)?;

    // primary が早期 exit するか peek
    std::thread::sleep(std::time::Duration::from_millis(EARLY_EXIT_CHECK_MS));

    if slot.is_alive() {
        return Ok((slot, rx));
    }

    let Some(fb_args) = cmd.fallback_args.as_ref() else {
        // fallback 無し → primary 死亡をそのまま error に格上げ (caller graceful degrade)
        anyhow::bail!(
            "Stand spawn early-exit (no fallback): program={} args={:?}",
            cmd.program,
            cmd.args
        );
    };

    tracing::warn!(
        "Stand primary spawn early-exit, fallback to args={:?}: program={}",
        fb_args,
        cmd.program
    );

    drop(slot); // 死亡 slot を Drop で kill+wait
    drop(rx);

    PtySlot::spawn(cwd, &cmd.program, fb_args, cols, rows)
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
        // Phase 5-D: `--continue` 失敗時 (session corrupt 等) は新規 session で fallback。
        fallback_args: Some(vec![]),
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
        // shell 起動失敗は何 fallback しても無理 (PATH / OS issue) なので None。
        fallback_args: None,
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
