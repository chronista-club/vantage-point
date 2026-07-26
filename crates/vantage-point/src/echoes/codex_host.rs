//! CodexAgentHost — codex を lane 単位で**常駐**駆動する RpcHost（gui engine host、doc 41）
//!
//! 旧 turn-scoped（`codex exec` を turn ごと spawn する TurnHost）を、`codex app-server`
//! 子プロセス 1 本 + JSONL JSON-RPC の常駐形に置き換えた（doc 39 §7「常駐型のみの一枚岩」の
//! codex 実装、実測 de-risk = doc 41 §1 全 PASS）。プロセスモデルは **1 session = 1 app-server**
//! （[`super::host::EchoesAgentHost`] と同型、doc 41 §2-1）。
//!
//! ## protocol（doc 41 §1、codex-cli 0.144.5 実測）
//!
//! - JSONL over stdio。JSON-RPC 2.0 だが `jsonrpc` field は wire で省略
//! - handshake: `initialize` → response → `initialized` notification → thread 解禁
//! - 会話: `thread/resume`（conversation あり）/ `thread/start` → `turn/start` → notification
//!   stream → `turn/completed`。中断は `turn/interrupt`（**プロセスを殺さない** — TurnHost の
//!   kill との最大の差。turn は status=interrupted で完了する）
//! - approval: `approvalPolicy: "never"` + `sandbox: "danger-full-access"` = 旧
//!   `--dangerously-bypass-approvals-and-sandbox` の等価（tui/gui parity ポリシー維持）
//!
//! ## 会話 id（thread id）は registry 直結（doc 40 §4）
//!
//! thread/start|resume の response で得た thread id は `session_registry::set_conversation` に
//! 書く（旧 `codex_session` store には**書かない** — 新 host は registry 直結の規約）。
//! resume 空振り（thread 消失等の error response）は `thread/start` へ self-heal し、新 thread id
//! が registry を上書きする（TurnHost の「記録破棄 → fresh」と同じ流儀の常駐版）。
//!
//! ## 途絶検知（常駐の規律）
//!
//! stdout close = app-server 途絶。意図的 stop 以外では [`EchoesEvent::Error`] を broadcast する
//! （[`super::host::EchoesAgentHost`] の #692 と同じ — chatview に途絶が見える）。
//!
//! data / calculations / actions:
//! - calculations: request/notification line の組み立て（`build_*`、純関数）+
//!   [`super::codex_rpc_translate::CodexRpcTranslator`]（item 層の純翻訳）
//! - actions: プロセス spawn / stdin 書き込み / reader loop の応答処理 / registry 書き込み / broadcast

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::codex_rpc_translate::CodexRpcTranslator;
use super::event::EchoesEvent;
use super::host::InFlight;

/// CodexAgentHost の起動設定。
#[derive(Debug, Clone)]
pub struct CodexRpcHostConfig {
    /// 会話の作業ディレクトリ（lane の repo dir）。thread の workspace 紐付けに効く。
    pub cwd: String,
    /// registry 書き込みキー（repo 名）。
    pub repo: String,
    /// registry 書き込みキー（session label: `conductor` / `conductor#2` …）。
    /// ⚠️ env の `VP_LANE` には使わない — そちらは [`Self::lane_label`]（素の label）。
    pub lane: String,
    /// identity env（`VP_LANE`）用の素の lane label（doc 51 §1 A3b — tui と同じ契約）。
    pub lane_label: String,
    /// identity env（`VP_SESSION_KEY`）用の session key（doc 40 §4 の hook identity と同じ）。
    pub session_key: crate::lane::session_registry::SessionKey,
    /// 再開する thread id（doc 40 registry の `conversation`）。None = 新規 thread。
    pub thread_id: Option<String>,
}

/// 送信済み request の種別（response 到着時の分岐に使う）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReqKind {
    Initialize,
    ThreadResume,
    ThreadStart,
    TurnStart,
    TurnInterrupt,
}

/// reader task / host メソッドが共有する可変状態（std Mutex — await を跨がずに触る）。
struct RpcState {
    /// 確定した thread id（handshake 完了まで None）。
    thread_id: Option<String>,
    /// 実行中 turn の id（`turn/interrupt` の宛先。idle は None）。
    turn_id: Option<String>,
    /// turn 実行中か（true の submit は queue へ）。
    turn_active: bool,
    /// thread 未確定 or turn 実行中に来た submit の待ち行列。
    queue: VecDeque<String>,
    /// disk にまだ載っていない増分 + commit 世代（[`super::host`] と同契約）。
    in_flight: InFlight,
    /// app-server 子プロセスの pid。
    child_pid: Option<u32>,
    /// JSON-RPC request id の採番（単調増加）。
    next_id: i64,
    /// 送信済み request の台帳（response 到着で回収）。
    pending: HashMap<i64, ReqKind>,
    /// 明示 stop 中か（reader loop 終端の途絶 Error を抑止）。
    stopping: bool,
    /// app-server 途絶（stdout close / stdin 書込失敗）を観測したか。true の submit は
    /// **Err を返す** — `ensure_and_submit_chat` の自己修復（engine drop → 再 ensure →
    /// 同一 message retry）に修復を委ねる（moody 指摘 #1: 常駐化で新たに背負った
    /// プロセス死亡時の責務。旧 TurnHost は turn ごと spawn なのでこの問題自体が無かった）。
    dead: bool,
    /// stderr の末尾数行（途絶 Error の診断材料 — 未ログイン / CLI 不整合の原因を
    /// user に見せる。moody 指摘 #2: 旧 TurnHost の stderr 合成の常駐版）。
    stderr_tail: VecDeque<String>,
}

impl RpcState {
    fn alloc(&mut self, kind: ReqKind) -> i64 {
        self.next_id += 1;
        self.pending.insert(self.next_id, kind);
        self.next_id
    }
}

/// reader task と host が共有する不変部 + 状態。
struct RpcInner {
    event_tx: broadcast::Sender<EchoesEvent>,
    repo: String,
    lane: String,
    cwd: String,
    /// stdin writer（tokio Mutex — submit と reader task の書き込みを直列化）。
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    state: Mutex<RpcState>,
    /// 子プロセス（kill_on_drop。stop で明示 kill、drop でも保険が効く）。
    child: Mutex<Option<Child>>,
}

impl RpcInner {
    /// JSONL 1 行を書く。失敗 = 途絶（Err を返す — submit 経路は自己修復に繋ぐため
    /// 握り潰さない。reader task 内の呼び出しは log のみで握る = stdout close 検知に収束）。
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

    /// reader task 内からの write（失敗は log のみ — 途絶は stdout close 検知に収束させる）。
    async fn write_line_logged(&self, line: &str) {
        if let Err(e) = self.write_line(line).await {
            tracing::warn!("codex app-server stdin write 失敗（途絶疑い）: {e}");
        }
    }

    fn emit(&self, event: EchoesEvent) {
        // in-flight fold（[`super::host::fold_in_flight`] と同規律のローカル版）:
        // 増分だけ tail に積み、会話が確定する event で世代を進めて捨てる。
        {
            let mut st = self.state.lock().expect("rpc state lock");
            match &event {
                EchoesEvent::MessageChunk { .. } | EchoesEvent::ThoughtChunk { .. } => {
                    st.in_flight.tail.push(event.clone());
                }
                EchoesEvent::SessionInit { .. }
                | EchoesEvent::TurnCompleted { .. }
                | EchoesEvent::Error { .. }
                | EchoesEvent::EngineExited { .. } => {
                    st.in_flight.tail.clear();
                    st.in_flight.seq = st.in_flight.seq.wrapping_add(1);
                }
                _ => {}
            }
        }
        let _ = self.event_tx.send(event);
    }

    /// thread id 確定（start/resume の response）: registry 書き込み + SessionInit + queue 排出。
    async fn adopt_thread(&self, thread_id: &str) {
        {
            let mut st = self.state.lock().expect("rpc state lock");
            st.thread_id = Some(thread_id.to_string());
        }
        // doc 40 §4: 新 host は registry 直結（codex_session store には書かない）。
        let (lane_label, key) = crate::lane::session_registry::parse_session_label(&self.lane);
        if let Err(e) = crate::lane::session_registry::set_conversation(
            &self.repo,
            lane_label,
            "codex",
            key,
            Some(thread_id),
        ) {
            tracing::warn!(
                "codex thread id の registry 記録失敗（repo={}, lane={}）: {e}",
                self.repo,
                self.lane
            );
        }
        self.emit(EchoesEvent::SessionInit {
            session_id: thread_id.to_string(),
            model: None,
            permission_mode: None,
            cwd: Some(self.cwd.clone()),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            slash_commands: Vec::new(),
        });
        self.drain_queue().await;
    }

    /// queue の先頭を turn/start として送る（thread 確定済み && idle の時だけ）。
    async fn drain_queue(&self) {
        let line = {
            let mut st = self.state.lock().expect("rpc state lock");
            if st.turn_active || st.thread_id.is_none() || st.queue.is_empty() {
                None
            } else {
                let prompt = st.queue.pop_front().expect("non-empty queue");
                let thread_id = st.thread_id.clone().expect("thread id");
                st.turn_active = true;
                let id = st.alloc(ReqKind::TurnStart);
                Some(build_turn_start(id, &thread_id, &prompt))
            }
        };
        if let Some(line) = line {
            self.write_line_logged(&line).await;
        }
    }
}

/// lane 単位の常駐 codex host（gui、doc 41）。旧 `TurnHost<CodexEngine>` の置換。
pub struct CodexAgentHost {
    inner: Arc<RpcInner>,
    reader: Option<JoinHandle<()>>,
}

impl CodexAgentHost {
    /// app-server 子プロセスを起動し、handshake（initialize → thread start/resume）を開始する。
    ///
    /// [`super::host::EchoesAgentHost`] と同じく spawn は eager（ensure 時にプロセスが立つ）。
    /// handshake は reader task 内で非同期に進み、完了前の submit は queue に積まれて
    /// thread 確定後に自動送出される。
    pub fn spawn(config: CodexRpcHostConfig) -> anyhow::Result<Self> {
        let mut cmd = tokio::process::Command::new(crate::lane::codex_session::codex_cli_path());
        cmd.arg("app-server")
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
            .map_err(|e| anyhow::anyhow!("codex app-server の起動に失敗: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server の stdout が取れません"))?;
        let stderr = child.stderr.take();
        let child_pid = child.id();

        let (event_tx, _rx) = broadcast::channel::<EchoesEvent>(256);
        let inner = Arc::new(RpcInner {
            event_tx,
            repo: config.repo,
            lane: config.lane,
            cwd: config.cwd,
            stdin: tokio::sync::Mutex::new(stdin),
            state: Mutex::new(RpcState {
                thread_id: None,
                turn_id: None,
                turn_active: false,
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
        // stderr drain（moody 指摘 #2）: 未ログイン / CLI 不整合の原因を log + 末尾保持して
        // 途絶 Error の診断材料にする（旧 TurnHost の stderr 合成の常駐版）。
        if let Some(stderr) = stderr {
            let drain = inner.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    tracing::warn!("codex app-server stderr: {line}");
                    let mut st = drain.state.lock().expect("rpc state lock");
                    if st.stderr_tail.len() >= 5 {
                        st.stderr_tail.pop_front();
                    }
                    st.stderr_tail.push_back(line);
                }
            });
        }
        let resume_target = config.thread_id;
        tracing::info!(
            "CodexAgentHost spawn（常駐 app-server、repo={}, lane={}, resume={:?}, pid={:?}）",
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

    pub fn subscribe(&self) -> broadcast::Receiver<EchoesEvent> {
        self.inner.event_tx.subscribe()
    }

    pub fn in_flight(&self) -> InFlight {
        self.inner
            .state
            .lock()
            .expect("rpc state lock")
            .in_flight
            .clone()
    }

    pub fn commit_seq(&self) -> u64 {
        self.inner
            .state
            .lock()
            .expect("rpc state lock")
            .in_flight
            .seq
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.state.lock().expect("rpc state lock").child_pid
    }

    /// ユーザープロンプトを投入する（idle なら即 turn/start、それ以外は queue）。
    ///
    /// **途絶時は Err を返す**（moody 指摘 #1）: `ensure_and_submit_chat` の自己修復
    /// （engine drop → 再 ensure → 同一 message retry）は submit の Err を条件に発火する。
    /// 新 host は registry の conversation から `thread/resume` するため、復旧後も会話文脈は
    /// 継がれる（= 途絶 Error の「次の送信で自動復旧」を実現する配線。旧 TurnHost は
    /// turn ごと spawn でこの責務自体が無かった — 常駐化で新たに背負った責務）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        let line = {
            let mut st = self.inner.state.lock().expect("rpc state lock");
            if st.dead {
                anyhow::bail!("codex app-server が終了しています（engine 再起動で復旧）");
            }
            if st.turn_active || st.thread_id.is_none() {
                st.queue.push_back(prompt.to_string());
                tracing::debug!(
                    "codex submit: {} → queue（depth={}）",
                    if st.turn_active {
                        "turn 実行中"
                    } else {
                        "thread 未確定"
                    },
                    st.queue.len()
                );
                None
            } else {
                let thread_id = st.thread_id.clone().expect("thread id");
                st.turn_active = true;
                let id = st.alloc(ReqKind::TurnStart);
                Some(build_turn_start(id, &thread_id, prompt))
            }
        };
        if let Some(line) = line
            && let Err(e) = self.inner.write_line(&line).await
        {
            let mut st = self.inner.state.lock().expect("rpc state lock");
            st.dead = true;
            st.turn_active = false;
            st.turn_id = None;
            anyhow::bail!("codex app-server への送信に失敗（途絶）: {e}");
        }
        Ok(())
    }

    /// 実行中 turn を中断する（`turn/interrupt` — プロセスは殺さない、doc 41 §2-2）。
    ///
    /// server が turn を status=interrupted で完了させ `turn/completed` を流す → 通常の完了
    /// 経路で streaming が畳まれる（TurnHost のような自前 TurnCompleted 偽装は不要）。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        let line = {
            let mut st = self.inner.state.lock().expect("rpc state lock");
            st.queue.clear();
            match (st.thread_id.clone(), st.turn_id.clone(), st.turn_active) {
                (Some(thread_id), Some(turn_id), true) => {
                    let id = st.alloc(ReqKind::TurnInterrupt);
                    Some(build_turn_interrupt(id, &thread_id, &turn_id))
                }
                // turn_active && turn_id 未着（turn/start 直後の短い窓）は queue clear のみ —
                // turn/started 到着後の再操作で止められる（moody #3 の窓、実害は小）。
                _ => None,
            }
        };
        if let Some(line) = line {
            // interrupt の書込失敗は lenient（途絶なら turn はもう走っていない — 途絶検知と
            // submit 側 Err が復旧を担う）。
            self.inner.write_line_logged(&line).await;
        }
        Ok(())
    }

    /// 明示 teardown（[`super::engine::ChatEngineSlot`] Drop から呼ぶ）。
    pub fn stop(&mut self) {
        {
            let mut st = self.inner.state.lock().expect("rpc state lock");
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
            "CodexAgentHost stop（repo={}, lane={}）",
            self.inner.repo,
            self.inner.lane
        );
    }
}

// =============================================================================
// calculations — JSON-RPC line 組み立て（純関数、単体テスト対象）
// =============================================================================

/// approval/sandbox の共通指定（doc 41 §1: 旧 `--dangerously-bypass` の等価。
/// claude bypassPermissions と同じ tui/gui parity ポリシー）。
fn thread_permission_fields() -> serde_json::Value {
    serde_json::json!({
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
    })
}

fn build_initialize(id: i64) -> String {
    serde_json::json!({
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "vantage_point",
                "title": "Vantage Point",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }
    })
    .to_string()
}

fn build_initialized() -> String {
    serde_json::json!({ "method": "initialized", "params": {} }).to_string()
}

fn build_thread_start(id: i64, cwd: &str) -> String {
    let mut params = thread_permission_fields();
    params["cwd"] = serde_json::json!(cwd);
    serde_json::json!({ "id": id, "method": "thread/start", "params": params }).to_string()
}

fn build_thread_resume(id: i64, thread_id: &str, cwd: &str) -> String {
    let mut params = thread_permission_fields();
    params["threadId"] = serde_json::json!(thread_id);
    params["cwd"] = serde_json::json!(cwd);
    serde_json::json!({ "id": id, "method": "thread/resume", "params": params }).to_string()
}

fn build_turn_start(id: i64, thread_id: &str, prompt: &str) -> String {
    serde_json::json!({
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "input": [{ "type": "text", "text": prompt }],
        }
    })
    .to_string()
}

fn build_turn_interrupt(id: i64, thread_id: &str, turn_id: &str) -> String {
    serde_json::json!({
        "id": id,
        "method": "turn/interrupt",
        "params": { "threadId": thread_id, "turnId": turn_id }
    })
    .to_string()
}

/// JSON-RPC error object から人間向け message を取り出す（形が崩れていても何か返す）。
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
    inner: Arc<RpcInner>,
    stdout: tokio::process::ChildStdout,
    resume_target: Option<String>,
) {
    let mut translator = CodexRpcTranslator::new();
    let mut lines = BufReader::new(stdout).lines();

    // 初手: initialize（response 到着で handshake が進む）。
    let init_line = {
        let mut st = inner.state.lock().expect("rpc state lock");
        let id = st.alloc(ReqKind::Initialize);
        build_initialize(id)
    };
    inner.write_line_logged(&init_line).await;

    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue; // 非 JSON 行は無視（JSONL 前提、doc 41 §1）
        };
        // server → client request（id + method）: approvalPolicy=never では飛んでこない想定だが、
        // 来たら未対応 error を返して turn を塞がない（fail-fast で server 側に判断を戻す）。
        if let (Some(id), Some(method)) =
            (msg.get("id"), msg.get("method").and_then(|m| m.as_str()))
        {
            tracing::warn!("codex app-server からの想定外 request: {method}");
            let resp = serde_json::json!({
                "id": id,
                "error": { "code": -32601, "message": "not supported by VP CodexRpcHost" }
            });
            inner.write_line_logged(&resp.to_string()).await;
            continue;
        }
        // response（id のみ）
        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            handle_response(&inner, id, &msg, &resume_target).await;
            continue;
        }
        // notification
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or_default();
        match method {
            "turn/started" => {
                let mut st = inner.state.lock().expect("rpc state lock");
                st.turn_active = true;
                st.turn_id = params
                    .pointer("/turn/id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            "turn/completed" => {
                let session_id = {
                    let mut st = inner.state.lock().expect("rpc state lock");
                    st.turn_active = false;
                    st.turn_id = None;
                    st.thread_id.clone().unwrap_or_default()
                };
                // status=interrupted も「turn の完了」（意図的中断はエラーではない、doc 41 §2-3）。
                // error field が載っていれば Error を併発して chatview に見せる。
                if let Some(err) = params.pointer("/turn/error").filter(|v| !v.is_null()) {
                    inner.emit(EchoesEvent::Error {
                        message: format!("codex turn error: {}", error_message(err)),
                    });
                }
                inner.emit(EchoesEvent::TurnCompleted {
                    session_id,
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                });
                inner.drain_queue().await;
            }
            "error" => {
                inner.emit(EchoesEvent::Error {
                    message: format!("codex: {}", error_message(&params)),
                });
            }
            _ => {
                for event in translator.ingest(method, &params) {
                    inner.emit(event);
                }
            }
        }
    }

    // stdout close = 途絶（常駐の規律、#692 と同じ）。dead を立てて以後の submit を Err に
    // 倒す = `ensure_and_submit_chat` の自己修復（drop → 再 ensure → retry）が発火する
    // （moody #1 の配線）。明示 stop では Error を出さない。
    let (stopping, stderr_tail) = {
        let mut st = inner.state.lock().expect("rpc state lock");
        st.dead = true;
        st.turn_active = false;
        st.turn_id = None;
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
            "codex app-server 途絶（repo={}, lane={}）",
            inner.repo,
            inner.lane
        );
        let detail = if stderr_tail.is_empty() {
            String::new()
        } else {
            format!("\n{stderr_tail}")
        };
        inner.emit(EchoesEvent::EngineExited {
            message: format!("codex app-server が休眠しました。次の送信で再開します。{detail}"),
        });
    }
}

async fn handle_response(
    inner: &Arc<RpcInner>,
    id: i64,
    msg: &serde_json::Value,
    resume_target: &Option<String>,
) {
    let kind = {
        let mut st = inner.state.lock().expect("rpc state lock");
        st.pending.remove(&id)
    };
    let Some(kind) = kind else {
        return; // 台帳に無い response（多重 or 未知）は無視
    };
    let error = msg.get("error").filter(|v| !v.is_null());
    match kind {
        ReqKind::Initialize => {
            if let Some(err) = error {
                inner.emit(EchoesEvent::Error {
                    message: format!("codex app-server initialize 失敗: {}", error_message(err)),
                });
                return;
            }
            inner.write_line_logged(&build_initialized()).await;
            let line = {
                let mut st = inner.state.lock().expect("rpc state lock");
                match resume_target {
                    Some(tid) => {
                        let id = st.alloc(ReqKind::ThreadResume);
                        build_thread_resume(id, tid, &inner.cwd)
                    }
                    None => {
                        let id = st.alloc(ReqKind::ThreadStart);
                        build_thread_start(id, &inner.cwd)
                    }
                }
            };
            inner.write_line_logged(&line).await;
        }
        ReqKind::ThreadResume => {
            if let Some(err) = error {
                // resume 空振り → fresh へ self-heal（doc 41 §2-2。新 thread id が registry を
                // 上書きするので記録破棄の別手順は不要）。
                tracing::warn!(
                    "codex thread/resume 失敗 → thread/start へ self-heal（repo={}, lane={}）: {}",
                    inner.repo,
                    inner.lane,
                    error_message(err)
                );
                let line = {
                    let mut st = inner.state.lock().expect("rpc state lock");
                    let id = st.alloc(ReqKind::ThreadStart);
                    build_thread_start(id, &inner.cwd)
                };
                inner.write_line_logged(&line).await;
                return;
            }
            if let Some(tid) = msg.pointer("/result/thread/id").and_then(|v| v.as_str()) {
                inner.adopt_thread(tid).await;
            }
        }
        ReqKind::ThreadStart => {
            if let Some(err) = error {
                inner.emit(EchoesEvent::Error {
                    message: format!("codex thread/start 失敗: {}", error_message(err)),
                });
                return;
            }
            if let Some(tid) = msg.pointer("/result/thread/id").and_then(|v| v.as_str()) {
                inner.adopt_thread(tid).await;
            }
        }
        ReqKind::TurnStart => {
            if let Some(err) = error {
                {
                    let mut st = inner.state.lock().expect("rpc state lock");
                    st.turn_active = false;
                    st.turn_id = None;
                }
                inner.emit(EchoesEvent::Error {
                    message: format!("codex turn/start 失敗: {}", error_message(err)),
                });
                inner.drain_queue().await;
                return;
            }
            // turn id は response（result.turn.id）でも `turn/started` でも入る（冪等）。
            if let Some(turn_id) = msg.pointer("/result/turn/id").and_then(|v| v.as_str()) {
                let mut st = inner.state.lock().expect("rpc state lock");
                st.turn_id = Some(turn_id.to_string());
            }
        }
        ReqKind::TurnInterrupt => {
            if let Some(err) = error {
                tracing::warn!("codex turn/interrupt 失敗: {}", error_message(err));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// request line の形を doc 41 §1 の実測 wire に固定する（protocol drift 検知）。
    #[test]
    fn request_lines_match_observed_wire_shapes() {
        let init: serde_json::Value = serde_json::from_str(&build_initialize(1)).unwrap();
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["clientInfo"]["name"], "vantage_point");
        assert!(
            init.get("jsonrpc").is_none(),
            "jsonrpc field は wire で省略（README 準拠）"
        );

        let start: serde_json::Value = serde_json::from_str(&build_thread_start(2, "/w")).unwrap();
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"]["cwd"], "/w");
        assert_eq!(start["params"]["approvalPolicy"], "never");
        assert_eq!(start["params"]["sandbox"], "danger-full-access");

        let resume: serde_json::Value =
            serde_json::from_str(&build_thread_resume(3, "019f-abc", "/w")).unwrap();
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "019f-abc");
        assert_eq!(resume["params"]["approvalPolicy"], "never");

        let turn: serde_json::Value =
            serde_json::from_str(&build_turn_start(4, "019f-abc", "hi -x")).unwrap();
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "019f-abc");
        assert_eq!(turn["params"]["input"][0]["type"], "text");
        assert_eq!(turn["params"]["input"][0]["text"], "hi -x");

        let intr: serde_json::Value =
            serde_json::from_str(&build_turn_interrupt(5, "019f-abc", "turn-1")).unwrap();
        assert_eq!(intr["method"], "turn/interrupt");
        assert_eq!(intr["params"]["turnId"], "turn-1");
    }

    /// JSONL は 1 行 = 1 message（改行を含まない）— stdin 書き込みの前提条件。
    #[test]
    fn built_lines_are_single_line() {
        for line in [
            build_initialize(1),
            build_initialized(),
            build_thread_start(2, "/w"),
            build_thread_resume(3, "t", "/w"),
            build_turn_start(4, "t", "複数\n行の\nprompt"),
            build_turn_interrupt(5, "t", "u"),
        ] {
            assert!(
                !line.contains('\n'),
                "JSONL 1 行に収まる（serde_json が \\n を escape）: {line}"
            );
        }
    }

    /// error object の message 抽出（形が崩れても何か返す）。
    #[test]
    fn error_message_extraction_is_lenient() {
        assert_eq!(
            error_message(&serde_json::json!({"code": -32001, "message": "overloaded"})),
            "overloaded"
        );
        assert_eq!(
            error_message(&serde_json::json!({"code": 1})),
            r#"{"code":1}"#
        );
    }

    /// 実機 E2E: 本物の `codex app-server` と handshake → turn → TurnCompleted まで。
    /// doc 41 §1 の de-risk（bun script）の Rust host 版。CI では走らない（codex CLI +
    /// ChatGPT auth + トークン消費があるため）— ローカルで
    /// `cargo test -p vantage-point --lib e2e_real_app_server -- --ignored --nocapture` で実行。
    #[tokio::test]
    #[ignore = "実機 codex CLI + ChatGPT auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_app_server_turn_roundtrip() {
        let _state = crate::test_env::state_dir_async().await; // registry 書き込みを tempdir に隔離
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut host = CodexAgentHost::spawn(CodexRpcHostConfig {
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: "vptest-rpc".into(),
            lane: "root".into(),
            lane_label: "root".into(),
            session_key: 1,
            thread_id: None,
        })
        .expect("spawn codex app-server");
        let mut rx = host.subscribe();
        host.submit("Reply with exactly: pong-rpc. Nothing else.")
            .await
            .expect("submit");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut session_id = String::new();
        let mut text = String::new();
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("90s 以内に完了する")
                .expect("recv");
            match ev {
                EchoesEvent::SessionInit {
                    session_id: sid, ..
                } => session_id = sid,
                EchoesEvent::MessageChunk { text: t } => text.push_str(&t),
                EchoesEvent::TurnCompleted { .. } => break,
                EchoesEvent::Error { message } => panic!("engine error: {message}"),
                _ => {}
            }
        }
        assert!(!session_id.is_empty(), "SessionInit（thread id）を観測");
        assert!(text.contains("pong-rpc"), "answer: {text}");
        // registry 直結の書き戻し（doc 40 §4）も実機で確認
        let reg = crate::lane::session_registry::load("vptest-rpc", "root", "codex");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some(session_id.as_str()),
            "thread id が registry に記録される"
        );

        // phase 2（moody #3 → 実測に昇格）: turn/interrupt の実機行使。
        // 長い turn を開始 → 最初の増分を観測してから interrupt → turn/completed で
        // streaming が畳まれる（turn_active が下りて次の turn が受け付けられる）ことを確認。
        host.submit("Count from 1 to 500 slowly, one number per line. Do not stop early.")
            .await
            .expect("submit long turn");
        let deadline2 = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let ev = tokio::time::timeout_at(deadline2, rx.recv())
                .await
                .expect("60s 以内に turn が走り出す")
                .expect("recv");
            match ev {
                EchoesEvent::MessageChunk { .. } | EchoesEvent::ThoughtChunk { .. } => break,
                EchoesEvent::Error { message } => panic!("engine error: {message}"),
                _ => {}
            }
        }
        host.interrupt().await.expect("interrupt");
        let deadline3 = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let ev = tokio::time::timeout_at(deadline3, rx.recv())
                .await
                .expect("interrupt 後 30s 以内に turn/completed が来る（doc 41 §2-2）")
                .expect("recv");
            if matches!(ev, EchoesEvent::TurnCompleted { .. }) {
                break;
            }
        }
        host.stop();
    }
}
