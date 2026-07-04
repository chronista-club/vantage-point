//! StandSpawner — Stand 名に応じた spawn command 構築（tmux decoupling PR2 で Rust-native 化）
//!
//! ## 構造（design doc §13: Act1-layered）
//!
//! ```text
//! PtySlot → $LOGIN_SHELL -l                    ← Act1: 常に生きる「床」（self-healing）
//!    ↓ initial_input（spawn 後に PTY へ type-ahead 注入）
//!    claude --resume 'ID' … || claude …        ← Act3（|| fallback は shell が native 処理）
//! ```
//!
//! 旧構造（PtySlot → bash script → tmux new-session → claude）の bash/mise/tmux 層は全廃。
//! - env 注入は PtySlot spawn の 1 箇所（TERM/LANG/PATH は `spawn_env`、 VP_* はここで構築）
//! - claude 終了後は床の login shell prompt に自然に戻る（旧 `; exec $SHELL -l` chain 不要）
//! - `--resume` 失敗 → fresh claude の fallback は shell の `||` が担う
//! - resume id は spawn 前に Rust が `lane::cc_session` を直読み
//!   （旧: bash が `vp lane last-session` を子→親 CLI 呼び出し = 層の逆転、解消）
//!
//! ## VP_* 環境変数
//!
//! - `VP_CWD`     : project directory
//! - `VP_SESSION` : lane 論理 identity（= `LaneAddress` の Display 形。旧 tmux session 名は
//!                  tmux の「`/` 禁止」制約由来の sanitize 形だった — tmux 撤去で不要に）
//! - `VP_PROJECT` : `addr.project`
//! - `VP_LANE`    : lane label（`conductor` / performer 名 / `unnamed`）
//! - `VP_PROFILE` : dev/brew namespace（設定時のみ）

use std::path::Path;

use anyhow::Result;
use tokio::sync::broadcast;

use super::lanes_state::{LaneAddress, LaneKind};
use crate::daemon::pty_slot::PtySlot;

/// wiremsg R2-c（チャネル B、決定 D2）: wire 未読通知 hook を VP が spawn 時に注入する。
/// dotfile 非依存で箱から動く。 hook 実体は `vp wire hook-check`（vp CLI 同梱）。
/// UserPromptSubmit で未読 wire を additionalContext 通知、 daemon 不在時は silent 成功
/// （fail-open）。 JSON に single quote が無いため `'…'` 埋め込みで安全に quote できる。
///
/// NOTE: SessionStart（session_id 記録、R3-b）は global settings（~/.claude/settings.json）の
/// hook に移管済み（inline だと手動起動 session の id を取りこぼすため）。
const WIRE_HOOKS: &str = r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"vp wire hook-check"}]}]}}"#;

/// Stand spawn 用 command（program + args + env + cwd + 初期入力）
#[derive(Debug, Clone)]
pub struct StandCommand {
    pub program: String,
    pub args: Vec<String>,
    /// spawn 後に PTY へ type-ahead 注入する初期入力（= Act3 の claude 起動 command line）。
    ///
    /// PTY line discipline が入力をバッファするため、 shell の rc 読込完了を待たずに書いてよい
    /// （shell が読み始めた時点で消費される）。 None = 床の shell のみ（Act1 で止まる）。
    pub initial_input: Option<String>,
    /// PtySlot spawn に渡す環境変数（VP_* identity）。 TERM/LANG/PATH は PtySlot 側の
    /// `spawn_env` が注入する（ここでは持たない — 注入点を 1 箇所に保つ）。
    pub env: Vec<(String, String)>,
    /// spawn 時の cwd = project directory（旧 install-root ダンスは script 層と共に廃止）。
    pub cwd: String,
}

/// 早期 exit 検知の wait 時間 (ms)。 床の login shell がこの窓内に死ぬ = 環境異常
/// （shell 不在 / PTY 失敗等）。 死因は PTY 出力の tail を添えて bail する。
/// initial_input の書込みもこの窓の後（= 床の生存確認後）に行う。
const EARLY_EXIT_CHECK_MS: u64 = 800;

/// `StandCommand` を spawn し、 床の生存確認後に initial_input を注入する。
///
/// 床（login shell）が `EARLY_EXIT_CHECK_MS` 以内に死んだら、 PTY 出力の末尾を添えて
/// bail する（死因の握り潰し防止 — console blackout 調査の教訓）。
pub fn spawn_stand(
    cmd: &StandCommand,
    cols: u16,
    rows: u16,
) -> Result<(PtySlot, broadcast::Receiver<Vec<u8>>)> {
    let (mut slot, mut rx) =
        PtySlot::spawn(cmd.cwd.as_str(), &cmd.program, &cmd.args, &cmd.env, cols, rows)?;

    // 床が早期 exit するか peek（rc 読込より短い可能性はあるが、 type-ahead は line discipline
    // がバッファするので「注入が早すぎて落ちる」ことはない — ここは純粋に死活確認）。
    std::thread::sleep(std::time::Duration::from_millis(EARLY_EXIT_CHECK_MS));

    if !slot.is_alive() {
        // 死因究明ログ（要所）: 早期 exit した床が PTY に書いた stderr/stdout を drain して
        // bail message に載せる。 無いと「800ms 以内に死んだ」事実しか残らない。
        let tail = drain_pty_tail(&mut rx);
        anyhow::bail!(
            "Stand spawn early-exit: program={} args={:?}{}",
            cmd.program,
            cmd.args,
            if tail.is_empty() {
                String::new()
            } else {
                format!(" 子プロセス出力(末尾)=<<{}>>", tail)
            }
        );
    }

    if let Some(input) = cmd.initial_input.as_deref()
        && let Err(e) = slot.write(input.as_bytes())
    {
        // 床は生きているので lane 自体は成立させる（user が手打ちで claude を起動できる）。
        tracing::warn!(
            "initial_input write failed (床の shell は生存): err={} program={} input_len={}",
            e,
            cmd.program,
            input.len()
        );
    }
    Ok((slot, rx))
}

/// 早期 exit した床の死因究明用に、 PTY broadcast channel に buffer された直近出力を
/// drain して文字列化する（最大 ~4KB）。 non-blocking（`try_recv`）。
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

/// 床になる login shell を (program, args) で解決する。
///
/// - **Unix**: `$SHELL` を尊重（SP が launchd 起動だと SHELL env 不在のことがある →
///   `/bin/zsh` → `/bin/bash` → `/bin/sh` の順で実在 shell に fallback）。 `-l` で
///   login shell 化し、 mise / volta / nvm 等の PATH を rc 経由で取り込む（Act1 = env の床）。
/// - **Windows**: git-bash（`vp_paths::shell::find_git_bash`）。 不在時は標準 install path を
///   program に据えて ENOENT を明示化する（Git for Windows が前提依存）。
#[cfg(not(windows))]
fn login_shell() -> (String, Vec<String>) {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ["/bin/zsh", "/bin/bash"]
                .iter()
                .find(|p| Path::new(p).exists())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "/bin/sh".to_string());
    (shell, vec!["-l".to_string()])
}

/// Windows 版 — git-bash を login + interactive で起動する。 詳細は non-windows 版 doc 参照。
#[cfg(windows)]
fn login_shell() -> (String, Vec<String>) {
    let program = match vp_paths::shell::find_git_bash() {
        Some(bash) => bash.to_string_lossy().into_owned(),
        None => {
            tracing::error!(
                "git-bash (Git for Windows) が見つかりません。 lane の床 shell を起動できません。 \
                 `winget install Git.Git` で導入してください。"
            );
            r"C:\Program Files\Git\bin\bash.exe".to_string()
        }
    };
    (program, vec!["--login".to_string(), "-i".to_string()])
}

/// CC session id として安全な形式か（英数 + ハイフンのみ）。
///
/// `--resume '<id>'` の single-quote 埋め込みが shell injection にならないための防壁。
/// 書き手（SessionStart hook）は UUID を記録するので通常は常に真。
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Act3: claude 起動 command line を組み立てる（旧 echoes bash script の CLAUDE_CMD 分岐の移植）。
///
/// CC 2.1 Background Agents insulate: `--continue` は「cwd の最新」を拾うため bg session 在りで
/// Agent View dashboard 化する（send-keys handoff が list-nav UI に化ける既知バグ）。 id を指名する
/// `--resume '<id>'` はこの罠を構造的に回避する（設計 mem_1CbXZyCiqrdgteGhRFDaHW / R3-b）。
///
/// - fresh（"New Conductor Session"）: 素の claude（resume/continue 回避）
/// - conductor + id あり: `--resume '<id>' || fresh`（session 消失時は fresh に fallback）
/// - conductor + id なし（初回/移行直後）: `--continue || fresh`（従来 chain 維持 —
///   一度 session が立てば hook が id を記録し、 以後は --resume 側に乗る）
/// - performer + id あり: `--resume '<id>' || fresh`（tmux 撤去で SP restart = claude 再起動に
///   なったため、 performer も会話継続を resume で担う。 id 指名なので dashboard 罠は踏まない）
/// - performer + id なし: fresh（`--continue` は dashboard 罠のため使わない）
fn claude_command(kind: LaneKind, fresh: bool, resume_id: Option<&str>) -> String {
    let fresh_cmd = format!("claude --settings '{}'", WIRE_HOOKS);
    if fresh {
        return fresh_cmd;
    }
    match (kind, resume_id.filter(|id| is_safe_session_id(id))) {
        (_, Some(id)) => format!(
            "claude --resume '{}' --settings '{}' || {}",
            id, WIRE_HOOKS, fresh_cmd
        ),
        (LaneKind::Conductor, None) => format!(
            "claude --continue --settings '{}' || {}",
            WIRE_HOOKS, fresh_cmd
        ),
        (LaneKind::Performer, None) => fresh_cmd,
    }
}

/// Stand 名に応じた spawn command を構築する（tmux decoupling PR2: Rust-native、 script 層なし）。
///
/// - `"echoes"`（+ 旧名 `"hd"`）: 床 + claude 注入（`fresh` / cc_session により resume 分岐）
/// - `"shell"`: 床のみ
/// - `"tmux"`（退役 stand）/ 未知名: 床のみ + warn（DB descriptor の legacy 値を graceful 吸収）
///
/// `fresh=true` は resume/continue を回避して素の claude を起動する
/// （sidebar "New Conductor Session"。 旧 `VP_FRESH=1` env の spawn パラメータ化）。
pub fn build_stand_command(
    stand_name: &str,
    addr: &LaneAddress,
    project_dir: &Path,
    fresh: bool,
) -> StandCommand {
    let project_cwd = project_dir.to_string_lossy().to_string();

    let mut env = vec![
        ("VP_CWD".into(), project_cwd.clone()),
        // VP_SESSION = lane の論理 identity（LaneAddress Display 形）。 statusline 等の表示用。
        ("VP_SESSION".into(), addr.to_string()),
        ("VP_PROJECT".into(), addr.project.clone()),
        ("VP_LANE".into(), lane_label(addr).into()),
    ];
    // VP_PROFILE 分離: dev profile を子プロセス（claude の statusline / vp CLI 呼び出し）へ
    // 明示伝播する。 未設定 (brew) は push しない。
    if let Some(profile) = vp_paths::vp_profile() {
        env.push(("VP_PROFILE".into(), profile.to_string()));
    }

    let (program, args) = login_shell();

    let initial_input = match stand_name {
        "echoes" | "hd" => {
            // resume id は lane 単位の state file（書き手 = global SessionStart hook）を直読み。
            let resume_id = crate::lane::cc_session::last(&addr.project, lane_label(addr));
            let cmd = claude_command(addr.kind, fresh, resume_id.as_deref());
            Some(format!("{}\r", cmd))
        }
        "shell" => None,
        other => {
            // "tmux"（PR2 で退役）や未知 stand の DB descriptor を床 shell で受ける。
            tracing::warn!(
                "unknown/legacy stand '{}' — 床の login shell で起動します (addr={})",
                other,
                addr
            );
            None
        }
    };

    StandCommand {
        program,
        args,
        initial_input,
        env,
        cwd: project_cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 床は login shell、 cwd は project dir（旧 install-root ダンスの廃止を固定）。
    #[test]
    fn build_stand_command_floor_is_login_shell_at_project_dir() {
        let addr = LaneAddress::conductor("vp");
        let cmd = build_stand_command("shell", &addr, Path::new("/work/vp"), false);
        assert_eq!(cmd.cwd, "/work/vp", "cwd は project dir 直（install root ではない）");
        assert!(cmd.initial_input.is_none(), "shell stand は床のみ");
        #[cfg(not(windows))]
        assert!(
            cmd.args.contains(&"-l".to_string()),
            "床は login shell (-l)、 got: {:?}",
            cmd.args
        );
    }

    /// VP_* env が注入されること（VP_SESSION は lane display 形 = tmux 名は廃止）。
    #[test]
    fn build_stand_command_injects_vp_env() {
        let addr = LaneAddress::performer("vantage-point", "sub");
        let cmd = build_stand_command("echoes", &addr, Path::new("/work/vp"), false);

        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(env.get("VP_CWD").map(String::as_str), Some("/work/vp"));
        assert_eq!(
            env.get("VP_SESSION").map(String::as_str),
            Some("vantage-point/performer/sub"),
            "VP_SESSION = LaneAddress Display 形"
        );
        assert_eq!(
            env.get("VP_PROJECT").map(String::as_str),
            Some("vantage-point")
        );
        assert_eq!(env.get("VP_LANE").map(String::as_str), Some("sub"));
    }

    /// echoes は claude を initial_input で注入（wire hook 同梱）。
    #[test]
    fn echoes_injects_claude_via_initial_input() {
        let addr = LaneAddress::performer("vp", "w1");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"), false);
        let input = cmd.initial_input.expect("echoes は initial_input あり");
        assert!(input.starts_with("claude"), "claude 起動 command: {input}");
        assert!(input.contains("wire hook-check"), "wire hook 同梱: {input}");
        assert!(input.ends_with('\r'), "Enter (CR) で submit: {input:?}");
    }

    /// 退役 stand ("tmux") / 未知 stand は床 shell に graceful 吸収。
    #[test]
    fn legacy_and_unknown_stands_fall_back_to_floor() {
        let addr = LaneAddress::conductor("vp");
        for stand in ["tmux", "opus-xhigh"] {
            let cmd = build_stand_command(stand, &addr, Path::new("/tmp"), false);
            assert!(
                cmd.initial_input.is_none(),
                "{stand} は床のみ（initial_input なし）"
            );
        }
    }

    /// claude_command の分岐: fresh / resume / continue / performer-fresh。
    #[test]
    fn claude_command_variants() {
        // fresh は resume/continue を含まない
        let fresh = claude_command(LaneKind::Conductor, true, Some("abc-123"));
        assert!(!fresh.contains("--resume") && !fresh.contains("--continue"));

        // conductor + id → --resume '<id>' || fresh
        let resume = claude_command(LaneKind::Conductor, false, Some("abc-123"));
        assert!(resume.contains("--resume 'abc-123'"), "{resume}");
        assert!(resume.contains("||"), "session 消失時の fresh fallback: {resume}");

        // conductor + id なし → --continue || fresh（初回/移行直後の従来 chain）
        let cont = claude_command(LaneKind::Conductor, false, None);
        assert!(cont.contains("--continue"), "{cont}");

        // performer + id → --resume（SP restart 後の会話継続、 dashboard 罠は id 指名で回避）
        let perf = claude_command(LaneKind::Performer, false, Some("abc-123"));
        assert!(perf.contains("--resume 'abc-123'"), "{perf}");

        // performer + id なし → fresh（--continue は dashboard 罠のため使わない）
        let perf_fresh = claude_command(LaneKind::Performer, false, None);
        assert!(
            !perf_fresh.contains("--continue") && !perf_fresh.contains("--resume"),
            "{perf_fresh}"
        );
    }

    /// 不正な session id（quote 破り / 空）は resume に採用されない。
    #[test]
    fn unsafe_session_ids_are_rejected() {
        for bad in ["", "a'b", "x;rm -rf /", "id with space"] {
            let cmd = claude_command(LaneKind::Conductor, false, Some(bad));
            assert!(
                !cmd.contains("--resume"),
                "不正 id '{bad}' は resume に使わない: {cmd}"
            );
        }
    }
}
