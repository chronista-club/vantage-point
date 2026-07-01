//! StandSpawner — Stand 名 (`vp:stand:{name}` task) に応じた process command 構築
//!
//! 関連 memory:
//! - `mem_1CaTpCQH8iLJ2PasRcPjHv` (Architecture v4: Process recursive、9 component minimum)
//! - `mem_1CaSmvKgsX2AQxRYFYgNM3` (Conductor pane shell — TheHand path、 PR-B 後は vp:stand:shell)
//!
//! ## 役割
//!
//! Lane (Session kind の Process) を起動する時、 内部の Stand を spawn するための command を
//! 構築する。 init_script は `.mise/tasks/vp/stand/{name}` の file-based script (tmux setup /
//! shell exec / LLM auto-launch を担当)。 VP は script の path を知っているので **`mise` を介さず
//! 直接 exec** する (依存削減 + mise trust footgun 回避、 `build_stand_command` 参照)。 install root
//! 解決失敗時のみ従来の `mise run vp:stand:{name}` に degrade する。
//!
//! - `echoes` → `.mise/tasks/vp/stand/echoes` (Layer 2: tmux + claude auto-launch、 旧 hd/HeavensDoor)
//! - `shell`  → `.mise/tasks/vp/stand/shell`  (Layer 0: bare login shell、 旧 TheHand)
//! - `tmux`   → `.mise/tasks/vp/stand/tmux`   (Layer 1: tmux session attach、 LLM なし)
//! - 任意の `{name}` script ─ 将来 stand 追加に対応 (mise 非経由なので project-local override は無し)
//!
//! ## VP_* 環境変数
//!
//! mise task は以下の env を VP から受け取る (doc 11 §3.3):
//! - `VP_CWD`     : project directory (= `cwd` 引数)
//! - `VP_SESSION` : tmux session 名 (deterministic、 `addr.tmux_session_name(stand_name)`)
//! - `VP_PROJECT` : `addr.project`
//! - `VP_LANE`    : lane label (`conductor` / performer name / `unnamed`)

use std::path::Path;

use anyhow::Result;
use tokio::sync::broadcast;

use super::lanes_state::{LaneAddress, LaneKind};
use crate::daemon::pty_slot::PtySlot;

/// Stand spawn 用 command (binary + args + env + cwd + 任意の初期入力)
#[derive(Debug, Clone)]
pub struct StandCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Phase 5-D の遺産: primary spawn が早期 exit した時に試す fallback args。
    ///  PR-B 後は mise task が早期 exit せず PTY を take over するので実質 dead path、
    ///  ただし shell spawn 自体の防御として field 自体は維持。 None = fallback なし。
    pub fallback_args: Option<Vec<String>>,
    /// Phase 6-E の遺産: spawn 直後に PTY に書き込む初期入力。
    ///
    /// PR-B 後は mise task が直接 PTY を take over するため None 固定。 旧 LlmStand /
    /// TheHand 経路では shell + initial_input で auto-launch していた。
    pub initial_input: Option<String>,
    /// doc 11 (PR-B): mise task に渡す環境変数。
    ///
    /// `mise run vp:stand:{name}` の child process env として注入、 task 内で
    /// `"$VP_CWD"` / `"$VP_SESSION"` 等で参照される (quoting 規約 doc 11 §3.3)。
    pub env: Vec<(String, String)>,
    /// doc 11 (PR-D / Z 系統): spawn 時の cwd。
    ///
    /// PR-D 以前は spawn_with_fallback の引数で受け取っていたが、 cwd 解決ロジック
    /// (install root vs project_dir fallback) を build_stand_command 内に集約するため
    /// StandCommand 自身に持たせた。 PtySlot::spawn は `cmd.cwd` を `current_dir()` に渡す。
    ///
    /// - mise task 経路 (PR-B 以降): VP install root (= `.mise/tasks/vp/stand/` の住処)
    ///   ─ user の project dir は env `VP_CWD` で task に渡される。
    /// - install root 解決失敗時 (degraded fallback): project_dir
    pub cwd: String,
}

/// 早期 exit 検知の wait 時間 (ms)。 PR-B 後は dead path だが defensive に維持。
const EARLY_EXIT_CHECK_MS: u64 = 800;

/// `StandCommand` を spawn、 primary が `EARLY_EXIT_CHECK_MS` 以内に死んだら fallback で retry。
///
/// PR-B 後は mise task 経路で fallback / initial_input は None 固定、 dead path だが
/// 既存 code shape を維持 (shell tinkering で task 経由しない直接 spawn の保険)。
///
/// PR-D (Z 系統): `cwd` は `cmd.cwd` から取得 (build_stand_command で install root に
/// 解決済み)。 caller は project_dir を別経路 (VP_CWD env) で扱う。
pub fn spawn_with_fallback(
    cmd: &StandCommand,
    cols: u16,
    rows: u16,
) -> Result<(PtySlot, broadcast::Receiver<Vec<u8>>)> {
    let cwd = cmd.cwd.as_str();
    let (mut slot, mut rx) = PtySlot::spawn(cwd, &cmd.program, &cmd.args, &cmd.env, cols, rows)?;

    // primary が早期 exit するか peek
    std::thread::sleep(std::time::Duration::from_millis(EARLY_EXIT_CHECK_MS));

    if slot.is_alive() {
        // initial_input は PR-B 後 None 固定、 でも defensive に維持。
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
        // 死因究明ログ (要所): 早期 exit した子プロセス (mise/tmux/claude) が PTY に書いた
        // stderr/stdout を broadcast channel から drain して bail message に載せる。これが
        // 無いと「800ms 以内に死んだ」事実しか残らず、死因 (例: tmux の
        // "open terminal failed: terminal does not support clear" = TERM 不在) が握り潰されて
        // diagnosis が長引く (本 fix の console blackout 調査がまさにこの穴を踏んだ)。
        let tail = drain_pty_tail(&mut rx);
        anyhow::bail!(
            "Stand spawn early-exit (no fallback): program={} args={:?}{}",
            cmd.program,
            cmd.args,
            if tail.is_empty() {
                String::new()
            } else {
                format!(" 子プロセス出力(末尾)=<<{}>>", tail)
            }
        );
    };

    tracing::warn!(
        "Stand primary spawn early-exit, fallback to args={:?}: program={}",
        fb_args,
        cmd.program
    );

    drop(slot);
    drop(rx);

    let (mut slot, rx) = PtySlot::spawn(cwd, &cmd.program, fb_args, &cmd.env, cols, rows)?;
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

/// 既存 tmux session があれば adopt（attach）、 無ければ従来の fresh spawn。
///
/// ## なぜ必要か（rebuild Epic 残骸 / reconcile gap、 2026-06-30）
/// L0 portless 化で port ベースの SP 重複検出（`find_running_sp_at_path` 等）が
/// 無効化された結果、 gentle daemon 再起動時に registry が空のまま `autostart` が
/// 全 project を重複 spawn しうる。 各重複 SP は起動最初期（`with_conductor`）に
/// conductor lane を fresh spawn するが、 tmux session は前世代から温存されている →
/// `mise run vp:stand:*`（`tmux new-session -A`）が `EARLY_EXIT_CHECK_MS` 窓の中で
/// 競合し死ぬ → `state=Dead` を daemon に register で上書きし、 実際は生きている
/// session が vp-app から Dead に見える（console blank）。
///
/// ## 修正
/// tmux session = ground truth。 在れば lane は生きている → fresh spawn せず
/// `tmux attach-session` する PtySlot を terminal source として立てて adopt する。
/// `mise` / `new-session` / `-e` の create 経路を通さないので重ね作成競合も
/// 800ms early-exit 誤判定も起きない（= 何個 SP が重複しても lane は 1 本・生存）。
///
/// 意図的な fresh restart（`restart_lane`、 tmux session を先に kill 済）は本関数を
/// 通さず `spawn_with_fallback` を直接使うので影響しない。
pub fn spawn_or_adopt(
    cmd: &StandCommand,
    session: &str,
    cols: u16,
    rows: u16,
) -> Result<(PtySlot, broadcast::Receiver<Vec<u8>>)> {
    if crate::tmux::session_exists(session) {
        let tmux = crate::tmux::tmux_bin().unwrap_or("tmux");
        let attach_args = [
            "attach-session".to_string(),
            "-t".to_string(),
            session.to_string(),
        ];
        match PtySlot::spawn(cmd.cwd.as_str(), tmux, &attach_args, &cmd.env, cols, rows) {
            Ok((slot, rx)) => {
                tracing::info!(
                    "Lane adopt: 既存 tmux session に attach (fresh spawn skip): session={}",
                    session
                );
                return Ok((slot, rx));
            }
            Err(e) => {
                // session 消失 race 等で attach 不能 → fresh spawn に fallback。
                tracing::warn!(
                    "Lane adopt の attach 失敗、 fresh spawn に fallback: session={} err={}",
                    session,
                    e
                );
            }
        }
    }
    spawn_with_fallback(cmd, cols, rows)
}

/// 早期 exit した stand の死因究明用に、 PTY broadcast channel に buffer された直近出力を
/// drain して文字列化する (最大 ~4KB)。 `spawn_with_fallback` の early-exit 分岐専用。
/// non-blocking (`try_recv`) なので bufferに溜まった分だけを拾い、 子の生死には影響しない。
fn drain_pty_tail(rx: &mut broadcast::Receiver<Vec<u8>>) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        buf.extend_from_slice(&chunk);
        if buf.len() > 4096 {
            break;
        }
    }
    String::from_utf8_lossy(&buf).trim().to_string()
}

/// LaneAddress の lane label を導出 (Conductor → "conductor"、 Performer(name) → name、 Performer(None) → "unnamed")
pub(crate) fn lane_label(addr: &LaneAddress) -> &str {
    match (&addr.kind, addr.name.as_deref()) {
        (LaneKind::Conductor, _) => "conductor",
        (LaneKind::Performer, Some(n)) => n,
        (LaneKind::Performer, None) => "unnamed",
    }
}

/// Stand 名に応じた spawn command を構築 (doc 11 PR-B、 mise task 経路)。
///
/// `mise run vp:stand:{stand_name}` を呼び、 mise task が tmux + shell + LLM auto-launch を
/// 担当する file-based init_script 設計に統一。
///
/// - `stand_name`: task 名 `vp:stand:{name}` の `name` 部分 (例: `"hd"` / `"shell"` / `"tmux"`)
/// - `addr`: `VP_SESSION` / `VP_PROJECT` / `VP_LANE` env 導出
/// - `project_dir`: `VP_CWD` env、 task 内で tmux session の cwd / fallback 経路の cd 先に使用
///
/// PR-D (Z 系統): spawn cwd は **VP install root** に固定 (`.mise/tasks/vp/stand/` の住処)。
/// mise の filesystem cascade で task が必ず見つかるよう、 user の project_dir ではなく
/// install root を cwd にする。 install root 解決失敗時は project_dir に fallback (= 旧 PR-B
/// 挙動に degrade)、 ただし `mise tasks ls` で task が見えない project では spawn 失敗。
///
/// 旧 `LaneStand` enum + `LaneStandSpec` trait dispatch は廃止 (stand_spec.rs 全削除)。
pub fn build_stand_command(
    stand_name: &str,
    addr: &LaneAddress,
    project_dir: &Path,
) -> StandCommand {
    let session = addr.tmux_session_name(stand_name);
    let project_cwd = project_dir.to_string_lossy().to_string();

    let env = vec![
        ("VP_CWD".into(), project_cwd.clone()),
        ("VP_SESSION".into(), session),
        ("VP_PROJECT".into(), addr.project.clone()),
        ("VP_LANE".into(), lane_label(addr).into()),
    ];

    // stand は `mise` を介さず script を直接 exec する (依存削減 + mise trust footgun 回避)。
    //  mise が spawn 経路で提供していたのは実質「task 名 → script file の解決」だけで、 VP は
    //  script の path (`install root/.mise/tasks/vp/stand/{name}`) を知っているので task runner は
    //  不要。 tool (tmux/claude/vp) の PATH は PtySlot の augment_path が既に補う (`~/.local/bin` /
    //  `/opt/homebrew/bin` / `~/.cargo/bin`) ため mise 無しで解決できる。 script は shebang
    //  (`#!/usr/bin/env bash`) を持つ実行可能ファイルなので、 program に path を渡せばそのまま走る。
    match super::install_root::locate_install_root() {
        Some(root) => StandCommand {
            program: root
                .join(".mise/tasks/vp/stand")
                .join(stand_name)
                .to_string_lossy()
                .to_string(),
            args: vec![],
            // stand script は早期 exit せず PTY を take over するので fallback / initial_input は None。
            fallback_args: None,
            initial_input: None,
            env,
            // cwd は install root。 script は VP_CWD で project を受け取り自身で cd する。
            cwd: root.to_string_lossy().to_string(),
        },
        // install root 解決失敗 (degraded): script path が引けないので、 従来の mise task runner
        //  経由に fallback する (mise が cwd=project からの task discovery を担う)。 警告 log は
        //  locate_install_root 内で emit 済。
        None => StandCommand {
            program: "mise".into(),
            args: vec!["run".into(), format!("vp:stand:{}", stand_name)],
            fallback_args: None,
            initial_input: None,
            env,
            cwd: project_cwd,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// build_stand_command が mise を介さず stand script を直接 program にすること。
    /// (install root は cargo test 環境では workspace root に解決される)
    #[test]
    fn build_stand_command_execs_stand_script_directly() {
        let addr = LaneAddress::conductor("vp");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        assert!(
            cmd.program.ends_with(".mise/tasks/vp/stand/echoes"),
            "program は stand script path のはず (mise 非経由)、 got: {}",
            cmd.program
        );
        assert!(
            std::path::Path::new(&cmd.program).exists(),
            "echoes script が実在するはず: {}",
            cmd.program
        );
        assert!(
            cmd.args.is_empty(),
            "script 直接 exec なので args は空のはず、 got: {:?}",
            cmd.args
        );
        assert!(cmd.fallback_args.is_none());
        assert!(cmd.initial_input.is_none());
        // spawn cwd は VP install root
        assert!(
            std::path::Path::new(&cmd.cwd)
                .join(".mise/tasks/vp/stand/echoes")
                .exists(),
            "spawn cwd は install root のはず、 got: {}",
            cmd.cwd
        );
    }

    /// VP_* env が doc 11 §3.3 通りに injected されていること。
    #[test]
    fn build_stand_command_injects_vp_env() {
        let addr = LaneAddress::performer("vantage-point", "sub");
        let cmd = build_stand_command("hd", &addr, Path::new("/work/vp"));

        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(env.get("VP_CWD").map(String::as_str), Some("/work/vp"));
        assert_eq!(
            env.get("VP_SESSION").map(String::as_str),
            Some("vp-vantage-point-sub-hd")
        );
        assert_eq!(
            env.get("VP_PROJECT").map(String::as_str),
            Some("vantage-point")
        );
        assert_eq!(env.get("VP_LANE").map(String::as_str), Some("sub"));
    }

    /// stand_name は script path にそのまま埋め込まれる (新規 stand の追加耐性)。
    #[test]
    fn build_stand_command_passes_arbitrary_stand_name() {
        let addr = LaneAddress::conductor("vp");
        let cmd = build_stand_command("opus-xhigh", &addr, Path::new("/tmp"));
        assert!(
            cmd.program.ends_with(".mise/tasks/vp/stand/opus-xhigh"),
            "未知 stand でも program path に stand_name が入るはず、 got: {}",
            cmd.program
        );
        // tmux_session_name にも stand_name そのまま入る
        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(
            env.get("VP_SESSION").map(String::as_str),
            Some("vp-vp-conductor-opus-xhigh")
        );
    }

    /// Conductor lane の VP_LANE は "conductor"、 Performer(None) は "unnamed"。
    #[test]
    fn build_stand_command_lane_label_variants() {
        let conductor = LaneAddress::conductor("vp");
        let cmd = build_stand_command("hd", &conductor, Path::new("/tmp"));
        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(env.get("VP_LANE").map(String::as_str), Some("conductor"));

        let unnamed = LaneAddress {
            project: "vp".into(),
            kind: LaneKind::Performer,
            name: None,
        };
        let cmd = build_stand_command("hd", &unnamed, Path::new("/tmp"));
        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(env.get("VP_LANE").map(String::as_str), Some("unnamed"));
    }
}
