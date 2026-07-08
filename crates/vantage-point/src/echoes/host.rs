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
//! data / calculations / actions:
//! - calculations: stdin メッセージ JSON 生成（[`user_message_json`]、純関数）
//! - actions: spawn / stdout→translate→broadcast / cc_session 記録 / stdin 書き込み

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;

use super::event::EchoesEvent;
use super::translate::EchoesTranslator;

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
    stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
    event_tx: broadcast::Sender<EchoesEvent>,
    pid: Option<u32>,
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

        // MVP は acceptEdits で auto-apply（--permission-prompt-tool は 2.1.197 で削除）。
        cmd.arg("--permission-mode").arg("acceptEdits");

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

        // stdout ポンプ: 行 → translate → broadcast（+ SessionInit で cc_session 記録）。
        let tx = event_tx.clone();
        let project = config.project.clone();
        let lane = config.lane.clone();
        tokio::spawn(async move {
            let mut translator = EchoesTranslator::new();
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                for event in translator.ingest(&line) {
                    if let EchoesEvent::SessionInit { session_id, .. } = &event {
                        record_session(&project, &lane, session_id);
                    }
                    // 受信者不在（Closed）は無視 — engine は購読者に依存せず動く。
                    let _ = tx.send(event);
                }
            }
            tracing::debug!("EchoesAgentHost stdout ポンプ終了（project={project}, lane={lane}）");
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
        Ok(Self {
            child,
            stdin: tokio::sync::Mutex::new(stdin),
            event_tx,
            pid,
        })
    }

    /// EchoesEvent の broadcast receiver を得る（echoes_pump などが購読）。
    pub fn subscribe(&self) -> broadcast::Receiver<EchoesEvent> {
        self.event_tx.subscribe()
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
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

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
