//! AcpAgentHost — ACP engine（grok / opencode / agy）を lane 単位で**常駐**駆動する host（gui、doc 42・43・88）
//!
//! `grok agent stdio`（doc 42）・`opencode acp`（doc 43）・`agy acp`（doc 88）はいずれも
//! **ACP（Agent Client Protocol、protocolVersion 1）**を実装しており（各 doc §1 実測）、wire が
//! 同一なので本 host がそのまま 3 engine を駆動する。engine 別に変わるのは spawn command と名乗り
//! （registry / ログ / エラー文言）だけで、[`AcpEngine`] に集約する。host のロジックは engine 非依存で、
//! [`super::codex_host::CodexAgentHost`]（RpcHost）と同型の per-session 常駐:
//!
//! - JSONL だが **`jsonrpc: "2.0"` field を含む**標準 JSON-RPC（codex は省略形）
//! - 会話確立 = `session/new` | `session/load`（load error → new へ self-heal）
//! - **turn 終了 = `session/prompt` request の response**（`{stopReason}`。codex の
//!   turn/completed notification と違い response 駆動）
//! - 中断 = `session/cancel` **notification**（応答なし — prompt response が cancelled で畳む）
//! - **`session/request_permission`（server→client request）を host が自動 allow**
//!   （claude bypassPermissions / codex approvalPolicy=never と同じ tui/gui parity。grok は
//!   spawn flag `--always-approve`、agy は `--dangerously-skip-permissions` と二段防御、
//!   opencode は自動 allow の一段のみ — doc 43 §6・doc 88 §2）
//! - `session/load` の replay 列（過去会話の session/update）は**翻訳しない** — VP 自前の
//!   replay_log 経由 replay と二重になるため、load 完了まで translator を繋がない（doc 42 §3）
//! - `session/new`・`session/load` の応答に含まれる model / mode と `available_commands_update` を
//!   [`ConversationEvent::SessionInit`] に反映し（turn 中の変更は turn の終わりまで保留 — doc 88 §6.3）、
//!   `usage_update` の context 使用量を [`ConversationEvent::TurnCompleted`] に載せる（doc 88 §3）
//! - Session Config Options（`configOptions` / `session/set_config_option`）: engine が出す軸と候補を
//!   保持して roster に渡し（[`AcpAgentHost::config_view`]）、session 確定後に registry の意図を
//!   当て直す（応答が揃うまで prompt を送らない）。user の切替は [`AcpConfigHandle`]（doc 88 §6）
//! - 画像は ACP の image content block で prompt に並べる（[`AcpAgentHost::submit_with_images`]）
//!
//! 会話 id（ACP sessionId）は registry 直結（doc 40 §4）— grok / opencode / agy に旧 store は存在しない
//! （**registry-native** = backfill bridge の対象外という正しい欠如）。
//! 途絶の自己修復配線（dead flag → submit Err → ensure_and_submit_chat の drop→再ensure→retry）と
//! stderr 診断は PR #802 の moody 教訓を最初から実装。

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::acp_config::{
    AcpConfigOption, AcpConfigView, needs_apply, parse_config_option_update, parse_config_options,
};
use super::acp_translate::AcpTranslator;
use super::event::ConversationEvent;
use super::host::{ImageInput, InFlight};

/// `set_config_option` の応答を待つ上限（user の切替 1 軸 / spawn 時の当て直し全体）。
/// agy の実測応答は即時（〜100ms）。
const CONFIG_SET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// AcpAgentHost が駆動する ACP engine の種別（spawn command + 名乗りの差分だけを持つ）。
///
/// host のロジック（session/new|load self-heal / prompt-response turn / session/cancel /
/// request_permission 自動 allow / dead flag 自己修復 / stderr 診断 / load replay 列破棄）は
/// engine 非依存。engine 別に変わるのは「どの CLI をどう起動するか」と「registry / ログ /
/// エラー文言の名乗り」だけなので、その差分をここに集約する（doc 43 §2・doc 88 §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpEngine {
    /// `grok agent stdio --always-approve`（xAI Grok CLI、doc 42）。CLI パス = PATH → `~/.grok/bin/grok`。
    Grok,
    /// `opencode acp`（opencode、doc 43）。CLI パス = PATH → `/opt/homebrew/bin/opencode`（brew）。
    /// approve 相当 flag は無い（request_permission 自動 allow の一段のみ）。model / provider は
    /// opencode config が SSOT で、VP は spawn に env も flag も注入しない（doc 43 §3）。
    OpenCode,
    /// `agy acp --dangerously-skip-permissions`（Google Antigravity CLI、doc 88）。
    /// CLI パス = PATH → `/opt/homebrew/bin/agy`（brew）。permission 二段防御（flag + 自動 allow）。
    Agy,
}

impl AcpEngine {
    /// registry / ログ / エラー文言の名乗り（[`super::EngineKind::agent_name`] と一致）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
            Self::Agy => "agy",
        }
    }

    /// CLI 実行パスの解決（launchd 起動 daemon の細い PATH 対策 — `session_store::resolve_cli`）。
    fn cli_path(self) -> String {
        match self {
            Self::Grok => {
                let home = std::env::var("HOME").unwrap_or_default();
                crate::lane::session_store::resolve_cli(
                    "grok",
                    &[std::path::PathBuf::from(home).join(".grok/bin/grok")],
                )
            }
            Self::OpenCode => crate::lane::session_store::resolve_cli(
                "opencode",
                &[std::path::PathBuf::from("/opt/homebrew/bin/opencode")],
            ),
            Self::Agy => crate::lane::session_store::resolve_cli(
                "agy",
                &[std::path::PathBuf::from("/opt/homebrew/bin/agy")],
            ),
        }
    }

    /// spawn 時の固定引数（cwd / stdio piping は spawn 側で付ける）。
    fn spawn_args(self) -> &'static [&'static str] {
        match self {
            // grok: agent サブコマンド + `--always-approve`（permission 二段防御、doc 42）。
            Self::Grok => &["agent", "--always-approve", "stdio"],
            // opencode: `acp` サブコマンドのみ（flag / env / config 注入なし — model は opencode
            //  config が SSOT、doc 43 §3）。
            Self::OpenCode => &["acp"],
            // agy: `acp` サブコマンド + `--dangerously-skip-permissions`（permission 二段防御、doc 88）。
            Self::Agy => &["acp", "--dangerously-skip-permissions"],
        }
    }
}

/// AcpAgentHost の起動設定。
#[derive(Debug, Clone)]
pub struct AcpHostConfig {
    /// 駆動する ACP engine（spawn command + 名乗りの差分）。
    pub engine: AcpEngine,
    /// 会話の作業ディレクトリ（lane の repo dir）。
    pub cwd: String,
    /// registry 書き込みキー（repo 名）。
    pub repo: String,
    /// registry 書き込みキー（session label: `main` / `main#2` …）。
    /// ⚠️ env の `VP_LANE` には使わない — そちらは [`Self::lane_label`]（素の label）。
    pub lane: String,
    /// identity env（`VP_LANE`）用の素の lane label（doc 51 §1 A3b — tui と同じ契約）。
    pub lane_label: String,
    /// identity env（`VP_SESSION_KEY`）用の session key（doc 40 §4 の hook identity と同じ）。
    pub session_key: crate::lane::session_registry::SessionKey,
    /// 再開する ACP sessionId（doc 40 registry の `conversation`）。None = 新規 session。
    pub session_id: Option<String>,
    /// session 確定後に `set_config_option` で当て直す `(config id, 値)` 列（registry の意図、
    /// doc 88 §6）。agy は切り替えた値を `session/load` の後に保たない（実測）ので、spawn の
    /// たびに戻す。engine 別の意図の形は pool が engine settings から組む（host は engine 非依存）。
    pub config_intent: Vec<(String, String)>,
}

/// 送信済み request の種別（response 到着時の分岐）。
#[derive(Debug, Clone, PartialEq)]
enum ReqKind {
    Initialize,
    SessionLoad,
    SessionNew,
    /// session/prompt — response 到着 = turn 終了（doc 42 §2 の response 駆動）。
    Prompt,
    /// session/set_config_option（doc 88 §6）。`reapply` = spawn 時の当て直し（応答が揃うまで
    /// prompt を送らない）/ false = user の切替（応答を待つ側へ返す）。
    SetConfig {
        config_id: String,
        value: String,
        reapply: bool,
    },
}

/// 待ち行列の 1 件（session 未確定 / turn 中 / 当て直し中に来た submit）。
#[derive(Debug, Clone, PartialEq)]
struct QueuedPrompt {
    text: String,
    images: Vec<ImageInput>,
}

/// reader task / host メソッドが共有する可変状態（std Mutex — await を跨がずに触る）。
struct AcpState {
    /// 確定した ACP sessionId（handshake 完了まで None）。
    session_id: Option<String>,
    /// `session/new` | `session/load` で得た現在 model（実測表示用）。
    model: Option<String>,
    /// `session/new` | `session/load` で得た現在 mode（実測表示用）。
    permission_mode: Option<String>,
    /// `available_commands_update` で届いた slash command 一覧。
    slash_commands: Vec<String>,
    /// `available_commands_update` で届いた slash command の説明（name → description）。
    command_docs: HashMap<String, String>,
    /// 最後に構成した `SessionInit`（replay で配り直すために保持）。
    session_init: Option<ConversationEvent>,
    /// `usage_update` で届いた直近の context 使用量 `(used_tokens, window_size)`。
    context_usage: Option<(u64, u64)>,
    /// turn 中に slash command が変わり、SessionInit の出し直しを turn の終わりへ送ったか。
    init_dirty: bool,
    /// engine が出した configOptions（`None` = 出さない engine / 未確定）。
    config: Option<Vec<AcpConfigOption>>,
    /// 直近の切替 / 当て直しの失敗（roster で panel に出す）。
    config_error: Option<String>,
    /// session 確定後に当て直す意図（[`AcpHostConfig::config_intent`]）。
    config_intent: Vec<(String, String)>,
    /// 当て直しの残り（1 軸ずつ送り、応答のたびに最新の configOptions で要否を決め直す）。
    reapply_plan: VecDeque<(String, String)>,
    /// 当て直しの 1 本が応答待ちか。true の間は queue を送らない。
    reapply_in_flight: bool,
    /// user の切替の応答を待つ側（request id → 結果の送り先）。
    config_waiters:
        HashMap<i64, tokio::sync::oneshot::Sender<Result<Vec<AcpConfigOption>, String>>>,
    /// turn 実行中か（true の submit は queue へ）。
    turn_active: bool,
    /// `session/load` の replay 列を捨てている最中か（load response で解除 — doc 42 §3）。
    loading: bool,
    /// session 未確定 or turn 実行中 or 当て直し中に来た submit の待ち行列。
    queue: VecDeque<QueuedPrompt>,
    /// disk にまだ載っていない増分 + commit 世代（[`super::host`] と同契約）。
    in_flight: InFlight,
    child_pid: Option<u32>,
    next_id: i64,
    pending: HashMap<i64, ReqKind>,
    /// 明示 stop 中か（途絶 Error の抑止）。
    stopping: bool,
    /// 途絶を観測したか。true の submit は Err（自己修復への配線 — doc 42 冒頭）。
    dead: bool,
    /// stderr の末尾数行（途絶 Error の診断材料 — auth 失効等の原因表示）。
    stderr_tail: VecDeque<String>,
}

impl AcpState {
    fn new(child_pid: Option<u32>) -> Self {
        Self {
            session_id: None,
            model: None,
            permission_mode: None,
            slash_commands: Vec::new(),
            command_docs: HashMap::new(),
            session_init: None,
            context_usage: None,
            init_dirty: false,
            config: None,
            config_error: None,
            config_intent: Vec::new(),
            reapply_plan: VecDeque::new(),
            reapply_in_flight: false,
            config_waiters: HashMap::new(),
            turn_active: false,
            loading: false,
            queue: VecDeque::new(),
            in_flight: InFlight::default(),
            child_pid,
            next_id: 0,
            pending: HashMap::new(),
            stopping: false,
            dead: false,
            stderr_tail: VecDeque::new(),
        }
    }

    fn alloc(&mut self, kind: ReqKind) -> i64 {
        self.next_id += 1;
        self.pending.insert(self.next_id, kind);
        self.next_id
    }

    /// 現在の session メタ情報から `SessionInit` を組み立てて `session_init` にも保持する。
    fn build_session_init(&mut self, cwd: &str) -> Option<ConversationEvent> {
        let session_id = self.session_id.clone()?;
        let init = ConversationEvent::SessionInit {
            session_id,
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            cwd: Some(cwd.to_string()),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            slash_commands: self.slash_commands.clone(),
            command_docs: self.command_docs.clone(),
        };
        self.session_init = Some(init.clone());
        Some(init)
    }

    /// `available_commands_update` を受ける。出し直す SessionInit を返す（無ければ None）。
    ///
    /// - 一覧が変わらなければ出さない（agy は set_config_option のたびに同じ一覧を送り直す、実測）
    /// - session 確定前は覚えるだけ（adopt の SessionInit に載る）
    /// - **turn 中は出さず `init_dirty` に回す** — GUI は SessionInit を「engine が立った」合図として
    ///   活動表示を idle に戻す（`session-activity-bridge.ts`）ので、応答中の lane が idle に見える
    fn note_commands(
        &mut self,
        commands: Vec<String>,
        docs: HashMap<String, String>,
        cwd: &str,
    ) -> Option<ConversationEvent> {
        if self.slash_commands == commands && self.command_docs == docs {
            return None;
        }
        self.slash_commands = commands;
        self.command_docs = docs;
        if self.turn_active {
            self.init_dirty = true;
            // 再接続の配り直し（splice）には最新を載せておく
            self.build_session_init(cwd);
            return None;
        }
        self.build_session_init(cwd)
    }

    /// turn の終わりに、保留していた SessionInit を 1 回だけ取り出す。
    fn take_deferred_init(&mut self, cwd: &str) -> Option<ConversationEvent> {
        if !std::mem::take(&mut self.init_dirty) {
            return None;
        }
        self.build_session_init(cwd)
    }

    /// queue の先頭を送れるなら session/prompt の line を返す（turn を開始扱いにする）。
    ///
    /// 送れないのは: session 未確定 / turn 中 / 当て直しの応答待ち / queue 空。
    fn take_next_prompt(&mut self) -> Option<String> {
        if self.turn_active || self.reapply_in_flight || self.queue.is_empty() {
            return None;
        }
        let session_id = self.session_id.clone()?;
        let next = self.queue.pop_front().expect("non-empty queue");
        self.turn_active = true;
        let id = self.alloc(ReqKind::Prompt);
        Some(build_prompt(id, &session_id, &next.text, &next.images))
    }

    /// 当て直しの次の 1 本の line を返す（無ければ当て直しを終える）。
    ///
    /// 計画の先頭から、**今の** configOptions で当てる必要があるもの（[`needs_apply`]）を探す。
    /// model を替えると他の軸が消えたり既定に戻ったりする（実測）ので、先に全体を決めずに
    /// 1 本ずつ応答を待って決め直す。
    fn next_reapply(&mut self) -> Option<String> {
        let Some(session_id) = self.session_id.clone() else {
            self.reapply_in_flight = false;
            return None;
        };
        while let Some((config_id, value)) = self.reapply_plan.pop_front() {
            if !needs_apply(self.config.as_deref().unwrap_or(&[]), &config_id, &value) {
                continue;
            }
            let id = self.alloc(ReqKind::SetConfig {
                config_id: config_id.clone(),
                value: value.clone(),
                reapply: true,
            });
            self.reapply_in_flight = true;
            return Some(build_set_config_option(id, &session_id, &config_id, &value));
        }
        self.reapply_in_flight = false;
        None
    }

    /// roster に載せる設定面（configOptions を出さない engine は None）。
    fn config_view(&self) -> Option<AcpConfigView> {
        self.config.as_ref().map(|options| AcpConfigView {
            options: options.clone(),
            error: self.config_error.clone(),
        })
    }
}

struct AcpInner {
    engine: AcpEngine,
    event_tx: broadcast::Sender<ConversationEvent>,
    repo: String,
    lane: String,
    cwd: String,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    state: Mutex<AcpState>,
    child: Mutex<Option<Child>>,
}

impl AcpInner {
    /// JSONL 1 行を書く（Err = 途絶。submit 経路は自己修復に繋ぐため握り潰さない）。
    async fn write_line(&self, line: &str) -> std::io::Result<()> {
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "stdin が閉じています",
            ));
        };
        let mut buf = line.as_bytes().to_vec();
        buf.push(b'\n');
        stdin.write_all(&buf).await?;
        stdin.flush().await
    }

    /// reader task 内からの write（失敗は log のみ — 途絶は stdout close 検知に収束）。
    async fn write_line_logged(&self, line: &str) {
        if let Err(e) = self.write_line(line).await {
            tracing::warn!(
                "{} acp stdin write 失敗（途絶疑い）: {e}",
                self.engine.name()
            );
        }
    }

    fn emit(&self, event: ConversationEvent) {
        {
            let mut st = self.state.lock().expect("acp state lock");
            match &event {
                ConversationEvent::MessageChunk { .. } | ConversationEvent::ThoughtChunk { .. } => {
                    st.in_flight.tail.push(event.clone());
                }
                ConversationEvent::SessionInit { .. }
                | ConversationEvent::TurnCompleted { .. }
                | ConversationEvent::Error { .. }
                | ConversationEvent::EngineExited { .. } => {
                    st.in_flight.tail.clear();
                    st.in_flight.seq = st.in_flight.seq.wrapping_add(1);
                }
                _ => {}
            }
        }
        let _ = self.event_tx.send(event);
    }

    /// sessionId 確定（new/load の response）: registry 書き込み + SessionInit + 設定の当て直し +
    /// queue 排出。`result` は new / load の result（model / mode / configOptions の源）。
    async fn adopt_session(self: &Arc<Self>, session_id: &str, result: Option<&serde_json::Value>) {
        let (model, permission_mode) = result.map(extract_session_meta).unwrap_or((None, None));
        let (init, reapply_line) = {
            let mut st = self.state.lock().expect("acp state lock");
            st.session_id = Some(session_id.to_string());
            st.loading = false;
            if model.is_some() {
                st.model = model;
            }
            if permission_mode.is_some() {
                st.permission_mode = permission_mode;
            }
            if let Some(options) = result
                .and_then(|r| r.get("configOptions"))
                .and_then(parse_config_options)
            {
                st.config = Some(options);
            }
            // registry の意図を 1 軸ずつ戻す（doc 88 §6 — agy は load 後に既定へ戻る、実測）。
            // 応答が揃うまで queue は送らない（take_next_prompt が待つ）。
            st.reapply_plan = st.config_intent.iter().cloned().collect();
            let line = st.next_reapply();
            let init = st
                .build_session_init(&self.cwd)
                .expect("session_id is Some");
            (init, line)
        };
        // doc 40 §4: registry 直結（grok / opencode / agy は registry-native — 旧 store が最初から無い）。
        let (lane_label, key) = crate::lane::session_registry::parse_session_label(&self.lane);
        if let Err(e) = crate::lane::session_registry::set_conversation(
            &self.repo,
            lane_label,
            self.engine.name(),
            key,
            Some(session_id),
        ) {
            tracing::warn!(
                "{} sessionId の registry 記録失敗（repo={}, lane={}）: {e}",
                self.engine.name(),
                self.repo,
                self.lane
            );
        }
        self.emit(init);
        if let Some(line) = reapply_line {
            self.write_line_logged(&line).await;
            self.watch_reapply();
        }
        self.drain_queue().await;
    }

    /// 当て直しの見張り: 上限までに終わらなければ打ち切って、待たせていた prompt を流す
    /// （応答が来ない engine で submit が黙って queue に溜まり続けるのを防ぐ）。
    fn watch_reapply(self: &Arc<Self>) {
        let inner = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CONFIG_SET_TIMEOUT).await;
            let gave_up = {
                let mut st = inner.state.lock().expect("acp state lock");
                if st.reapply_in_flight {
                    st.reapply_in_flight = false;
                    st.reapply_plan.clear();
                    st.config_error = Some(format!(
                        "前回の設定を戻す応答が {} 秒以内にありません（選び直すと解消します）",
                        CONFIG_SET_TIMEOUT.as_secs()
                    ));
                    true
                } else {
                    false
                }
            };
            if gave_up {
                tracing::warn!(
                    "{} 設定の当て直しを打ち切り（repo={}, lane={}）",
                    inner.engine.name(),
                    inner.repo,
                    inner.lane
                );
                inner.drain_queue().await;
            }
        });
    }

    /// queue の先頭を session/prompt として送る（[`AcpState::take_next_prompt`] が送れる時だけ）。
    async fn drain_queue(&self) {
        let line = self
            .state
            .lock()
            .expect("acp state lock")
            .take_next_prompt();
        if let Some(line) = line {
            self.write_line_logged(&line).await;
        }
    }
}

/// lane 単位の常駐 ACP host（gui、doc 42・43・88）。grok / opencode / agy の 3 engine を
/// [`AcpEngine`] パラメータ（cli_path / spawn_args / name）で共有する — host ロジックは
/// engine 非依存。
pub struct AcpAgentHost {
    inner: Arc<AcpInner>,
    reader: Option<JoinHandle<()>>,
}

impl AcpAgentHost {
    /// engine の ACP プロセス（grok = `grok agent --always-approve stdio` / opencode =
    /// `opencode acp` / agy = `agy acp --dangerously-skip-permissions`）を起動し、
    /// handshake（initialize → session/load|new）を開始する。
    /// 完了前の submit は queue に積まれ自動送出される。
    pub fn spawn(config: AcpHostConfig) -> anyhow::Result<Self> {
        let engine = config.engine;
        let mut cmd = tokio::process::Command::new(engine.cli_path());
        for key in crate::spawn_env::inherited_agent_host_env_to_strip() {
            cmd.env_remove(key);
        }
        cmd.args(engine.spawn_args())
            .current_dir(&config.cwd)
            // identity env（doc 51 §1 A3b）: engine（とその shell tool の子）が `vp now` /
            // wire で自分を名乗る口。tui の agent_spawner / claude host と同じ契約。
            .env("VP_REPO", &config.repo)
            .env("VP_LANE", &config.lane_label)
            .env("VP_SESSION_KEY", config.session_key.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("{} の起動に失敗: {e}", engine.name()))?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("{} の stdout が取れません", engine.name()))?;
        let stderr = child.stderr.take();
        let child_pid = child.id();

        let (event_tx, _rx) = broadcast::channel::<ConversationEvent>(256);
        let inner = Arc::new(AcpInner {
            engine,
            event_tx,
            repo: config.repo,
            lane: config.lane,
            cwd: config.cwd,
            stdin: tokio::sync::Mutex::new(stdin),
            state: Mutex::new(AcpState {
                config_intent: config.config_intent,
                ..AcpState::new(child_pid)
            }),
            child: Mutex::new(Some(child)),
        });
        if let Some(stderr) = stderr {
            let drain = inner.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    tracing::warn!("{} stderr: {line}", drain.engine.name());
                    let mut st = drain.state.lock().expect("acp state lock");
                    if st.stderr_tail.len() >= 5 {
                        st.stderr_tail.pop_front();
                    }
                    st.stderr_tail.push_back(line);
                }
            });
        }
        let resume_target = config.session_id;
        tracing::info!(
            "AcpAgentHost spawn（常駐 {} ACP、repo={}, lane={}, resume={:?}, pid={:?}）",
            engine.name(),
            inner.repo,
            inner.lane,
            resume_target.as_deref().unwrap_or("new"),
            child_pid
        );
        let reader = tokio::spawn(run_reader(inner.clone(), stdout, resume_target));
        Ok(Self {
            inner,
            reader: Some(reader),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.inner.event_tx.subscribe()
    }

    /// engine の設定面（roster の `acp_config` の供給源、doc 88 §6）。
    pub fn config_view(&self) -> Option<AcpConfigView> {
        self.inner
            .state
            .lock()
            .expect("acp state lock")
            .config_view()
    }

    /// user の設定切替の口（pool の lock を離してから応答を待つため、host の外へ clone して持ち出す）。
    pub fn config_handle(&self) -> AcpConfigHandle {
        AcpConfigHandle {
            inner: self.inner.clone(),
        }
    }

    pub fn session_init(&self) -> Option<ConversationEvent> {
        self.inner
            .state
            .lock()
            .expect("acp state lock")
            .session_init
            .clone()
    }

    pub fn in_flight(&self) -> InFlight {
        self.inner
            .state
            .lock()
            .expect("acp state lock")
            .in_flight
            .clone()
    }

    pub fn commit_seq(&self) -> u64 {
        self.inner
            .state
            .lock()
            .expect("acp state lock")
            .in_flight
            .seq
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.state.lock().expect("acp state lock").child_pid
    }

    /// ユーザープロンプトを投入する（idle なら即 session/prompt、それ以外は queue）。
    /// 途絶時は Err（`ensure_and_submit_chat` の自己修復に繋ぐ — doc 42 冒頭）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        self.submit_with_images(prompt, &[]).await
    }

    /// 画像を添えて投入する（ACP の image content block、doc 88 §6）。画像を運べるかは
    /// engine の能力（[`super::EngineKind::image_capable`]）で、呼び手（ChatHost）が gate する。
    pub async fn submit_with_images(
        &self,
        prompt: &str,
        images: &[ImageInput],
    ) -> anyhow::Result<()> {
        let line = {
            let mut st = self.inner.state.lock().expect("acp state lock");
            if st.dead {
                anyhow::bail!(
                    "{} が終了しています（engine 再起動で復旧）",
                    self.inner.engine.name()
                );
            }
            st.queue.push_back(QueuedPrompt {
                text: prompt.to_string(),
                images: images.to_vec(),
            });
            // 送れる状態なら今送る（session 未確定 / turn 中 / 当て直し中は queue で待つ）
            st.take_next_prompt()
        };
        if let Some(line) = line
            && let Err(e) = self.inner.write_line(&line).await
        {
            let mut st = self.inner.state.lock().expect("acp state lock");
            st.dead = true;
            st.turn_active = false;
            anyhow::bail!("{} への送信に失敗（途絶）: {e}", self.inner.engine.name());
        }
        Ok(())
    }

    /// 実行中 turn を中断する（`session/cancel` notification — プロセスは殺さない）。
    /// prompt response が stopReason=cancelled で返り、通常の完了経路で streaming が畳まれる。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        let line = {
            let mut st = self.inner.state.lock().expect("acp state lock");
            st.queue.clear();
            match (&st.session_id, st.turn_active) {
                (Some(session_id), true) => Some(build_cancel(session_id)),
                _ => None,
            }
        };
        if let Some(line) = line {
            self.inner.write_line_logged(&line).await;
        }
        Ok(())
    }

    /// 明示 teardown（[`super::engine::ChatEngineSlot`] Drop から呼ぶ）。
    pub fn stop(&mut self) {
        {
            let mut st = self.inner.state.lock().expect("acp state lock");
            st.stopping = true;
            st.dead = true;
            st.queue.clear();
            // reader を abort するので途絶の後始末（下の run_reader の末尾）は走らない —
            // 設定切替の応答を待つ側をここで解く（sender を落とす =「応答の前に終了」）。
            st.config_waiters.clear();
        }
        if let Some(mut child) = self.inner.child.lock().expect("child lock").take() {
            let _ = child.start_kill();
        }
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        tracing::info!(
            "AcpAgentHost stop（repo={}, lane={}）",
            self.inner.repo,
            self.inner.lane
        );
    }
}

/// user の設定切替の口（[`AcpAgentHost::config_handle`]）。host の状態を共有する薄い clone。
///
/// pool の lock を握ったまま engine の応答を待たないために、host から切り離して持ち出す。
#[derive(Clone)]
pub struct AcpConfigHandle {
    inner: Arc<AcpInner>,
}

impl AcpConfigHandle {
    /// `session/set_config_option` を送り、engine の応答まで待つ（上限 [`CONFIG_SET_TIMEOUT`]）。
    ///
    /// 成功で configOptions（候補を含む全体）が更新され、それを返す（model を替えると他の軸の
    /// 候補が変わるため、呼び手が意図を整えるのに使う）。失敗は engine のエラー文を返し、
    /// roster の `acp_config.error` にも残る。永続（registry）は呼び手の仕事（engine 別の形）。
    pub async fn set(&self, config_id: &str, value: &str) -> Result<Vec<AcpConfigOption>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (id, line) = {
            let mut st = self.inner.state.lock().expect("acp state lock");
            if st.dead {
                return Err(format!(
                    "{} が終了しています（chat を開き直すと立ち上がります）",
                    self.inner.engine.name()
                ));
            }
            let Some(session_id) = st.session_id.clone() else {
                return Err(format!(
                    "{} の会話がまだ始まっていません（少し待ってから切り替えてください）",
                    self.inner.engine.name()
                ));
            };
            let id = st.alloc(ReqKind::SetConfig {
                config_id: config_id.to_string(),
                value: value.to_string(),
                reapply: false,
            });
            st.config_waiters.insert(id, tx);
            (
                id,
                build_set_config_option(id, &session_id, config_id, value),
            )
        };
        if let Err(e) = self.inner.write_line(&line).await {
            self.inner
                .state
                .lock()
                .expect("acp state lock")
                .config_waiters
                .remove(&id);
            return Err(format!(
                "{} への送信に失敗（途絶）: {e}",
                self.inner.engine.name()
            ));
        }
        match tokio::time::timeout(CONFIG_SET_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!(
                "{} が応答の前に終了しました",
                self.inner.engine.name()
            )),
            Err(_) => {
                let message = format!(
                    "{} から {} 秒以内に応答がありません",
                    self.inner.engine.name(),
                    CONFIG_SET_TIMEOUT.as_secs()
                );
                // 遅れて届いた応答は捨てる（pending から外す）— 呼び手は registry に書かずに
                // 失敗を返すので、遅い成功で engine の状態だけが進む食い違いを作らない。
                // engine が実際に変えた値は config_option_update で roster に出る。
                let mut st = self.inner.state.lock().expect("acp state lock");
                st.config_waiters.remove(&id);
                st.pending.remove(&id);
                st.config_error = Some(message.clone());
                Err(message)
            }
        }
    }
}

// =============================================================================
// calculations — JSON-RPC line 組み立て（純関数、単体テスト対象）
// =============================================================================

fn build_initialize(id: i64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            // fs capability は宣言しない（agent 自前の tool で読む — VP は console/chat 面のみ）。
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } },
        }
    })
    .to_string()
}

fn build_session_new(id: i64, cwd: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/new",
        "params": { "cwd": cwd, "mcpServers": [] }
    })
    .to_string()
}

fn build_session_load(id: i64, session_id: &str, cwd: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/load",
        "params": { "sessionId": session_id, "cwd": cwd, "mcpServers": [] }
    })
    .to_string()
}

/// `session/prompt`。画像は ACP の image content block（`{type, mimeType, data}`）で text の後ろ。
fn build_prompt(id: i64, session_id: &str, prompt: &str, images: &[ImageInput]) -> String {
    let mut blocks = vec![serde_json::json!({ "type": "text", "text": prompt })];
    blocks.extend(images.iter().map(|img| {
        serde_json::json!({ "type": "image", "mimeType": img.media_type, "data": img.data_base64 })
    }));
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/prompt",
        "params": { "sessionId": session_id, "prompt": blocks }
    })
    .to_string()
}

/// `session/set_config_option`（doc 88 §6 — 応答は configOptions の全体）。
fn build_set_config_option(id: i64, session_id: &str, config_id: &str, value: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/set_config_option",
        "params": { "sessionId": session_id, "configId": config_id, "value": value }
    })
    .to_string()
}

fn build_cancel(session_id: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": { "sessionId": session_id }
    })
    .to_string()
}

/// `session/request_permission` への自動 allow 応答（doc 42 §2 — bypass parity）。
/// allow 系 option（kind が "allow" 始まり）を優先、無ければ先頭 option。
fn build_permission_allow(id: &serde_json::Value, params: &serde_json::Value) -> String {
    let options = params
        .get("options")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let chosen = options
        .iter()
        .find(|o| {
            o.get("kind")
                .and_then(|k| k.as_str())
                .is_some_and(|k| k.starts_with("allow"))
        })
        .or_else(|| options.first());
    let option_id = chosen
        .and_then(|o| o.get("optionId"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "outcome": { "outcome": "selected", "optionId": option_id } }
    })
    .to_string()
}

fn error_message(err: &serde_json::Value) -> String {
    err.get("message")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| err.to_string())
}

/// `session/new` / `session/load` の result から `(model, permission_mode)` を抽出する（純関数）。
///
/// - `model`: `models.currentModelId` を優先し、無ければ `configOptions` の `id=="model"` の `currentValue`。
/// - `permission_mode`: `modes.currentModeId`。
fn extract_session_meta(result: &serde_json::Value) -> (Option<String>, Option<String>) {
    let model = result
        .pointer("/models/currentModelId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            result
                .get("configOptions")
                .and_then(|v| v.as_array())?
                .iter()
                .find(|opt| opt.get("id").and_then(|v| v.as_str()) == Some("model"))?
                .get("currentValue")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
    let permission_mode = result
        .pointer("/modes/currentModeId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (model, permission_mode)
}

/// `session/update` の `available_commands_update` から `(slash_commands, command_docs)` を抽出する（純関数）。
fn extract_available_commands(
    update: &serde_json::Value,
) -> Option<(Vec<String>, HashMap<String, String>)> {
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("available_commands_update") {
        return None;
    }
    let arr = update.get("availableCommands")?.as_array()?;
    let mut commands = Vec::with_capacity(arr.len());
    let mut docs = HashMap::new();
    for item in arr {
        let Some(name) = item
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        commands.push(name.to_string());
        if let Some(desc) = item
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            docs.insert(name.to_string(), desc.to_string());
        }
    }
    Some((commands, docs))
}

/// `session/update` の `current_mode_update` から mode id を抽出する（純関数、実測 agy 1.5.9）。
fn extract_current_mode(update: &serde_json::Value) -> Option<String> {
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("current_mode_update") {
        return None;
    }
    update
        .get("currentModeId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `session/update` の `usage_update` から `(used_tokens, window_size)` を抽出する（純関数）。
fn extract_usage_update(update: &serde_json::Value) -> Option<(u64, u64)> {
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("usage_update") {
        return None;
    }
    let used = update.get("used")?.as_u64()?;
    let size = update.get("size")?.as_u64().filter(|&s| s > 0)?;
    Some((used, size))
}

// =============================================================================
// actions — reader loop（handshake 状態機械 + notification 配信）
// =============================================================================

async fn run_reader(
    inner: Arc<AcpInner>,
    stdout: tokio::process::ChildStdout,
    resume_target: Option<String>,
) {
    let mut translator = AcpTranslator::new();
    let mut lines = BufReader::new(stdout).lines();

    let init_line = {
        let mut st = inner.state.lock().expect("acp state lock");
        let id = st.alloc(ReqKind::Initialize);
        build_initialize(id)
    };
    inner.write_line_logged(&init_line).await;

    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // server → client request（id + method）: permission は自動 allow、他は未対応 error。
        if let (Some(id), Some(method)) =
            (msg.get("id"), msg.get("method").and_then(|m| m.as_str()))
        {
            let params = msg.get("params").cloned().unwrap_or_default();
            if method == "session/request_permission" {
                // 自動 allow（bypass parity）。grok は `--always-approve`、agy は
                // `--dangerously-skip-permissions` と二段防御、opencode はこの一段のみ
                // （doc 42 §6 / doc 43 §6 / doc 88 §2）。
                inner
                    .write_line_logged(&build_permission_allow(id, &params))
                    .await;
            } else {
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "not supported by VP AcpAgentHost" }
                });
                inner.write_line_logged(&resp.to_string()).await;
            }
            continue;
        }
        // response
        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            handle_response(&inner, id, &msg, &resume_target).await;
            continue;
        }
        // notification
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method == "session/update" {
            // load 中の replay 列は捨てる（VP 自前 replay との二重化防止 — doc 42 §3）。
            if inner.state.lock().expect("acp state lock").loading {
                continue;
            }
            if let Some(update) = msg.pointer("/params/update") {
                if let Some((commands, docs)) = extract_available_commands(update) {
                    // 変化した時だけ SessionInit を出し直す。turn 中は終わりまで保留（note_commands）。
                    let updated_init = inner
                        .state
                        .lock()
                        .expect("acp state lock")
                        .note_commands(commands, docs, &inner.cwd);
                    if let Some(init) = updated_init {
                        inner.emit(init);
                    }
                    continue;
                }
                if let Some(usage) = extract_usage_update(update) {
                    inner.state.lock().expect("acp state lock").context_usage = Some(usage);
                    continue;
                }
                // 設定面の更新（set_config_option の後 / engine 自身の変更、doc 88 §6）は roster が
                // 拾う状態だけを書き換える（会話面の event にはしない）。
                if let Some(options) = parse_config_option_update(update) {
                    inner.state.lock().expect("acp state lock").config = Some(options);
                    continue;
                }
                if let Some(mode) = extract_current_mode(update) {
                    inner.state.lock().expect("acp state lock").permission_mode = Some(mode);
                    continue;
                }
                for event in translator.ingest(update) {
                    inner.emit(event);
                }
            }
        }
        // `_x.ai/*` 拡張・その他の notification は無視（doc 42 §1 で会話成立を実測確認）。
    }

    // stdout close = 途絶。dead を立てて submit を Err に倒す（自己修復への配線）。
    let (stopping, stderr_tail) = {
        let mut st = inner.state.lock().expect("acp state lock");
        st.dead = true;
        st.turn_active = false;
        // 切替の応答を待つ側を解く（sender を落とす = 「応答の前に終了」を返す）
        st.config_waiters.clear();
        (
            st.stopping,
            st.stderr_tail
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    if !stopping {
        tracing::warn!(
            "{} 途絶（repo={}, lane={}）",
            inner.engine.name(),
            inner.repo,
            inner.lane
        );
        let detail = if stderr_tail.is_empty() {
            String::new()
        } else {
            format!("\n{stderr_tail}")
        };
        inner.emit(ConversationEvent::EngineExited {
            message: format!(
                "{} が休眠しました。次の送信で再開します。{detail}",
                inner.engine.name()
            ),
        });
    }
}

async fn handle_response(
    inner: &Arc<AcpInner>,
    id: i64,
    msg: &serde_json::Value,
    resume_target: &Option<String>,
) {
    let kind = {
        let mut st = inner.state.lock().expect("acp state lock");
        st.pending.remove(&id)
    };
    let Some(kind) = kind else {
        return;
    };
    let error = msg.get("error").filter(|v| !v.is_null());
    match kind {
        ReqKind::Initialize => {
            if let Some(err) = error {
                inner.emit(ConversationEvent::Error {
                    message: format!(
                        "{} initialize 失敗: {}",
                        inner.engine.name(),
                        error_message(err)
                    ),
                });
                return;
            }
            let line = {
                let mut st = inner.state.lock().expect("acp state lock");
                match resume_target {
                    Some(sid) => {
                        st.loading = true; // load の replay 列を捨てる窓を開く
                        let id = st.alloc(ReqKind::SessionLoad);
                        build_session_load(id, sid, &inner.cwd)
                    }
                    None => {
                        let id = st.alloc(ReqKind::SessionNew);
                        build_session_new(id, &inner.cwd)
                    }
                }
            };
            inner.write_line_logged(&line).await;
        }
        ReqKind::SessionLoad => {
            if let Some(err) = error {
                // load 空振り → new へ self-heal（新 sessionId が registry を上書き）。
                tracing::warn!(
                    "{} session/load 失敗 → session/new へ self-heal（repo={}, lane={}）: {}",
                    inner.engine.name(),
                    inner.repo,
                    inner.lane,
                    error_message(err)
                );
                let line = {
                    let mut st = inner.state.lock().expect("acp state lock");
                    st.loading = false;
                    let id = st.alloc(ReqKind::SessionNew);
                    build_session_new(id, &inner.cwd)
                };
                inner.write_line_logged(&line).await;
                return;
            }
            // load 成功: sessionId は指名したものがそのまま有効。
            if let Some(sid) = resume_target.as_deref() {
                inner.adopt_session(sid, msg.get("result")).await;
            }
        }
        ReqKind::SessionNew => {
            if let Some(err) = error {
                inner.emit(ConversationEvent::Error {
                    message: format!(
                        "{} session/new 失敗: {}",
                        inner.engine.name(),
                        error_message(err)
                    ),
                });
                return;
            }
            if let Some(sid) = msg.pointer("/result/sessionId").and_then(|v| v.as_str()) {
                inner.adopt_session(sid, msg.get("result")).await;
            }
        }
        ReqKind::Prompt => {
            // response 到着 = turn 終了（stopReason 問わず — cancelled も完了、doc 42 §2）。
            let (session_id, context_usage, deferred_init) = {
                let mut st = inner.state.lock().expect("acp state lock");
                st.turn_active = false;
                (
                    st.session_id.clone().unwrap_or_default(),
                    st.context_usage,
                    st.take_deferred_init(&inner.cwd),
                )
            };
            if let Some(err) = error {
                inner.emit(ConversationEvent::Error {
                    message: format!("{} turn 失敗: {}", inner.engine.name(), error_message(err)),
                });
            }
            // turn 中に保留した SessionInit は TurnCompleted の**前**に出す（活動表示は
            // idle → completed と即座に進み、応答中に idle を挟まない）。
            if let Some(init) = deferred_init {
                inner.emit(init);
            }
            let (context_tokens, context_window) = context_usage
                .map(|(u, w)| (Some(u), Some(w)))
                .unwrap_or((None, None));
            inner.emit(ConversationEvent::TurnCompleted {
                session_id,
                cost_usd: None,
                context_tokens,
                context_window,
            });
            inner.drain_queue().await;
        }
        ReqKind::SetConfig {
            config_id,
            value,
            reapply,
        } => {
            let (waiter, result, next_step) = {
                let mut st = inner.state.lock().expect("acp state lock");
                let result = match error {
                    Some(err) => {
                        let message = format!(
                            "{config_id} を {value} にできません: {}",
                            error_message(err)
                        );
                        // 当て直しの失敗は registry から消さない — 使える model は契約や枠で
                        // 変わり得る（`_meta.antigravity.tier`）ので、一時的に出ない値を自動で
                        // 消すと user の選択が黙って失われる。選び直すと次の成功で消える。
                        st.config_error = Some(if reapply {
                            format!(
                                "前回の設定を戻せませんでした（選び直すと解消します） — {message}"
                            )
                        } else {
                            message.clone()
                        });
                        Err(message)
                    }
                    None => {
                        if let Some(options) = msg
                            .pointer("/result/configOptions")
                            .and_then(parse_config_options)
                        {
                            st.config = Some(options);
                        }
                        // 当て直しの成功で user の失敗表示は消さない（別の軸の失敗かもしれない）
                        if !reapply {
                            st.config_error = None;
                        }
                        Ok(st.config.clone().unwrap_or_default())
                    }
                };
                // 当て直し: 失敗しても残りの軸は続ける。最新の configOptions で次の 1 本を決める
                // （尽きたら None = 待たせていた prompt を流す）。
                let next_step = reapply.then(|| st.next_reapply());
                (st.config_waiters.remove(&id), result, next_step)
            };
            if let Err(e) = &result {
                tracing::warn!(
                    "{} set_config_option 失敗（repo={}, lane={}, reapply={reapply}）: {e}",
                    inner.engine.name(),
                    inner.repo,
                    inner.lane
                );
            }
            if let Some(waiter) = waiter {
                let _ = waiter.send(result);
            }
            match next_step {
                Some(Some(line)) => inner.write_line_logged(&line).await,
                // 当て直しが終わった = 待たせていた prompt を送ってよい
                Some(None) => inner.drain_queue().await,
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// request line の形を doc 42 §1 の実測 wire に固定（ACP は jsonrpc field を含む）。
    #[test]
    fn request_lines_match_acp_wire_shapes() {
        let init: serde_json::Value = serde_json::from_str(&build_initialize(1)).unwrap();
        assert_eq!(
            init["jsonrpc"], "2.0",
            "ACP は jsonrpc field を含む（codex と違う）"
        );
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["protocolVersion"], 1);
        assert_eq!(
            init["params"]["clientCapabilities"]["fs"]["readTextFile"],
            false
        );

        let new: serde_json::Value = serde_json::from_str(&build_session_new(2, "/w")).unwrap();
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["params"]["cwd"], "/w");

        let load: serde_json::Value =
            serde_json::from_str(&build_session_load(3, "019f-abc", "/w")).unwrap();
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["sessionId"], "019f-abc");

        let prompt: serde_json::Value =
            serde_json::from_str(&build_prompt(4, "019f-abc", "hi", &[])).unwrap();
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["prompt"][0]["type"], "text");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "hi");

        let cancel: serde_json::Value = serde_json::from_str(&build_cancel("019f-abc")).unwrap();
        assert_eq!(cancel["method"], "session/cancel");
        assert!(
            cancel.get("id").is_none(),
            "cancel は notification（id なし）"
        );
    }

    /// permission 自動 allow: allow 系 kind を優先、無ければ先頭。id は verbatim echo。
    #[test]
    fn permission_allow_picks_allow_kind_option() {
        let params: serde_json::Value = serde_json::from_str(
            r#"{"options":[{"optionId":"rej","kind":"reject_once"},{"optionId":"ok","kind":"allow_once"}]}"#,
        )
        .unwrap();
        let resp: serde_json::Value =
            serde_json::from_str(&build_permission_allow(&serde_json::json!(7), &params)).unwrap();
        assert_eq!(resp["id"], 7);
        assert_eq!(resp["result"]["outcome"]["outcome"], "selected");
        assert_eq!(
            resp["result"]["outcome"]["optionId"], "ok",
            "allow 系を優先"
        );

        // allow 系が無い場合は先頭 option（防御的 — 実測では allow 系が常在）
        let params2: serde_json::Value =
            serde_json::from_str(r#"{"options":[{"optionId":"first","kind":"reject_once"}]}"#)
                .unwrap();
        let resp2: serde_json::Value =
            serde_json::from_str(&build_permission_allow(&serde_json::json!(8), &params2)).unwrap();
        assert_eq!(resp2["result"]["outcome"]["optionId"], "first");
    }

    /// JSONL 1 行 = 1 message（改行を含まない）。
    #[test]
    fn built_lines_are_single_line() {
        for line in [
            build_initialize(1),
            build_session_new(2, "/w"),
            build_session_load(3, "s", "/w"),
            build_prompt(4, "s", "複数\n行", &[]),
            build_set_config_option(5, "s", "mode", "plan"),
            build_cancel("s"),
        ] {
            assert!(!line.contains('\n'), "JSONL 1 行: {line}");
        }
    }

    /// 画像は ACP の image content block（`{type, mimeType, data}`）で text の後ろに並べる
    /// （doc 88 §6。agy は `promptCapabilities.image: true`）。
    #[test]
    fn build_prompt_carries_images_as_acp_image_blocks() {
        let img = crate::conversation::ImageInput {
            media_type: "image/png".into(),
            data_base64: "iVBORw0KGgo=".into(),
        };
        let v: serde_json::Value =
            serde_json::from_str(&build_prompt(4, "sid", "何色？", &[img])).unwrap();
        let prompt = v["params"]["prompt"].as_array().expect("prompt 配列");
        assert_eq!(prompt.len(), 2);
        assert_eq!(
            prompt[0],
            serde_json::json!({"type": "text", "text": "何色？"})
        );
        assert_eq!(
            prompt[1],
            serde_json::json!({"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo="})
        );
    }

    /// `session/set_config_option` の形（実測 agy 1.5.9 で応答を確認した params）。
    #[test]
    fn set_config_option_request_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&build_set_config_option(9, "sid", "thought_level", "low"))
                .unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 9);
        assert_eq!(v["method"], "session/set_config_option");
        assert_eq!(
            v["params"],
            serde_json::json!({"sessionId": "sid", "configId": "thought_level", "value": "low"})
        );
    }

    /// `current_mode_update`（mode を切り替えた時に届く、実測）から mode を拾う。
    #[test]
    fn extract_current_mode_from_update() {
        let update = serde_json::json!({"sessionUpdate": "current_mode_update", "currentModeId": "accept-edits"});
        assert_eq!(
            extract_current_mode(&update).as_deref(),
            Some("accept-edits")
        );
        let other = serde_json::json!({"sessionUpdate": "usage_update", "used": 1, "size": 2});
        assert_eq!(extract_current_mode(&other), None);
    }

    /// slash command の更新（`available_commands_update`）は SessionInit を出し直すが、
    /// **turn の途中では出さず turn の終わりに 1 回だけ**出す。GUI は SessionInit を
    /// 「engine が立った」合図として活動表示を idle に戻すため、turn 中に出すと応答中の
    /// lane が idle に見える（session-activity-bridge / console.ts の vp:console-ready）。
    #[test]
    fn command_updates_defer_session_init_until_turn_end() {
        let mut st = AcpState::new(None);
        // session 確定前: 覚えるだけ（adopt_session の SessionInit に載る）
        assert!(
            st.note_commands(vec!["goal".into()], HashMap::new(), "/w")
                .is_none()
        );
        assert_eq!(st.slash_commands, vec!["goal".to_string()]);

        st.session_id = Some("sid".into());
        // idle で変化 → 即出す
        assert!(
            st.note_commands(vec!["goal".into(), "plan".into()], HashMap::new(), "/w")
                .is_some()
        );
        // 同じ一覧 → 出さない（agy は set_config_option のたびに同じ一覧を送り直す、実測）
        assert!(
            st.note_commands(vec!["goal".into(), "plan".into()], HashMap::new(), "/w")
                .is_none()
        );
        // turn 中の変化 → 保留
        st.turn_active = true;
        assert!(
            st.note_commands(vec!["compact".into()], HashMap::new(), "/w")
                .is_none()
        );
        st.turn_active = false;
        match st.take_deferred_init("/w") {
            Some(ConversationEvent::SessionInit { slash_commands, .. }) => {
                assert_eq!(slash_commands, vec!["compact".to_string()])
            }
            other => panic!("turn の終わりに保留分の SessionInit を出す: {other:?}"),
        }
        assert!(st.take_deferred_init("/w").is_none(), "1 回だけ");
    }

    /// 当て直し（registry の意図を set_config_option で戻す）の応答待ちの間は prompt を送らない
    /// — 前回の model / mode に戻る前の engine で turn を走らせない（doc 88 §6）。
    #[test]
    fn queued_prompt_waits_for_config_reapply() {
        let mut st = AcpState::new(None);
        st.session_id = Some("sid".into());
        st.queue.push_back(QueuedPrompt {
            text: "hi".into(),
            images: Vec::new(),
        });
        st.reapply_in_flight = true;
        assert!(st.take_next_prompt().is_none(), "当て直し中は待つ");
        st.reapply_in_flight = false;
        let line = st.take_next_prompt().expect("当て直し後に送る");
        assert!(line.contains("session/prompt"));
        assert!(st.turn_active);
        assert!(st.take_next_prompt().is_none(), "turn 中は次を送らない");
    }

    /// 当て直しは 1 軸ずつ、**直前の応答で更新した configOptions で**要否を決める（実測: model を
    /// 替えると thought_level の軸が消える / 既定に戻る）。現在値と同じ・軸の無いものは飛ばす。
    #[test]
    fn reapply_goes_one_axis_at_a_time_against_latest_config() {
        let config = |level: Option<&str>| {
            let mut opts = vec![serde_json::json!({"id": "model", "type": "select",
                "currentValue": "gemini-4-argon", "options": [{"value": "gemini-4-argon"}]})];
            if let Some(level) = level {
                opts.push(serde_json::json!({"id": "thought_level", "type": "select",
                    "currentValue": level, "options": [{"value": "low"}, {"value": "medium"}]}));
            }
            parse_config_options(&serde_json::Value::Array(opts))
        };
        let mut st = AcpState::new(None);
        st.session_id = Some("sid".into());
        st.config = config(Some("medium"));
        st.reapply_plan = VecDeque::from(vec![
            ("model".to_string(), "gemini-4-argon".to_string()),
            ("thought_level".to_string(), "low".to_string()),
            ("mode".to_string(), "plan".to_string()),
        ]);
        // model は現在値と同じ → 飛ばして thought_level を送る
        let line = st.next_reapply().expect("thought_level を送る");
        assert!(line.contains("\"configId\":\"thought_level\""), "{line}");
        assert!(st.reapply_in_flight);
        // 応答で軸が消えた（model によっては thought_level が無い）→ mode も軸が無い → 終わり
        st.reapply_in_flight = false;
        st.config = config(None);
        assert!(st.next_reapply().is_none(), "残りは engine に軸が無い");
        assert!(!st.reapply_in_flight);
        assert!(st.reapply_plan.is_empty());
    }

    /// engine パラメタ化（doc 43 §2・doc 88 §2）: spawn 引数と名乗りが engine 別に切り替わる。
    /// grok = `agent --always-approve stdio`（permission 二段防御）、opencode = `acp` のみ
    /// （flag 注入なし — model は opencode config が SSOT）、agy = `acp --dangerously-skip-permissions`
    /// （permission 二段防御）。名乗りは registry / ログ / エラー文言に載る agent 名（EngineKind::agent_name と一致）。
    #[test]
    fn acp_engine_spawn_args_and_name_are_engine_specific() {
        assert_eq!(
            AcpEngine::Grok.spawn_args(),
            &["agent", "--always-approve", "stdio"],
            "grok は agent サブコマンド + 二段防御 flag"
        );
        assert_eq!(
            AcpEngine::OpenCode.spawn_args(),
            &["acp"],
            "opencode は acp サブコマンドのみ（flag なし — doc 43 §3）"
        );
        assert_eq!(
            AcpEngine::Agy.spawn_args(),
            &["acp", "--dangerously-skip-permissions"],
            "agy は acp サブコマンド + 二段防御 flag（doc 88 §2）"
        );
        assert_eq!(AcpEngine::Grok.name(), "grok");
        assert_eq!(AcpEngine::OpenCode.name(), "opencode");
        assert_eq!(AcpEngine::Agy.name(), "agy");
        // agent 名（EngineKind）と registry / ログの名乗り（AcpEngine）が一致する不変条件。
        assert_eq!(
            AcpEngine::Grok.name(),
            super::super::EngineKind::Grok.agent_name()
        );
        assert_eq!(
            AcpEngine::OpenCode.name(),
            super::super::EngineKind::OpenCode.agent_name()
        );
        assert_eq!(
            AcpEngine::Agy.name(),
            super::super::EngineKind::Agy.agent_name()
        );
    }

    /// doc 88 §3: agy acp の実測応答から model / mode / available_commands / usage を抽出する。
    #[test]
    fn extract_session_meta_and_updates_from_agy_real_wire() {
        let session_new_result: serde_json::Value = serde_json::from_str(
            r#"{
                "sessionId": "29fa9ed4-cd23-429f-9a63-a5d490b0d9b3",
                "models": { "currentModelId": "gemini-4-argon-medium" },
                "modes": { "currentModeId": "default" }
            }"#,
        )
        .unwrap();
        assert_eq!(
            extract_session_meta(&session_new_result),
            (
                Some("gemini-4-argon-medium".to_string()),
                Some("default".to_string())
            )
        );

        let cmds_update: serde_json::Value = serde_json::from_str(
            r#"{
                "sessionUpdate": "available_commands_update",
                "availableCommands": [
                    {"name": "goal", "description": "Run until finished."},
                    {"name": "compact", "description": "Compact history."}
                ]
            }"#,
        )
        .unwrap();
        let (cmds, docs) = extract_available_commands(&cmds_update).expect("commands");
        assert_eq!(cmds, vec!["goal", "compact"]);
        assert_eq!(
            docs.get("goal").map(String::as_str),
            Some("Run until finished.")
        );

        let usage_update: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate": "usage_update", "used": 9546, "size": 1048576}"#,
        )
        .unwrap();
        assert_eq!(extract_usage_update(&usage_update), Some((9546, 1_048_576)));
    }

    /// 実機 E2E: 本物の `grok agent stdio` と handshake → session/new → prompt →
    /// TurnCompleted + registry 書き戻しまで。CI では走らない（grok CLI + auth +
    /// トークン消費）— ローカルで `cargo test -p vantage-point --lib e2e_real_grok -- --ignored`。
    #[tokio::test]
    #[ignore = "実機 grok CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_grok_acp_turn_roundtrip() {
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut host = AcpAgentHost::spawn(AcpHostConfig {
            engine: AcpEngine::Grok,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: "vptest-acp".into(),
            lane: "root".into(),
            lane_label: "root".into(),
            session_key: 1,
            session_id: None,
            config_intent: Vec::new(),
        })
        .expect("spawn grok agent stdio");
        let mut rx = host.subscribe();
        host.submit("Reply with exactly: pong-acp. Nothing else.")
            .await
            .expect("submit");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut session_id = String::new();
        let mut text = String::new();
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("120s 以内に完了する")
                .expect("recv");
            match ev {
                ConversationEvent::SessionInit {
                    session_id: sid, ..
                } => session_id = sid,
                ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                ConversationEvent::TurnCompleted { .. } => break,
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
                _ => {}
            }
        }
        assert!(!session_id.is_empty(), "SessionInit（ACP sessionId）を観測");
        assert!(text.contains("pong-acp"), "answer: {text}");
        let reg = crate::lane::session_registry::load("vptest-acp", "lead", "grok");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some(session_id.as_str()),
            "sessionId が registry に記録される（registry-native）"
        );
        host.stop();
    }

    /// agy の実機 E2E で使う model（`VP_AGY_E2E_MODEL`、未設定 = agy の既定）。
    ///
    /// agy の使用枠は model ごとに別（2026-11-17 実測: 既定の Gemini 4 Argon が枠切れでも
    /// `gemini-3.8-flash` は応答した）。指定すると registry の意図と同じ経路（spawn 後の当て直し
    /// → 終わるまで prompt を待たせる）で切り替わるので、その経路の確認も兼ねる。
    fn agy_e2e_intent() -> Vec<(String, String)> {
        std::env::var("VP_AGY_E2E_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .map(|m| vec![("model".to_string(), m)])
            .unwrap_or_default()
    }

    /// 実機 E2E: 本物の `agy acp` と handshake → session/new → prompt → TurnCompleted
    /// （model / slash_commands / context_tokens / registry 記録まで検証）。
    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_acp_turn_roundtrip() {
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut host = AcpAgentHost::spawn(AcpHostConfig {
            engine: AcpEngine::Agy,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: "vptest-acp-agy".into(),
            lane: "main".into(),
            lane_label: "main".into(),
            session_key: 1,
            session_id: None,
            config_intent: agy_e2e_intent(),
        })
        .expect("spawn agy acp");
        let mut rx = host.subscribe();
        host.submit("Reply with exactly: pong-agy-acp. Nothing else.")
            .await
            .expect("submit");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut session_id = String::new();
        let mut model_seen = None;
        let mut commands_seen = Vec::new();
        let mut text = String::new();
        let context_window_seen;
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("120s 以内に完了する")
                .expect("recv");
            match ev {
                ConversationEvent::SessionInit {
                    session_id: sid,
                    model,
                    slash_commands,
                    ..
                } => {
                    session_id = sid;
                    if model.is_some() {
                        model_seen = model;
                    }
                    if !slash_commands.is_empty() {
                        commands_seen = slash_commands;
                    }
                }
                ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                ConversationEvent::TurnCompleted { context_window, .. } => {
                    context_window_seen = context_window;
                    break;
                }
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
                _ => {}
            }
        }
        assert!(!session_id.is_empty(), "SessionInit（ACP sessionId）を観測");
        assert!(model_seen.is_some(), "実測 model を観測");
        assert!(!commands_seen.is_empty(), "slash_commands を観測");
        assert!(context_window_seen.is_some(), "context_window を観測");
        assert!(text.contains("pong-agy-acp"), "answer: {text}");
        assert!(host.session_init().is_some(), "host が SessionInit を保持");
        let reg = crate::lane::session_registry::load("vptest-acp-agy", "main", "agy");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some(session_id.as_str()),
            "sessionId が registry に記録される（registry-native）"
        );
        host.stop();
    }

    /// 実機 E2E（prompt は送らない = model の使用枠を消費しない）: configOptions の取得 →
    /// user の切替 → 不正値の失敗 → model と thought_level の切替 → 再 spawn（session/load）で
    /// 2 軸とも当て直される（doc 88 §6）。
    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_config_set_and_reapply() {
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let spawn = |session_id: Option<String>, config_intent: Vec<(String, String)>| {
            AcpAgentHost::spawn(AcpHostConfig {
                engine: AcpEngine::Agy,
                cwd: cwd.clone(),
                repo: "vptest-acp-agy-config".into(),
                lane: "main".into(),
                lane_label: "main".into(),
                session_key: 1,
                session_id,
                config_intent,
            })
            .expect("spawn agy acp")
        };
        async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
            while !ok() {
                assert!(tokio::time::Instant::now() < deadline, "60s 以内に {what}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
        let current = |host: &AcpAgentHost, id: &str| -> Option<String> {
            host.config_view()?
                .options
                .into_iter()
                .find(|o| o.id == id)
                .map(|o| o.current_value)
        };

        // 1) 新規 session: configOptions が見える
        let mut host = spawn(None, Vec::new());
        wait_until("configOptions を受け取る", || {
            host.config_view().is_some()
        })
        .await;
        let view = host.config_view().expect("config");
        let level = view
            .options
            .iter()
            .find(|o| o.id == "thought_level")
            .expect("thought_level 軸");
        let alt = level
            .choices
            .iter()
            .find(|c| c.value != level.current_value)
            .expect("別の候補")
            .value
            .clone();

        // 2) user の切替: 応答の候補に現在値が反映される
        let options = host
            .config_handle()
            .set("thought_level", &alt)
            .await
            .expect("set_config_option");
        assert_eq!(
            options
                .iter()
                .find(|o| o.id == "thought_level")
                .map(|o| o.current_value.as_str()),
            Some(alt.as_str())
        );
        assert_eq!(
            current(&host, "thought_level").as_deref(),
            Some(alt.as_str())
        );

        // 3) 不正値: engine のエラー文が返り、roster の error にも残る
        let err = host
            .config_handle()
            .set("thought_level", "no-such-level")
            .await
            .expect_err("不正値は弾かれる");
        assert!(err.contains("no-such-level"), "engine のエラー文: {err}");
        assert!(host.config_view().and_then(|v| v.error).is_some());

        // 3b) 複数の軸: 既定と違う model に替え、その model で既定（medium）以外の level を選ぶ
        let model_choices: Vec<String> = host
            .config_view()
            .expect("config")
            .options
            .into_iter()
            .find(|o| o.id == "model")
            .expect("model 軸")
            .choices
            .into_iter()
            .map(|c| c.value)
            .collect();
        let default_model = current(&host, "model").expect("model");
        let mut picked = None;
        for m in model_choices.iter().filter(|m| **m != default_model) {
            let options = host
                .config_handle()
                .set("model", m)
                .await
                .expect("model 切替");
            // model によっては thought_level の軸が無い（実測: orchestrator / gpt-oss）
            if let Some(level) = options.iter().find(|o| o.id == "thought_level")
                && let Some(l) = level.choices.iter().find(|c| c.value != "medium")
            {
                picked = Some((m.clone(), l.value.clone()));
                break;
            }
        }
        let (model, level) = picked.expect("thought_level を持つ別 model");
        host.config_handle()
            .set("thought_level", &level)
            .await
            .expect("level 切替");

        let reg = crate::lane::session_registry::load("vptest-acp-agy-config", "main", "agy");
        let sid = reg.sessions[0]
            .conversation
            .clone()
            .expect("sessionId 記録");
        host.stop();

        // 4) 再 spawn（load）: agy は既定に戻すが、意図を渡せば host が 1 軸ずつ当て直す
        let mut host2 = spawn(
            Some(sid),
            vec![
                ("model".to_string(), model.clone()),
                ("thought_level".to_string(), level.clone()),
            ],
        );
        wait_until(
            "load 後に model と thought_level が当て直される",
            || {
                current(&host2, "model").as_deref() == Some(model.as_str())
                    && current(&host2, "thought_level").as_deref() == Some(level.as_str())
            },
        )
        .await;
        assert!(
            host2.config_view().and_then(|v| v.error).is_none(),
            "当て直しは成功（model={model}, thought_level={level}）"
        );
        host2.stop();
    }

    /// 実機 E2E: 画像付き prompt（8x8 の赤 PNG）が ACP の image block で届き、色を答える
    /// （doc 88 §6。Claude の実測 2026-08-30 と同じ手順）。prompt を 1 回送る = 使用枠を少し使う。
    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_image_prompt() {
        const RED_8X8_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEklEQVR4nGP4z8CAFWEXHbQSACj/P8Fu7N9hAAAAAElFTkSuQmCC";
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut host = AcpAgentHost::spawn(AcpHostConfig {
            engine: AcpEngine::Agy,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: "vptest-acp-agy-image".into(),
            lane: "main".into(),
            lane_label: "main".into(),
            session_key: 1,
            session_id: None,
            config_intent: agy_e2e_intent(),
        })
        .expect("spawn agy acp");
        let mut rx = host.subscribe();
        host.submit_with_images(
            "What color is this image? Answer with one English word in lowercase.",
            &[ImageInput {
                media_type: "image/png".into(),
                data_base64: RED_8X8_PNG.into(),
            }],
        )
        .await
        .expect("submit");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut text = String::new();
        let mut seen = Vec::new();
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("120s 以内に完了する")
                .expect("recv");
            seen.push(format!("{ev:?}"));
            match ev {
                ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                ConversationEvent::TurnCompleted { .. } => break,
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
                _ => {}
            }
        }
        eprintln!("agy の答え: {text:?}");
        // 単語として一致を見る（枠切れの文などに部分一致で通らないように）
        assert!(
            text.to_lowercase()
                .split(|c: char| !c.is_alphabetic())
                .any(|w| w == "red"),
            "answer: {text:?} / events: {seen:#?}"
        );
        host.stop();
    }
}
