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

/// Stand spawn 用 command (binary + args + 任意の初期入力)
#[derive(Debug, Clone)]
pub struct StandCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Phase 5-D: primary spawn が早期 exit した時に試す fallback args。
    ///  HD の `--continue` が前 session corrupt 等で起動しない時に空 args (= 新規 session) で再試行。
    ///  `None` なら fallback 無し (= 失敗 = error 返却)。
    ///
    /// Phase 6-E (Slice 2) 以降: shell-hosted Stand では shell 自体は早期 exit しないため
    /// 実質 dead path。 ただし shell spawn 自体の防御として field 自体は維持。
    pub fallback_args: Option<Vec<String>>,
    /// Phase 6-E (Slice 2): spawn 直後に PTY に書き込む初期入力 (shell-hosted Stand 用)。
    ///
    /// 例: `LlmStand` 経由の HD は `program="zsh", args=["-l"]` で shell を立て、
    /// `initial_input = Some("claude --continue || claude\n")` で auto-launch。
    /// `||` chain は shell に retry を任せる ─ memory `mem_1CaVnfJRgWtuRgZD9yQSoV`
    /// の「lifecycle の違いが直感的」 原則の体現 (役者 lifecycle と舞台 lifecycle が独立)。
    pub initial_input: Option<String>,
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
        // Phase 6-E (Slice 2): shell-hosted Stand の auto-launch ─ initial_input を PTY に書く。
        // 失敗は warn のみ (spawn 自体は成功している)、 shell prompt は user に表示される。
        if let Some(input) = cmd.initial_input.as_deref()
            && let Err(e) = slot.write(input.as_bytes())
        {
            tracing::warn!(
                "initial_input write failed (Stand spawn keeps shell alive): err={} program={} input_len={}",
                e,
                cmd.program,
                input.len()
            );
        }
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

    let (mut slot, rx) = PtySlot::spawn(cwd, &cmd.program, fb_args, cols, rows)?;
    // fallback 経路でも initial_input を書き込む (shell-hosted Stand での一貫性)。
    if let Some(input) = cmd.initial_input.as_deref()
        && let Err(e) = slot.write(input.as_bytes())
    {
        tracing::warn!(
            "initial_input write failed on fallback (shell alive): err={} program={}",
            e,
            cmd.program
        );
    }
    Ok((slot, rx))
}

/// LaneStand に応じた spawn command を構築
///
/// Phase 6-E (VP-107): 内部実装を `LaneStandSpec` trait dispatch に委譲。
/// wire format (`LaneStand` enum) は維持しつつ、 `to_spec()` adapter で trait object
/// に変換、 `build()` を呼ぶ流れに統一。 caller (`lanes_state.rs` / `routes/lanes.rs`)
/// は無変更で動作する (返り値の `StandCommand` shape 同一)。
///
/// `cwd` は将来 cwd-aware command (例: project_dir に応じた custom env) のため
/// 受け取るが、 6-E でも未使用 (Phase 6.5 の Lane manifest で活用予定)。
pub fn build_stand_command(stand: LaneStand, _cwd: &Path) -> StandCommand {
    stand.to_spec().build()
}

#[cfg(test)]
mod tests {
    use super::*;

    // wire-compat regression test: `LaneStand` enum 経由でも shell-hosted 挙動が保たれる。
    // 詳細な trait impl 単位 test は `stand_spec` module に存在。

    /// Phase 6-E Slice 2: HD は shell + initial_input で `claude --continue || claude\n` を auto-launch。
    /// Slice 1 の「直接 claude spawn」 から 「shell-hosted」 に挙動が変わったことを test も反映。
    #[test]
    fn heavens_door_is_shell_hosted_with_claude_invocation() {
        let cmd = build_stand_command(LaneStand::HeavensDoor, Path::new("/tmp"));
        assert!(
            cmd.program.contains("zsh") || cmd.program.contains("bash"),
            "HD は shell-hosted (program=zsh/bash 想定)、 got {}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-l".to_string()]);
        let input = cmd.initial_input.expect("HD は initial_input 必須");
        assert!(input.contains("claude --continue"));
        assert!(
            input.contains("|| claude"),
            "fallback chain (|| claude) 必須"
        );
        assert!(input.ends_with('\n'), "PTY 入力は改行で行確定");
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
        // 素 shell は auto-launch なし
        assert!(cmd.initial_input.is_none());
    }
}
