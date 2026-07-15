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
//!
//! ## エンジン別 stand
//!
//! `stand_name` で注入する Act3 を切り替える。 claude と cursor-agent は CLI surface が酷似
//! （TUI 起動 / ID 指名 resume / 事前 create-chat）なので、 同じ Act1-layered 構造に載る:
//! - `"echoes"`: claude（[`claude_command`]、 cc_session `--resume` + wire hook + model alias）
//! - `"cursor"`: cursor-agent（[`cursor_command`]、 cursor_session `--resume`、 hook/model は無し）
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
    /// replay buffer の disk 永続 path（`build_stand_command` が lane address から算出）。
    ///
    /// Some のとき PtySlot は spawn 時に前回画面を seed + 定期/Drop で flush する
    /// （SP / daemon 再起動をまたいで console を復元する。 [`crate::daemon::pty_slot`]）。
    pub replay_path: Option<std::path::PathBuf>,
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
    let (mut slot, mut rx) = PtySlot::spawn(
        cmd.cwd.as_str(),
        &cmd.program,
        &cmd.args,
        &cmd.env,
        cols,
        rows,
        cmd.replay_path.clone(),
    )?;

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
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
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
///
/// `model`（co-evolution #1）: lane 単位の model alias（`engine_model` 由来）。 Some のとき
/// **全 claude 起動**（fresh / resume / continue の全分岐、`||` fallback 先も含む）に `--model
/// <alias>` を注入する。 alias は `engine_model::is_valid_model` が `[A-Za-z0-9._-]`（先頭 `-`
/// 不可）を保証済みなので、 shell metachar / 空白を含まず unquoted 埋め込みで injection 安全。
fn claude_command(
    kind: LaneKind,
    fresh: bool,
    resume_id: Option<&str>,
    model: Option<&str>,
) -> String {
    // `--model <alias> `（末尾 space 込み、None なら空文字）。 全分岐の "claude " 直後に挿す。
    let model_flag = match model.filter(|m| crate::lane::engine_model::is_valid_model(m)) {
        Some(m) => format!("--model {} ", m),
        None => String::new(),
    };
    let fresh_cmd = format!("claude {}--settings '{}'", model_flag, WIRE_HOOKS);
    if fresh {
        return fresh_cmd;
    }
    match (kind, resume_id.filter(|id| is_safe_session_id(id))) {
        (_, Some(id)) => format!(
            "claude {}--resume '{}' --settings '{}' || {}",
            model_flag, id, WIRE_HOOKS, fresh_cmd
        ),
        (LaneKind::Conductor, None) => format!(
            "claude {}--continue --settings '{}' || {}",
            model_flag, WIRE_HOOKS, fresh_cmd
        ),
        (LaneKind::Performer, None) => fresh_cmd,
    }
}

/// Act3（cursor stand）: cursor-agent 起動 command line を組み立てる。
///
/// cursor-agent は claude と CLI surface が酷似する。 claude の `--continue`（「cwd の最新」を
/// 拾う dashboard 罠、 上記 [`claude_command`] 参照）と同型のリスクを避けるため、 cursor でも
/// latest resume（`--continue` / 引数なし `resume`）は使わず、 `cursor_session::ensure_chat_id`
/// が create-chat で先取りした chatId を `--resume '<id>'` で指名する。
///
/// - `Some(id)`（`cursor_session::is_valid_chat_id` 検証済）: `cursor-agent --resume '<id>' ||
///   cursor-agent`（chatId 消失時は素の cursor-agent に fallback、 shell の `||` が native 処理）
/// - `None`: `cursor-agent`（新規チャット）
///
/// **model 注入はしない**（v1）: `engine_model` は claude alias 前提の state で、 cursor の model は
/// cursor-agent TUI 内の `/model` で選ぶ。 claude 経路（[`claude_command`]）とは意図的に別系統。
///
/// wire hook（`--settings '{WIRE_HOOKS}'`）も注入しない: cursor に相当する hook 機構が無いため。
fn cursor_command(resume_id: Option<&str>) -> String {
    match resume_id.filter(|id| crate::lane::cursor_session::is_valid_chat_id(id)) {
        Some(id) => format!("cursor-agent --resume '{}' || cursor-agent", id),
        None => "cursor-agent".to_string(),
    }
}

/// Stand 名に応じた spawn command を構築する（tmux decoupling PR2: Rust-native、 script 層なし）。
///
/// - `"echoes"`（+ 旧名 `"hd"`）: 床 + claude 注入（`fresh` / cc_session により resume 分岐）
/// - `"cursor"`: 床 + cursor-agent 注入（`fresh` / cursor_session により resume 分岐）
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

    // mise trust footgun 回避（env-only、 mise は exec しない = 依存境界維持、 PR2 実機検証で発見）:
    // 床 = login shell 化により、 user rc の mise activate が新 worktree (`.vp/lanes/*`) の
    // 未 trust config に interactive prompt（"Trust them?"）を出して床を塞ぎ、 initial_input
    // （claude 起動 command）がダイアログに食われる。 lane cwd を MISE_TRUSTED_CONFIG_PATHS に
    // 足して抑止する（worktree の mise config は repo root と同一内容 = 信頼済みと同義）。
    // mise 不在環境では読まれない無害な env。 既存値には platform separator で追記。
    {
        let mut trusted: Vec<std::path::PathBuf> = std::env::var_os("MISE_TRUSTED_CONFIG_PATHS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        trusted.push(project_dir.to_path_buf());
        if let Ok(joined) = std::env::join_paths(trusted) {
            env.push((
                "MISE_TRUSTED_CONFIG_PATHS".into(),
                joined.to_string_lossy().into_owned(),
            ));
        }
    }

    let (program, args) = login_shell();

    let initial_input = match stand_name {
        "echoes" | "hd" => {
            // resume id は lane 単位の state file（書き手 = global SessionStart hook）を直読み。
            let resume_id = crate::lane::cc_session::last(&addr.project, lane_label(addr));
            // model は lane 単位の state file（`engine_model`、Act I/II 共有）を直読み。
            // 未記録 = None = claude default（co-evolution #1）。 respawn（SP restart）でも
            // ここで毎回読むため、 一度指定した model は再起動をまたいで維持される。
            let model = crate::lane::engine_model::last(&addr.project, lane_label(addr));
            let cmd = claude_command(addr.kind, fresh, resume_id.as_deref(), model.as_deref());
            Some(format!("{}\r", cmd))
        }
        "cursor" => {
            if fresh {
                // fresh（"New Session"）は新規チャット。 create-chat は exec せず素の cursor-agent を
                // 起動する（fresh path を exec-free に保つ = 決定的、 テストで固定できる）。 記録済の
                // 旧 chatId は clear して次回の非 fresh spawn で採番し直す。
                let _ = crate::lane::cursor_session::clear(&addr.project, lane_label(addr));
                Some("cursor-agent\r".to_string())
            } else {
                // 非 fresh: chatId を確保（既存あれば再利用、 無ければ create-chat で採番）して
                // `--resume '<id>'` で指名 resume する。 exec 失敗は None に倒れ、 素の
                // cursor-agent 起動になる（fail-open、 `cursor_session` doc 参照）。
                let id = crate::lane::cursor_session::ensure_chat_id(
                    &addr.project,
                    lane_label(addr),
                    project_dir,
                );
                Some(format!("{}\r", cursor_command(id.as_deref())))
            }
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
        // console replay の disk 永続 path（lane 単位）。 SP 再起動をまたぐ画面復元に使う。
        replay_path: Some(crate::daemon::pty_slot::replay_file_path(
            &addr.project,
            lane_label(addr),
        )),
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
        assert_eq!(
            cmd.cwd, "/work/vp",
            "cwd は project dir 直（install root ではない）"
        );
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
        // mise trust footgun 回避: lane cwd が MISE_TRUSTED_CONFIG_PATHS に含まれる
        assert!(
            env.get("MISE_TRUSTED_CONFIG_PATHS")
                .is_some_and(|v| v.contains("/work/vp")),
            "lane cwd が mise trust に含まれるはず: {:?}",
            env.get("MISE_TRUSTED_CONFIG_PATHS")
        );
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
        let fresh = claude_command(LaneKind::Conductor, true, Some("abc-123"), None);
        assert!(!fresh.contains("--resume") && !fresh.contains("--continue"));

        // conductor + id → --resume '<id>' || fresh
        let resume = claude_command(LaneKind::Conductor, false, Some("abc-123"), None);
        assert!(resume.contains("--resume 'abc-123'"), "{resume}");
        assert!(
            resume.contains("||"),
            "session 消失時の fresh fallback: {resume}"
        );

        // conductor + id なし → --continue || fresh（初回/移行直後の従来 chain）
        let cont = claude_command(LaneKind::Conductor, false, None, None);
        assert!(cont.contains("--continue"), "{cont}");

        // performer + id → --resume（SP restart 後の会話継続、 dashboard 罠は id 指名で回避）
        let perf = claude_command(LaneKind::Performer, false, Some("abc-123"), None);
        assert!(perf.contains("--resume 'abc-123'"), "{perf}");

        // performer + id なし → fresh（--continue は dashboard 罠のため使わない）
        let perf_fresh = claude_command(LaneKind::Performer, false, None, None);
        assert!(
            !perf_fresh.contains("--continue") && !perf_fresh.contains("--resume"),
            "{perf_fresh}"
        );
    }

    /// 不正な session id（quote 破り / 空）は resume に採用されない。
    #[test]
    fn unsafe_session_ids_are_rejected() {
        for bad in ["", "a'b", "x;rm -rf /", "id with space"] {
            let cmd = claude_command(LaneKind::Conductor, false, Some(bad), None);
            assert!(
                !cmd.contains("--resume"),
                "不正 id '{bad}' は resume に使わない: {cmd}"
            );
        }
    }

    /// model 指定（co-evolution #1）: 全分岐の全 claude 起動に `--model <alias>` が乗る。
    #[test]
    fn model_flag_injected_into_all_claude_invocations() {
        // fresh: 単一 claude に --model
        let fresh = claude_command(LaneKind::Performer, true, None, Some("sonnet"));
        assert_eq!(
            fresh,
            "claude --model sonnet --settings '{\"hooks\":{\"UserPromptSubmit\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"vp wire hook-check\"}]}]}}'",
            "fresh は --model 付き単一 claude: {fresh}"
        );

        // resume: 主 claude と `||` fallback 先の fresh、 両方に --model が乗る
        let resume = claude_command(LaneKind::Performer, false, Some("abc-123"), Some("opus"));
        assert_eq!(
            resume.matches("--model opus").count(),
            2,
            "resume 主 + fallback fresh の両方に --model: {resume}"
        );
        assert!(
            resume.contains("--model opus --resume 'abc-123'"),
            "{resume}"
        );

        // continue: 主 claude と fallback fresh の両方
        let cont = claude_command(LaneKind::Conductor, false, None, Some("claude-fable-5"));
        assert_eq!(cont.matches("--model claude-fable-5").count(), 2, "{cont}");
    }

    /// 不正な model 名（injection 形 / 空 / 先頭 `-`）は `--model` に採用されない。
    #[test]
    fn unsafe_models_are_rejected() {
        for bad in ["", "opus --dangerously", "a;rm -rf /", "-x", "mo del"] {
            let cmd = claude_command(LaneKind::Performer, true, None, Some(bad));
            assert!(
                !cmd.contains("--model"),
                "不正 model '{bad}' は --model に使わない: {cmd}"
            );
        }
    }

    /// WIRE_HOOKS は妥当な JSON かつ single quote を含まない（`'…'` 埋め込みの前提条件）。
    /// raw string literal はコンパイル時に検証されず、壊れると lane 起動の実機でしか発覚しない。
    #[test]
    fn wire_hooks_is_valid_json_without_single_quotes() {
        let parsed: serde_json::Value =
            serde_json::from_str(WIRE_HOOKS).expect("WIRE_HOOKS は妥当な JSON");
        assert!(
            parsed.pointer("/hooks/UserPromptSubmit").is_some(),
            "UserPromptSubmit hook を含む: {parsed}"
        );
        assert!(
            !WIRE_HOOKS.contains('\''),
            "single quote を含むと `--settings '...'` の quote が破れる"
        );
    }

    /// fresh=true は builder 経由（統合）でも resume/continue を含まない
    /// （sidebar "New Conductor Session" の契約。 実行環境の cc_session state に依存しない）。
    #[test]
    fn fresh_true_never_resumes_via_builder() {
        let addr = LaneAddress::conductor("vp");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"), true);
        let input = cmd.initial_input.expect("echoes は initial_input あり");
        assert!(
            !input.contains("--resume") && !input.contains("--continue"),
            "fresh は素の claude: {input}"
        );
    }

    /// cursor_command の分岐: id あり → `--resume '<id>' || cursor-agent`、 なし → 素の cursor-agent。
    #[test]
    fn cursor_command_variants() {
        // id あり → 指名 resume + `||` fallback。
        let resume = cursor_command(Some("chat_abc-123"));
        assert_eq!(
            resume,
            "cursor-agent --resume 'chat_abc-123' || cursor-agent"
        );

        // id なし → 素の cursor-agent（新規チャット）。
        let fresh = cursor_command(None);
        assert_eq!(fresh, "cursor-agent");

        // model / wire hook は注入しない（v1 スコープ、 claude 経路とは別系統）。
        assert!(!resume.contains("--model") && !resume.contains("--settings"));
        assert!(!fresh.contains("--model") && !fresh.contains("--settings"));
    }

    /// 不正な chatId（quote 破り / 空 / 空白）は resume に採用されず素の cursor-agent に倒れる。
    #[test]
    fn cursor_command_rejects_unsafe_chat_ids() {
        for bad in ["", "a'b", "x;rm -rf /", "id with space", "has.dot"] {
            let cmd = cursor_command(Some(bad));
            assert_eq!(
                cmd, "cursor-agent",
                "不正 chatId '{bad}' は resume に使わず素の cursor-agent: {cmd}"
            );
        }
    }

    /// build_stand_command("cursor", …, fresh=true) は床 login shell + `cursor-agent\r`
    /// の initial_input を返す（決定的 — create-chat の exec を経由しない）。
    #[test]
    fn cursor_fresh_injects_bare_cursor_agent() {
        let addr = LaneAddress::conductor("vp");
        let cmd = build_stand_command("cursor", &addr, Path::new("/tmp"), true);
        #[cfg(not(windows))]
        assert!(
            cmd.args.contains(&"-l".to_string()),
            "床は login shell (-l)、 got: {:?}",
            cmd.args
        );
        assert_eq!(
            cmd.initial_input.as_deref(),
            Some("cursor-agent\r"),
            "fresh は素の cursor-agent を Enter 付きで注入（exec なし）"
        );
    }
}
