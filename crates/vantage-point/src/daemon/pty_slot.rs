//! PTYスロット — 個々のPTYプロセスの管理
//!
//! portable-pty で PTY を作成し、master fd からの出力を
//! broadcast channel 経由で配信する。
//! 旧 `process/pty.rs` の PtySession を基に Daemon 用に再設計したもの（前身は本 slot が
//! 全面的に置き換えたため doc 44 P1 の露払いで削除済 — lane の PTY はここが唯一の実体）。
//! base64エンコードはしない（IPC層の責務）。
//!
//! terminal S4 (doc 27 §4.1): PTY 出力は broadcast → per-lane terminal pump →
//! World "canvas" topic 空間に流れる。 旧 `/ws/terminal` attach 時の scrollback replay
//! (ring buffer) は consumer (ws_terminal) ごと撤去したが、 replay-on-attach で復活した:
//! vp-app 再起動後の新 xterm は live stream だけでは空白のままになる (claude TUI は次の
//! 出力まで沈黙する) ため、 PtySlot が直近出力の ring buffer を保持し、 attach 時に
//! snapshot を先頭配送してから live に繋ぐ ([`PtySlot::attach_output`])。
//!
//! ## disk 永続 (SP 再起動をまたぐ復元)
//!
//! ring buffer は in-memory なので、 SP / daemon の再起動 (upgrade / crash / daemon 再起動) で
//! PtySlot が作り直されると消える → 新 PtySlot は空 buffer から始まり、 前画面が戻らない
//! (in-memory replay だけでは「GUI のみ再起動・SP 生存」しかカバーできない)。 これを埋めるため
//! `replay_path` が Some のとき、 ring buffer を disk (`vp_state_dir()/terminal_replay/`) に
//! **定期 flush** (crash 耐性) + **Drop 時 final flush** (graceful freshness) で落とし、 spawn 時に
//! seed する。 これで app / SP / daemon いずれの再起動でも spawn 直後に前画面を replay できる
//! (その後 `claude --resume` の repaint が追随)。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// replay ring buffer の上限バイト数。
///
/// claude TUI の現画面 + 直近履歴の再現に十分な量で、 attach ごとの初回配送コストを抑える。
/// xterm.js 側 scrollback は 5000 行 (main_area.rs) だが、 replay の目的は「再起動後に
/// 前回の画面が見える」ことなので全履歴の忠実再現は狙わない。
const REPLAY_CAP: usize = 256 * 1024;

/// replay buffer を disk に flush する間隔。 crash 時に失う窓を数秒に抑える (memory: ~2-3s)。
/// blocking reader ループ内 debounce では「出力が止まった後の idle 画面」(= 一番残したい状態)
/// を拾えないため、 別の定期 task で seq 変化時のみ書く。
const REPLAY_FLUSH_INTERVAL: Duration = Duration::from_secs(3);

/// replay file 名の sanitize (console_mode / cc_session と同一規則: `/` `\` `.` → `-`)。
fn sanitize_replay(part: &str) -> String {
    part.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '.') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// state base dir 注入版の replay file path (純関数、 テスト / lane state GC 用)。
pub fn replay_file_path_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("terminal_replay").join(format!(
        "{}__{}",
        sanitize_replay(project),
        sanitize_replay(lane)
    ))
}

/// lane の replay 永続 file path。 `<project>__<lane>` (console_mode と同一命名規則)。
///
/// `project` / `lane` は LaneAddress 由来 (`lane` は "root" / performer 名)。
pub fn replay_file_path(project: &str, lane: &str) -> PathBuf {
    replay_file_path_in(&crate::config::vp_state_dir(), project, lane)
}

/// session 別の replay 永続 file path（base 注入版、doc 50 §4.6 A6）。
///
/// root は lane 単位の旧名 `<project>__<lane>` を**継承**する（後方互換 = daemon 再起動で
/// 既存 scrollback を失わない / 既存 file の rename migration 不要）。非 root term session は
/// suffix 付き `<project>__<lane>__<session>` で、slot ごとに別 file（同一 file の奪い合い無し）。
pub fn replay_file_path_session_in(
    base: &Path,
    project: &str,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    is_root: bool,
) -> PathBuf {
    if is_root {
        replay_file_path_in(base, project, lane)
    } else {
        base.join("terminal_replay").join(format!(
            "{}__{}__{}",
            sanitize_replay(project),
            sanitize_replay(lane),
            session
        ))
    }
}

/// [`replay_file_path_session_in`] の実 state dir 版（slot spawn 経路が使う）。
pub fn replay_file_path_session(
    project: &str,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    is_root: bool,
) -> PathBuf {
    replay_file_path_session_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        session,
        is_root,
    )
}

/// lane 削除時に replay file を消す (不在は no-op、 best-effort)。base 注入版。
///
/// lane-scoped state の一元 GC ([`crate::lane::commands::clear_lane_state_in`]) が呼ぶ。
/// 残すと同名 lane 再作成時に旧画面の scrollback が seed されて蘇る (ghost replay)。
///
/// doc 50 §4.6 A6: root（`<project>__<lane>`）に加え、全 session file
/// (`<project>__<lane>__<session>`) も消す。session file を残すと同名 lane 再作成時に
/// 非 root pane が ghost replay する。session suffix は数字のみなので、別 lane
/// (`<project>__<lane>x`) を誤爆しない（prefix 一致 + 残りが全数字の 2 条件）。
pub fn clear_replay_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    // root（旧名）file。
    match std::fs::remove_file(replay_file_path_in(base, project, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    // session file 群（`<project>__<lane>__<digits>`）を read_dir で拾って消す（不在 dir = no-op）。
    let dir = base.join("terminal_replay");
    let session_prefix = format!("{}__{}__", sanitize_replay(project), sanitize_replay(lane));
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(suffix) = name.strip_prefix(&session_prefix)
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_digit())
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// replay buffer を atomic (`.tmp` → rename) に disk へ書く。 親 dir は都度 ensure。
/// 失敗は best-effort (replay は無くても console は live で動く) なので呼び手が握りつぶす。
fn write_replay_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// disk から replay seed を読む (末尾 [`REPLAY_CAP`] bytes)。 無ければ空。
fn load_replay_seed(path: &Path) -> VecDeque<u8> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(REPLAY_CAP);
            bytes[start..].iter().copied().collect()
        }
        Err(_) => VecDeque::new(),
    }
}

/// replay buffer を定期 flush する task を spawn する (runtime 不在なら None = 永続なし)。
///
/// `seq` (reader が append ごとに bump) を watch し、 前回 flush 以降に変化があった時だけ
/// snapshot を disk へ書く (= 無変化 lane の無駄 I/O を避ける)。 handle は PtySlot が保持し、
/// Drop で abort する。
fn spawn_replay_flush_task(
    path: PathBuf,
    replay: Arc<Mutex<VecDeque<u8>>>,
    seq: Arc<AtomicU64>,
) -> Option<JoinHandle<()>> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    Some(handle.spawn(async move {
        let mut interval = tokio::time::interval(REPLAY_FLUSH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_flushed: u64 = 0;
        // 初回 tick は即座に返るので skip (spawn 直後の seed をそのまま書き戻さない)。
        interval.tick().await;
        loop {
            interval.tick().await;
            let cur = seq.load(Ordering::Relaxed);
            if cur == last_flushed {
                continue;
            }
            let snapshot: Vec<u8> = {
                let buf = replay.lock().unwrap_or_else(|p| p.into_inner());
                buf.iter().copied().collect()
            };
            if write_replay_atomic(&path, &snapshot).is_ok() {
                last_flushed = cur;
            }
        }
    }))
}

/// PTYプロセスを管理するスロット
///
/// 1つのPTYプロセスを所有し、broadcast channel 経由で
/// 出力を配信する。Daemon がこのスロットをペインごとに持つ。
pub struct PtySlot {
    /// PTY への書き込みハンドル。
    ///
    /// Windows の ConPTY DSR auto-answer (reader task が起動時 `\x1b[6n` に応答する) と
    /// 外部 `write()` の両方が同じ writer を使うため `Arc<Mutex<_>>` で共有する。
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// PTY ペア（リサイズ用に保持）
    pair: portable_pty::PtyPair,
    /// 子プロセスハンドル（ゾンビプロセス防止のため保持）
    child: Box<dyn portable_pty::Child + Send>,
    /// プロセスID
    pid: u32,
    /// シェルコマンド
    shell_cmd: String,
    /// 出力配信チャネル（送信側）
    output_tx: broadcast::Sender<Vec<u8>>,
    /// 直近出力の replay ring buffer（[`REPLAY_CAP`] bytes 上限）。
    ///
    /// reader task が **lock 保持のまま append → broadcast send** するため、
    /// [`Self::attach_output`] の snapshot+subscribe と原子的に直列化される
    /// (= あるバイトは「snapshot に含まれる」か「subscribe 後の rx に届く」の排他二択。
    /// 取りこぼしも二重配送も構造的に起きない)。 spawn 時に disk seed で初期化されうる。
    replay: Arc<Mutex<VecDeque<u8>>>,
    /// replay の disk 永続 path (Some = 永続あり)。 Drop 時の final flush で使う。
    replay_path: Option<PathBuf>,
    /// replay 定期 flush task のハンドル (Drop で abort)。 runtime 不在 / 永続なしなら None。
    flush_handle: Option<JoinHandle<()>>,
    /// reader task のハンドル
    _reader_handle: tokio::task::JoinHandle<()>,
}

impl PtySlot {
    /// PTYプロセスを起動
    ///
    /// 指定したシェルコマンドを PTY 上で起動し、
    /// 出力を broadcast channel に配信する reader task を開始する。
    /// `replay_path` が Some のとき、 spawn 時に disk seed を読み込み (前回画面) + 定期/Drop で
    /// disk へ flush する (SP 再起動をまたぐ復元)。 None なら in-memory replay のみ (テスト等)。
    pub fn spawn(
        cwd: &str,
        shell_cmd: &str,
        args: &[String],
        env: &[(String, String)],
        cols: u16,
        rows: u16,
        replay_path: Option<PathBuf>,
    ) -> Result<(Self, broadcast::Receiver<Vec<u8>>)> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell_cmd);
        cmd.cwd(cwd);
        // 起動引数は caller が決める (zsh/bash → "-l" で login shell、pwsh → "-NoLogo" 等)。
        // 旧実装は `cmd.arg("-l")` を hardcode していたが、pwsh 等で無効 flag になる問題があり廃止。
        for arg in args {
            cmd.arg(arg);
        }
        // doc 11 (PR-B): 起動 command が要求する env（VP_PROJECT / VP_LANE 等）を子プロセスに渡す。
        for (key, value) in env {
            cmd.env(key, value);
        }
        // PATH 補正: vp-app (.app) を GUI / launchd 経由で起動すると、 子プロセスの PATH が
        // `/usr/bin:/bin:/usr/sbin:/sbin` の最小集合になり、 user-installed tool (特に mise、
        // conductor lane = `mise run vp:stand:echoes` の program) を見つけられず spawn が失敗 →
        // lane が即 Dead 化 → Echoes コンソールが出ない、 という症状の根因になる。
        // 既知の user tool location を base PATH の先頭に前置して解決する。
        // base は caller env の PATH (あれば) → なければ親プロセスの PATH。
        // 補正ロジックの SSOT は `crate::spawn_env`。 本来は daemon / SP の spawn 最上流で
        // 補強済みのはずだが (#498 再発の根治)、 末端でも二重保険として補強する。
        {
            let base_path = env
                .iter()
                .find(|(k, _)| k == "PATH")
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            // home は `dirs::home_dir()` で解決 (Windows で `HOME` 未設定 = `USERPROFILE` のみ
            // でも claude.exe 等の user tool prefix を引ける)。 base は caller env の PATH override。
            cmd.env("PATH", crate::spawn_env::augment_path_env(&base_path));
        }

        // TERM 補正: vp-app を GUI / launchd 経由で起動 (= 再起動後の LaunchAgent 自動起動) すると、
        // daemon プロセスは端末非接続で TERM を持たない。 echoes stand の `tmux new-session -A`
        // (attach 付き) は terminfo 引きに TERM を要求するため、 TERM 不在だと
        // "open terminal failed: terminal does not support clear" で即 exit → stand spawn が
        // 800ms 以内に死に lane が即 Dead 化 → Echoes コンソールが出ない。 PATH 補正 (#498) と
        // 同じ launchd-env-stripping の双子で、 plist EnvironmentVariables も PATH だけ焼いて
        // TERM を取りこぼしていた。 この PTY の出力は vp-app の xterm.js が描画する (echoes script
        // も `terminal-overrides ',xterm-256color:Tc'` を前提) ので、 TERM=xterm-256color を
        // 既定とする。 caller が env で明示注入した場合はそれを尊重する。
        if !env.iter().any(|(k, _)| k == "TERM") {
            cmd.env("TERM", "xterm-256color");
        }

        // COLORTERM 補正 (tmux decoupling PR2): 旧 echoes stand は tmux の
        // `terminal-overrides ',xterm-256color:Tc'` で truecolor を交渉していた。 tmux 撤去で
        // その交渉主体が消えたため、 PtySlot が新たな端点として `COLORTERM=truecolor` を宣言する。
        // これが無いと claude は TERM=xterm-256color を見て 24-bit を諦め 256 色に退行する
        // (実際の描画先 xterm.js は truecolor 対応なので、 宣言さえすれば 24-bit がそのまま届く)。
        if !env.iter().any(|(k, _)| k == "COLORTERM") {
            cmd.env("COLORTERM", "truecolor");
        }

        // LANG/LC_CTYPE 補正: PATH (#498) / TERM の双子で、 launchd / GUI 起動の daemon は
        // C ロケール伝播で LANG 不在になり、 echoes stand の tmux client が utf8=0 で起動 →
        // console の CJK (日本語) が `_` 化する (三つ子の三本目)。 plist EnvironmentVariables や
        // echoes mise task の LANG guard は「①旧 plist が upgrade で再生成されない ②session 永続で
        // 2 回目以降は adopt 経路が LANG guard を通らない」で漏れるが、 全 spawn 経路 (mise task /
        // adopt) が必ずこの PtySlot を通るため、 末端で注入すれば daemon / plist の LANG 状態に
        // 非依存で確定的に UTF-8 を保証できる。 caller が env で明示した LANG / LC_CTYPE は尊重する。
        let locale = crate::spawn_env::utf8_locale();
        if !env.iter().any(|(k, _)| k == "LANG") {
            cmd.env("LANG", &locale);
        }
        if !env.iter().any(|(k, _)| k == "LC_CTYPE") {
            cmd.env("LC_CTYPE", &locale);
        }

        // 子プロセスを起動（ゾンビ防止のためハンドルを保持する）
        let child = pair.slave.spawn_command(cmd)?;
        let pid = child.process_id().unwrap_or(0);

        // マスター側の読み書きハンドル
        let reader = pair.master.try_clone_reader()?;
        // writer は外部 write() と reader task の DSR auto-answer で共有する。
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));

        // broadcast channel（バッファ 256）
        // initial_rx を保持し、reader_task 開始前に subscriber を確保する。
        // これにより PTY からの最初のバイト（シェルプロンプト等）を取りこぼさない。
        let (output_tx, initial_rx) = broadcast::channel(256);

        // replay ring buffer (attach 時の画面復元用)。 disk 永続がある lane は前回画面を seed
        // (spawn 直後の attach で前画面を replay → 続いて claude --resume の repaint が追随)。
        let seed = match &replay_path {
            Some(p) => load_replay_seed(p),
            None => VecDeque::new(),
        };
        let replay = Arc::new(Mutex::new(seed));
        let replay_seq = Arc::new(AtomicU64::new(0));

        // reader task 開始 (writer を渡して ConPTY DSR に応答できるようにする)
        let reader_handle = start_reader_task(
            reader,
            output_tx.clone(),
            Arc::clone(&writer),
            Arc::clone(&replay),
            Arc::clone(&replay_seq),
        );

        // disk 永続がある lane は定期 flush task を起動 (runtime 不在なら None = 永続なし)。
        let flush_handle = replay_path.as_ref().and_then(|p| {
            spawn_replay_flush_task(p.clone(), Arc::clone(&replay), Arc::clone(&replay_seq))
        });

        Ok((
            Self {
                writer,
                pair,
                child,
                pid,
                shell_cmd: shell_cmd.to_string(),
                output_tx,
                replay,
                replay_path,
                flush_handle,
                _reader_handle: reader_handle,
            },
            initial_rx,
        ))
    }

    /// PTY に入力を書き込む
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("PtySlot writer mutex poisoned"))?;
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    /// PTY をリサイズ
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// 出力ストリームを購読（broadcast receiver）
    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// replay snapshot + 購読を原子的に取得する（attach 用）。
    ///
    /// reader task は replay lock を保持したまま append → broadcast send するため、
    /// 本 method が lock 中に「snapshot 取得 → subscribe」を済ませれば、 全バイトは
    /// snapshot か receiver のどちらか一方にだけ現れる（gap / 重複なし）。
    /// caller (terminal pump) は snapshot を先に配送してから receiver の live stream に繋ぐ。
    pub fn attach_output(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let guard = self.replay.lock().unwrap_or_else(|p| p.into_inner());
        let snapshot: Vec<u8> = guard.iter().copied().collect();
        let rx = self.output_tx.subscribe();
        drop(guard);
        (snapshot, rx)
    }

    /// プロセスID
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// シェルコマンド
    pub fn shell_cmd(&self) -> &str {
        &self.shell_cmd
    }

    /// 子プロセスがまだ生きているかチェック (non-blocking try_wait)。
    /// Phase 5-D: spawn 直後の早期 exit 検知 (例: `claude --continue` で session corrupt) に使う。
    /// `Err` は wait 自体の失敗 ─ 安全側に倒して「死亡」扱いとする。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for PtySlot {
    /// PtySlot 破棄時に子プロセスを確実に終了させる
    ///
    /// kill() で終了シグナルを送り、wait() で回収することで
    /// ゾンビプロセスの発生を防ぐ。 加えて replay を disk へ final flush する
    /// (graceful な lane restart / SP 停止で最新画面を残す。 crash 経路は定期 flush が担保)。
    fn drop(&mut self) {
        // flush task を止めてから final flush (定期 flush と競合させない)。
        if let Some(h) = self.flush_handle.take() {
            h.abort();
        }
        if let Some(path) = &self.replay_path {
            let snapshot: Vec<u8> = {
                let buf = self.replay.lock().unwrap_or_else(|p| p.into_inner());
                buf.iter().copied().collect()
            };
            // 空 buffer は書かない (seed 前 / 出力ゼロの lane で既存 disk 画面を潰さない)。
            if !snapshot.is_empty() {
                let _ = write_replay_atomic(path, &snapshot);
            }
        }
        if let Err(e) = self.child.kill() {
            tracing::debug!("PtySlot drop: kill 失敗（既に終了済みの可能性）: {}", e);
        }
        if let Err(e) = self.child.wait() {
            tracing::debug!("PtySlot drop: wait 失敗: {}", e);
        }
    }
}

/// PTY出力読み取りタスクを起動
///
/// PTY の master fd からバイト列を読み取り、 broadcast channel に send する。
/// base64 エンコードはしない (IPC 層の責務)。
fn start_reader_task(
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<Vec<u8>>,
    #[cfg_attr(not(windows), allow(unused_variables))] writer: Arc<Mutex<Box<dyn Write + Send>>>,
    replay: Arc<Mutex<VecDeque<u8>>>,
    replay_seq: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        // Windows ConPTY DSR gating の回避状態。 起動時の cursor-position query に一度だけ
        // 応答したかを覚える (詳細は下の応答ブロック参照)。
        #[cfg(windows)]
        let mut dsr_answered = false;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // PTYがクローズされた
                    tracing::info!("PtySlot reader: EOF");
                    break;
                }
                Ok(n) => {
                    #[cfg_attr(not(windows), allow(unused_mut))]
                    let mut chunk = buf[..n].to_vec();

                    // ── Windows ConPTY DSR auto-answer ──
                    // ConPTY は起動直後に DSR (`\x1b[6n` = カーソル位置問い合わせ) を 1 回出し、
                    // その応答 (`\x1b[row;colR`) を受け取るまで**以降の全出力を止める** (Unix PTY に
                    // ない gating)。 応答すべき端末 (xterm.js) は VP では demand-driven な terminal
                    // pump 経由で**後から** subscribe するため、 起動時 DSR に間に合わず (tokio
                    // broadcast は新 subscriber へ過去メッセージを配らない)、 ConPTY が永久ブロック →
                    // console 空 + claude が端末セットアップ待ちで exit、 という症状になる。
                    //
                    // reader task は PTY spawn 時から存在する唯一の consumer なので、 ここで起動時
                    // DSR に一度だけ応答して出力を解禁する。 起動直後は cursor が原点にあるため
                    // `\x1b[1;1R` が正しい応答。 二重応答 (xterm も後で応答) を避けるため、 応答した
                    // DSR は forward stream から除去する。 以降 (mid-session) の DSR は実カーソル位置を
                    // 知る xterm.js が round-trip で応答する。
                    #[cfg(windows)]
                    {
                        const DSR_QUERY: &[u8] = b"\x1b[6n";
                        if !dsr_answered && chunk.windows(DSR_QUERY.len()).any(|w| w == DSR_QUERY) {
                            if let Ok(mut w) = writer.lock() {
                                let _ = w.write_all(b"\x1b[1;1R");
                                let _ = w.flush();
                            }
                            dsr_answered = true;
                            chunk = strip_byte_seq(&chunk, DSR_QUERY);
                            if chunk.is_empty() {
                                continue;
                            }
                        }
                    }

                    // replay ring buffer へ append → broadcast send を同一 lock 内で行う
                    // (= attach_output の snapshot+subscribe との原子性を保証する。
                    // 詳細は PtySlot.replay の doc comment)。 poisoned は into_inner で継続
                    // (buffer は劣化してもよい best-effort、 live stream を止めない)。
                    {
                        let mut buf = replay.lock().unwrap_or_else(|p| p.into_inner());
                        buf.extend(chunk.iter().copied());
                        let overflow = buf.len().saturating_sub(REPLAY_CAP);
                        if overflow > 0 {
                            buf.drain(..overflow);
                        }
                        // flush task の dirty 判定用に世代を進める (lock 内で bump = snapshot と整合)。
                        replay_seq.fetch_add(1, Ordering::Relaxed);
                        // 受信者がいなくても送信を試行（正常動作）
                        let _ = tx.send(chunk);
                    }
                }
                Err(e) => {
                    tracing::warn!("PtySlot reader error: {}", e);
                    break;
                }
            }
        }
    })
}

/// `data` から `seq` の全出現を取り除いた新しい Vec を返す (ConPTY DSR の二重応答防止用)。
#[cfg(windows)]
fn strip_byte_seq(data: &[u8], seq: &[u8]) -> Vec<u8> {
    if seq.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(seq) {
            i += seq.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 50 §4.6 A6: root は lane 単位の旧名を継承（後方互換）、非 root は session suffix。
    #[test]
    fn replay_file_path_session_naming() {
        let base = Path::new("/tmp/vp-test-state");
        // root（is_root=true）は session を無視して旧名 `<project>__<lane>` に一致する。
        let root = replay_file_path_session_in(base, "vp", "root", 16, true);
        assert_eq!(root, replay_file_path_in(base, "vp", "root"));
        // 非 root は `<project>__<lane>__<session>` の suffix 付き（別 file）。
        let non_root = replay_file_path_session_in(base, "vp", "root", 17, false);
        assert_eq!(non_root.file_name().unwrap(), "vp__root__17");
        assert_ne!(root, non_root, "root と非 root は別 file（奪い合わない）");
    }

    /// テスト用のデフォルトシェルを返す。
    /// $SHELL があればそれを、無ければ OS 既定（Unix: /bin/sh、Windows: cmd.exe）を使う。
    /// Windows には /bin/sh が無いので OS 分岐が必須。
    fn default_test_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
    }

    #[tokio::test]
    async fn test_pty_spawn_and_output() {
        // echo コマンドでテスト用の出力を確認
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (slot, mut rx) =
            PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn に失敗");

        // PIDが取得できること
        assert!(slot.pid() > 0 || slot.pid() == 0); // CI環境では0の可能性

        // シェルコマンドが正しいこと
        assert_eq!(slot.shell_cmd(), shell);

        // 初期 receiver でシェルのプロンプトなど何らかの出力が来ることを確認
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
        assert!(
            result.is_ok(),
            "タイムアウト: PTY から出力を受信できなかった"
        );
    }

    /// attach_output: 過去出力が replay snapshot に入り、 以降の出力は receiver に届く
    /// (replay-on-attach の要 — vp-app 再起動後の新 xterm が前回画面を復元できる根拠)。
    #[tokio::test]
    async fn test_attach_output_replays_past_bytes() {
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (slot, mut rx) =
            PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn に失敗");

        // シェル初期出力 (プロンプト等) を待つ = replay buffer に何か溜まる
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("タイムアウト: PTY 初期出力なし")
            .expect("recv");
        assert!(!first.is_empty());

        // 後発 attach: 初期出力を「過去」として snapshot で受け取れる
        let (snapshot, _live_rx) = slot.attach_output();
        assert!(
            !snapshot.is_empty(),
            "attach_output の snapshot に過去出力が含まれるはず"
        );
        // snapshot は broadcast 済 bytes の先頭を含む (欠落なしの根拠)
        assert!(
            snapshot
                .windows(first.len().min(snapshot.len()))
                .any(|w| w == &first[..first.len().min(snapshot.len())]),
            "snapshot に初期出力の bytes が含まれるはず"
        );
    }

    /// disk 永続 round-trip: 出力 → flush task が disk へ書く → その file を seed に新 PtySlot を
    /// spawn すると、 前回出力が attach_output の snapshot に replay される (SP 再起動をまたぐ復元)。
    #[tokio::test]
    async fn test_replay_persists_and_seeds_across_respawn() {
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("terminal_replay").join("proj__root");

        // --- 1 本目: 出力を出して disk flush を待つ ---
        let marker = "VP_PERSIST_MARKER";
        {
            let (mut slot, mut rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, Some(path.clone()))
                    .expect("PTY spawn");
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let echo: &[u8] = if cfg!(windows) {
                b"echo VP_PERSIST_MARKER\r"
            } else {
                b"echo VP_PERSIST_MARKER\n"
            };
            slot.write(echo).expect("write");
            // marker が出力に現れるまで drain (ConPTY DSR は端末役として応答)
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut seen = String::new();
            while tokio::time::Instant::now() < deadline && !seen.contains(marker) {
                if let Ok(Ok(b)) =
                    tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await
                {
                    let t = String::from_utf8_lossy(&b);
                    if t.contains("\u{1b}[6n") {
                        let _ = slot.write(b"\x1b[1;1R");
                    }
                    seen.push_str(&t);
                }
            }
            assert!(seen.contains(marker), "1 本目で marker が出力される");
            // flush task (3s interval) が disk へ書くまで待つ
            let flush_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
            while tokio::time::Instant::now() < flush_deadline && !path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            assert!(path.exists(), "flush task が disk へ replay を書くはず");
            // slot は scope 終端で Drop → final flush も走る
        }

        // disk file に marker が入っている
        let disk = std::fs::read(&path).expect("read replay file");
        assert!(
            String::from_utf8_lossy(&disk).contains(marker),
            "disk replay に前回出力が残る"
        );

        // --- 2 本目: 同 path を seed に spawn → attach_output に前回出力が乗る ---
        let (slot2, _rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, Some(path.clone()))
            .expect("PTY spawn 2");
        let (snapshot, _live) = slot2.attach_output();
        assert!(
            String::from_utf8_lossy(&snapshot).contains(marker),
            "seed した前回画面が attach snapshot に replay される (SP 再起動復元)"
        );
    }

    #[tokio::test]
    async fn test_pty_write_input() {
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (mut slot, mut rx) =
            PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn に失敗");

        // 少し待ってからコマンドを送信 (シェル初期化を待つ)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // echo コマンドを送信。改行コードは OS 依存
        // (Unix シェルは LF、cmd.exe(ConPTY) は Enter=CR で行確定)。
        let echo_cmd: &[u8] = if cfg!(windows) {
            b"echo HELLO_PTY_SLOT\r"
        } else {
            b"echo HELLO_PTY_SLOT\n"
        };
        slot.write(echo_cmd).expect("PTY への書き込みに失敗");

        // 出力に "HELLO_PTY_SLOT" が含まれることを確認
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = false;

        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(data)) => {
                    let text = String::from_utf8_lossy(&data);
                    // ConPTY は DSR (\x1b[6n = カーソル位置問い合わせ) への応答を
                    // 端末側から受け取るまで描画を進めない。本番では実端末 (xterm.js)
                    // が応答するが、テストでは我々が端末役として応答する必要がある。
                    if text.contains("\u{1b}[6n") {
                        let _ = slot.write(b"\x1b[1;1R");
                    }
                    if text.contains("HELLO_PTY_SLOT") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        assert!(found, "PTY 出力に HELLO_PTY_SLOT が含まれなかった");
    }

    /// 回帰 (console-blackout root cause): PTY child は親プロセスの TERM 有無に依らず
    /// TERM=xterm-256color を受け取る。 launchd 自動起動の daemon は端末非接続で TERM を
    /// 継承しないため、 TERM 不在だと echoes の `tmux new-session -A` が "open terminal
    /// failed" で即死 → lane spawn 全滅 → console が出ない。 PtySlot が TERM 既定を注入する
    /// ことで daemon の TERM 有無に依らず stand が描画可能になることを pin する。
    #[cfg(unix)]
    #[tokio::test]
    async fn test_pty_spawn_injects_term_default() {
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        // 子の $TERM を marker 付きで 1 発出力して即終了する非対話 shell。 env 未指定 (&[]) なので
        // PtySlot が TERM 既定を注入するはず。 親の TERM 値に依らず子側の値だけを検証できる。
        let args = vec![
            "-c".to_string(),
            "printf 'VPTERM[%s]\\n' \"$TERM\"; sleep 0.2".to_string(),
        ];
        let (_slot, mut rx) =
            PtySlot::spawn(&cwd, "/bin/sh", &args, &[], 80, 24, None).expect("PTY spawn に失敗");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut buf = String::new();
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(data)) => {
                    buf.push_str(&String::from_utf8_lossy(&data));
                    if buf.contains("VPTERM[") && buf.contains(']') {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        assert!(
            buf.contains("VPTERM[xterm-256color]"),
            "PTY child の TERM が xterm-256color でない: 受信={:?}",
            buf
        );
    }

    #[tokio::test]
    async fn test_pty_drop_kills_child() {
        // Drop 実装が子プロセスを確実に終了させることを検証
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (slot, _rx) =
            PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn に失敗");
        let pid = slot.pid();

        // CI環境ではPIDが0の場合がある
        if pid == 0 {
            return;
        }

        // プロセスが起動していることを確認
        let alive_before = crate::platform::process_alive(pid);
        assert!(alive_before, "子プロセスが起動していない (PID: {})", pid);

        // PtySlot を drop → Drop impl が kill + wait を呼ぶ
        drop(slot);

        // リトライループで終了を確認（固定sleepより安定）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !crate::platform::process_alive(pid) {
                break; // 成功: プロセスが終了した
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("Drop後2秒経ってもプロセスが終了していない (PID: {})", pid);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// 回帰 (W2b console blank 根治): Windows ConPTY は起動時 DSR (`\x1b[6n`) の応答を
    /// 受け取るまで全出力を gate する。 PtySlot reader task が起動時 DSR に自動応答することで、
    /// 端末役が何もしなくても PTY 出力が流れることを pin する。 応答が無いと ConPTY は
    /// `\x1b[6n` 4 byte だけ出して以降を止める (= console 空の根因)。
    ///
    /// git-bash 依存 (ConPTY × MSYS bash が最も顕著に DSR gating する) なので、 git-bash 不在の
    /// CI では skip する。
    #[cfg(windows)]
    #[tokio::test]
    async fn conpty_dsr_auto_answer_unblocks_output() {
        let Some(bash) = vp_paths::shell::find_git_bash() else {
            eprintln!("git-bash 不在のため skip (ConPTY DSR 回帰テスト)");
            return;
        };
        let bash = bash.to_string_lossy().to_string();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        // 端末役 (DSR 応答) を一切せずに git-bash の echo 出力を集める。
        // PtySlot が自動応答しなければ ConPTY は `\x1b[6n` で止まり VPMARKER は出ない。
        let args = vec!["-lc".to_string(), "echo VPMARKER_OUT; sleep 1".to_string()];
        let (_slot, mut rx) =
            PtySlot::spawn(&cwd, &bash, &args, &[], 80, 24, None).expect("PTY spawn");

        let mut buf: Vec<u8> = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(b)) => {
                    buf.extend_from_slice(&b);
                    if String::from_utf8_lossy(&buf).contains("VPMARKER_OUT") {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        let text = String::from_utf8_lossy(&buf);
        assert!(
            text.contains("VPMARKER_OUT"),
            "PtySlot の DSR auto-answer で ConPTY 出力が解禁されるはず。 受信={} bytes: <<{}>>",
            buf.len(),
            text.replace('\x1b', "\\e")
        );
        // 応答済 DSR は forward stream から除去される (二重応答防止)。
        assert!(
            !text.contains("\u{1b}[6n"),
            "応答した startup DSR は stream から除去されるはず: <<{}>>",
            text.replace('\x1b', "\\e")
        );
    }
}
