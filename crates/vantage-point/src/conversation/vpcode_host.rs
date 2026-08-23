//! VpcodeHost — vpcode を lane 単位で**常駐**駆動する VCP host（gui engine host）
//!
//! VCP = VP Conversation Protocol（仕様 SSOT = vpcode repo `docs/design/001-vcp.md`）。
//! stdio JSONL、envelope は `{"kind": ...}`（serde tag / snake_case — [`ConversationEvent`]
//! と同形）。**出力語彙が ConversationEvent と 1:1** なのが VCP の存在意義で、翻訳は
//! ほぼ素通し（claude translator 1,127 行との対比）。プロセスモデルは 1 session = 1 vpcode
//! （[`super::host::ClaudeHost`] / [`super::codex_host::CodexAgentHost`] と同型）。
//!
//! ## 他 host との違い 3 点（VCP の設計がもたらす簡略化）
//!
//! - **queue は handshake 窓のみ**: turn 実行中の `user_message` は engine 側が steering
//!   （次の step 境界で同一 turn に差し込み）として扱うため、host は ready 後は常に直送する
//!   （codex の turn_active queue に相当する状態機械が不要）
//! - **会話 id は VP 発行**（VCP R3 — engine は自分の id を持たない）: 新規はここで uuid を
//!   採番して registry に書き、hello.session.id で渡す。engine 側の id との突き合わせが無い
//! - **permission は無政策 engine**（VCP §8）: 毎 tool 実行前に `permission_request` が来て
//!   engine は block する。判定は 100% VP 側（[`Self::respond_permission`] — GUI の
//!   PromptCard / conversation_respond 経路がそのまま使える）
//!
//! ## transcript（resume 正本）は translator 手前で押収
//!
//! `kind: "transcript"` は **storage-plane** のイベントで、display-plane の
//! [`ConversationEvent`] に昇格させない — reader がここで受け取り
//! [`super::vpcode_transcript`]（封筒 JSONL side-car）へ直接 append する。配信流路
//! （pump / topic / webview）に 64KB 級 blob が流れない（mako 裁定 2026-08-22、
//! wire thread 01a028f5 系の 3 者合意。前例 = claude host の control frame 剥ぎ取り）。
//! resume は spawn 時に store から messages を平坦連結して `hello.transcript` に入れる。
//! 宙ぶらりん tool_call の修復は **engine の責務**（§9 — 修復分は通常の transcript
//! flush で返ってくるので、host はそのまま append すれば収束する）。
//!
//! ## 途絶検知（常駐の規律）
//!
//! stdout close = vpcode 途絶。意図的 stop 以外では [`ConversationEvent::EngineExited`] を
//! broadcast する（VCP §6「プロセス死は event にしない — host が合成する」の実装側）。
//!
//! data / calculations / actions:
//! - calculations: `build_hello` / `translate_event`（純関数、単体テスト対象）
//! - actions: プロセス spawn / stdin 書き込み / reader loop / transcript append / broadcast

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::event::ConversationEvent;
use super::host::{InFlight, PermissionDecision};

/// VpcodeHost の起動設定。
#[derive(Debug, Clone)]
pub struct VpcodeHostConfig {
    /// 会話の作業ディレクトリ（lane の repo dir）。
    pub cwd: String,
    /// registry / transcript store の書き込みキー（repo 名）。
    pub repo: String,
    /// store 鍵（session label: `main` / `main#2` …）。
    /// ⚠️ env の `VP_LANE` には使わない — そちらは [`Self::lane_label`]。
    pub lane: String,
    /// identity env（`VP_LANE`）用の素の lane label。
    pub lane_label: String,
    /// identity env（`VP_SESSION_KEY`）用の session key。
    pub session_key: crate::lane::session_registry::SessionKey,
    /// 再開する会話 id（registry の `conversation`）。None = 新規（**VP がここで採番** — VCP R3）。
    pub session_id: Option<String>,
    /// session の model 指定（None = spawn 側で解決: env VPCODE_MODEL → catalog 先頭）。
    pub model: Option<String>,
}

/// reader task / host メソッドが共有する可変状態（std Mutex — await を跨がずに触る）。
struct VcpState {
    /// hello → ready の handshake が完了したか（完了前の submit は queue へ）。
    ready: bool,
    /// handshake 完了前に来た submit の待ち行列。ready 後は**常に直送**
    /// （turn 実行中の user_message = engine 側 steering、VCP §7）。
    queue: VecDeque<String>,
    /// disk にまだ載っていない増分 + commit 世代（[`super::host`] と同契約）。
    in_flight: InFlight,
    /// vpcode 子プロセスの pid。
    child_pid: Option<u32>,
    /// transcript 封筒 chain の直前 id（spawn 時に store から復元）。
    last_envelope_id: Option<String>,
    /// 明示 stop 中か（reader loop 終端の途絶 event を抑止）。
    stopping: bool,
    /// vpcode 途絶（stdout close / stdin 書込失敗）を観測したか。true の submit は Err —
    /// `ensure_and_submit_chat` の自己修復（engine drop → 再 ensure → retry）に委ねる。
    dead: bool,
    /// stderr の末尾数行（途絶 event の診断材料）。
    stderr_tail: VecDeque<String>,
}

/// reader task と host が共有する不変部 + 状態。
struct VcpInner {
    event_tx: broadcast::Sender<ConversationEvent>,
    repo: String,
    lane: String,
    /// stdin writer（tokio Mutex — submit と reader task の書き込みを直列化）。
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    state: Mutex<VcpState>,
    child: Mutex<Option<Child>>,
}

impl VcpInner {
    async fn write_line(&self, line: &str) -> std::io::Result<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| std::io::Error::other("vpcode stdin は閉じられています"))?;
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        stdin.write_all(&buf).await?;
        stdin.flush().await
    }

    fn emit(&self, event: ConversationEvent) {
        // in-flight fold（codex_host / super::host と同規律のローカル版）。
        {
            let mut st = self.state.lock().expect("vcp state lock");
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
}

/// vpcode を VCP（`--vcp`）で常駐駆動する host。
pub struct VpcodeHost {
    inner: Arc<VcpInner>,
    reader: Option<JoinHandle<()>>,
    /// この host が駆動する会話 id（VP 発行、hello.session.id — 常に確定している）。
    session_id: String,
}

impl VpcodeHost {
    /// vpcode 子プロセスを起動し、hello（transcript 込み）を送って handshake を開始する。
    pub fn spawn(config: VpcodeHostConfig) -> anyhow::Result<Self> {
        // model は **hello の必須 field**（vpcode に「engine 既定」は存在しない — P0 oneshot
        // も --model / VPCODE_MODEL 必須、2026-08-22 実機で確認）。解決は [`resolve_model`]。
        // catalog も空（P2 動的化で LM Studio 不達等）なら「次の一手が打てる文」で落とす
        // （VCP「診断できる失敗」原則）。
        let model = resolve_model(config.model.clone(), std::env::var("VPCODE_MODEL").ok())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "vpcode の model を解決できません（session の model 指定か、daemon 環境の VPCODE_MODEL）。候補は `vpcode --models` で一覧できます"
                )
            })?;
        // 会話 id は VP 発行（VCP R3）。新規はここで採番して registry へ —
        // engine 側発行の id を待つ codex（adopt_thread）と違い、spawn 時点で確定する。
        let session_id = match &config.session_id {
            Some(id) => id.clone(),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let (lane_label, key) =
                    crate::lane::session_registry::parse_session_label(&config.lane);
                if let Err(e) = crate::lane::session_registry::set_conversation(
                    &config.repo,
                    lane_label,
                    "vpcode",
                    key,
                    Some(&id),
                ) {
                    tracing::warn!("vpcode: 会話 id の registry 書き込みに失敗: {e}");
                }
                id
            }
        };
        // resume 材料（封筒を剥いだ平坦 messages — 空 = 新規会話として hello から省略）。
        let transcript = super::vpcode_transcript::load_messages(&config.repo, &config.lane);
        let last_envelope_id = super::vpcode_transcript::last_id(&config.repo, &config.lane);

        let mut cmd = tokio::process::Command::new("vpcode");
        cmd.arg("--vcp")
            .current_dir(&config.cwd)
            // identity env（doc 51 §1 A3b）: engine（とその bash tool の子）が `vp now` /
            // wire で自分を名乗る口。他 host と同じ契約。
            .env("VP_REPO", &config.repo)
            .env("VP_LANE", &config.lane_label)
            .env("VP_SESSION_KEY", config.session_key.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("vpcode の起動に失敗（PATH に vpcode が要ります）: {e}")
        })?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("vpcode の stdout が取れません"))?;
        let stderr = child.stderr.take();
        let child_pid = child.id();

        let (event_tx, _rx) = broadcast::channel::<ConversationEvent>(256);
        let inner = Arc::new(VcpInner {
            event_tx,
            repo: config.repo.clone(),
            lane: config.lane.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            state: Mutex::new(VcpState {
                ready: false,
                queue: VecDeque::new(),
                in_flight: InFlight::default(),
                child_pid,
                last_envelope_id,
                stopping: false,
                dead: false,
                stderr_tail: VecDeque::new(),
            }),
            child: Mutex::new(Some(child)),
        });
        // stderr drain（protocol 外の人間向け log、VCP §3）: 途絶 event の診断材料に末尾保持。
        if let Some(stderr) = stderr {
            let drain = inner.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    tracing::debug!("vpcode stderr: {line}");
                    let mut st = drain.state.lock().expect("vcp state lock");
                    if st.stderr_tail.len() >= 5 {
                        st.stderr_tail.pop_front();
                    }
                    st.stderr_tail.push_back(line);
                }
            });
        }
        tracing::info!(
            "VpcodeHost spawn（常駐 --vcp、repo={}, lane={}, session_id={}, resume_msgs={}, pid={:?}）",
            config.repo,
            config.lane,
            session_id,
            transcript.len(),
            child_pid
        );
        let hello = build_hello(&session_id, &config.cwd, &model, &transcript);
        let reader = tokio::spawn(run_reader(inner.clone(), stdout, hello));
        Ok(Self {
            inner,
            reader: Some(reader),
            session_id,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.inner.event_tx.subscribe()
    }

    pub fn in_flight(&self) -> InFlight {
        self.inner
            .state
            .lock()
            .expect("vcp state lock")
            .in_flight
            .clone()
    }

    pub fn commit_seq(&self) -> u64 {
        self.inner
            .state
            .lock()
            .expect("vcp state lock")
            .in_flight
            .seq
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.state.lock().expect("vcp state lock").child_pid
    }

    /// この host の会話 id（VP 発行）。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// ユーザープロンプトを投入する（ready 前は queue、ready 後は常に直送 —
    /// turn 実行中の直送は engine 側 steering として同一 turn に差し込まれる、VCP §7）。
    ///
    /// **途絶時は Err**: `ensure_and_submit_chat` の自己修復（engine drop → 再 ensure →
    /// retry）が Err を条件に発火する。復旧後は transcript store からの resume で文脈が継がれる。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        let line = {
            let mut st = self.inner.state.lock().expect("vcp state lock");
            if st.dead {
                anyhow::bail!("vpcode が終了しています（engine 再起動で復旧）");
            }
            if !st.ready {
                st.queue.push_back(prompt.to_string());
                tracing::debug!(
                    "vpcode submit: handshake 前 → queue（depth={}）",
                    st.queue.len()
                );
                None
            } else {
                Some(build_user_message(prompt))
            }
        };
        if let Some(line) = line
            && let Err(e) = self.inner.write_line(&line).await
        {
            let mut st = self.inner.state.lock().expect("vcp state lock");
            st.dead = true;
            anyhow::bail!("vpcode への送信に失敗（途絶）: {e}");
        }
        Ok(())
    }

    /// 実行中 turn の中断（次の step 境界で効く — bash 実行中は ~50ms で kill_tree、VCP §7）。
    /// turn は engine 側で `stop_reason: "interrupted"` として完了する。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        {
            let mut st = self.inner.state.lock().expect("vcp state lock");
            st.queue.clear();
            if st.dead {
                return Ok(());
            }
        }
        // 書込失敗は lenient（途絶なら turn はもう走っていない）。
        if let Err(e) = self
            .inner
            .write_line(&serde_json::json!({"kind": "interrupt"}).to_string())
            .await
        {
            tracing::warn!("vpcode interrupt 送信失敗（途絶疑い）: {e}");
        }
        Ok(())
    }

    /// `permission_request` への回答（VCP §8 control_response）。deny の `message` は
    /// tool 結果として model へ渡る「次の一手が打てる文」。
    pub async fn respond_permission(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> anyhow::Result<()> {
        let line = match decision {
            PermissionDecision::Allow { .. } => serde_json::json!({
                "kind": "control_response",
                "request_id": request_id,
                "allow": true,
            }),
            PermissionDecision::Deny { message } => serde_json::json!({
                "kind": "control_response",
                "request_id": request_id,
                "allow": false,
                "note": message,
            }),
        };
        self.inner
            .write_line(&line.to_string())
            .await
            .map_err(|e| anyhow::anyhow!("vpcode control_response の送信に失敗: {e}"))
    }

    /// 明示 teardown（[`super::engine::ChatEngineSlot`] Drop から呼ぶ）。
    ///
    /// graceful shutdown（VCP §5 — 実行中 bash を kill_tree して exit 0）は同期文脈から
    /// 送れないため kill に倒す（kill_on_drop と同挙動）。会話の正本は transcript store に
    /// flush 済みで、次回 spawn の resume で継がれる — 「プロセスは死ぬがコンテキストは蘇る」。
    pub fn stop(&mut self) {
        {
            let mut st = self.inner.state.lock().expect("vcp state lock");
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
            "VpcodeHost stop（repo={}, lane={}）",
            self.inner.repo,
            self.inner.lane
        );
    }
}

// =============================================================================
// calculations — VCP line の組み立てと翻訳（純関数、単体テスト対象）
// =============================================================================

/// model の解決（純関数）: session 指定 → env `VPCODE_MODEL` → catalog 先頭（VP 既定）。
/// 空文字は「未指定」として次段へ倒す。env が catalog より先なのは dev override の慣行
/// （VP_PROFILE / CHRONISTA_HUB_ADDR と同型）。None = catalog も空（呼び手が診断 Err に変換）。
fn resolve_model(session: Option<String>, env: Option<String>) -> Option<String> {
    session
        .filter(|m| !m.is_empty())
        .or_else(|| env.filter(|m| !m.is_empty()))
        .or_else(|| {
            crate::conversation::EngineKind::Vpcode
                .model_choices()
                .first()
                .map(|c| c.value.clone())
        })
}

/// hello（VCP §4）。`model` は**必須**（vpcode に engine 既定は無い — spawn 側で解決済み）。
/// `transcript` が空なら field ごと省略（新規会話）。system role の混入は engine 側が
/// 診断付きで拒否する（§9 — こちらは flush 由来のみを保存しているので混入経路が無い、二重防壁）。
fn build_hello(
    session_id: &str,
    cwd: &str,
    model: &str,
    transcript: &[serde_json::Value],
) -> String {
    let mut hello = serde_json::json!({
        "kind": "hello",
        "protocol_version": 1,
        "session": { "id": session_id },
        "cwd": cwd,
        "model": { "id": model },
    });
    if !transcript.is_empty() {
        hello["transcript"] = serde_json::Value::Array(transcript.to_vec());
    }
    hello.to_string()
}

fn build_user_message(text: &str) -> String {
    serde_json::json!({ "kind": "user_message", "text": text }).to_string()
}

/// VCP イベント 1 行 → 翻訳結果。
enum Translated {
    /// GUI へ broadcast する（素通し変換）。
    Event(ConversationEvent),
    /// transcript flush（storage-plane — reader が side-car へ append、broadcast しない）。
    Transcript(serde_json::Value),
    /// handshake 完了（ready）。SessionInit + queue 排出のトリガ。
    Ready(ConversationEvent),
    /// 消費しない（turn_started / inbox_consumed / 未知 kind — additive 互換で無視）。
    Skip,
}

/// VCP → ConversationEvent の翻訳（純関数）。語彙が 1:1 なのでほぼ素通し —
/// ここが数十行で済むことが VCP の存在意義（claude translator 1,127 行との対比）。
fn translate_line(v: &serde_json::Value) -> Translated {
    let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let s = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    match kind {
        "ready" => Translated::Ready(ConversationEvent::SessionInit {
            session_id: s("session_id"),
            model: v.get("model").and_then(|m| m.as_str()).map(str::to_string),
            permission_mode: None,
            cwd: v.get("cwd").and_then(|c| c.as_str()).map(str::to_string),
            tools: v
                .get("tools")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            mcp_servers: Vec::new(),
            slash_commands: Vec::new(),
            command_docs: std::collections::HashMap::new(),
        }),
        "message_chunk" => Translated::Event(ConversationEvent::MessageChunk { text: s("text") }),
        "tool_call" => Translated::Event(ConversationEvent::ToolCall {
            id: s("id"),
            name: s("name"),
            input: v.get("input").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "tool_call_update" => Translated::Event(ConversationEvent::ToolCallUpdate {
            tool_use_id: s("tool_use_id"),
            content: s("content"),
            is_error: v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false),
        }),
        "permission_request" => Translated::Event(ConversationEvent::PermissionRequest {
            request_id: s("request_id"),
            tool_name: s("tool_name"),
            input: v.get("input").cloned().unwrap_or(serde_json::Value::Null),
        }),
        "turn_completed" => Translated::Event(ConversationEvent::TurnCompleted {
            session_id: s("session_id"),
            cost_usd: v.get("cost_usd").and_then(serde_json::Value::as_f64),
            context_tokens: v.get("context_tokens").and_then(serde_json::Value::as_u64),
            context_window: v.get("context_window").and_then(serde_json::Value::as_u64),
        }),
        "error" => Translated::Event(ConversationEvent::Error {
            message: s("message"),
        }),
        "transcript" => Translated::Transcript(v.clone()),
        // turn_started / inbox_consumed は P1 の GUI に対応語彙なし（VCP 独自、無視可）。
        // 未知 kind も同じ扱い = additive-only 前方互換（VCP §2-3）。
        other => {
            if !matches!(other, "turn_started" | "inbox_consumed") {
                tracing::warn!("vpcode: 未知の VCP kind '{other}' を無視します（additive 互換）");
            }
            Translated::Skip
        }
    }
}

// =============================================================================
// actions — reader loop
// =============================================================================

/// stdout reader: hello 送信 → 行 parse → 翻訳 → broadcast / transcript append。
async fn run_reader(inner: Arc<VcpInner>, stdout: tokio::process::ChildStdout, hello_line: String) {
    // hello は reader 開始後に送る（ready 応答を取りこぼさない順序 — 書込失敗は途絶扱い）。
    if let Err(e) = inner.write_line(&hello_line).await {
        tracing::warn!("vpcode hello 送信失敗: {e}");
    }
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            tracing::warn!("vpcode: JSONL parse 失敗の行を無視: {line}");
            continue;
        };
        match translate_line(&v) {
            Translated::Ready(event) => {
                inner.emit(event);
                // handshake 完了 → 溜まった submit を排出（到着順）。
                let drained: Vec<String> = {
                    let mut st = inner.state.lock().expect("vcp state lock");
                    st.ready = true;
                    st.queue.drain(..).collect()
                };
                for prompt in drained {
                    let msg = build_user_message(&prompt);
                    if let Err(e) = inner.write_line(&msg).await {
                        tracing::warn!("vpcode queued submit の送信失敗: {e}");
                    }
                }
            }
            Translated::Transcript(payload) => {
                // storage-plane: 封筒に包んで side-car へ（broadcast しない — module doc）。
                let envelope = {
                    let mut st = inner.state.lock().expect("vcp state lock");
                    let env = super::vpcode_transcript::TranscriptEnvelope {
                        id: uuid::Uuid::new_v4().to_string(),
                        prev: st.last_envelope_id.clone(),
                        ts: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                        payload,
                    };
                    st.last_envelope_id = Some(env.id.clone());
                    env
                };
                if let Err(e) =
                    super::vpcode_transcript::append(&inner.repo, &inner.lane, &envelope)
                {
                    tracing::warn!(
                        "vpcode transcript の保存に失敗（resume が欠ける恐れ、repo={}, lane={}）: {e}",
                        inner.repo,
                        inner.lane
                    );
                }
            }
            Translated::Event(event) => inner.emit(event),
            Translated::Skip => {}
        }
    }
    // stdout close = vpcode 途絶。意図的 stop 以外は EngineExited を合成（VCP §6）。
    let (stopping, tail) = {
        let mut st = inner.state.lock().expect("vcp state lock");
        st.dead = true;
        st.child_pid = None;
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
        let detail = if tail.is_empty() {
            String::new()
        } else {
            format!("\n{tail}")
        };
        inner.emit(ConversationEvent::EngineExited {
            message: format!(
                "vpcode が終了しました。次のメッセージ送信で再起動します（会話は transcript から再開）。{detail}"
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// hello の組み立て: model は必須（engine に既定が無い — 2026-08-22 実機確認）、
    /// transcript は新規なら省略 / resume は平坦 messages を同梱。
    #[test]
    fn build_hello_requires_model_and_carries_transcript() {
        let fresh = build_hello("sid-1", "/work", "qwen/x", &[]);
        let v: serde_json::Value = serde_json::from_str(&fresh).expect("json");
        assert_eq!(v["kind"], "hello");
        assert_eq!(v["protocol_version"], 1);
        assert_eq!(v["session"]["id"], "sid-1");
        assert_eq!(v["model"]["id"], "qwen/x", "model は必須 field");
        assert!(v.get("transcript").is_none(), "新規は transcript を省略");

        let msgs = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let resume = build_hello("sid-1", "/work", "qwen/x", &msgs);
        let v: serde_json::Value = serde_json::from_str(&resume).expect("json");
        assert_eq!(v["transcript"].as_array().map(Vec::len), Some(1));
        // 平坦な messages（封筒の入れ子でない）
        assert_eq!(v["transcript"][0]["role"], "user");
    }

    /// model 解決の優先順: session 指定 → env → catalog 先頭。空文字は「未指定」。
    /// catalog 先頭 fallback があるので GUI の新規 session は picker 操作なしで動く
    /// （旧実装は fallback 無しで、model 口の無い GUI では必ず spawn Err = 詰みだった）。
    #[test]
    fn resolve_model_priority_session_env_catalog() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            resolve_model(s("a"), s("b")),
            s("a"),
            "session 指定が最優先"
        );
        assert_eq!(resolve_model(None, s("b")), s("b"), "env が次点");
        assert_eq!(resolve_model(s(""), s("b")), s("b"), "空文字は未指定扱い");
        let fallback = resolve_model(None, None).expect("catalog 先頭に fallback");
        assert_eq!(
            Some(fallback.as_str()),
            crate::conversation::EngineKind::Vpcode
                .model_choices()
                .first()
                .map(|c| c.value.as_str()),
            "fallback = catalog 先頭（VP 既定）"
        );
    }

    /// 翻訳の素通し性: 各 kind が対応 variant へ 1:1 で写る。
    #[test]
    fn translate_maps_vcp_kinds_one_to_one() {
        let chunk = serde_json::json!({"kind": "message_chunk", "text": "hi"});
        assert!(matches!(
            translate_line(&chunk),
            Translated::Event(ConversationEvent::MessageChunk { text }) if text == "hi"
        ));

        let tool = serde_json::json!({"kind": "tool_call", "id": "t1", "name": "bash", "input": {"command": "ls"}});
        assert!(matches!(
            translate_line(&tool),
            Translated::Event(ConversationEvent::ToolCall { id, name, .. }) if id == "t1" && name == "bash"
        ));

        let perm = serde_json::json!({"kind": "permission_request", "request_id": "perm_1", "tool_name": "bash", "input": {}});
        assert!(matches!(
            translate_line(&perm),
            Translated::Event(ConversationEvent::PermissionRequest { request_id, .. }) if request_id == "perm_1"
        ));

        let done = serde_json::json!({"kind": "turn_completed", "session_id": "s", "stop_reason": "end_turn", "context_tokens": 100, "context_window": 4096});
        assert!(matches!(
            translate_line(&done),
            Translated::Event(ConversationEvent::TurnCompleted {
                context_tokens: Some(100),
                context_window: Some(4096),
                ..
            })
        ));

        let ready = serde_json::json!({"kind": "ready", "session_id": "s", "model": "m", "cwd": "/w", "tools": [{"name": "bash"}]});
        match translate_line(&ready) {
            Translated::Ready(ConversationEvent::SessionInit {
                session_id,
                model,
                tools,
                ..
            }) => {
                assert_eq!(session_id, "s");
                assert_eq!(model.as_deref(), Some("m"));
                assert_eq!(tools, vec!["bash".to_string()]);
            }
            _ => panic!("ready → SessionInit"),
        }
    }

    /// storage-plane の押収と additive 互換: transcript は Event にならず、未知 kind は Skip。
    #[test]
    fn transcript_is_intercepted_and_unknown_kinds_are_skipped() {
        let tr = serde_json::json!({"kind": "transcript", "messages": [{"role": "user", "content": "x"}]});
        assert!(matches!(translate_line(&tr), Translated::Transcript(_)));

        for k in ["turn_started", "inbox_consumed", "kind_from_the_future"] {
            let v = serde_json::json!({ "kind": k });
            assert!(
                matches!(translate_line(&v), Translated::Skip),
                "{k} は Skip"
            );
        }
    }
}
