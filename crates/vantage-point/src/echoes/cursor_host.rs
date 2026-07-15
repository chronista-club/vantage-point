//! CursorAgentHost — cursor-agent を lane 単位で **turn-scoped** に駆動する（Act II engine host）
//!
//! [`super::host::EchoesAgentHost`]（claude）と同じ外形（subscribe / in_flight / commit_seq /
//! pid / submit / interrupt / stop）を持つが、内部の駆動モデルが根本的に違う。
//!
//! ## なぜ turn-scoped か（設計判断）
//!
//! cursor-agent には claude の `--input-format stream-json` に相当する「常駐 stdin 連投」機構が
//! **無い**。よって EchoesAgentHost のような「1 プロセス常駐 + stdin にメッセージを流し込む」形は
//! 取れない。代わりに **1 turn = 1 プロセス**とし、submit のたびに
//! `cursor-agent -p "<prompt>" [--resume '<chatId>'] --output-format stream-json --stream-partial-output`
//! を spawn する。stdout を [`super::cursor_translate::CursorTranslator`] に通して [`EchoesEvent`] に
//! 翻訳し broadcast する。会話継続は `--resume '<chatId>'` が担い、chatId は毎 turn の `system/init`
//! から書き戻す（下記 record-from-init）。
//!
//! ## 偽 Error を出さない（claude host との差）
//!
//! EchoesAgentHost は「stdout close = engine 途絶」と見なして無条件に [`EchoesEvent::Error`] を
//! broadcast する（常駐プロセスなので stdout close = 異常）。cursor は turn ごとにプロセスが**正常に**
//! 終了するため、この論理をそのまま持ち込むと毎 turn 偽 Error が出る。よって cursor host は
//! `translator.saw_result()` が true（= 終端 `result` を観測 = 正常終端）なら **何もしない**。
//! false のときだけ（未ログイン / crash）Error を broadcast する。
//!
//! ## record-from-init（Act I ⇄ II 会話共有）
//!
//! `system/init` の session_id（= chatId）を [`crate::lane::cursor_session::record`] で永続する。
//! Act I（console）も同じ state file（`cursor_sessions/<project>__<lane>`）を読むため、II で進めた
//! 会話は I へ、I で進めた会話は II へ `--resume` で継がれる。
//!
//! ## interrupt = kill
//!
//! cursor には claude の control channel（`interrupt` control_request）が無い。よって中断は
//! 実行中の子プロセスの kill で行う（turn task を abort → `kill_on_drop` が子を殺す）。中断後は
//! [`EchoesEvent::TurnCompleted`] を broadcast して streaming を畳む（意図的中断はエラーではない
//! ので Error バブルは出さない）。
//!
//! ## nudge の意味論（走行中は queue）
//!
//! turn 実行中の submit は即実行できない（プロセスは 1 turn 1 個）。走行中の submit は queue に
//! 積み、現 turn 終了後に順次 spawn する（wire nudge の mid-turn 注入対応）。
//!
//! data / calculations / actions:
//! - calculations: stream 翻訳（[`super::cursor_translate`]）/ self-heal 条件（[`TurnOutcome::should_self_heal`]）
//!   / tail 結合（[`combine_tails`]）— 純粋。
//! - actions: プロセス spawn / stdout→translate→broadcast / chatId 記録 / in-flight tail 更新 / kill。

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::cursor_translate::CursorTranslator;
use super::event::EchoesEvent;
use super::host::InFlight;

/// CursorAgentHost の起動設定。
#[derive(Debug, Clone)]
pub struct CursorHostConfig {
    /// engine の作業ディレクトリ（lane の project dir）。chat の workspace 紐付けに効く。
    pub cwd: String,
    /// cursor_session 記録キー（project 名）。
    pub project: String,
    /// cursor_session 記録キー（lane label: conductor / performer 名）。
    pub lane: String,
    /// 再開する chatId（`--resume`）。Act I ⇄ II 共有の state file から解決した値。
    pub chat_id: Option<String>,
}

/// turn task と host が共有する内部状態（std Mutex — await を跨がずに触る）。
struct CursorState {
    /// 現在の chatId（`system/init` で更新 = record-from-init）。次 turn の `--resume` に使う。
    chat_id: Option<String>,
    /// disk にまだ載っていない増分 + commit 世代（EchoesAgentHost と同型の外形維持用）。
    /// cursor は cc_session を持たず transcript replay も通らない（demand_start は no_session path）
    /// ため実際には消費されないが、in_flight() / commit_seq() の契約を満たすために維持する。
    in_flight: InFlight,
    /// 実行中 turn の task handle（None = idle）。interrupt / stop は take + abort する。
    turn: Option<JoinHandle<()>>,
    /// 実行中の子プロセス pid（idle は None）。
    child_pid: Option<u32>,
    /// 走行中に来た submit の待ち行列（現 turn 終了後に順次 spawn）。
    queue: VecDeque<String>,
}

/// turn task / host メソッドが共有する不変部 + 状態。`Arc` で turn task へ clone する。
struct CursorInner {
    event_tx: broadcast::Sender<EchoesEvent>,
    cwd: String,
    project: String,
    lane: String,
    state: Mutex<CursorState>,
}

/// lane 単位の turn-scoped cursor-agent host。
pub struct CursorAgentHost {
    inner: Arc<CursorInner>,
}

/// 1 turn（= 1 プロセス）の実行結果サマリ（finish_turn / self-heal 判定の材料）。
struct TurnOutcome {
    /// 終端 `result` を観測したか（true = 正常終端 = translator が TurnCompleted/Error emit 済）。
    saw_result: bool,
    /// assistant/tool の実イベントを 1 つでも出したか。
    produced_content: bool,
    /// JSON でなかった stdout（未ログインエラー本文など）。
    plain_tail: String,
    /// stderr の tail（Error message の補材）。
    stderr_tail: String,
    /// プロセス spawn 自体に失敗した場合のメッセージ（cursor-agent 不在等）。
    spawn_error: Option<String>,
}

impl TurnOutcome {
    fn spawn_error(msg: String) -> Self {
        Self {
            saw_result: false,
            produced_content: false,
            plain_tail: String::new(),
            stderr_tail: String::new(),
            spawn_error: Some(msg),
        }
    }

    /// resume 失敗の self-heal を発火すべきか（純粋）。
    ///
    /// `--resume` 付きで走らせて、正常終端（result）も実イベントも無かった = chatId が cursor 側で
    /// 消えて resume が空振りした可能性が高い。spawn 失敗（binary 不在）は再試行しても無駄なので除く。
    fn should_self_heal(&self) -> bool {
        self.spawn_error.is_none() && !self.saw_result && !self.produced_content
    }
}

impl CursorAgentHost {
    /// host を用意する（**プロセスは起動しない** — ensure_chat_engine を exec-free に保つ）。
    ///
    /// 実プロセスは初回 [`Self::submit`] で spawn される（turn-scoped）。
    pub fn spawn(config: CursorHostConfig) -> Self {
        // broadcast 容量は EchoesAgentHost に倣う（256）。
        let (event_tx, _rx) = broadcast::channel::<EchoesEvent>(256);
        let inner = Arc::new(CursorInner {
            event_tx,
            cwd: config.cwd,
            project: config.project,
            lane: config.lane,
            state: Mutex::new(CursorState {
                chat_id: config.chat_id,
                in_flight: InFlight::default(),
                turn: None,
                child_pid: None,
                queue: VecDeque::new(),
            }),
        });
        tracing::info!(
            "CursorAgentHost ready (project={}, lane={}, resume={:?})",
            inner.project,
            inner.lane,
            inner
                .state
                .lock()
                .expect("cursor state lock")
                .chat_id
                .as_deref()
                .unwrap_or("new")
        );
        Self { inner }
    }

    /// EchoesEvent の broadcast receiver を得る（echoes_pump が購読）。
    pub fn subscribe(&self) -> broadcast::Receiver<EchoesEvent> {
        self.inner.event_tx.subscribe()
    }

    /// disk にまだ載っていない増分 + commit 世代（EchoesAgentHost と同契約、外形維持用）。
    pub fn in_flight(&self) -> InFlight {
        self.inner
            .state
            .lock()
            .expect("cursor state lock")
            .in_flight
            .clone()
    }

    /// commit 世代のみ。
    pub fn commit_seq(&self) -> u64 {
        self.inner
            .state
            .lock()
            .expect("cursor state lock")
            .in_flight
            .seq
    }

    /// 実行中 turn の子プロセス pid（idle は None）。
    pub fn pid(&self) -> Option<u32> {
        self.inner
            .state
            .lock()
            .expect("cursor state lock")
            .child_pid
    }

    /// ユーザープロンプトを投入する（1 ターン開始 or queue 追加）。
    ///
    /// turn 実行中なら queue に積んで即 return（mid-turn の wire nudge 対応）。idle なら turn task を
    /// spawn する。`&self`: 内部 std Mutex で状態を直列化（LanePool read lock 下から呼べる）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        let mut st = self.inner.state.lock().expect("cursor state lock");
        if st.turn.is_some() {
            st.queue.push_back(prompt.to_string());
            tracing::debug!(
                "cursor submit: turn 実行中 → queue に格納（project={}, lane={}, depth={}）",
                self.inner.project,
                self.inner.lane,
                st.queue.len()
            );
            return Ok(());
        }
        self.inner.spawn_turn(&mut st, prompt.to_string());
        Ok(())
    }

    /// 実行中 turn を中断する（cursor に control channel は無いので子プロセス kill）。
    ///
    /// turn task を abort（→ `kill_on_drop` が子を殺す）+ queue クリア。意図的中断はエラーではない
    /// ので、streaming を畳むために [`EchoesEvent::TurnCompleted`] を broadcast する（Error は出さない）。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        let (handle, chat_id) = {
            let mut st = self.inner.state.lock().expect("cursor state lock");
            st.queue.clear();
            st.child_pid = None;
            st.in_flight.tail.clear();
            st.in_flight.seq = st.in_flight.seq.wrapping_add(1);
            (st.turn.take(), st.chat_id.clone())
        };
        if let Some(handle) = handle {
            handle.abort();
            let _ = self.inner.event_tx.send(EchoesEvent::TurnCompleted {
                session_id: chat_id.unwrap_or_default(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            });
            tracing::info!(
                "cursor interrupt: turn kill（project={}, lane={}）",
                self.inner.project,
                self.inner.lane
            );
        }
        Ok(())
    }

    /// engine を停止する（実行中 turn kill + queue クリア）。Drop 相当の明示 teardown。
    pub fn stop(&mut self) {
        self.inner.abort_turn();
    }
}

impl Drop for CursorAgentHost {
    fn drop(&mut self) {
        // 実行中 turn task は Arc<CursorInner> を握って生き続けるため、host drop だけでは子が
        // orphan になる。明示 abort で kill_on_drop を効かせる（stop() の安全網）。
        self.inner.abort_turn();
    }
}

impl CursorInner {
    /// 次の turn を spawn し、handle を state に格納する（状態 lock 保持下で呼ぶ）。
    fn spawn_turn(self: &Arc<Self>, st: &mut CursorState, prompt: String) {
        let chat_id = st.chat_id.clone();
        let inner = Arc::clone(self);
        let handle = tokio::spawn(async move {
            inner.run_turn(prompt, chat_id).await;
        });
        st.turn = Some(handle);
    }

    /// 1 turn を最後まで駆動する（resume → 必要なら self-heal 再実行 → 終端処理）。
    async fn run_turn(self: Arc<Self>, prompt: String, chat_id: Option<String>) {
        let outcome = self.run_once(&prompt, chat_id.as_deref()).await;
        // resume 失敗の self-heal 1 回: chatId を破棄し resume 無しで同一 prompt を再実行
        //（Act I の `|| cursor-agent` fallback と同じ意味論）。
        let outcome = if chat_id.is_some() && outcome.should_self_heal() {
            tracing::info!(
                "cursor turn: resume 空振りと判断 → chatId 破棄して resume 無しで再実行（project={}, lane={}）",
                self.project,
                self.lane
            );
            let _ = crate::lane::cursor_session::clear(&self.project, &self.lane);
            self.state.lock().expect("cursor state lock").chat_id = None;
            self.run_once(&prompt, None).await
        } else {
            outcome
        };
        self.finish_turn(outcome);
    }

    /// cursor-agent を 1 プロセス spawn し、stdout を翻訳→broadcast しながら終了まで待つ。
    async fn run_once(&self, prompt: &str, resume: Option<&str>) -> TurnOutcome {
        let program = crate::lane::cursor_session::cursor_cli_path();
        let mut cmd = tokio::process::Command::new(&program);
        // prompt は positional arg（Command の arg 渡し = shell 非経由なので injection 安全）。
        cmd.arg("-p")
            .arg(prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--stream-partial-output");
        if let Some(id) = resume {
            cmd.arg("--resume").arg(id);
        }
        if !self.cwd.is_empty() {
            cmd.current_dir(&self.cwd);
        }
        // 親（SP）の env を継承（spawn_env 済みの PATH 等）。
        cmd.envs(std::env::vars());
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // turn task が abort されたら子も殺す（interrupt / host drop の kill 経路）。
        cmd.kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "cursor-agent の spawn に失敗（program={program}, project={}, lane={}）: {e}",
                    self.project,
                    self.lane
                );
                return TurnOutcome::spawn_error(e.to_string());
            }
        };
        self.state.lock().expect("cursor state lock").child_pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        // stderr は tail だけ別 task で drain（pipe buffer 詰まりでの deadlock 回避 + error 補材）。
        let stderr_task = stderr.map(|se| {
            tokio::spawn(async move {
                let mut buf = String::new();
                let mut lines = BufReader::new(se).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(&l);
                }
                buf
            })
        });

        let mut translator = CursorTranslator::new();
        if let Some(stdout) = stdout {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                for event in translator.ingest(&line) {
                    if let EchoesEvent::SessionInit { session_id, .. } = &event {
                        self.record_session(session_id);
                    }
                    self.fold_and_broadcast(event);
                }
            }
        }
        let _ = child.wait().await;
        self.state.lock().expect("cursor state lock").child_pid = None;
        let stderr_tail = match stderr_task {
            Some(t) => t.await.unwrap_or_default(),
            None => String::new(),
        };

        TurnOutcome {
            saw_result: translator.saw_result(),
            produced_content: translator.produced_content(),
            plain_tail: translator.plain_text_tail(),
            stderr_tail,
            spawn_error: None,
        }
    }

    /// turn 終端処理: 応答なし終了なら Error を出し、in-flight を reset して次 queued turn を回す。
    fn finish_turn(self: &Arc<Self>, outcome: TurnOutcome) {
        if let Some(err) = outcome.spawn_error {
            self.broadcast(EchoesEvent::Error {
                message: format!(
                    "cursor-agent の起動に失敗しました（PATH に cursor-agent があるか確認）: {err}"
                ),
            });
        } else if !outcome.saw_result {
            // saw_result==true なら translator が既に TurnCompleted/Error を emit 済 = 何もしない。
            // false = 未ログイン / crash 等で応答なく終了。ここで初めて Error を出す
            //（claude host の「stdout close = 無条件 Error」は turn-scoped では偽陽性なので持ち込まない）。
            let detail = combine_tails(&outcome.plain_tail, &outcome.stderr_tail);
            self.broadcast(EchoesEvent::Error {
                message: format!("cursor-agent が応答なく終了しました: {detail}"),
            });
        }

        let mut st = self.state.lock().expect("cursor state lock");
        st.in_flight.tail.clear();
        st.in_flight.seq = st.in_flight.seq.wrapping_add(1);
        st.child_pid = None;
        st.turn = None;
        if let Some(next) = st.queue.pop_front() {
            self.spawn_turn(&mut st, next);
        }
    }

    /// `system/init` の chatId を永続 + 内部状態へ反映（record-from-init）。
    fn record_session(&self, session_id: &str) {
        if let Err(e) = crate::lane::cursor_session::record(&self.project, &self.lane, session_id) {
            tracing::warn!(
                "cursor chatId 記録失敗（project={}, lane={}）: {e}",
                self.project,
                self.lane
            );
        }
        self.state.lock().expect("cursor state lock").chat_id = Some(session_id.to_string());
    }

    /// 翻訳結果を in-flight tail に畳んでから broadcast する（EchoesAgentHost の fold_in_flight 相当）。
    fn fold_and_broadcast(&self, event: EchoesEvent) {
        {
            let mut st = self.state.lock().expect("cursor state lock");
            match &event {
                EchoesEvent::MessageChunk { .. } | EchoesEvent::ThoughtChunk { .. } => {
                    st.in_flight.tail.push(event.clone());
                }
                EchoesEvent::SessionInit { .. }
                | EchoesEvent::TurnCompleted { .. }
                | EchoesEvent::Error { .. } => {
                    st.in_flight.tail.clear();
                    st.in_flight.seq = st.in_flight.seq.wrapping_add(1);
                }
                _ => {}
            }
        }
        // 受信者不在（Closed）は無視 — engine は購読者に依存せず動く。
        let _ = self.event_tx.send(event);
    }

    fn broadcast(&self, event: EchoesEvent) {
        let _ = self.event_tx.send(event);
    }

    /// 実行中 turn を kill + queue クリア（stop / Drop 共用）。
    fn abort_turn(&self) {
        let handle = {
            let mut st = self.state.lock().expect("cursor state lock");
            st.queue.clear();
            st.child_pid = None;
            st.turn.take()
        };
        if let Some(h) = handle {
            h.abort();
        }
    }
}

/// plain（stdout 非 JSON）と stderr の tail を Error message 用に結合する（純関数）。
fn combine_tails(plain: &str, stderr: &str) -> String {
    let plain = plain.trim();
    let stderr = stderr.trim();
    match (plain.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{plain} / stderr: {stderr}"),
        (false, true) => plain.to_string(),
        (true, false) => format!("stderr: {stderr}"),
        (true, true) => "(出力なし)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_tails_prefers_plain_and_annotates_stderr() {
        assert_eq!(combine_tails("boom", ""), "boom");
        assert_eq!(combine_tails("", "err line"), "stderr: err line");
        assert_eq!(combine_tails("plain", "err"), "plain / stderr: err");
        assert_eq!(combine_tails("  ", "\n"), "(出力なし)");
    }

    #[test]
    fn should_self_heal_only_when_resume_produced_nothing() {
        // 正常終端 → self-heal しない。
        let ok = TurnOutcome {
            saw_result: true,
            produced_content: true,
            plain_tail: String::new(),
            stderr_tail: String::new(),
            spawn_error: None,
        };
        assert!(!ok.should_self_heal());

        // result も content も無い → self-heal 候補。
        let empty = TurnOutcome {
            saw_result: false,
            produced_content: false,
            plain_tail: "no chat".into(),
            stderr_tail: String::new(),
            spawn_error: None,
        };
        assert!(empty.should_self_heal());

        // spawn 失敗は再試行無駄 → self-heal しない。
        let spawn_err = TurnOutcome::spawn_error("not found".into());
        assert!(!spawn_err.should_self_heal());

        // content は出たが result 未観測（途中で切れた）→ self-heal しない（重複防止）。
        let partial = TurnOutcome {
            saw_result: false,
            produced_content: true,
            plain_tail: String::new(),
            stderr_tail: String::new(),
            spawn_error: None,
        };
        assert!(!partial.should_self_heal());
    }
}
