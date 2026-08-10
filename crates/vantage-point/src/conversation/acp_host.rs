//! AcpAgentHost — ACP engine（grok / opencode）を lane 単位で**常駐**駆動する host（gui、doc 42・43）
//!
//! `grok agent stdio`（doc 42）と `opencode acp`（doc 43）はどちらも **ACP（Agent Client
//! Protocol、protocolVersion 1）**を実装しており（各 doc §1 実測）、wire が同一なので本 host が
//! そのまま 2 engine を駆動する。engine 別に変わるのは spawn command と名乗り（registry / ログ /
//! エラー文言）だけで、[`AcpEngine`] に集約する。host のロジックは engine 非依存で、
//! [`super::codex_host::CodexAgentHost`]（RpcHost）と同型の per-session 常駐:
//!
//! - JSONL だが **`jsonrpc: "2.0"` field を含む**標準 JSON-RPC（codex は省略形）
//! - 会話確立 = `session/new` | `session/load`（load error → new へ self-heal）
//! - **turn 終了 = `session/prompt` request の response**（`{stopReason}`。codex の
//!   turn/completed notification と違い response 駆動）
//! - 中断 = `session/cancel` **notification**（応答なし — prompt response が cancelled で畳む）
//! - **`session/request_permission`（server→client request）を host が自動 allow**
//!   （claude bypassPermissions / codex approvalPolicy=never と同じ tui/gui parity。grok は
//!   spawn flag `--always-approve` と二段防御、opencode は自動 allow の一段のみ — doc 43 §6）
//! - `session/load` の replay 列（過去会話の session/update）は**翻訳しない** — VP 自前の
//!   replay_log 経由 replay と二重になるため、load 完了まで translator を繋がない（doc 42 §3）
//!
//! 会話 id（ACP sessionId）は registry 直結（doc 40 §4）— grok / opencode に旧 store は存在しない
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

use super::acp_translate::AcpTranslator;
use super::event::ConversationEvent;
use super::host::InFlight;

/// AcpAgentHost が駆動する ACP engine の種別（spawn command + 名乗りの差分だけを持つ）。
///
/// host のロジック（session/new|load self-heal / prompt-response turn / session/cancel /
/// request_permission 自動 allow / dead flag 自己修復 / stderr 診断 / load replay 列破棄）は
/// engine 非依存。engine 別に変わるのは「どの CLI をどう起動するか」と「registry / ログ /
/// エラー文言の名乗り」だけなので、その差分をここに集約する（doc 43 §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpEngine {
    /// `grok agent stdio --always-approve`（xAI Grok CLI、doc 42）。CLI パス = PATH → `~/.grok/bin/grok`。
    Grok,
    /// `opencode acp`（opencode、doc 43）。CLI パス = PATH → `/opt/homebrew/bin/opencode`（brew）。
    /// approve 相当 flag は無い（request_permission 自動 allow の一段のみ）。model / provider は
    /// opencode config が SSOT で、VP は spawn に env も flag も注入しない（doc 43 §3）。
    OpenCode,
}

impl AcpEngine {
    /// registry / ログ / エラー文言の名乗り（[`super::EngineKind::agent_name`] と一致）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
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
}

/// 送信済み request の種別（response 到着時の分岐）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReqKind {
    Initialize,
    SessionLoad,
    SessionNew,
    /// session/prompt — response 到着 = turn 終了（doc 42 §2 の response 駆動）。
    Prompt,
}

/// reader task / host メソッドが共有する可変状態（std Mutex — await を跨がずに触る）。
struct AcpState {
    /// 確定した ACP sessionId（handshake 完了まで None）。
    session_id: Option<String>,
    /// turn 実行中か（true の submit は queue へ）。
    turn_active: bool,
    /// `session/load` の replay 列を捨てている最中か（load response で解除 — doc 42 §3）。
    loading: bool,
    /// session 未確定 or turn 実行中に来た submit の待ち行列。
    queue: VecDeque<String>,
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
    fn alloc(&mut self, kind: ReqKind) -> i64 {
        self.next_id += 1;
        self.pending.insert(self.next_id, kind);
        self.next_id
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

    /// sessionId 確定（new/load の response）: registry 書き込み + SessionInit + queue 排出。
    async fn adopt_session(&self, session_id: &str) {
        {
            let mut st = self.state.lock().expect("acp state lock");
            st.session_id = Some(session_id.to_string());
            st.loading = false;
        }
        // doc 40 §4: registry 直結（grok / opencode は registry-native — 旧 store が最初から無い）。
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
        self.emit(ConversationEvent::SessionInit {
            session_id: session_id.to_string(),
            model: None,
            permission_mode: None,
            cwd: Some(self.cwd.clone()),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            slash_commands: Vec::new(),
            command_docs: Default::default(),
        });
        self.drain_queue().await;
    }

    /// queue の先頭を session/prompt として送る（session 確定済み && idle の時だけ）。
    async fn drain_queue(&self) {
        let line = {
            let mut st = self.state.lock().expect("acp state lock");
            if st.turn_active || st.session_id.is_none() || st.queue.is_empty() {
                None
            } else {
                let prompt = st.queue.pop_front().expect("non-empty queue");
                let session_id = st.session_id.clone().expect("session id");
                st.turn_active = true;
                let id = st.alloc(ReqKind::Prompt);
                Some(build_prompt(id, &session_id, &prompt))
            }
        };
        if let Some(line) = line {
            self.write_line_logged(&line).await;
        }
    }
}

/// lane 単位の常駐 ACP host（gui、doc 42・43）。grok / opencode の 2 engine を
/// [`AcpEngine`] パラメータ（cli_path / spawn_args / name）で共有する — host ロジックは
/// engine 非依存。
pub struct AcpAgentHost {
    inner: Arc<AcpInner>,
    reader: Option<JoinHandle<()>>,
}

impl AcpAgentHost {
    /// engine の ACP プロセス（grok = `grok agent --always-approve stdio` / opencode =
    /// `opencode acp`）を起動し、handshake（initialize → session/load|new）を開始する。
    /// 完了前の submit は queue に積まれ自動送出される。
    pub fn spawn(config: AcpHostConfig) -> anyhow::Result<Self> {
        let engine = config.engine;
        let mut cmd = tokio::process::Command::new(engine.cli_path());
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
                session_id: None,
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
        let line = {
            let mut st = self.inner.state.lock().expect("acp state lock");
            if st.dead {
                anyhow::bail!(
                    "{} が終了しています（engine 再起動で復旧）",
                    self.inner.engine.name()
                );
            }
            if st.turn_active || st.session_id.is_none() {
                st.queue.push_back(prompt.to_string());
                None
            } else {
                let session_id = st.session_id.clone().expect("session id");
                st.turn_active = true;
                let id = st.alloc(ReqKind::Prompt);
                Some(build_prompt(id, &session_id, prompt))
            }
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
            st.queue.clear();
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

fn build_prompt(id: i64, session_id: &str, prompt: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/prompt",
        "params": { "sessionId": session_id, "prompt": [{ "type": "text", "text": prompt }] }
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
                // 自動 allow（bypass parity）。grok は `--always-approve` と二段防御、opencode は
                // この一段のみ（flag で来なくなるかは未実測 — doc 42 §6 / doc 43 §6）。
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
                inner.adopt_session(sid).await;
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
                inner.adopt_session(sid).await;
            }
        }
        ReqKind::Prompt => {
            // response 到着 = turn 終了（stopReason 問わず — cancelled も完了、doc 42 §2）。
            let session_id = {
                let mut st = inner.state.lock().expect("acp state lock");
                st.turn_active = false;
                st.session_id.clone().unwrap_or_default()
            };
            if let Some(err) = error {
                inner.emit(ConversationEvent::Error {
                    message: format!("{} turn 失敗: {}", inner.engine.name(), error_message(err)),
                });
            }
            inner.emit(ConversationEvent::TurnCompleted {
                session_id,
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            });
            inner.drain_queue().await;
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
            serde_json::from_str(&build_prompt(4, "019f-abc", "hi")).unwrap();
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
            build_prompt(4, "s", "複数\n行"),
            build_cancel("s"),
        ] {
            assert!(!line.contains('\n'), "JSONL 1 行: {line}");
        }
    }

    /// engine パラメタ化（doc 43 §2）: spawn 引数と名乗りが engine 別に切り替わる。
    /// grok = `agent --always-approve stdio`（permission 二段防御）、opencode = `acp` のみ
    /// （flag 注入なし — model は opencode config が SSOT）。名乗りは registry / ログ / エラー
    /// 文言に載る agent 名（EngineKind::agent_name と一致）。
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
        assert_eq!(AcpEngine::Grok.name(), "grok");
        assert_eq!(AcpEngine::OpenCode.name(), "opencode");
        // agent 名（EngineKind）と registry / ログの名乗り（AcpEngine）が一致する不変条件。
        assert_eq!(
            AcpEngine::Grok.name(),
            super::super::EngineKind::Grok.agent_name()
        );
        assert_eq!(
            AcpEngine::OpenCode.name(),
            super::super::EngineKind::OpenCode.agent_name()
        );
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
        let reg = crate::lane::session_registry::load("vptest-acp", "root", "grok");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some(session_id.as_str()),
            "sessionId が registry に記録される（registry-native）"
        );
        host.stop();
    }
}
