//! CodexAgentHost — codex を lane 単位で**常駐**駆動する RpcHost（gui engine host、doc 41）
//!
//! 旧 turn-scoped（`codex exec` を turn ごと spawn する TurnHost）を、`codex app-server`
//! 子プロセス 1 本 + JSONL JSON-RPC の常駐形に置き換えた（doc 39 §7「常駐型のみの一枚岩」の
//! codex 実装、実測 de-risk = doc 41 §1 全 PASS）。プロセスモデルは **1 session = 1 app-server**
//! （[`super::host::ClaudeHost`] と同型、doc 41 §2-1）。
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
//! resume の error response は元 ID を保持して利用者へ通知する。次の送信で host を
//! 作り直し、同じ ID に再試行する。新規会話への自動 fallback は行わない（doc 41 §2-5）。
//!
//! ## 途絶検知（常駐の規律）
//!
//! stdout close = app-server 途絶。意図的 stop 以外では [`ConversationEvent::Error`] を broadcast する
//! （[`super::host::ClaudeHost`] の #692 と同じ — chatview に途絶が見える）。
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

use super::codex_history::CodexHistory;
use super::codex_rpc_translate::CodexRpcTranslator;
use super::event::ConversationEvent;
use super::host::InFlight;

/// CodexAgentHost の起動設定。
#[derive(Debug, Clone)]
pub struct CodexRpcHostConfig {
    /// 会話の作業ディレクトリ（lane の repo dir）。thread の workspace 紐付けに効く。
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
    /// 再開する thread id（doc 40 registry の `conversation`）。None = 新規 thread。
    pub thread_id: Option<String>,
}

/// 送信済み request の種別（response 到着時の分岐に使う）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReqKind {
    Initialize,
    ThreadResume,
    ThreadRead,
    ThreadStart,
    TurnStart,
    TurnInterrupt,
}

/// reader task / host メソッドが共有する可変状態（std Mutex — await を跨がずに触る）。
struct RpcState {
    history: Option<CodexHistory>,
    startup_error: Option<String>,
    /// 初回 thread/read 待機中の対象。本文更新が競合した場合は復元を中断する。
    hydration_target: Option<String>,
    /// 確定した thread id（handshake 完了まで None）。
    thread_id: Option<String>,
    /// 実行中 turn の id（`turn/interrupt` の宛先。idle は None）。
    turn_id: Option<String>,
    /// turn 実行中か（true の submit は queue へ）。
    turn_active: bool,
    /// thread 未確定 or turn 実行中に来た submit の待ち行列。
    queue: VecDeque<(String, Option<String>)>,
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
    /// app-server 途絶（stdout close / stdin 書込失敗）または起動 error を観測したか。true の submit は
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
    event_tx: broadcast::Sender<ConversationEvent>,
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
    /// 起動を待つ prompt を失敗として畳み、次の submit を host 再生成へ繋ぐ。
    /// registry は触らず、再生成時も元の新規 / 再開の選択を保つ。
    fn fail_startup(&self, message: String) {
        let mut st = self.state.lock().expect("rpc state lock");
        self.fail_startup_locked(&mut st, message);
    }

    fn fail_startup_locked(&self, st: &mut RpcState, message: String) {
        let message = format!("{message}\n送信内容は実行されていません。再送すると再試行します。");
        st.dead = true;
        st.startup_error = Some(message.clone());
        st.turn_active = false;
        st.turn_id = None;
        st.queue.clear();
        st.pending.clear();
        st.hydration_target = None;
        self.emit_locked(st, ConversationEvent::Error { message });
    }

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

    fn emit(&self, event: ConversationEvent) {
        let mut st = self.state.lock().expect("rpc state lock");
        self.emit_locked(&mut st, event);
    }

    fn emit_locked(&self, st: &mut RpcState, event: ConversationEvent) {
        // in-flight fold（[`super::host::fold_in_flight`] と同規律のローカル版）:
        // 増分だけ tail に積み、会話が確定する event で世代を進めて捨てる。
        {
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

    /// thread id 確定（start/resume の response）: registry 書き込み + SessionInit + queue 排出。
    async fn adopt_thread(&self, thread: &serde_json::Value, thread_id: &str) {
        let history = match CodexHistory::from_thread(thread, thread_id) {
            Ok(history) => history,
            Err(reason) => {
                self.fail_startup(format!(
                    "Codex の履歴を復元できませんでした。元の会話 ID は保持しています: {reason}"
                ));
                return;
            }
        };
        {
            let mut st = self.state.lock().expect("rpc state lock");
            if st.dead {
                return;
            }
            st.hydration_target = None;
            st.thread_id = Some(thread_id.to_string());
            st.turn_id = thread["turns"]
                .as_array()
                .and_then(|turns| turns.iter().rev().find(|t| t["status"] == "inProgress"))
                .and_then(|t| t["id"].as_str())
                .map(str::to_owned);
            st.turn_active = st.turn_id.is_some();
            st.history = Some(history);
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
        self.emit(ConversationEvent::SessionInit {
            session_id: thread_id.to_string(),
            model: None,
            permission_mode: None,
            cwd: Some(self.cwd.clone()),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            slash_commands: Vec::new(),
            command_docs: Default::default(),
        });
        self.request_history();
        self.drain_queue().await;
    }

    /// ready 前の demand は adopt_thread の snapshot に合流する。失敗時は表示を消さない。
    fn request_history(&self) {
        let mut st = self.state.lock().expect("rpc state lock");
        if let Some(history) = &st.history {
            let (events, user_message_ids, truncated) = history.snapshot();
            let event = ConversationEvent::CodexHistory {
                thread_id: st.thread_id.clone().unwrap_or_default(),
                events,
                user_message_ids,
                in_flight: st.turn_active,
                truncated,
            };
            self.emit_locked(&mut st, event);
        } else if st.dead {
            let message = st
                .startup_error
                .clone()
                .unwrap_or_else(|| "Codex が休眠しています。再送すると再試行します。".into());
            self.emit_locked(&mut st, ConversationEvent::Error { message });
        }
    }

    /// queue の先頭を turn/start として送る（thread 確定済み && idle の時だけ）。
    async fn drain_queue(&self) {
        let line = {
            let mut st = self.state.lock().expect("rpc state lock");
            if st.turn_active || st.thread_id.is_none() || st.queue.is_empty() {
                None
            } else {
                let (prompt, client_id) = st.queue.pop_front().expect("non-empty queue");
                let thread_id = st.thread_id.clone().expect("thread id");
                st.turn_active = true;
                let id = st.alloc(ReqKind::TurnStart);
                Some(build_identified_turn_start(
                    id,
                    &thread_id,
                    &prompt,
                    client_id.as_deref(),
                ))
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
    /// [`super::host::ClaudeHost`] と同じく spawn は eager（ensure 時にプロセスが立つ）。
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

        let (event_tx, _rx) = broadcast::channel::<ConversationEvent>(256);
        let inner = Arc::new(RpcInner {
            event_tx,
            repo: config.repo,
            lane: config.lane,
            cwd: config.cwd,
            stdin: tokio::sync::Mutex::new(stdin),
            state: Mutex::new(RpcState {
                history: None,
                startup_error: None,
                hydration_target: None,
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

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.inner.event_tx.subscribe()
    }

    pub fn request_history(&self) {
        self.inner.request_history();
    }

    pub fn history_recovery(&self) -> Arc<dyn Fn() + Send + Sync> {
        let inner = Arc::downgrade(&self.inner);
        Arc::new(move || {
            if let Some(inner) = inner.upgrade() {
                inner.request_history();
            }
        })
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
    /// **途絶・起動失敗時は Err を返す**: `ensure_and_submit_chat` の自己修復
    /// （engine drop → 再 ensure → 同一 message retry）は submit の Err を条件に発火する。
    /// 新 host は registry の conversation から `thread/resume` するため、復旧後も会話文脈は
    /// 継がれる（= 途絶 Error の「次の送信で自動復旧」を実現する配線。旧 TurnHost は
    /// turn ごと spawn でこの責務自体が無かった — 常駐化で新たに背負った責務）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        self.submit_with_client_id(prompt, None).await
    }

    pub async fn submit_with_client_id(
        &self,
        prompt: &str,
        client_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let line = {
            let mut st = self.inner.state.lock().expect("rpc state lock");
            if st.dead {
                anyhow::bail!(
                    "codex app-server が利用できません（途絶・起動失敗。再起動で再試行）"
                );
            }
            if st.turn_active || st.thread_id.is_none() {
                st.queue
                    .push_back((prompt.to_string(), client_id.map(str::to_owned)));
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
                Some(build_identified_turn_start(
                    id, &thread_id, prompt, client_id,
                ))
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

fn build_identified_turn_start(
    id: i64,
    thread_id: &str,
    prompt: &str,
    client_id: Option<&str>,
) -> String {
    let mut request: serde_json::Value =
        serde_json::from_str(&build_turn_start(id, thread_id, prompt)).expect("turn request JSON");
    if let Some(client_id) = client_id {
        request["params"]["clientUserMessageId"] = serde_json::json!(client_id);
    }
    request.to_string()
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
        if process_notification(&inner, &mut translator, method, &params) {
            inner.drain_queue().await;
        }
    }

    // stdout close = 途絶。履歴を保持し、次の submit による再生成へ繋ぐ。
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
        inner.emit(ConversationEvent::EngineExited {
            message: format!(
                "codex app-server が休眠しました。次の送信で再開します。\n{stderr_tail}"
            ),
        });
    }
}

/// 履歴反映と live enqueue を同じ lock で直列化し、snapshot の境界を守る。
fn process_notification(
    inner: &RpcInner,
    translator: &mut CodexRpcTranslator,
    method: &str,
    params: &serde_json::Value,
) -> bool {
    let mut st = inner.state.lock().expect("rpc state lock");
    if st.dead {
        return false;
    }
    if st.hydration_target.as_deref().is_some_and(|target| {
        params["threadId"].as_str() == Some(target)
            && (method.starts_with("item/") || matches!(method, "turn/started" | "turn/completed"))
    }) {
        inner.fail_startup_locked(
            &mut st,
            "Codex の履歴取得中に会話が更新されました。元の会話 ID と表示は保持しています。会話の更新が落ち着いてから再試行してください。".into(),
        );
        return false;
    }
    if params["threadId"]
        .as_str()
        .is_some_and(|id| st.thread_id.as_deref() != Some(id))
    {
        return false;
    }
    let restored = st
        .history
        .as_ref()
        .is_some_and(|history| history.restored_pending_item(params));
    if restored && method == "item/started" {
        return false;
    }
    if st
        .history
        .as_mut()
        .is_some_and(|history| history.ingest(method, params))
    {
        return false;
    }
    if restored && method == "item/completed" {
        let (events, user_message_ids, truncated) =
            st.history.as_ref().expect("restored history").snapshot();
        let snapshot = ConversationEvent::CodexHistory {
            thread_id: st.thread_id.clone().unwrap_or_default(),
            events,
            user_message_ids,
            in_flight: st.turn_active,
            truncated,
        };
        inner.emit_locked(&mut st, snapshot);
        return false;
    }
    match method {
        "turn/started" => {
            st.turn_active = true;
            st.turn_id = params
                .pointer("/turn/id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        "turn/completed" => {
            let session_id = {
                st.turn_active = false;
                st.turn_id = None;
                st.thread_id.clone().unwrap_or_default()
            };
            // status=interrupted も「turn の完了」（意図的中断はエラーではない、doc 41 §2-3）。
            // error field が載っていれば Error を併発して chatview に見せる。
            if let Some(err) = params.pointer("/turn/error").filter(|v| !v.is_null()) {
                inner.emit_locked(
                    &mut st,
                    ConversationEvent::Error {
                        message: format!("codex turn error: {}", error_message(err)),
                    },
                );
            }
            inner.emit_locked(
                &mut st,
                ConversationEvent::TurnCompleted {
                    session_id,
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                },
            );
            return true;
        }
        "error" => {
            inner.emit_locked(
                &mut st,
                ConversationEvent::Error {
                    message: format!("codex: {}", error_message(params)),
                },
            );
        }
        _ => {
            for event in translator.ingest(method, params) {
                inner.emit_locked(&mut st, event);
            }
        }
    }
    false
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
                inner.fail_startup(format!(
                    "codex app-server initialize 失敗: {}",
                    error_message(err)
                ));
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
                inner.fail_startup(format!(
                    "Codex の会話を再開できませんでした。元の会話 ID は保持しています: {}",
                    error_message(err)
                ));
                return;
            }
            let thread = &msg["result"]["thread"];
            let result = &msg["result"];
            if thread["historyMode"] == "paginated"
                || result["turnsBackwardsCursor"].is_string()
                || result["itemsBackwardsCursor"].is_string()
            {
                if thread["id"].as_str() != resume_target.as_deref() {
                    inner.fail_startup(
                        "Codex 履歴の会話 ID が一致しません。元の会話 ID は保持しています。".into(),
                    );
                    return;
                }
                let id = {
                    let mut st = inner.state.lock().expect("rpc state lock");
                    st.hydration_target = resume_target.clone();
                    st.alloc(ReqKind::ThreadRead)
                };
                let request = serde_json::json!({"id":id,"method":"thread/read","params":{"threadId":resume_target,"includeTurns":true}});
                inner.write_line_logged(&request.to_string()).await;
                return;
            }
            inner
                .adopt_thread(thread, resume_target.as_deref().unwrap_or(""))
                .await;
        }
        ReqKind::ThreadRead => {
            if let Some(err) = error {
                inner.fail_startup(format!(
                    "Codex の履歴を取得できませんでした。元の会話 ID は保持しています: {}",
                    error_message(err)
                ));
                return;
            }
            inner
                .adopt_thread(
                    &msg["result"]["thread"],
                    resume_target.as_deref().unwrap_or(""),
                )
                .await;
        }
        ReqKind::ThreadStart => {
            if let Some(err) = error {
                inner.fail_startup(format!("codex thread/start 失敗: {}", error_message(err)));
                return;
            }
            if let Some(tid) = msg.pointer("/result/thread/id").and_then(|v| v.as_str()) {
                inner.adopt_thread(&msg["result"]["thread"], tid).await;
            } else {
                inner.fail_startup("Codex thread/start に会話 ID がありません".into());
            }
        }
        ReqKind::TurnStart => {
            if let Some(err) = error {
                {
                    let mut st = inner.state.lock().expect("rpc state lock");
                    st.turn_active = false;
                    st.turn_id = None;
                }
                inner.emit(ConversationEvent::Error {
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

    #[tokio::test]
    async fn concurrent_hydration_update_cannot_adopt_stale_active_state() {
        let _state = crate::test_env::state_dir_async().await;
        let host = response_test_host();
        let mut rx = host.subscribe();
        host.submit("pending prompt").await.unwrap();
        let id = host
            .inner
            .state
            .lock()
            .unwrap()
            .alloc(ReqKind::ThreadResume);
        handle_response(&host.inner, id, &serde_json::json!({"result":{"thread":{"id":"t","historyMode":"paginated","turns":[]}}}), &Some("t".into())).await;
        let read_id = *host
            .inner
            .state
            .lock()
            .unwrap()
            .pending
            .keys()
            .next()
            .unwrap();
        process_notification(
            &host.inner,
            &mut CodexRpcTranslator::new(),
            "turn/completed",
            &serde_json::json!({"threadId":"other-thread","turn":{"id":"other","status":"completed"}}),
        );
        assert!(
            !host.inner.state.lock().unwrap().dead,
            "別 thread の通知では中断しない"
        );
        process_notification(
            &host.inner,
            &mut CodexRpcTranslator::new(),
            "turn/completed",
            &serde_json::json!({"threadId":"t","turn":{"id":"active","status":"completed"}}),
        );
        handle_response(&host.inner, read_id, &serde_json::json!({"result":{"thread":{"id":"t","turns":[{"id":"active","status":"inProgress","items":[]}]}}}), &Some("t".into())).await;
        let st = host.inner.state.lock().unwrap();
        assert!(st.dead, "曖昧な snapshot を採用せず明示的な再試行へ戻す");
        assert!(!st.turn_active);
        assert!(st.queue.is_empty());
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, ConversationEvent::Error { .. }))
        );
        assert!(!events.iter().any(|ev| matches!(
            ev,
            ConversationEvent::CodexHistory { .. } | ConversationEvent::SessionInit { .. }
        )));
    }

    #[tokio::test]
    async fn restored_active_item_completion_does_not_repeat_its_text() {
        let _state = crate::test_env::state_dir_async().await;
        let host = response_test_host();
        host.inner.adopt_thread(&serde_json::json!({"id":"t","turns":[{
            "id":"running","status":"inProgress","items":[{"id":"a","type":"agentMessage","text":"復元済み"}]
        }]}), "t").await;
        let mut rx = host.subscribe();
        let mut translator = CodexRpcTranslator::new();
        process_notification(
            &host.inner,
            &mut translator,
            "item/completed",
            &serde_json::json!({"threadId":"t","turnId":"running","item":{"id":"a","type":"agentMessage","text":"復元済み"}}),
        );
        assert!(
            !std::iter::from_fn(|| rx.try_recv().ok())
                .any(|ev| matches!(ev, ConversationEvent::MessageChunk { .. })),
            "snapshot 済みの本文を完成通知で二度 append しない"
        );
    }

    #[tokio::test]
    async fn restored_running_tool_completion_only_updates_the_existing_call() {
        let _state = crate::test_env::state_dir_async().await;
        let host = response_test_host();
        host.inner.adopt_thread(&serde_json::json!({"id":"t","turns":[{
            "id":"running","status":"inProgress","items":[{"id":"tool","type":"commandExecution","command":"test","status":"inProgress"}]
        }]}), "t").await;
        let mut rx = host.subscribe();
        process_notification(
            &host.inner,
            &mut CodexRpcTranslator::new(),
            "item/completed",
            &serde_json::json!({"threadId":"t","turnId":"running","item":{"id":"tool","type":"commandExecution","command":"test","status":"completed","aggregatedOutput":"ok"}}),
        );
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, ConversationEvent::ToolCall { .. }))
        );
        assert!(events.iter().any(|ev| matches!(
            ev,
            ConversationEvent::ToolCallUpdate { .. } | ConversationEvent::CodexHistory { .. }
        )));
    }

    #[tokio::test]
    async fn restored_text_completion_keeps_unobserved_suffix() {
        let _state = crate::test_env::state_dir_async().await;
        let host = response_test_host();
        host.inner.adopt_thread(&serde_json::json!({"id":"t","turns":[{
            "id":"running","status":"inProgress","items":[{"id":"a","type":"agentMessage","text":"途中"}]
        }]}), "t").await;
        let mut rx = host.subscribe();
        process_notification(
            &host.inner,
            &mut CodexRpcTranslator::new(),
            "item/completed",
            &serde_json::json!({"threadId":"t","turnId":"running","item":{"id":"a","type":"agentMessage","text":"途中の続き"}}),
        );
        assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|ev| matches!(ev, ConversationEvent::CodexHistory { events, .. } if events.contains(&ConversationEvent::MessageChunk { text:"途中の続き".into() }))));
    }

    #[tokio::test]
    async fn paginated_resume_hydrates_full_items_before_adopting() {
        let _state = crate::test_env::state_dir_async().await;
        let host = response_test_host();
        let mut rx = host.subscribe();
        let id = host
            .inner
            .state
            .lock()
            .unwrap()
            .alloc(ReqKind::ThreadResume);
        handle_response(&host.inner, id, &serde_json::json!({"id":id,"result":{
            "turnsBackwardsCursor":"cursor", "thread":{"id":"paged","historyMode":"paginated","turns":[
                {"id":"old","status":"completed","itemsView":"summary","items":[]}
            ]}
        }}), &Some("paged".into())).await;
        assert!(
            !host.inner.state.lock().unwrap().dead,
            "本文を thread/read で取得する前に失敗にしない"
        );
        assert!(rx.try_recv().is_err(), "不完全な履歴で現在の表示を消さない");
        let id = *host
            .inner
            .state
            .lock()
            .unwrap()
            .pending
            .keys()
            .next()
            .expect("履歴取得 request");
        handle_response(&host.inner, id, &serde_json::json!({"id":id,"result":{"thread":{"id":"paged","turns":[
            {"id":"old","status":"completed","items":[{"id":"a","type":"agentMessage","text":"復元"}]}
        ]}}}), &Some("paged".into())).await;
        assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|ev| matches!(ev, ConversationEvent::CodexHistory { events, .. } if events.contains(&ConversationEvent::MessageChunk { text:"復元".into() }))));
    }

    // mem_1Cex9hm7knkwwNWrjqTEBu — Console の native 履歴が host の出力に届く。
    #[tokio::test]
    async fn resume_emits_native_history_without_a_new_turn() {
        let _state = crate::test_env::state_dir_async().await;
        let host = response_test_host();
        let mut rx = host.subscribe();
        let id = host
            .inner
            .state
            .lock()
            .unwrap()
            .alloc(ReqKind::ThreadResume);
        handle_response(&host.inner, id, &serde_json::json!({"id":id,"result":{"thread":{
            "id":"console-thread","turns":[{"id":"old","status":"completed","items":[
                {"id":"user","type":"userMessage","content":[{"type":"text","text":"Console の質問"}]},
                {"id":"answer","type":"agentMessage","text":"Console の応答"}
            ]}]
        }}}), &Some("console-thread".into())).await;
        let mut snapshot = None;
        while let Ok(ev) = rx.try_recv() {
            if let ConversationEvent::CodexHistory {
                events, in_flight, ..
            } = ev
            {
                assert!(!in_flight);
                snapshot = Some(events);
            }
        }
        let events = snapshot.expect("resume の本文を表示に配送する");
        assert!(events.contains(&ConversationEvent::UserMessage {
            text: "Console の質問".into()
        }));
        assert!(events.contains(&ConversationEvent::MessageChunk {
            text: "Console の応答".into()
        }));
        assert!(!host.inner.state.lock().unwrap().turn_active);
    }

    // Task: mem_1Cex2VPFy3gQnXtpYqF1in
    fn response_test_host() -> CodexAgentHost {
        let (event_tx, _) = broadcast::channel(32);
        CodexAgentHost {
            inner: Arc::new(RpcInner {
                event_tx,
                repo: "codex-resume-test".into(),
                lane: "main".into(),
                cwd: "/workspace".into(),
                stdin: tokio::sync::Mutex::new(None),
                state: Mutex::new(RpcState {
                    history: None,
                    startup_error: None,
                    hydration_target: None,
                    thread_id: None,
                    turn_id: None,
                    turn_active: false,
                    queue: VecDeque::new(),
                    in_flight: InFlight::default(),
                    child_pid: None,
                    next_id: 0,
                    pending: HashMap::new(),
                    stopping: false,
                    dead: false,
                    stderr_tail: VecDeque::new(),
                }),
                child: Mutex::new(None),
            }),
            reader: None,
        }
    }

    #[tokio::test]
    async fn resume_error_keeps_conversation_and_rejects_further_submits() {
        let _state = crate::test_env::state_dir_async().await;
        let original = "01a08fa2-a700-7f73-9889-bb52a4258923";
        crate::lane::session_registry::set_conversation(
            "codex-resume-test",
            "main",
            "codex",
            1,
            Some(original),
        )
        .unwrap();
        for reason in [
            "no rollout found",
            "required MCP server failed to initialize",
        ] {
            let host = response_test_host();
            let mut rx = host.subscribe();
            host.submit("queued before resume response").await.unwrap();
            let id = host
                .inner
                .state
                .lock()
                .unwrap()
                .alloc(ReqKind::ThreadResume);
            handle_response(
                &host.inner,
                id,
                &serde_json::json!({
                    "id": id, "error": {"code": -32603, "message": reason}
                }),
                &Some(original.into()),
            )
            .await;

            {
                let state = host.inner.state.lock().unwrap();
                assert!(
                    state.pending.is_empty(),
                    "resume error must not request a new thread"
                );
                assert!(
                    state.queue.is_empty(),
                    "failed prompts must not leak into a later turn"
                );
                assert!(!state.turn_active);
            }
            assert!(
                matches!(rx.try_recv().unwrap(), ConversationEvent::Error { message } if message.contains(reason))
            );
            assert!(
                host.submit("retry by rebuilding the host").await.is_err(),
                "the existing Chat retry path needs Err, not an indefinitely queued prompt"
            );
            let registry =
                crate::lane::session_registry::load("codex-resume-test", "main", "codex");
            assert_eq!(registry.sessions[0].conversation.as_deref(), Some(original));
        }
    }

    #[tokio::test]
    async fn startup_errors_allow_host_retry_instead_of_queueing_forever() {
        for kind in [ReqKind::Initialize, ReqKind::ThreadStart] {
            let host = response_test_host();
            let mut rx = host.subscribe();
            host.submit("waiting for startup").await.unwrap();
            let id = host.inner.state.lock().unwrap().alloc(kind);
            handle_response(
                &host.inner,
                id,
                &serde_json::json!({
                    "id": id, "error": {"code": -32603, "message": "startup unavailable"}
                }),
                &None,
            )
            .await;
            assert!(
                matches!(rx.try_recv().unwrap(), ConversationEvent::Error { message } if message.contains("startup unavailable"))
            );
            assert!(host.submit("retry").await.is_err());
            assert!(host.inner.state.lock().unwrap().queue.is_empty());
        }
    }

    #[tokio::test]
    async fn successful_resume_and_explicit_start_adopt_the_returned_thread() {
        let _state = crate::test_env::state_dir_async().await;
        for (kind, target) in [
            (
                ReqKind::ThreadResume,
                Some("01a08fa2-a700-7f73-9889-bb52a4258923"),
            ),
            (ReqKind::ThreadStart, None),
        ] {
            let host = response_test_host();
            let mut rx = host.subscribe();
            let thread = target.unwrap_or("01a09005-f22f-7dd3-9e7b-0ad53926478b");
            let id = host.inner.state.lock().unwrap().alloc(kind);
            handle_response(
                &host.inner,
                id,
                &serde_json::json!({
                    "id": id, "result": {"thread": {"id": thread, "turns":[]}}
                }),
                &target.map(str::to_owned),
            )
            .await;
            assert!(
                matches!(rx.try_recv().unwrap(), ConversationEvent::SessionInit { session_id, .. } if session_id == thread)
            );
            let registry =
                crate::lane::session_registry::load("codex-resume-test", "main", "codex");
            assert_eq!(registry.sessions[0].conversation.as_deref(), Some(thread));
            assert!(!host.inner.state.lock().unwrap().dead);
        }
    }

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
                ConversationEvent::SessionInit {
                    session_id: sid, ..
                } => session_id = sid,
                ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                ConversationEvent::TurnCompleted { .. } => break,
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
                _ => {}
            }
        }
        assert!(!session_id.is_empty(), "SessionInit（thread id）を観測");
        assert!(text.contains("pong-rpc"), "answer: {text}");
        // registry 直結の書き戻し（doc 40 §4）も実機で確認
        let reg = crate::lane::session_registry::load("vptest-rpc", "main", "codex");
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
                ConversationEvent::MessageChunk { .. } | ConversationEvent::ThoughtChunk { .. } => {
                    break;
                }
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
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
            if matches!(ev, ConversationEvent::TurnCompleted { .. }) {
                break;
            }
        }
        host.stop();
    }
}
