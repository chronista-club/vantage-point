//! EchoesAgentHost — headless claude を lane 単位で常駐駆動する（Act II engine host）
//!
//! doc 32 §3。`claude -p --input-format stream-json --output-format stream-json` を
//! piped stdio で spawn し、stdout を [`EchoesTranslator`] に通して [`EchoesEvent`] を
//! `broadcast::Sender` に流す。Act I（PtySlot + TUI）とは別系統の「headless engine」。
//!
//! PtySlot ⇄ terminal_pump と同型: host が producer（broadcast tx を持つ）、
//! `echoes_pump` が consumer（TopicRouter へ route）。engine プロセスは会話を保持するため
//! lane が Act II の間は常駐する（demand-driven ではない）。
//!
//! ## in-flight tail（replay の冪等性を「生成中」まで広げる）
//!
//! claude は message を **完了時にしか** transcript(jsonl) へ flush しない。 一方 echoes topic は
//! 非 retained（`TopicRouter::route` はその時点の subscriber にだけ配る）。 よって assistant が
//! 生成中に WS/QUIC が瞬断すると、 demand 再発火で走る transcript replay は「生成中 message の
//! 直前まで」しか復元できず、 瞬断前に届いていた delta は永久に失われる。 GUI は `ReplayStart` で
//! items を reset しているため、 復帰後の `MessageChunk` が **文の途中から新しい assistant バブル**
//! を立ててしまう（旧 #699 の既知ギャップ）。
//!
//! そこで host は「まだ disk に無い増分」= **in-flight tail** を保持する。 transcript commit 境界
//! （`assistant` / `user` 行、[`super::translate::Ingested::commits_transcript`]）で tail を捨て、
//! それ以降の [`EchoesEvent::MessageChunk`] / [`EchoesEvent::ThoughtChunk`] だけを積む。
//! replay 時に transcript の後ろへ tail を継げば、 生成の真っ最中に着地しても現在状態が厳密に
//! 再現される（= 冪等性が turn 境界に縛られなくなる）。
//!
//! ⚠️ tail に [`EchoesEvent::ToolCall`] を積んではならない。 translator は ToolCall を
//! `content_block_stop` で発火するが、 その block の commit（snapshot 行）は **1 行前に来ている**
//! ため、 ToolCall は既に transcript 側に存在する（`translate` の module doc / golden test 参照）。
//!
//! tail と transcript の間には「読んだ直後に commit が挟まる」窓がある。 [`Self::in_flight`] は
//! commit 世代 [`InFlight::seq`] を添えて返すので、 呼び手は transcript 読み後に seq を検算して
//! 世代が動いていたら読み直せる（[`crate::process::unison_server`] の replay handler）。
//!
//! data / calculations / actions:
//! - calculations: stdin メッセージ JSON 生成（[`user_message_json`]、純関数）
//! - actions: spawn / stdout→translate→broadcast / in-flight tail 更新 / cc_session 記録 / stdin 書き込み

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;

use super::event::{EchoesEvent, QuestionSpec};
use super::translate::EchoesTranslator;

/// disk（transcript）にまだ載っていない増分と、その commit 世代。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InFlight {
    /// commit 世代。 transcript に 1 行 flush されるたび +1。 transcript を読む側は
    /// 「読む前」と「読んだ後」で同値なら tail が transcript と整合すると判定できる。
    pub seq: u64,
    /// 直近 commit 以降の [`EchoesEvent::MessageChunk`] / [`EchoesEvent::ThoughtChunk`]。
    pub tail: Vec<EchoesEvent>,
}

impl InFlight {
    /// tail を捨てて commit 世代を進める。 世代を進めることで、 並行して transcript を読んでいる
    /// replay handler が「状態が動いた」ことを検知して読み直せる。
    fn reset(&mut self) {
        self.tail.clear();
        self.seq = self.seq.wrapping_add(1);
    }
}

/// tail に積んでよい event か（= commit 前にしか存在しない増分）。
///
/// ToolCall / ToolCallUpdate / Plan は commit 済み block 由来なので積まない（module doc の ⚠️）。
fn is_uncommitted_chunk(event: &EchoesEvent) -> bool {
    matches!(
        event,
        EchoesEvent::MessageChunk { .. } | EchoesEvent::ThoughtChunk { .. }
    )
}

/// stdout 1 行分の翻訳結果を in-flight tail に畳み込む（純粋 mutation = 単体テスト可能）。
///
/// - commit 境界（`assistant` / `user` 行）: tail を捨てて世代 +1。
/// - SessionInit（新 engine の起点）/ TurnCompleted / Error: 会話が確定するので同じく捨てる。
/// - それ以外の増分（MessageChunk / ThoughtChunk）だけを積む。
fn fold_in_flight(f: &mut InFlight, out: &super::translate::Ingested) {
    if out.commits_transcript {
        f.reset();
    }
    for event in &out.events {
        match event {
            EchoesEvent::SessionInit { .. }
            | EchoesEvent::TurnCompleted { .. }
            | EchoesEvent::Error { .. } => f.reset(),
            e if is_uncommitted_chunk(e) => f.tail.push(e.clone()),
            _ => {}
        }
    }
}

/// EchoesAgentHost の起動設定。
#[derive(Debug, Clone)]
pub struct EchoesHostConfig {
    /// engine の作業ディレクトリ（lane の project dir）。
    pub cwd: String,
    /// cc_session 記録キー（project 名）。
    pub project: String,
    /// cc_session 記録キー（lane label: conductor / performer 名）。
    pub lane: String,
    /// 再開する session id（`--resume`）。Act I ⇄ II 切替 / SP 再起動復帰に使う。
    pub resume_session_id: Option<String>,
    /// 使用モデル（`--model`）。None = claude default。
    pub model: Option<String>,
    /// claude CLI パス（未指定なら PATH / well-known から解決）。
    pub claude_cli_path: Option<String>,
}

/// lane 単位の headless claude engine host。
///
/// doc 33: LanePool のエンジンスロットに保持される。`RwLock<LanePool>` の **read lock 下で
/// submit** できるよう、stdin は内部 `tokio::sync::Mutex` で持つ（`submit(&self)`）。
pub struct EchoesAgentHost {
    child: Child,
    /// user message（submit）と control frame（respond/interrupt/handshake）を直列化する stdin。
    /// initialize handshake を spawn 時に別 task から書くため Arc で共有する。
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    event_tx: broadcast::Sender<EchoesEvent>,
    pid: Option<u32>,
    /// stdout ポンプが更新する in-flight tail（module doc）。 await を跨がないので std Mutex。
    in_flight: Arc<Mutex<InFlight>>,
    /// 逆方向 can_use_tool の pending（request_id → 原 input + tool_name、doc 35 §2.3）。
    /// 回答時に updatedInput を原 input からエコー再構築するために保持する。
    pending: Arc<Mutex<HashMap<String, PendingPermission>>>,
}

impl EchoesAgentHost {
    /// headless claude を spawn し、stdout ポンプを起動する。
    ///
    /// stdout の各行は [`EchoesTranslator`] を通り、[`EchoesEvent`] として broadcast される。
    /// `SessionInit` を観測したら session id を cc_session に記録する（resume の SSOT）。
    pub fn spawn(config: EchoesHostConfig) -> anyhow::Result<Self> {
        let claude_path = crate::agent::get_claude_cli_path(config.claude_cli_path.as_deref());
        let mut cmd = Command::new(&claude_path);
        // 親（SP）の env を継承 — spawn_env 済みの PATH 等を引き継ぐ。
        cmd.envs(std::env::vars());
        // cwd 空は「継承」（呼び元の cwd を使う）— test / project_dir 未解決時の防御。
        if !config.cwd.is_empty() {
            cmd.current_dir(&config.cwd);
        }

        // 双方向 stream-json + partial（Step 0 で確定した Act II 駆動形、doc 32 §10）。
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--include-partial-messages")
            .arg("--verbose");

        // Act I（TUI）が bypassPermissions で全ツール素通しなのに Act II を揃える（doc 33 §9、
        // user 要件 2026-07-09「act I レベルにここも合わせよう」）。acceptEdits だと Edit は
        // auto-apply されるが Bash 等は headless で許可待ち→承認 UI が無かったため error 化して
        // いた。bypassPermissions で TUI と同じ体験にする。
        cmd.arg("--permission-mode").arg("bypassPermissions");

        // doc 35 §2.1: 質問/承認を stdio control channel に routing する点火 flag。claude 2.1.197 の
        // --help には出ない hidden flag だが受理・機能する（§8 実測、旧「2.1.197 で削除済」コメントの
        // 誤りを訂正）。bypassPermissions のまま通常 tool（Write/Bash）は素通し、AskUserQuestion だけが
        // 逆方向 can_use_tool として飛ぶ → 既定体験を退行させずに HITL 質問面を開ける。
        cmd.arg("--permission-prompt-tool").arg("stdio");

        if let Some(ref model) = config.model {
            cmd.arg("--model").arg(model);
        }
        // resume: session id 保持で文脈継続（Step 0 Spike C で実証）。
        if let Some(ref sid) = config.resume_session_id {
            cmd.arg("--resume").arg(sid);
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // host が drop されたら engine プロセスも殺す（map から外れた headless claude を
        // orphan にしない）。stop() を明示しない撤収経路の安全網。
        cmd.kill_on_drop(true);

        tracing::info!(
            "EchoesAgentHost spawn (project={}, lane={}, resume={:?})",
            config.project,
            config.lane,
            config.resume_session_id.as_deref().unwrap_or("new")
        );

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("headless claude の起動に失敗（PATH に claude があるか確認）: {e}")
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdin のキャプチャに失敗"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdout のキャプチャに失敗"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("stderr のキャプチャに失敗"))?;

        // broadcast: 複数 consumer（echoes_pump / 将来の Act I mirror）に配れる。
        let (event_tx, _rx) = broadcast::channel::<EchoesEvent>(256);

        let in_flight = Arc::new(Mutex::new(InFlight::default()));
        let pending = Arc::new(Mutex::new(HashMap::<String, PendingPermission>::new()));

        // stdout ポンプ: 行 → translate → in-flight tail 更新 → broadcast
        //（+ SessionInit で cc_session 記録 + control frame の抜き取り）。
        let tx = event_tx.clone();
        let project = config.project.clone();
        let lane = config.lane.clone();
        let pump_in_flight = in_flight.clone();
        let pump_pending = pending.clone();
        tokio::spawn(async move {
            let mut translator = EchoesTranslator::new();
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // control protocol frame（逆方向 can_use_tool / 我々の control への応答）は
                // translator の手前で抜く（doc 35 §2.4、EchoesEvent 語彙を engine 制御で汚さない）。
                if let Some(frame) = classify_control_frame(&line) {
                    match frame {
                        ControlFrame::CanUseTool {
                            request_id,
                            tool_name,
                            input,
                        } => {
                            // pending 登録: 回答時に原 input から updatedInput をエコー再構築する。
                            pump_pending.lock().expect("pending lock").insert(
                                request_id.clone(),
                                PendingPermission {
                                    tool_name: tool_name.clone(),
                                    input: input.clone(),
                                },
                            );
                            let _ = tx.send(can_use_tool_to_event(&request_id, &tool_name, &input));
                        }
                        // 我々の能動 control（initialize handshake 等）への応答。PR1 は消費のみ。
                        ControlFrame::Response => {}
                    }
                    continue;
                }
                let out = translator.ingest(&line);
                // tail 更新は broadcast より先。 replay handler が「配信済みだが tail に無い」
                // 増分を取りこぼさない順序（tail ⊇ 配信済み未 commit 分 を保つ）。
                fold_in_flight(&mut pump_in_flight.lock().expect("in_flight lock"), &out);
                for event in out.events {
                    if let EchoesEvent::SessionInit { session_id, .. } = &event {
                        record_session(&project, &lane, session_id);
                    }
                    // 受信者不在（Closed）は無視 — engine は購読者に依存せず動く。
                    let _ = tx.send(event);
                }
            }
            tracing::debug!("EchoesAgentHost stdout ポンプ終了（project={project}, lane={lane}）");
            // stream 途絶（engine crash / stop）を GUI に可視化する。stdout close =
            // engine プロセス終了だが、従来は debug log に落ちるだけで購読者（chatview）に
            // 届かず「止まった?」が見えなかった（Act I は PTY 切断が xterm に即見える）。
            // 既存 Error 経路に相乗りして chatview に途絶を出す。
            // （stop/crash の区別・EngineExited 専用 variant・再起動ボタンは後続 PR）
            // engine が死んだ = tail の続きはもう来ない。 replay に出さない。
            pump_in_flight.lock().expect("in_flight lock").reset();
            // 未応答の can_use_tool はもう解決できない。pending を掃除する（stale request_id 防止）。
            pump_pending.lock().expect("pending lock").clear();
            let _ = tx.send(EchoesEvent::Error {
                message: "エンジンとの接続が途絶しました（メッセージ送信で再起動します）"
                    .to_string(),
            });
        });

        // stderr はログのみ。
        let lane_err = config.lane.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("headless claude stderr (lane={lane_err}): {line}");
            }
        });

        let pid = child.id();
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));

        // initialize handshake（control channel 確立、doc 35 §2.2）。spawn は sync fn で await
        // できないので task に逃す。stdin Mutex 経由なので submit / respond と直列化される。
        {
            let init_stdin = Arc::clone(&stdin);
            tokio::spawn(async move {
                let frame = control_initialize_json("init-1");
                let mut guard = init_stdin.lock().await;
                if let Err(e) = write_line(&mut guard, &frame).await {
                    tracing::warn!("initialize handshake 送信失敗: {e}");
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            event_tx,
            pid,
            in_flight,
            pending,
        })
    }

    /// EchoesEvent の broadcast receiver を得る（echoes_pump などが購読）。
    pub fn subscribe(&self) -> broadcast::Receiver<EchoesEvent> {
        self.event_tx.subscribe()
    }

    /// disk にまだ載っていない増分（+ commit 世代）のスナップショット（module doc）。
    ///
    /// replay handler はこれを transcript の後ろへ継ぐ。 transcript 読みの前後で
    /// [`InFlight::seq`] が変わっていなければ、 tail と transcript は重複も欠落もしない。
    pub fn in_flight(&self) -> InFlight {
        self.in_flight.lock().expect("in_flight lock").clone()
    }

    /// 現在の commit 世代のみ（transcript 読み後の検算用）。
    pub fn commit_seq(&self) -> u64 {
        self.in_flight.lock().expect("in_flight lock").seq
    }

    /// engine プロセスの pid（spawn 直後に採取。LaneInfo.pid に載せる）。
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// ユーザープロンプトを engine に投入する（1 ターン開始）。
    ///
    /// `&self`: 内部 Mutex で stdin を直列化（LanePool read lock 下から呼べる）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        let json = user_message_json(prompt);
        let mut stdin = self.stdin.lock().await;
        write_line(&mut stdin, &json).await
    }

    /// 逆方向 can_use_tool（質問/承認）への回答を control_response で書き戻す（doc 35 §2.3）。
    ///
    /// pending から原 input を引き、`updatedInput` を原 input + answers でエコー再構築する。
    /// `&self`: 内部 Mutex で stdin を直列化（submit と同じく LanePool read lock 下から呼べる）。
    pub async fn respond_permission(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> anyhow::Result<()> {
        let pending = self
            .pending
            .lock()
            .expect("pending lock")
            .remove(request_id);
        let frame = build_control_response(request_id, pending.as_ref(), &decision);
        let mut stdin = self.stdin.lock().await;
        write_line(&mut stdin, &frame).await
    }

    /// engine プロセスを停止する（drop でも kill_on_drop が効くが、明示停止用）。
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        self.child.kill().await?;
        Ok(())
    }
}

/// stdin 用 user メッセージ JSON を生成する（純関数）。
///
/// 形式: `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`
fn user_message_json(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
    .to_string()
}

/// SessionInit で観測した session id を cc_session に記録する（resume の SSOT）。
fn record_session(project: &str, lane: &str, session_id: &str) {
    if let Err(e) = crate::lane::cc_session::record(project, lane, session_id) {
        tracing::warn!("cc_session 記録失敗（project={project}, lane={lane}）: {e}");
    }
}

/// stdin へ 1 JSON 行を書いて flush する（submit / control frame 共通の action）。
async fn write_line(stdin: &mut tokio::process::ChildStdin, line: &str) -> anyhow::Result<()> {
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

/// initialize handshake の control_request JSON（doc 35 §2.2、純関数）。
fn control_initialize_json(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "initialize", "hooks": serde_json::Value::Null }
    })
    .to_string()
}

/// 逆方向 can_use_tool への回答（doc 35 §2.3）。
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    /// AskUserQuestion の回答: answers を原 input に合流して allow で返す。
    Answer(serde_json::Value),
    /// tool 承認 allow（PR3）: 原 input のまま allow。
    Allow,
    /// 却下: deny + 理由。
    Deny { message: String },
}

/// pending 中の逆方向 can_use_tool（回答時のエコー再構築に使う）。
#[derive(Debug, Clone)]
struct PendingPermission {
    /// 承認対象 tool 名（PR3 の PermissionRequest 分岐で使用予定）。
    #[allow(dead_code)]
    tool_name: String,
    /// 原 input。updatedInput のエコー再構築に使う。
    input: serde_json::Value,
}

/// control_response JSON を組み立てる（純関数、doc 35 §2.3 の wire）。
///
/// `pending` は原 input（AskUserQuestion なら `{questions:[...]}`）。answers を合流して
/// `updatedInput` を忠実にエコーする（typed subset からの再構築で field 欠損しないため）。
fn build_control_response(
    request_id: &str,
    pending: Option<&PendingPermission>,
    decision: &PermissionDecision,
) -> String {
    let response = match decision {
        PermissionDecision::Answer(answers) => {
            let mut updated = pending
                .map(|p| p.input.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(obj) = updated.as_object_mut() {
                obj.insert("answers".to_string(), answers.clone());
            }
            serde_json::json!({ "behavior": "allow", "updatedInput": updated })
        }
        PermissionDecision::Allow => {
            let updated = pending
                .map(|p| p.input.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            serde_json::json!({ "behavior": "allow", "updatedInput": updated })
        }
        PermissionDecision::Deny { message } => {
            serde_json::json!({ "behavior": "deny", "message": message })
        }
    };
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        }
    })
    .to_string()
}

/// stdout 1 行の control protocol frame 分類（doc 35 §2.4）。
enum ControlFrame {
    /// 逆方向 can_use_tool（質問/承認要求）。
    CanUseTool {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    /// 我々の能動 control（initialize/interrupt/set_permission_mode）への応答。
    Response,
}

/// 行が control frame なら分類、そうでなければ None（= translator へ素通し）。純関数。
fn classify_control_frame(line: &str) -> Option<ControlFrame> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("type").and_then(|t| t.as_str())? {
        "control_request" => {
            let req = v.get("request")?;
            if req.get("subtype").and_then(|s| s.as_str())? != "can_use_tool" {
                // 想定外 subtype の control_request は translator 素通し（serde(other) が握る）。
                return None;
            }
            Some(ControlFrame::CanUseTool {
                request_id: v.get("request_id")?.as_str()?.to_string(),
                tool_name: req.get("tool_name")?.as_str()?.to_string(),
                input: req.get("input").cloned().unwrap_or(serde_json::Value::Null),
            })
        }
        "control_response" => Some(ControlFrame::Response),
        _ => None,
    }
}

/// can_use_tool を EchoesEvent に翻訳（doc 35 §2.4）。純関数。
///
/// PR1 は AskUserQuestion のみ Question 化。非 AskUserQuestion（Write/Bash 等の承認）は PR3 で
/// PermissionRequest として扱う予定 — bypassPermissions 下では飛ばない想定なので、PR1 では来たら
/// Error で可視化する（engine を無応答で放置しない配線は PR3 で入れる）。
fn can_use_tool_to_event(
    request_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
) -> EchoesEvent {
    if tool_name == "AskUserQuestion" {
        let raw = input
            .get("questions")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match serde_json::from_value::<Vec<QuestionSpec>>(raw) {
            Ok(questions) => EchoesEvent::Question {
                request_id: request_id.to_string(),
                questions,
            },
            Err(e) => EchoesEvent::Error {
                message: format!("AskUserQuestion の解釈に失敗: {e}"),
            },
        }
    } else {
        EchoesEvent::Error {
            message: format!("未対応の承認要求 tool={tool_name}（permission 面は PR3 で対応）"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// stream-json の行列を translator + fold_in_flight に通し、最終 tail を得る helper。
    /// 実 engine の stdout ポンプと同じ順序で畳む。
    fn tail_after(lines: &[&str]) -> InFlight {
        let mut translator = EchoesTranslator::new();
        let mut f = InFlight::default();
        for line in lines {
            fold_in_flight(&mut f, &translator.ingest(line));
        }
        f
    }

    fn text_delta(text: &str) -> String {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}}}"#
        )
    }

    const TEXT_BLOCK_START: &str = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
    const ASSISTANT_SNAPSHOT: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"完成"}]},"session_id":"s"}"#;

    /// 生成中（commit 前）の delta は tail に積まれる = replay で継げる。
    #[test]
    fn in_flight_tail_accumulates_uncommitted_text() {
        let f = tail_after(&[TEXT_BLOCK_START, &text_delta("こん"), &text_delta("にちは")]);
        assert_eq!(
            f.tail,
            vec![
                EchoesEvent::MessageChunk {
                    text: "こん".into()
                },
                EchoesEvent::MessageChunk {
                    text: "にちは".into()
                },
            ]
        );
    }

    /// commit 境界（assistant スナップショット）で tail は捨てられ、世代が進む。
    /// disk に載った内容を tail が二重に持たないことの担保。
    #[test]
    fn commit_clears_tail_and_bumps_seq() {
        let f = tail_after(&[TEXT_BLOCK_START, &text_delta("完成"), ASSISTANT_SNAPSHOT]);
        assert_eq!(f.tail, Vec::new(), "commit 済み text は tail に残さない");
        assert_eq!(f.seq, 1, "commit 世代が進む");
    }

    /// commit 後に始まった次 block の delta だけが tail に残る（transcript の続きになる）。
    #[test]
    fn tail_holds_only_chunks_after_last_commit() {
        let f = tail_after(&[
            TEXT_BLOCK_START,
            &text_delta("前"),
            ASSISTANT_SNAPSHOT,
            TEXT_BLOCK_START,
            &text_delta("後"),
        ]);
        assert_eq!(
            f.tail,
            vec![EchoesEvent::MessageChunk { text: "後".into() }]
        );
    }

    /// 回帰: ToolCall は tail に載らない。 translator は `content_block_stop` で ToolCall を出すが、
    /// その block の commit（snapshot 行）は 1 行前に来ているので transcript 側に既に居る。
    /// tail にも積むと replay で tool カードが二重化する。
    #[test]
    fn tool_call_is_never_buffered_in_tail() {
        let f = tail_after(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu-1","name":"Bash","input":{}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}}"#,
            // 実測順: snapshot(commit) → content_block_stop(ToolCall 発火)
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu-1","name":"Bash","input":{"command":"ls"}}]},"session_id":"s"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        ]);
        assert_eq!(f.tail, Vec::new(), "ToolCall は tail 対象外");
    }

    /// tool_result（user 行）も commit 境界。 ToolCallUpdate は tail に積まない。
    #[test]
    fn tool_result_commits_and_is_not_buffered() {
        let f = tail_after(&[
            TEXT_BLOCK_START,
            &text_delta("途中"),
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"ok","is_error":false}]}}"#,
        ]);
        assert_eq!(f.tail, Vec::new());
        assert_eq!(f.seq, 1);
    }

    /// turn 完了 / engine error / 新 session は tail を捨てる（会話が確定 or 起点リセット）。
    #[test]
    fn terminal_events_reset_tail() {
        for last in [
            r#"{"type":"result","subtype":"success","session_id":"s","is_error":false}"#,
            r#"{"type":"result","subtype":"error","session_id":"s","is_error":true,"result":"boom"}"#,
            r#"{"type":"system","subtype":"init","session_id":"s2","cwd":"/tmp"}"#,
        ] {
            let f = tail_after(&[TEXT_BLOCK_START, &text_delta("途中"), last]);
            assert_eq!(f.tail, Vec::new(), "tail が残っている: {last}");
            assert!(f.seq > 0, "世代が進む: {last}");
        }
    }

    #[test]
    fn user_message_json_is_valid_stream_json() {
        let json = user_message_json("こんにちは");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["type"], "text");
        assert_eq!(v["message"]["content"][0]["text"], "こんにちは");
    }

    #[test]
    fn user_message_json_escapes_quotes_and_newlines() {
        // quote / 改行 / バックスラッシュを含むプロンプトでも valid JSON 1 行になる。
        let tricky = "say \"hi\"\nnext\tline \\ end";
        let json = user_message_json(tricky);
        assert!(
            !json.contains('\n'),
            "serialized JSON は 1 行（生改行なし）"
        );
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["message"]["content"][0]["text"], tricky);
    }

    // --- doc 35: control protocol client（質問レール PR1）---------------------------

    #[test]
    fn classify_control_frame_detects_can_use_tool() {
        let line = r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[]}}}"#;
        match classify_control_frame(line) {
            Some(ControlFrame::CanUseTool {
                request_id,
                tool_name,
                ..
            }) => {
                assert_eq!(request_id, "r1");
                assert_eq!(tool_name, "AskUserQuestion");
            }
            _ => panic!("can_use_tool を検出できず"),
        }
    }

    #[test]
    fn classify_control_frame_detects_response_and_ignores_others() {
        let resp =
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1"}}"#;
        assert!(matches!(
            classify_control_frame(resp),
            Some(ControlFrame::Response)
        ));
        // 通常の stream イベントや非 JSON は control frame ではない（translator へ素通し）。
        assert!(classify_control_frame(TEXT_BLOCK_START).is_none());
        assert!(classify_control_frame("not json").is_none());
        // 想定外 subtype の control_request も None（素通し）。
        let other =
            r#"{"type":"control_request","request_id":"x","request":{"subtype":"mcp_message"}}"#;
        assert!(classify_control_frame(other).is_none());
    }

    #[test]
    fn can_use_tool_askuserquestion_maps_to_question() {
        let input = serde_json::json!({
            "questions": [{
                "question": "Which language?",
                "header": "Language",
                "options": [
                    {"label": "English", "description": "en"},
                    {"label": "Japanese", "description": "ja"}
                ],
                "multiSelect": false
            }]
        });
        match can_use_tool_to_event("r2", "AskUserQuestion", &input) {
            EchoesEvent::Question {
                request_id,
                questions,
            } => {
                assert_eq!(request_id, "r2");
                assert_eq!(questions.len(), 1);
                assert_eq!(questions[0].question, "Which language?");
                assert_eq!(questions[0].options.len(), 2);
                assert_eq!(questions[0].options[0].label, "English");
                assert!(!questions[0].multi_select);
            }
            other => panic!("Question を期待: {other:?}"),
        }
    }

    #[test]
    fn can_use_tool_non_question_is_error_in_pr1() {
        // PR1 は AskUserQuestion 以外の承認要求を Error で可視化（permission 面は PR3）。
        match can_use_tool_to_event("r5", "Bash", &serde_json::json!({"command": "ls"})) {
            EchoesEvent::Error { message } => assert!(message.contains("Bash")),
            other => panic!("Error を期待: {other:?}"),
        }
    }

    #[test]
    fn build_control_response_answer_merges_updated_input() {
        let pending = PendingPermission {
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::json!({"questions": [{"question": "Q"}]}),
        };
        let answers = serde_json::json!({"Q": "A"});
        let frame =
            build_control_response("r3", Some(&pending), &PermissionDecision::Answer(answers));
        let v: serde_json::Value = serde_json::from_str(&frame).expect("valid json");
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "r3");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        // updatedInput は原 questions を保ちつつ answers を合流（エコー忠実性）。
        assert_eq!(
            v["response"]["response"]["updatedInput"]["answers"]["Q"],
            "A"
        );
        assert!(v["response"]["response"]["updatedInput"]["questions"].is_array());
    }

    #[test]
    fn build_control_response_deny_carries_message() {
        let frame = build_control_response(
            "r4",
            None,
            &PermissionDecision::Deny {
                message: "no".to_string(),
            },
        );
        let v: serde_json::Value = serde_json::from_str(&frame).expect("valid json");
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["response"]["message"], "no");
    }

    #[test]
    fn control_initialize_json_is_wellformed() {
        let v: serde_json::Value =
            serde_json::from_str(&control_initialize_json("init-1")).expect("valid json");
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "init-1");
        assert_eq!(v["request"]["subtype"], "initialize");
    }

    /// 実機統合: headless claude を spawn → submit → EchoesEvent 列を受け取り、
    /// SessionInit / MessageChunk(PONG) / TurnCompleted が揃うことを確認する。
    /// `cargo test -p vantage-point --ignored echoes_host_roundtrip` で実行（要 claude CLI）。
    #[tokio::test]
    #[ignore = "requires claude CLI + subscription"]
    async fn echoes_host_roundtrip() {
        use std::time::Duration;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut host = EchoesAgentHost::spawn(EchoesHostConfig {
            cwd: tmp.path().to_string_lossy().to_string(),
            project: "vp-test".to_string(),
            lane: "spike".to_string(),
            resume_session_id: None,
            model: Some("haiku".to_string()),
            claude_cli_path: None,
        })
        .expect("spawn host");

        let mut rx = host.subscribe();
        host.submit("Reply with exactly: PONG")
            .await
            .expect("submit");

        let mut got_init = false;
        let mut text = String::new();
        let mut got_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(90), rx.recv()).await {
                Ok(Ok(ev)) => match ev {
                    EchoesEvent::SessionInit { session_id, .. } => {
                        assert!(!session_id.is_empty());
                        got_init = true;
                    }
                    EchoesEvent::MessageChunk { text: t } => text.push_str(&t),
                    EchoesEvent::TurnCompleted { .. } => {
                        got_done = true;
                        break;
                    }
                    EchoesEvent::Error { message } => panic!("engine error: {message}"),
                    _ => {}
                },
                _ => break,
            }
        }
        host.stop().await.ok();

        assert!(got_init, "SessionInit を受信");
        assert!(got_done, "TurnCompleted を受信");
        assert!(
            text.to_uppercase().contains("PONG"),
            "本文に PONG を含む: {text:?}"
        );
    }
}
