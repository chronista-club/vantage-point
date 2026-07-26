//! StandSpawner — Stand 名に応じた spawn command 構築（tmux decoupling PR2 で Rust-native 化）
//!
//! ## 構造（design doc §13: Act1-layered）
//!
//! ```text
//! PtySlot → $LOGIN_SHELL -l                    ← Act1: 常に生きる「shell 層」（self-healing）
//!    ↓ initial_input（spawn 後に PTY へ type-ahead 注入）
//!    claude --resume 'ID' … || claude …        ← Act3（|| fallback は shell が native 処理）
//! ```
//!
//! 旧構造（PtySlot → bash script → tmux new-session → claude）の bash/mise/tmux 層は全廃。
//! - env 注入は PtySlot spawn の 1 箇所（TERM/LANG/PATH は `spawn_env`、 VP_* はここで構築）
//! - claude 終了後は slot の login shell prompt に自然に戻る（旧 `; exec $SHELL -l` chain 不要）
//! - `--resume` 失敗 → fresh claude の fallback は shell の `||` が担う
//!
//! ## エンジン別 stand（対応表の SSOT は [`crate::echoes::EngineKind`]、doc 37）
//!
//! `stand_name` で注入する Act3 を切り替える。各 engine CLI は「TUI 起動 / ID 指名 resume」の
//! surface が揃っているため、同じ Act1-layered 構造に載る:
//! - `"echoes"`（claude）: [`claude_command`]、 cc_session `--resume` + wire hook + model alias
//! - `"codex"`: [`codex_command`]、 codex_session `resume`（採番は Act II の record-from-init のみ
//!   — codex に create-chat 相当が無いため、Act I 単独ではまず素の `codex` で開始する）
//! - `"grok"`: [`grok_command`]、registry の会話 id を `-r '<id>'` で指名 resume（doc 42）
//! - `"opencode"`: [`opencode_command`]、registry の会話 id を `-s '<id>'` で指名 resume（doc 43。
//!   model は opencode config が SSOT — VP は注入しない）
//! - resume id は spawn 前に Rust が各 `lane::*_session`（または registry）を直読み
//!   （旧: bash が `vp lane last-session` を子→親 CLI 呼び出し = 層の逆転、解消）
//!
//! ## VP_* 環境変数
//!
//! - `VP_REPO`     : `addr.repo`
//! - `VP_LANE`        : lane label（`conductor` / performer 名 / `unnamed`）
//! - `VP_SESSION_KEY` : この slot が化身する session の key（doc 40 §4 — hook が「自分が
//!                      どの session か」を名乗るための identity。旧 `VP_SESSION`
//!                      （= lane display 形、doc 40 PR-3 で退役）とは**別物**なので名前も分けた）
//! - `VP_PROFILE`     : dev/brew namespace（設定時のみ）

use std::path::Path;

use anyhow::Result;
use tokio::sync::broadcast;

use super::lanes_state::LaneAddress;
use crate::daemon::pty_slot::PtySlot;

/// wiremsg R2-c（チャネル B、決定 D2）: wire 未読通知 hook を VP が spawn 時に注入する。
/// dotfile 非依存で箱から動く。 hook 実体は `vp wire hook-check`（vp CLI 同梱）。
/// UserPromptSubmit で未読 wire を additionalContext 通知、 daemon 不在時は silent 成功
/// （fail-open）。 JSON に single quote が無いため `'…'` 埋め込みで安全に quote できる。
///
/// NOTE（doc 40 §4/§6）: hook は会話 id の**報告者** — SessionStart = Issued（発行時点の
/// eager 表示）/ UserPromptSubmit = Spoken（authoritative）を repo へ送るだけで、記録判断
/// （報告 session の解決 + F1/F2 guard = 「resume 失敗 `||` fallback の幻 session が健在な
/// 旧会話を上書きしない」）は repo の `session_registry::record_conversation` 1 箇所が持つ。
/// hook が名乗る session は spawn 時に注入する `VP_SESSION_KEY`（下の env 表）。
/// 旧「UserPromptSubmit のみ記録」（#795）はこの guard を hook 側の鈍器で担っていた名残。
const WIRE_HOOKS: &str = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"vp wire hook-check"}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"vp wire hook-check"}]}]}}"#;

/// Stand spawn 用 command（program + args + env + cwd + 初期入力）
#[derive(Debug, Clone)]
pub struct StandCommand {
    pub program: String,
    pub args: Vec<String>,
    /// spawn 後に PTY へ type-ahead 注入する初期入力（= Act3 の claude 起動 command line）。
    ///
    /// PTY line discipline が入力をバッファするため、 shell の rc 読込完了を待たずに書いてよい
    /// （shell が読み始めた時点で消費される）。 None = slot の shell のみ（Act1 で止まる）。
    pub initial_input: Option<String>,
    /// PtySlot spawn に渡す環境変数（VP_* identity）。 TERM/LANG/PATH は PtySlot 側の
    /// `spawn_env` が注入する（ここでは持たない — 注入点を 1 箇所に保つ）。
    pub env: Vec<(String, String)>,
    /// spawn 時の cwd = repo directory（旧 install-root ダンスは script 層と共に廃止）。
    pub cwd: String,
    /// replay buffer の disk 永続 path（`build_stand_command` が lane address から算出）。
    ///
    /// Some のとき PtySlot は spawn 時に前回画面を seed + 定期/Drop で flush する
    /// （repo / daemon 再起動をまたいで console を復元する。 [`crate::daemon::pty_slot`]）。
    pub replay_path: Option<std::path::PathBuf>,
}

/// 早期 exit 検知の wait 時間 (ms)。 slot の login shell がこの窓内に死ぬ = 環境異常
/// （shell 不在 / PTY 失敗等）。 死因は PTY 出力の tail を添えて bail する。
/// initial_input の書込みもこの窓の後（= shell の生存確認後）に行う。
const EARLY_EXIT_CHECK_MS: u64 = 800;

/// `StandCommand` を spawn し、 shell の生存確認後に initial_input を注入する。
///
/// shell（login shell）が `EARLY_EXIT_CHECK_MS` 以内に死んだら、 PTY 出力の末尾を添えて
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

    // shell が早期 exit するか peek（rc 読込より短い可能性はあるが、 type-ahead は line discipline
    // がバッファするので「注入が早すぎて落ちる」ことはない — ここは純粋に死活確認）。
    std::thread::sleep(std::time::Duration::from_millis(EARLY_EXIT_CHECK_MS));

    if !slot.is_alive() {
        // 死因究明ログ（要所）: 早期 exit した shell が PTY に書いた stderr/stdout を drain して
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
        // shell は生きているので lane 自体は成立させる（user が手打ちで claude を起動できる）。
        tracing::warn!(
            "initial_input write failed (shell は生存): err={} program={} input_len={}",
            e,
            cmd.program,
            input.len()
        );
    }
    Ok((slot, rx))
}

/// 早期 exit した shell の死因究明用に、 PTY broadcast channel に buffer された直近出力を
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

/// LaneAddress の lane label を導出する。
///
/// doc 44 P2（フラット化）で `name` が全 lane 必須になったため、そのまま返すだけになった。
/// 旧実装は kind で 3 分岐していた（Conductor → "root" / Performer(name) → name /
/// Performer(None) → "unnamed"）— 最後の枝は型が許した表現不能な状態の穴埋めで、
/// フラット化で**表現できなくなった**（`name: String` は常に在る）。
pub(crate) fn lane_label(addr: &LaneAddress) -> &str {
    &addr.name
}

/// slot の shell になる login shell を (program, args) で解決する。
///
/// - **Unix**: `$SHELL` を尊重（repo が launchd 起動だと SHELL env 不在のことがある →
///   `/bin/zsh` → `/bin/bash` → `/bin/sh` の順で実在 shell に fallback）。 `-l` で
///   login shell 化し、 mise / volta / nvm 等の PATH を rc 経由で取り込む（Act1 = env の shell 層）。
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
                "git-bash (Git for Windows) が見つかりません。 lane の slot の shell を起動できません。 \
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
/// **分岐は「VP が会話 id を知っているか」の 1 本だけ**（doc 53 §12.1、2026-07-25）:
///
/// - id あり: `--resume '<id>' || resume-failed || fresh`（session 消失時は
///   失敗を記録してから fresh に fallback — 無音 fallback がポインタ自壊の証拠を消していた F4 対策）
/// - id なし: 素の claude（fresh）
///
/// ⚠️ **旧「conductor + id なし → `--continue`」は退役した**（mako 判断）。`--continue` は
/// 「cwd の最新会話」= **VP の帳簿の外**へ手を伸ばす推測で、registry が会話 id の SSOT に
/// なった今は「VP が見せている会話」と「実際に繋がる会話」が乖離しうる（doc 54 の
/// 「隠れた変換」の一種。同 §3.7 の `|| claude` fallback 退役と同じ判断）。
/// 副産物として **`bare` が `conversation.is_none()` から完全に導出**でき、
/// 「今回は素で立てる」という呼び手の 1 bit（旧 `fresh` 引数 / `RespawnMode`）が消える。
/// 代償: VP の registry を失った状態で起点 lane を開くと以前の会話を継がず新規で始まる。
///
/// `model`（co-evolution #1）: lane 単位の model alias（`engine_model` 由来）。 Some のとき
/// **全 claude 起動**（fresh / resume / continue の全分岐、`||` fallback 先も含む）に `--model
/// <alias>` を注入する。 alias は `engine_model::is_valid_model` が `[A-Za-z0-9._-]`（先頭 `-`
/// 不可）を保証済みなので、 shell metachar / 空白を含まず unquoted 埋め込みで injection 安全。
fn claude_command(resume_id: Option<&str>, model: Option<&str>) -> String {
    // `--model <alias> `（末尾 space 込み、None なら空文字）。 全分岐の "claude " 直後に挿す。
    let model_flag = match model.filter(|m| crate::lane::engine_model::is_valid_model(m)) {
        Some(m) => format!("--model {} ", m),
        None => String::new(),
    };
    let fresh_cmd = format!("claude {}--settings '{}'", model_flag, WIRE_HOOKS);
    // `|| vp lane resume-failed '<x>' ||` の 3 連 chain: resume-failed は「記録して常に
    // exit 1」の中継専用コマンドで、失敗を伝播させて次の fresh fallback へ繋ぐ。
    // shell group `{ …; }` を使わないのは fish 互換のため（slot の shell は user の login shell）。
    // vp が PATH に無くても command-not-found = 非ゼロで chain は進む（fail-open）。
    //
    // doc 53 §12.1: 旧 `is_root` 分岐（起点 lane だけ `--continue`）は退役。理由は上の doc —
    // 「cwd の最新」は VP の帳簿の外で、performer 側では既に dashboard 罠として使っていな
    // かった（= 同じ罠を起点 lane にだけ許していた非対称でもあった）。
    match resume_id.filter(|id| is_safe_session_id(id)) {
        Some(id) => format!(
            "claude {}--resume '{}' --settings '{}' || vp lane resume-failed '{}' || {}",
            model_flag, id, WIRE_HOOKS, id, fresh_cmd
        ),
        None => fresh_cmd,
    }
}

/// Act3（codex stand）: codex 起動 command line を組み立てる。
///
/// codex の TUI resume は `codex resume '<id>'`（id は UUID の指名 — `--last` は claude
/// `--continue` と同型の「最新」曖昧性があるため使わない、doc 37 §7）。id の供給源は
/// Act II（[`crate::echoes::codex_host`] の record-from-init）だけ — codex には cursor の
/// create-chat 相当（id 先取り）が無いため、Act I 単独ではまず素の `codex` で始まり、
/// Act II を一度でも通ると以後は resume で継がれる。
///
/// - `Some(id)`（`codex_session::is_valid_thread_id` 検証済）: `codex resume '<id>' || codex`
///   （thread 消失時は素の codex に fallback、 shell の `||` が native 処理）
/// - `None`: `codex`（新規会話）
///
/// wire hook / model 注入はしない（hook 機構は claude 専用。model は codex 側で選択 —
/// `EngineKind::model_switchable` 参照）。
fn codex_command(resume_id: Option<&str>) -> String {
    match resume_id.filter(|id| crate::lane::codex_session::is_valid_thread_id(id)) {
        Some(id) => format!("codex resume '{}' || codex", id),
        None => "codex".to_string(),
    }
}

/// grok の Act I 起動 command（doc 42 — TUI は `-r '<id>'` で ACP sessionId を指名 resume）。
///
/// - `Some(id)`（英数+ハイフン検証済 = `--resume '<id>'` injection 防壁）: `grok -r '<id>' || grok`
///   （session 消失時は素の grok に fallback、shell の `||` が native 処理）
/// - `None`: `grok`（新規会話）
fn grok_command(resume_id: Option<&str>) -> String {
    match resume_id.filter(|id| is_safe_session_id(id)) {
        Some(id) => format!("grok -r '{}' || grok", id),
        None => "grok".to_string(),
    }
}

/// opencode session id（`ses_` prefix + 英数字）として安全な形式か。
///
/// opencode の ACP sessionId は `ses_…`（実測 `ses_089ead04bffe5oIJcQTHwwTZo8`、doc 43 §1）で
/// underscore を含むため [`is_safe_session_id`]（英数 + ハイフン）では弾かれる。underscore は
/// prefix のみ・残りは英数字なので `-s '<id>'` の single-quote 埋め込みでも injection にならない。
fn is_safe_opencode_session_id(id: &str) -> bool {
    id.strip_prefix("ses_")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// opencode の Act I 起動 command（doc 43 §4 — TUI は `-s '<id>'` で ACP sessionId を指名 resume）。
///
/// - `Some(id)`（[`is_safe_opencode_session_id`] 検証済 = injection 防壁）: `opencode -s '<id>' || opencode`
///   （session 消失時は素の opencode に fallback、shell の `||` が native 処理）
/// - `None`: `opencode`（新規会話）
///
/// model / provider は opencode config が SSOT（VP は `-m` 等を注入しない、doc 43 §3）。
fn opencode_command(resume_id: Option<&str>) -> String {
    match resume_id.filter(|id| is_safe_opencode_session_id(id)) {
        Some(id) => format!("opencode -s '{}' || opencode", id),
        None => "opencode".to_string(),
    }
}

/// Stand 名に応じた spawn command を構築する（tmux decoupling PR2: Rust-native、 script 層なし）。
///
/// - `"echoes"`（+ 旧名 `"hd"`）: slot + claude 注入（`fresh` / cc_session により resume 分岐）
/// - `"codex"` / `"grok"` / `"opencode"`: slot + engine CLI 注入（`fresh` / registry の会話 id により resume 分岐）
/// - `"shell"`: shell のみ
/// - `"tmux"`（退役 stand）/ 未知名（撤去済み `"cursor"` / `"agy"` 含む）: shell のみ + warn
///   （DB descriptor の legacy 値を graceful 吸収）
///
/// doc 53 §12.1: 旧 `fresh` 引数（resume/continue を回避して素で立てる指示）は退役した。
/// 「素で立てるか」は registry の会話 id の有無から導出される — 呼び手は intent（registry）を
/// 書くだけでよく、spawn の作法を知らなくてよい。
///
/// **この形は root session の slot 専用**（既存の boot / respawn / restart 経路）。非 root
/// session に slot を立てる producer（doc 46 P5）は
/// [`build_stand_command_for_session`] に session key を渡すこと。
pub fn build_stand_command(stand_name: &str, addr: &LaneAddress, repo_dir: &Path) -> StandCommand {
    build_stand_command_for_session(stand_name, addr, repo_dir, None)
}

/// [`build_stand_command`] の session 明示版（doc 46 P5 — slot は lane に 1 枚ではなく
/// session ごと）。`session` = **この slot が化身する session**（`None` = root）。
///
/// この 1 引数が決めるのは 4 つで、いずれも「root の値」ではなく**その session の値**である
/// ことが producer の正しさの核（root 決め打ちだと 2 本目の claude が root の会話を乗っ取る）:
///
/// | 決まるもの | 由来 |
/// |---|---|
/// | `VP_SESSION_KEY` | この session の key（hook がこれを名乗り、repo は報告 session に会話 id を書く、doc 40 §4-1） |
/// | engine（Act3 の arm） | この session の entry の `stand` |
/// | resume id | この session の entry の `conversation` |
/// | replay 永続 | **全 slot** が session 別 file へ（doc 50 §4.6 A6、後述） |
///
/// replay（`replay_path`）は元々 root 限定だった。理由は「file が lane 単位の 1 本しかないので
/// 非 root に同じ file を渡すと 2 本の console が奪い合う」「非 root は再起動で復元されないので
/// **読み手のいない書き込み**になる」「lane GC が知る file は 1 本だけなので消し漏れが生まれる」。
/// **A6 でこの 3 つの前提すべてを解いた**:
///
/// - file は **session 単位**（`__<session>`）になり奪い合いが構造的に消えた
///   （身元を role にすると root 付け替えで再発する — [`crate::daemon::pty_slot::replay_file_path_session_in`]）
/// - 非 root term も boot で復元される（`lane_reconcile::reconcile_lane`、doc 53 §12）ので読み手が居る
/// - lane GC / Reset は prefix 掃き（`clear_replay_in`）で session file 群も消す
pub fn build_stand_command_for_session(
    stand_name: &str,
    addr: &LaneAddress,
    repo_dir: &Path,
    session: Option<crate::lane::session_registry::SessionKey>,
) -> StandCommand {
    let repo_cwd = repo_dir.to_string_lossy().to_string();

    // doc 40 PR-3: VP_CWD / VP_SESSION は退役（repo 内読み手ゼロ + user statusline 消費なしを
    // 確認済み、doc 40 §8）。identity env は wire/hook が読む VP_REPO / VP_LANE +
    // session を名乗る VP_SESSION_KEY（registry load 後に push、doc 40 §4）。
    let mut env = vec![
        ("VP_REPO".into(), addr.repo.clone()),
        ("VP_LANE".into(), lane_label(addr).into()),
    ];
    // VP_PROFILE 分離: dev profile を子プロセス（claude の statusline / vp CLI 呼び出し）へ
    // 明示伝播する。 未設定 (brew) は push しない。
    if let Some(profile) = vp_paths::vp_profile() {
        env.push(("VP_PROFILE".into(), profile.to_string()));
    }

    // mise trust footgun 回避（env-only、 mise は exec しない = 依存境界維持、 PR2 実機検証で発見）:
    // slot の shell = login shell 化により、 user rc の mise activate が新 worktree (`.vp/lanes/*`) の
    // 未 trust config に interactive prompt（"Trust them?"）を出して shell への入力を塞ぎ、 initial_input
    // （claude 起動 command）がダイアログに食われる。 lane cwd を MISE_TRUSTED_CONFIG_PATHS に
    // 足して抑止する（worktree の mise config は repo root と同一内容 = 信頼済みと同義）。
    // mise 不在環境では読まれない無害な env。 既存値には platform separator で追記。
    {
        let mut trusted: Vec<std::path::PathBuf> = std::env::var_os("MISE_TRUSTED_CONFIG_PATHS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        trusted.push(repo_dir.to_path_buf());
        if let Ok(joined) = std::env::join_paths(trusted) {
            env.push((
                "MISE_TRUSTED_CONFIG_PATHS".into(),
                joined.to_string_lossy().into_owned(),
            ));
        }
    }

    let (program, args) = login_shell();

    // doc 39 P1 → doc 40: resume id / 会話 id は **session registry の entry**（SSOT、doc 40 §5）。
    // 既定（`session=None`）で化身するのは root session（lane の人格）で、doc 46 P5 の producer
    // だけが非 root を名指しする。registry file 不在 = root=1 の N=1 特殊ケースで従来互換。
    // engine_model は lane 単位（Act I/II 共有）のまま。
    let reg = crate::lane::session_registry::load(&addr.repo, lane_label(addr), stand_name);
    // A6 の後始末: 旧名 replay file（lane 単位）を現 root の session file へ 1 回だけ移設する。
    // slot の replay_path を決める経路はここ 1 本なので、移設もここに置けば取りこぼさない
    // （呼び手ごとに migration を書かせると 1 つ忘れる = [[normalize-at-module-boundary]]）。
    // 冪等・best-effort なので毎 spawn 通しても実質 `exists()` 1 回。
    crate::daemon::pty_slot::migrate_legacy_replay(&addr.repo, lane_label(addr), reg.root);
    // この slot が化身する session（`None` = root = 従来の全経路）。
    let key = session.unwrap_or(reg.root);
    let entry = reg.sessions.iter().find(|s| s.key == key);
    let conversation = entry.and_then(|s| s.conversation.clone());
    // doc 40 §4 / doc 46 P5: この slot が化身する session を子プロセスへ名乗らせる。
    // hook（`vp wire hook-check`）はこの値を報告に載せ、repo は**報告された session** に
    // 会話 id を書く（root 固定だと、同じ lane の 2 本目の claude が root の会話を上書きして
    // `--resume` が同居人の会話に化ける — doc 46 §3 の producer blocker）。
    env.push(("VP_SESSION_KEY".into(), key.to_string()));
    // doc 39 P4-A: slot に載る engine は **その session の stand** が決める（lane 作成時固定の
    // `stand_name` ではない）。cross-engine の Root 切替（picker）で root を別 engine の session に
    // 向けると、respawn する slot もその engine で立つ。spawn 全経路（boot / respawn / restart /
    // doc 46 P5 の producer）が この 1 箇所を通るため、engine 追従の修正点はここ一つで足りる。
    // entry 不在（registry 破損 / 指定 key が実在しない）は防御的に `stand_name` へ fallback。
    let effective_stand = entry.map(|s| s.stand.as_str()).unwrap_or(stand_name);

    // stand 名 → engine の対応表は EngineKind が SSOT（stringly 比較をここに散らさない）。
    // 選択鍵は effective_stand（= その session の engine、doc 39 P4-A）— lane 固定の stand_name
    // でなく session の stand で arm を選ぶことが cross-engine root 解禁の核。
    let initial_input = match crate::echoes::EngineKind::from_stand(effective_stand) {
        Some(crate::echoes::EngineKind::Claude) => {
            // transcript_exists pre-flight（doc 33 C2 の Act II と対称化）: 発話ゼロで
            // transcript を書かなかった「幻 id」を `--resume` に渡さない。None に倒せば
            // 素の claude で立つ（doc 53 §12.1 で `--continue` fallback は退役）。
            let resume_id = conversation
                .clone()
                .filter(|id| crate::lane::cc_session::transcript_exists(id));
            // model は lane 単位の state file（`engine_model`、Act I/II 共有）を直読み。
            // 未記録 = None = claude default（co-evolution #1）。 respawn（repo restart）でも
            // ここで毎回読むため、 一度指定した model は再起動をまたいで維持される。
            let model = crate::lane::engine_model::last(&addr.repo, lane_label(addr));
            // doc 53 §12.1: 「素で立てるか」は **registry の会話 id の有無だけ**で決まる。
            // 旧実装は `fresh || (key >= 2 && resume_id.is_none())` で、`--continue` 分岐が
            // 起点 lane × id 無しに存在したため「Reset 直後（id を捨てた）」と「初回（まだ
            // id が無い）」を区別する 1 bit（`fresh`）が要っていた。`--continue` 退役で
            // 両者は同じ「VP が会話を知らない」に畳まれ、呼び手の 1 bit が消える。
            let cmd = claude_command(resume_id.as_deref(), model.as_deref());
            Some(format!("{}\r", cmd))
        }
        Some(crate::echoes::EngineKind::Codex) => {
            // doc 53 §12.1: resume 先は registry の会話 id 1 本（None = 素で立つ）。
            // 旧 `if fresh { 素 } else { resume }` は冗長だった — `codex_command(None)` は
            // 元から素の `codex` を返すので、呼び手の 1 bit は何も足していなかった。
            Some(format!("{}\r", codex_command(conversation.as_deref())))
        }
        Some(crate::echoes::EngineKind::Grok) => {
            // doc 53 §12.1: resume 先は registry の会話 id 1 本（None = 素で立つ）。
            // 旧 `if fresh { 素 } else { resume }` は冗長だった — `grok_command(None)` は
            // 元から素の `grok` を返すので、呼び手の 1 bit は何も足していなかった。
            Some(format!("{}\r", grok_command(conversation.as_deref())))
        }
        Some(crate::echoes::EngineKind::OpenCode) => {
            // doc 53 §12.1: resume 先は registry の会話 id 1 本（None = 素で立つ）。
            // 旧 `if fresh { 素 } else { resume }` は冗長だった — `opencode_command(None)` は
            // 元から素の `opencode` を返すので、呼び手の 1 bit は何も足していなかった。
            Some(format!("{}\r", opencode_command(conversation.as_deref())))
        }
        None if effective_stand == "shell" => None,
        None => {
            // "tmux"（PR2 で退役）/ 撤去済み "cursor"・"agy"（sweep 6.5）/ 未知 stand の
            // DB descriptor を shell 層で受ける（graceful degradation）。effective_stand は
            // root session の stand（cross-engine root で lane 固定 stand と食い違い得る）。
            tracing::warn!(
                "unknown/legacy stand '{}' (lane stand '{}') — slot の login shell で起動します (addr={})",
                effective_stand,
                stand_name,
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
        cwd: repo_cwd,
        // console replay の disk 永続 path（doc 50 §4.6 A6）。daemon 再起動をまたぐ画面復元に
        // 使う。**全 slot** が session 別 file を持つ（旧: root のみ）— A6 で root 以外の session も
        // term pane になれるため、非 root term の scrollback も daemon 再起動で残す。
        //
        // ⚠️ file の身元は **session**（`__<session>`）で、role（誰が root か）ではない。role で
        // 決めると root 付け替えで「新 root が旧 root の画面を seed」「新旧 2 本の slot が同じ
        // file を奪い合う」が起きる（[`crate::daemon::pty_slot::replay_file_path_session_in`]）。
        replay_path: Some(crate::daemon::pty_slot::replay_file_path_session(
            &addr.repo,
            lane_label(addr),
            key,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// slot の shell は login shell、 cwd は repo dir（旧 install-root ダンスの廃止を固定）。
    #[test]
    fn build_stand_command_floor_is_login_shell_at_repo_dir() {
        // build_stand_command は registry を読む（doc 39 P4-A: slot の engine は root session の
        // stand に追従）。実 vp_state_dir の conductor registry を拾わないよう tempdir に隔離する
        // （sibling の build_stand_command テスト群と同じ規律 — 未隔離だと実 registry の root=echoes を
        // 拾い「shell なのに claude を注入」になって間欠 fail する）。
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let cmd = build_stand_command("shell", &addr, Path::new("/work/vp"));
        assert_eq!(
            cmd.cwd, "/work/vp",
            "cwd は repo dir 直（install root ではない）"
        );
        assert!(cmd.initial_input.is_none(), "shell stand は shell のみ");
        #[cfg(not(windows))]
        assert!(
            cmd.args.contains(&"-l".to_string()),
            "slot の shell は login shell (-l)、 got: {:?}",
            cmd.args
        );
    }

    /// VP_* env が注入されること（doc 40 PR-3: VP_CWD / VP_SESSION は退役 — 注入されないことも固定）。
    #[test]
    fn build_stand_command_injects_vp_env() {
        // build_stand_command は registry を読む — 実 vp_state_dir を拾わないよう隔離（sibling 規律）。
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::performer("vantage-point", "sub");
        let cmd = build_stand_command("echoes", &addr, Path::new("/work/vp"));

        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert!(
            !env.contains_key("VP_CWD") && !env.contains_key("VP_SESSION"),
            "退役済み env は注入しない（doc 40 PR-3）"
        );
        assert_eq!(
            env.get("VP_REPO").map(String::as_str),
            Some("vantage-point")
        );
        assert_eq!(env.get("VP_LANE").map(String::as_str), Some("sub"));
        // doc 40 §4: slot が化身する session を名乗る env（registry 不在 = root=1）。
        assert_eq!(
            env.get("VP_SESSION_KEY").map(String::as_str),
            Some("1"),
            "hook が「自分がどの session か」を名乗るための identity"
        );
        // mise trust footgun 回避: lane cwd が MISE_TRUSTED_CONFIG_PATHS に含まれる
        assert!(
            env.get("MISE_TRUSTED_CONFIG_PATHS")
                .is_some_and(|v| v.contains("/work/vp")),
            "lane cwd が mise trust に含まれるはず: {:?}",
            env.get("MISE_TRUSTED_CONFIG_PATHS")
        );
    }

    /// `VP_SESSION_KEY` は「slot が化身する session」に追従する（lane 固定の 1 ではない）。
    /// root を #2 に移した lane で spawn すると、その claude の hook は #2 を名乗る
    /// = 会話 id が #2 に記録される（doc 40 §4 / doc 46 P5）。
    #[test]
    fn session_key_env_follows_the_root_session() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        crate::lane::session_registry::create_root(
            "vp",
            "root",
            "echoes",
            "echoes",
            crate::lane::session_registry::SessionAct::Tui,
        )
        .expect("create_root #2");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(
            env.get("VP_SESSION_KEY").map(String::as_str),
            Some("2"),
            "root を移したら slot が名乗る session も移る"
        );
    }

    /// doc 46 P5 producer の核: 指名した session の **entry がすべてを決める**（root ではない）。
    ///
    /// root(#1) = echoes（会話 id 付き）の lane で、同居人 #2（codex）の slot を組むと:
    /// - `VP_SESSION_KEY` は **2**（hook がこれを名乗る → 会話 id は #2 に記録され、root の
    ///   `--resume` が同居人の会話に化けない = doc 46 §3 の producer blocker の解）
    /// - engine は **codex**（root の claude に引きずられない）
    /// - resume は **#2 の会話 id**（root の id を継がない）
    /// - replay の disk 永続は **root だけ**（lane 単位の 1 file を 2 本で奪い合わせない）
    #[test]
    fn slot_command_follows_the_named_session_not_root() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        // root(#1) = echoes に会話 id を持たせる（混入したら判るよう別 id）。
        crate::lane::session_registry::set_conversation(
            "vp",
            "root",
            "echoes",
            1,
            Some("11111111-1111-1111-1111-111111111111"),
        )
        .expect("root の会話 id");
        // 同居人 #2 = codex（producer が採番するのと同じ形: Act=Tui / 非 focus）。
        let key = crate::lane::session_registry::create(
            "vp",
            "root",
            "echoes",
            "codex",
            crate::lane::session_registry::SessionAct::Tui,
            false,
        )
        .expect("create #2");
        crate::lane::session_registry::set_conversation(
            "vp",
            "root",
            "echoes",
            key,
            Some("01999999-9999-7999-8999-999999999999"),
        )
        .expect("#2 の会話 id");

        let cmd = build_stand_command_for_session("echoes", &addr, Path::new("/tmp"), Some(key));
        let env: std::collections::HashMap<_, _> = cmd.env.iter().cloned().collect();
        assert_eq!(
            env.get("VP_SESSION_KEY").map(String::as_str),
            Some("2"),
            "slot は自分の session を名乗る（root 決め打ちではない）"
        );
        let input = cmd.initial_input.expect("codex は initial_input あり");
        assert!(
            input.starts_with("codex") && !input.contains("claude"),
            "engine は #2 の stand（root の claude ではない）: {input}"
        );
        assert!(
            input.contains("01999999-9999-7999-8999-999999999999")
                && !input.contains("11111111-1111-1111-1111-111111111111"),
            "resume は #2 の会話 id（root の id を継がない）: {input}"
        );
        // doc 50 §4.6 A6: 非 root slot も replay を disk に持つ（term pane になれるため）。
        // ただし root の旧名ではなく session suffix 付き `<repo>__<lane>__<session>`。
        let non_root_replay = cmd
            .replay_path
            .as_ref()
            .expect("非 root slot も replay を disk に持つ (A6)");
        assert!(
            non_root_replay
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("__2")),
            "非 root は session suffix 付き file: {non_root_replay:?}"
        );

        // 同じ lane の root 版（session 省略）は従来どおり #1 / claude / replay あり（旧名継承）。
        let root_cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        let root_env: std::collections::HashMap<_, _> = root_cmd.env.iter().cloned().collect();
        assert_eq!(
            root_env.get("VP_SESSION_KEY").map(String::as_str),
            Some("1")
        );
        assert!(
            root_cmd
                .initial_input
                .as_deref()
                .is_some_and(|i| i.starts_with("claude")),
            "root は claude のまま（同居人の engine に引きずられない）"
        );
        let root_replay = root_cmd
            .replay_path
            .as_ref()
            .expect("root slot は従来どおり replay を disk に永続する");
        // team-b 6 回目 2026-07-25: **root も session suffix**（旧: root だけ lane 単位の旧名）。
        // file の身元を role に縛ると、root 付け替えで新 root が旧 root の画面を seed し、
        // 旧 root の生存 slot と同じ file を奪い合う（`replay_file_path_session_in` の doc）。
        assert!(
            root_replay
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("__1")),
            "root も自分の session file を持つ（role ではなく identity で決まる）: {root_replay:?}"
        );
        assert_ne!(
            non_root_replay, root_replay,
            "root と非 root は別 file（同一 file を奪い合わない）"
        );
    }

    /// doc 39 P2: 未発話の非 #1 root（VP が作った新品 session）は `--continue` に落とさない。
    /// conductor + id なしの `--continue` fallback は session #1 専用の互換層 — 非 #1 root で
    /// 踏むと cwd 最新拾いが旧 root の会話を新 root に混入させる（moody 指摘の Bare 失敗後
    /// Resume 復帰経路）。bare 起動に倒すことを固定する。
    #[test]
    fn unspoken_non_first_root_spawns_bare_not_continue() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        // root を #2（新品、record 無し）へ — Act I ✨ New 直後の registry 状態。
        crate::lane::session_registry::create_root(
            "vp",
            "root",
            "echoes",
            "echoes",
            crate::lane::session_registry::SessionAct::Tui,
        )
        .expect("create_root");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        let input = cmd.initial_input.expect("echoes は initial_input あり");
        assert!(
            !input.contains("--continue") && !input.contains("--resume"),
            "未発話の非 #1 root は bare 起動（--continue/--resume なし）: {input}"
        );
        assert!(input.starts_with("claude"), "claude 起動 command: {input}");
    }

    /// echoes は claude を initial_input で注入（wire hook 同梱）。
    #[test]
    fn echoes_injects_claude_via_initial_input() {
        let addr = LaneAddress::performer("vp", "w1");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        let input = cmd.initial_input.expect("echoes は initial_input あり");
        assert!(input.starts_with("claude"), "claude 起動 command: {input}");
        assert!(input.contains("wire hook-check"), "wire hook 同梱: {input}");
        assert!(input.ends_with('\r'), "Enter (CR) で submit: {input:?}");
    }

    /// 退役 stand ("tmux") / 未知 stand は shell 層に graceful 吸収。
    #[test]
    fn legacy_and_unknown_stands_fall_back_to_floor() {
        // build_stand_command は registry を読む（doc 39 P4-A: slot の engine は root stand 追従）。
        // 実 vp_state_dir の conductor registry（root=echoes）を拾うと未知 stand でも claude 注入に
        // なるため、tempdir に隔離して「未知 stand → shell のみ」の意図を検証する（sibling 規律）。
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        for stand in ["tmux", "opus-xhigh"] {
            let cmd = build_stand_command(stand, &addr, Path::new("/tmp"));
            assert!(
                cmd.initial_input.is_none(),
                "{stand} は shell のみ（initial_input なし）"
            );
        }
    }

    /// doc 53 §12.1: claude_command の分岐は **会話 id の有無 1 本**。
    ///
    /// 旧実装は `(起点 lane か, id の有無)` の 2 軸 3 分岐で、起点 lane × id 無しだけ
    /// `--continue`（cwd の最新会話）だった。**この枝を退役**したので、lane の種類は
    /// 起動 command に影響しない（= 呼び手が「どの lane か」を渡す必要も消えた）。
    #[test]
    fn claude_command_branches_on_conversation_id_only() {
        // id あり → --resume '<id>' || resume-failed || fresh
        let resume = claude_command(Some("abc-123"), None);
        assert!(resume.contains("--resume 'abc-123'"), "{resume}");
        assert!(
            resume.contains("||"),
            "session 消失時の fresh fallback: {resume}"
        );
        // F4 観測装置: fallback 進入は無音にしない（解剖 memory cc-session-pointer-self-destruction）
        assert!(
            resume.contains("vp lane resume-failed 'abc-123'"),
            "resume 失敗の観測中継が chain に入る: {resume}"
        );

        // id なし → 素の claude（**`--continue` は使わない** — VP が知らない会話は継がない）
        let fresh = claude_command(None, None);
        assert!(
            !fresh.contains("--continue"),
            "cwd の最新会話を推測で拾わない（doc 53 §12.1）: {fresh}"
        );
        assert!(!fresh.contains("--resume"), "{fresh}");
        // 失敗するものが無い = 観測中継も入らない
        assert!(!fresh.contains("resume-failed"), "{fresh}");
    }

    /// 不正な session id（quote 破り / 空）は resume に採用されない。
    #[test]
    fn unsafe_session_ids_are_rejected() {
        for bad in ["", "a'b", "x;rm -rf /", "id with space"] {
            let cmd = claude_command(Some(bad), None);
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
        let fresh = claude_command(None, Some("sonnet"));
        assert_eq!(
            fresh,
            format!("claude --model sonnet --settings '{WIRE_HOOKS}'"),
            "fresh は --model 付き単一 claude: {fresh}"
        );

        // resume: 主 claude と `||` fallback 先の fresh、 両方に --model が乗る
        let resume = claude_command(Some("abc-123"), Some("opus"));
        assert_eq!(
            resume.matches("--model opus").count(),
            2,
            "resume 主 + fallback fresh の両方に --model: {resume}"
        );
        assert!(
            resume.contains("--model opus --resume 'abc-123'"),
            "{resume}"
        );

        // id 無し（素の claude）にも --model が乗る。doc 53 §12.1 で `--continue` 枝が
        // 消えたので、ここは fallback を持たない単一 command = 1 回だけ。
        let bare = claude_command(None, Some("claude-fable-5"));
        assert_eq!(bare.matches("--model claude-fable-5").count(), 1, "{bare}");
    }

    /// 不正な model 名（injection 形 / 空 / 先頭 `-`）は `--model` に採用されない。
    #[test]
    fn unsafe_models_are_rejected() {
        for bad in ["", "opus --dangerously", "a;rm -rf /", "-x", "mo del"] {
            let cmd = claude_command(None, Some(bad));
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
            parsed.pointer("/hooks/SessionStart").is_some(),
            "SessionStart hook（doc 40 Issued 報告 = 発行時点の eager 表示）を含む: {parsed}"
        );
        assert!(
            !WIRE_HOOKS.contains('\''),
            "single quote を含むと `--settings '...'` の quote が破れる"
        );
    }

    /// doc 53 §12.1: **registry に会話 id が無い session は素の claude で立つ**
    /// （builder 経由の統合確認 — `--continue` 退役でここが lane の種類に依らなくなった）。
    ///
    /// ⚠️ **state 隔離が必須**（team-b 指摘 2026-07-25）。旧テストは `fresh: bool` を渡して
    /// いた時代のもので、`claude_command` が `if fresh { return }` を **resume_id を見る前**に
    /// 通っていたため実 registry に依存しなかった。引数が消えて分岐が registry 直読み 1 本に
    /// なった今、隔離を欠くと**この開発機の実 state**（repo 名が文字通り `"vp"`）を読み、
    /// 会話 id が記録されている限り毎回落ちる（CI は空なので通る = 開発機だけ赤）。
    /// [[dev-machine-masks-ci-failure]] の逆向き — 環境依存を持ち込んだ側。
    #[test]
    fn session_without_conversation_spawns_bare_via_builder() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        let input = cmd.initial_input.expect("echoes は initial_input あり");
        assert!(
            !input.contains("--resume") && !input.contains("--continue"),
            "会話 id が無ければ素の claude（cwd の最新を推測で拾わない）: {input}"
        );
    }

    /// opencode の Act I 起動（doc 43 §4）: id 有りは `-s '<id>' || opencode` 指名 resume、
    /// id 無しは素の opencode。injection 形 / grok 形（underscore 無し）の id は resume に採らない。
    #[test]
    fn opencode_command_variants() {
        // 実測形式（ses_ prefix + 英数字）→ `-s '<id>'` 指名 + fallback
        let resume = opencode_command(Some("ses_089ead04bffe5oIJcQTHwwTZo8"));
        assert_eq!(
            resume, "opencode -s 'ses_089ead04bffe5oIJcQTHwwTZo8' || opencode",
            "id 有りは指名 resume + 素の opencode fallback: {resume}"
        );
        // id 無し → 素の opencode（新規会話）
        assert_eq!(opencode_command(None), "opencode");
        // injection 形 / prefix 欠落は resume に採らない（is_safe_opencode_session_id 防壁）
        for bad in ["ses_bad'; rm", "089ead04", "ses_", ""] {
            let cmd = opencode_command(Some(bad));
            assert_eq!(cmd, "opencode", "不正 id '{bad}' は素の opencode: {cmd}");
        }
        assert!(is_safe_opencode_session_id(
            "ses_089ead04bffe5oIJcQTHwwTZo8"
        ));
        assert!(!is_safe_opencode_session_id("089ead04"), "ses_ prefix 必須");
    }

    /// build_stand_command（opencode stand）: fresh は素の opencode、非 fresh は registry の
    /// 会話 id 有無で resume 分岐。model flag は注入しない（opencode config が SSOT、doc 43 §3）。
    #[test]
    fn build_stand_command_opencode_arm() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        // 会話 id 未記録 → 素の opencode（新規会話）。model / provider flag は無い。
        let cmd = build_stand_command("opencode", &addr, Path::new("/tmp"));
        let input = cmd.initial_input.expect("opencode は initial_input あり");
        assert!(input.starts_with("opencode"), "opencode 起動: {input}");
        assert!(
            !input.contains("-m ") && !input.contains("--model"),
            "model は opencode config 管理（VP は注入しない）: {input}"
        );
        assert!(input.ends_with('\r'), "Enter (CR) で submit: {input:?}");
        // fresh も素の opencode（registry-native なので clear 対象の旧 store が無い）。
        let fresh = build_stand_command("opencode", &addr, Path::new("/tmp"));
        assert_eq!(fresh.initial_input.as_deref(), Some("opencode\r"));
    }

    /// 撤去済み stand（`"cursor"` — sweep 6.5）の DB descriptor は slot の login shell で graceful に
    /// 受ける（engine 注入なし = `initial_input` は None、warn ログのみ）。
    #[test]
    fn removed_stand_falls_back_to_bare_floor() {
        // build_stand_command は registry を読む — 実 conductor registry を拾わないよう隔離（sibling 規律）。
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let cmd = build_stand_command("cursor", &addr, Path::new("/tmp"));
        #[cfg(not(windows))]
        assert!(
            cmd.args.contains(&"-l".to_string()),
            "slot の shell は login shell (-l)、 got: {:?}",
            cmd.args
        );
        assert!(
            cmd.initial_input.is_none(),
            "撤去済み stand は engine を注入せず shell のみ、 got: {:?}",
            cmd.initial_input
        );
    }

    /// doc 39 P4-A: slot の engine は lane 固定 stand でなく **root session の stand** で決まる。
    /// lane stand=echoes でも root を codex session に向けたら slot は codex が立つ（cross-engine の
    /// Root 切替後の respawn 追従）。effective_stand の解決が engine arm 選択に効くことを固定する。
    #[test]
    fn build_stand_command_follows_root_session_engine() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        // lane stand=echoes だが root(#2) を codex に向ける（picker の cross-engine 切替後の registry）。
        crate::lane::session_registry::create_root(
            "vp",
            "root",
            "echoes",
            "codex",
            crate::lane::session_registry::SessionAct::Tui,
        )
        .expect("create_root codex");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        let input = cmd.initial_input.expect("codex root は initial_input あり");
        assert!(
            input.starts_with("codex") && !input.contains("claude"),
            "root が codex なら slot は codex 起動（lane stand=echoes に引きずられない）: {input}"
        );
    }

    /// doc 39 P4-A: root entry の stand が legacy / 撤去済み engine（cursor 等）なら、lane stand が
    /// echoes でも shell 層に graceful fallback する（engine 注入なし = initial_input は None）。
    #[test]
    fn build_stand_command_root_legacy_stand_falls_back_to_floor() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        // lane stand=echoes だが root(#2) を撤去済み "cursor" に向ける（disk に残る legacy 値の再現）。
        crate::lane::session_registry::create_root(
            "vp",
            "root",
            "echoes",
            "cursor",
            crate::lane::session_registry::SessionAct::Tui,
        )
        .expect("create_root cursor");
        let cmd = build_stand_command("echoes", &addr, Path::new("/tmp"));
        assert!(
            cmd.initial_input.is_none(),
            "未知 root stand は engine を注入せず shell のみ、 got: {:?}",
            cmd.initial_input
        );
    }
}
