//! ProcessRunner — 汎用プロセスライフサイクルマネージャー
//!
//! 任意のコマンドを子プロセスとして spawn し、stdout/stderr を
//! Canvas にストリーミングする。stdin 経由でコード注入（inject）可能。
//!
//! ## モード
//! - **Managed**: tokio::process::Command で直接管理
//! - **Tmux**: tmux split-window + capture-pane（将来追加）
//!
//! ## ホットインジェクション
//! Ruby / Bun 等の REPL ランタイムに対して、実行中のプロセスに
//! stdin 経由でコードを注入し、プロセスを止めずに機能を拡張できる。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::hub::Hub;
use crate::protocol::RepoMessage;

/// プロセスの状態
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Completed { exit_code: Option<i32> },
    Failed { error: String },
}

/// 実行中のプロセス情報（外部公開用）
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub process_id: String,
    pub name: String,
    pub command: String,
    pub pane_id: String,
    pub status: ProcessStatus,
    pub started_at: u64,
}

/// プロセス起動パラメータ
#[derive(Debug, Deserialize)]
pub struct RunParams {
    /// 実行コマンド（例: "ruby", "bun", "cargo test"）
    pub command: String,
    /// コマンド引数
    #[serde(default)]
    pub args: Vec<String>,
    /// 表示名（省略時はコマンド名）
    pub name: Option<String>,
    /// Canvas 出力先ペイン
    pub pane_id: Option<String>,
    /// 作業ディレクトリ（省略時はrepoディレクトリ）
    pub working_dir: Option<String>,
    /// stdin に最初に流すブートストラップコード
    pub bootstrap: Option<String>,
}

/// 短命実行パラメータ
#[derive(Debug, Deserialize)]
pub struct RunEvalParams {
    /// 実行コマンド
    pub command: String,
    /// コマンド引数
    #[serde(default)]
    pub args: Vec<String>,
    /// Canvas 出力先ペイン
    pub pane_id: Option<String>,
    /// 作業ディレクトリ
    pub working_dir: Option<String>,
}

/// コード注入パラメータ
#[derive(Debug, Deserialize)]
pub struct InjectParams {
    /// 対象プロセスID
    pub process_id: String,
    /// 注入するコード
    pub code: String,
}

/// 短命実行結果
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
}

/// 内部管理用のプロセスエントリ
struct ProcessEntry {
    info: ProcessInfo,
    /// Graceful shutdown シグナル
    shutdown_tx: Option<mpsc::Sender<()>>,
    /// stdin 注入チャネル
    inject_tx: Option<mpsc::Sender<String>>,
    /// 完了時刻（クリーンアップ判定用）
    completed_at: Option<std::time::Instant>,
    /// 終了の確認（棚卸し 9-2 段階 3）。`update_status` が Completed / Failed に遷移させた時に
    /// `true` を送る。`stop_all` はこれを待って「停止を要求した仕事の終わり」を確認する。
    done_tx: tokio::sync::watch::Sender<bool>,
    done_rx: tokio::sync::watch::Receiver<bool>,
}

/// プロセスレジストリ
pub struct ProcessRegistry {
    processes: HashMap<String, ProcessEntry>,
    counter: u32,
    /// 受付を閉じたか（棚卸し 9-2 段階 3、停止契約 ①）。`stop_all` が立て、以後 `register` は
    /// 拒む。repo stop の後に in-flight の `process_run` が子を spawn しても、登録できずに
    /// kill されるので「畳んだはずの repo の runner が生き残る」窓が閉じる。
    closing: bool,
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessRegistry {
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            counter: 0,
            closing: false,
        }
    }

    /// 新しいプロセスIDを生成
    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("proc-{:04}", self.counter)
    }

    /// 実行中プロセス一覧
    pub fn list(&self) -> Vec<ProcessInfo> {
        self.processes.values().map(|e| e.info.clone()).collect()
    }

    /// プロセスを登録
    fn register(
        &mut self,
        process_id: String,
        name: String,
        command: String,
        pane_id: String,
        shutdown_tx: mpsc::Sender<()>,
        inject_tx: mpsc::Sender<String>,
    ) -> Result<(), String> {
        if self.closing {
            return Err("process registry は停止中（repo stop 後の起動は拒む）".to_string());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);

        self.processes.insert(
            process_id.clone(),
            ProcessEntry {
                info: ProcessInfo {
                    process_id,
                    name,
                    command,
                    pane_id,
                    status: ProcessStatus::Running,
                    started_at: now,
                },
                shutdown_tx: Some(shutdown_tx),
                inject_tx: Some(inject_tx),
                completed_at: None,
                done_tx,
                done_rx,
            },
        );
        Ok(())
    }

    /// プロセス状態を更新
    pub fn update_status(&mut self, process_id: &str, status: ProcessStatus) {
        if let Some(entry) = self.processes.get_mut(process_id) {
            // 完了・失敗への遷移時に完了時刻を記録
            match &status {
                ProcessStatus::Completed { .. } | ProcessStatus::Failed { .. } => {
                    entry.completed_at = Some(std::time::Instant::now());
                }
                _ => {}
            }
            let finished = matches!(
                status,
                ProcessStatus::Completed { .. } | ProcessStatus::Failed { .. }
            );
            entry.info.status = status;
            entry.shutdown_tx = None;
            entry.inject_tx = None;
            if finished {
                let _ = entry.done_tx.send(true);
            }
        }
    }

    /// 受付を閉じ、走っている runner の停止手段を**lock の外で使う形**で取り出す
    /// （棚卸し 9-2 段階 3、停止契約 ②）。返り値 = (process_id, shutdown 送信口, 終了通知)。
    ///
    /// ここで送信も待機もしない — runner の終了処理（`stream_output`）は `update_status` で
    /// この registry の lock を取るので、lock を握ったまま待つと行き詰まる。
    fn close_and_take_stop_handles(
        &mut self,
    ) -> Vec<(String, mpsc::Sender<()>, tokio::sync::watch::Receiver<bool>)> {
        self.closing = true;
        self.processes
            .iter()
            .filter_map(|(id, e)| {
                e.shutdown_tx
                    .as_ref()
                    .map(|tx| (id.clone(), tx.clone(), e.done_rx.clone()))
            })
            .collect()
    }

    /// 完了済みエントリのうち、指定秒数以上経過したものを削除
    pub fn cleanup_completed(&mut self, max_age_secs: u64) {
        let now = std::time::Instant::now();
        self.processes.retain(|_id, entry| {
            match &entry.info.status {
                ProcessStatus::Completed { .. } | ProcessStatus::Failed { .. } => entry
                    .completed_at
                    .map(|t| now.duration_since(t).as_secs() < max_age_secs)
                    .unwrap_or(true),
                _ => true, // Running 等は常に残す
            }
        });
    }

    /// Graceful shutdown シグナルを送信
    pub async fn send_shutdown(&self, process_id: &str) -> bool {
        if let Some(entry) = self.processes.get(process_id)
            && let Some(tx) = &entry.shutdown_tx
        {
            return tx.send(()).await.is_ok();
        }
        false
    }

    /// stdin にコードを注入
    pub async fn inject(&self, process_id: &str, code: &str) -> Result<(), String> {
        let entry = self
            .processes
            .get(process_id)
            .ok_or_else(|| format!("プロセス {} が見つかりません", process_id))?;

        let tx = entry
            .inject_tx
            .as_ref()
            .ok_or_else(|| format!("プロセス {} は inject を受け付けていません", process_id))?;

        tx.send(code.to_string())
            .await
            .map_err(|_| format!("プロセス {} への inject に失敗しました", process_id))
    }
}

/// コマンドを即座に実行（短命）
pub async fn process_run_eval(
    params: &RunEvalParams,
    repo_dir: &str,
    hub: &Hub,
) -> Result<RunResult, String> {
    let start = std::time::Instant::now();
    let pane_id = params.pane_id.as_deref().unwrap_or("main");
    let work_dir = params.working_dir.as_deref().unwrap_or(repo_dir);

    let output = Command::new(&params.command)
        .args(&params.args)
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| format!("コマンド実行失敗: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Canvas に結果を表示
    let display = format_output_html(&stdout, &stderr, output.status.code());
    hub.broadcast(RepoMessage::Show {
        pane_id: pane_id.to_string(),
        content: crate::protocol::Content::Html(display),
        append: false,
        title: Some(params.command.clone()),
        // process runner 出力は Main lane の Canvas 向け
        lane: None,
        scope: None,
    });

    Ok(RunResult {
        stdout,
        stderr,
        exit_code: output.status.code(),
        elapsed_ms,
    })
}

/// プロセスを起動（長期稼働）
pub async fn process_run(
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
    params: &RunParams,
    repo_dir: &str,
    hub: &Hub,
) -> Result<String, String> {
    // 新プロセス起動前に完了済みエントリを掃除（5分経過分）
    registry.lock().await.cleanup_completed(300);

    let pane_id = params.pane_id.as_deref().unwrap_or("main");
    let work_dir = params.working_dir.as_deref().unwrap_or(repo_dir);

    // プロセス起動
    let mut child = Command::new(&params.command)
        .args(&params.args)
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("プロセス起動失敗: {}", e))?;

    let process_id = registry.lock().await.next_id();
    let display_name = params
        .name
        .as_deref()
        .unwrap_or(&params.command)
        .to_string();
    let command_display = if params.args.is_empty() {
        params.command.clone()
    } else {
        format!("{} {}", params.command, params.args.join(" "))
    };

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
    let (inject_tx, inject_rx) = mpsc::channel::<String>(64);

    // `let` で受けて guard を先に落とす — `if let` の scrutinee にすると then-block の
    // `kill().await` の間も registry の lock を握る（停止契約 ② に反する）。
    let registered = registry.lock().await.register(
        process_id.clone(),
        display_name,
        command_display,
        pane_id.to_string(),
        shutdown_tx,
        inject_tx,
    );
    if let Err(e) = registered {
        // 受付が閉じた後に滑り込んだ spawn（停止契約 ①）。登録できない子は畳んで返す
        let _ = child.kill().await;
        return Err(e);
    }

    // ブートストラップコードがあれば stdin に送信
    let bootstrap = params.bootstrap.clone();

    // 出力ストリーミングタスクを起動
    let hub_clone = hub.clone();
    let pane_id_owned = pane_id.to_string();
    let process_id_clone = process_id.clone();
    let registry_clone = registry.clone();

    tokio::spawn(async move {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();

        stream_output(
            &mut child,
            stdout,
            stderr,
            stdin,
            shutdown_rx,
            inject_rx,
            bootstrap,
            &hub_clone,
            &pane_id_owned,
            &process_id_clone,
            &registry_clone,
        )
        .await;
    });

    Ok(process_id)
}

/// プロセスを停止
pub async fn process_stop(
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
    process_id: &str,
) -> Result<(), String> {
    let sent = registry.lock().await.send_shutdown(process_id).await;
    if !sent {
        return Err(format!(
            "プロセス {} が見つからないか、既に停止しています",
            process_id
        ));
    }
    Ok(())
}

/// 受付を閉じて走っている runner を全部止め、**終了まで確認する**（棚卸し 9-2 段階 3）。
///
/// repo stop（`shutdown_repo`）から呼ぶ。返り値 = 停止を要求した runner 数。
///
/// 停止契約（doc 63 §8）:
/// - ① 受付を閉じる — `closing` を立てるので、以後の `process_run` は登録できず子を kill する
/// - ② lock 内で停止対象を取り出し、lock 外で通知・終了待ち — `stream_output` の終了処理が
///   同じ lock を取るため
/// - ③ 親 task と中で作る task の両方 — 親（`stream_output`）は `child.wait()` の後、stdout /
///   stderr の pump を `abort()` してから `update_status` を呼ぶので、done を待てば両方が終わっている
///   （pipe の EOF だけに頼ると、孫が pipe を継いだ場合に pump が残る）
///
/// 終了待ちは runner ごとに `STOP_ALL_CONFIRM_TIMEOUT`。`stream_output` は shutdown 受信後
/// 5 秒で kill するので、それより長く取る。超えたら warn を出して次へ（daemon の graceful
/// shutdown を 1 つの runner に人質に取らせない）。
pub async fn stop_all(registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>) -> usize {
    let targets = registry.lock().await.close_and_take_stop_handles();
    let count = targets.len();
    for (id, tx, _) in &targets {
        if tx.send(()).await.is_err() {
            tracing::debug!("runner {id}: shutdown 送信先が既に閉じている（終了済み）");
        }
    }
    for (id, _, mut done) in targets {
        match tokio::time::timeout(STOP_ALL_CONFIRM_TIMEOUT, done.wait_for(|d| *d)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => tracing::debug!("runner {id}: entry が先に消えた（終了済み扱い）"),
            Err(_) => tracing::warn!(
                "runner {id}: {}s 以内に終了を確認できなかった（kill 済みのはず、registry は未更新）",
                STOP_ALL_CONFIRM_TIMEOUT.as_secs()
            ),
        }
    }
    count
}

/// `stop_all` が runner 1 つの終了を待つ上限。`stream_output` の shutdown → kill 猶予（5 秒）より長く。
const STOP_ALL_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// コードを注入
pub async fn process_inject(
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
    params: &InjectParams,
) -> Result<(), String> {
    registry
        .lock()
        .await
        .inject(&params.process_id, &params.code)
        .await
}

/// プロセスの出力をストリーミング + inject 受信
#[allow(clippy::too_many_arguments)]
async fn stream_output(
    child: &mut tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    mut stdin: Option<tokio::process::ChildStdin>,
    mut shutdown_rx: mpsc::Receiver<()>,
    mut inject_rx: mpsc::Receiver<String>,
    bootstrap: Option<String>,
    hub: &Hub,
    pane_id: &str,
    process_id: &str,
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
) {
    // ブートストラップコードを stdin に送信
    if let (Some(handle), Some(code)) = (&mut stdin, bootstrap) {
        if let Err(e) = handle.write_all(code.as_bytes()).await {
            tracing::warn!("ブートストラップ送信失敗: {}", e);
        }
        let _ = handle.write_all(b"\n").await;
    }

    // stdout/stderr を並行で読み取り
    let (line_tx, mut line_rx) = mpsc::channel::<(String, String)>(256);
    // pump の JoinHandle は親が保持する（停止契約 ③）。shutdown 経路では子を畳んだ後に abort する —
    // 子が孫に pipe を継がせていると write 端が閉じず `next_line` が返らないので、pipe の EOF だけに
    // 頼ると pump が残る。通常終了（全 sender drop = `line_rx` が None）では既に抜けているので no-op。
    let mut pumps: Vec<tokio::task::JoinHandle<()>> = Vec::with_capacity(2);

    if let Some(out) = stdout {
        let tx = line_tx.clone();
        pumps.push(tokio::spawn(async move {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send(("stdout".to_string(), line)).await.is_err() {
                    break;
                }
            }
        }));
    }

    if let Some(err) = stderr {
        let tx = line_tx.clone();
        pumps.push(tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send(("stderr".to_string(), line)).await.is_err() {
                    break;
                }
            }
        }));
    }
    drop(line_tx);

    loop {
        tokio::select! {
            // Shutdown シグナル
            _ = shutdown_rx.recv() => {
                // stdin を閉じてプロセスに EOF 通知
                drop(stdin.take());

                // タイムアウト後に強制 kill
                let exit_code = tokio::select! {
                    status = child.wait() => status.ok().and_then(|s| s.code()),
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                        let _ = child.kill().await;
                        Some(-9)
                    }
                };
                // 子が終わったので pump を回収してから「終了」を registry に流す（停止契約 ③ —
                // done を待つ側は、親も pump も終わっていることを信じてよい）
                for pump in &pumps {
                    pump.abort();
                }
                registry
                    .lock()
                    .await
                    .update_status(process_id, ProcessStatus::Completed { exit_code });

                hub.broadcast(RepoMessage::Show {
                    pane_id: pane_id.to_string(),
                    content: crate::protocol::Content::Log("[process stopped]\n".to_string()),
                    append: true,
                    title: None,
                    lane: None,
                    scope: None,
                });
                return;
            }

            // コード注入
            code = inject_rx.recv() => {
                match code {
                    Some(code) => {
                        if let Some(ref mut handle) = stdin {
                            let payload = if code.ends_with('\n') {
                                code
                            } else {
                                format!("{}\n", code)
                            };
                            if let Err(e) = handle.write_all(payload.as_bytes()).await {
                                tracing::warn!("inject 送信失敗 ({}): {}", process_id, e);
                            }
                        }
                    }
                    None => {
                        // inject チャネル閉鎖（レジストリからの登録解除）
                    }
                }
            }

            // stdout/stderr 出力
            line = line_rx.recv() => {
                match line {
                    Some((stream, text)) => {
                        let content = if stream == "stderr" {
                            crate::protocol::Content::Html(format!(
                                "<span style=\"color:#e06060\">{}</span>\n",
                                html_escape(&text)
                            ))
                        } else {
                            crate::protocol::Content::Log(format!("{}\n", text))
                        };
                        hub.broadcast(RepoMessage::Show {
                            pane_id: pane_id.to_string(),
                            content,
                            append: true,
                            title: None,
                            lane: None,
                            scope: None,
                        });
                    }
                    None => {
                        // 全ストリーム終了
                        let status = child.wait().await;
                        let exit_code = status.ok().and_then(|s| s.code());
                        registry.lock().await.update_status(
                            process_id,
                            ProcessStatus::Completed { exit_code },
                        );
                        hub.broadcast(RepoMessage::Show {
                            pane_id: pane_id.to_string(),
                            content: crate::protocol::Content::Log(
                                format!("[process exited: {:?}]\n", exit_code),
                            ),
                            append: true,
                            title: None,
                            lane: None,
                            scope: None,
                        });
                        return;
                    }
                }
            }
        }
    }
}

/// HTML エスケープ
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 実行結果を HTML フォーマット
fn format_output_html(stdout: &str, stderr: &str, exit_code: Option<i32>) -> String {
    let mut html = String::from(
        "<div style=\"font-family:'FiraCode Nerd Font','Fira Code',monospace;font-size:13px;line-height:1.5\">",
    );

    if !stdout.is_empty() {
        html.push_str("<pre style=\"margin:0;color:#c8d3d5;white-space:pre-wrap\">");
        html.push_str(&html_escape(stdout));
        html.push_str("</pre>");
    }

    if !stderr.is_empty() {
        html.push_str(
            "<pre style=\"margin:0;color:#e06060;white-space:pre-wrap;border-top:1px solid #333;padding-top:8px;margin-top:8px\">",
        );
        html.push_str(&html_escape(stderr));
        html.push_str("</pre>");
    }

    if let Some(code) = exit_code
        && code != 0
    {
        html.push_str(&format!(
            "<div style=\"color:#e06060;font-size:11px;margin-top:8px\">exit code: {}</div>",
            code
        ));
    }

    html.push_str("</div>");
    html
}

// =========================================================================
// Ruby 互換レイヤー（既存 MCP ツール・HTTP ルートとの後方互換）
// =========================================================================

/// Ruby 用ブートストラップ — stdin 経由のコード実行ループ付き
pub fn ruby_bootstrap(user_code: &str) -> String {
    // セキュリティ: ローカル実行環境のため、Claude CLI と同じ信頼レベル
    format!(
        r#"$shutdown_requested = false
$stdin_thread = Thread.new do
  while (line = $stdin.gets)
    begin
      binding.eval(line.strip)  # ホットインジェクション: 任意の Ruby コードを実行時に注入
    rescue => e
      $stderr.puts e.message
    end
  end
end

begin
{}
ensure
  $stdin_thread.kill if $stdin_thread
end"#,
        user_code
    )
}

/// Ruby コードを即座に実行（後方互換）
pub async fn ruby_eval(
    code: Option<&str>,
    file_path: Option<&str>,
    pane_id: &str,
    repo_dir: &str,
    hub: &Hub,
) -> Result<RunResult, String> {
    let mut args = Vec::new();

    if let Some(file) = file_path {
        let full_path = Path::new(repo_dir).join(file);
        // dunce: 両辺とも同じ流儀で正規化しないと `starts_with` の封じ込め判定が壊れる。
        let canonical =
            dunce::canonicalize(&full_path).map_err(|e| format!("パス解決エラー: {}", e))?;
        let repo_canonical = dunce::canonicalize(repo_dir)
            .map_err(|e| format!("repoディレクトリ解決エラー: {}", e))?;
        if !canonical.starts_with(&repo_canonical) {
            return Err(format!(
                "repoディレクトリ外のファイルにはアクセスできません: {}",
                file
            ));
        }
        args.push(canonical.to_string_lossy().to_string());
    } else if let Some(c) = code {
        args.push("-e".to_string());
        args.push(c.to_string());
    } else {
        return Err("code または file が必要です".to_string());
    }

    process_run_eval(
        &RunEvalParams {
            command: "ruby".to_string(),
            args,
            pane_id: Some(pane_id.to_string()),
            working_dir: None,
        },
        repo_dir,
        hub,
    )
    .await
}

/// Ruby デーモンプロセスとして起動（後方互換）
pub async fn ruby_run(
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
    code: Option<&str>,
    file_path: Option<&str>,
    name: Option<&str>,
    pane_id: &str,
    repo_dir: &str,
    hub: &Hub,
) -> Result<String, String> {
    let ruby_code = if let Some(file) = file_path {
        let full_path = Path::new(repo_dir).join(file);
        // dunce: 両辺とも同じ流儀で正規化しないと `starts_with` の封じ込め判定が壊れる。
        let canonical =
            dunce::canonicalize(&full_path).map_err(|e| format!("パス解決エラー: {}", e))?;
        let repo_canonical = dunce::canonicalize(repo_dir)
            .map_err(|e| format!("repoディレクトリ解決エラー: {}", e))?;
        if !canonical.starts_with(&repo_canonical) {
            return Err(format!(
                "repoディレクトリ外のファイルにはアクセスできません: {}",
                file
            ));
        }
        // TOCTOU 回避: exists() チェックせず直接読み込み
        let content = tokio::fs::read_to_string(&canonical)
            .await
            .map_err(|e| format!("ファイル読み込みエラー: {} ({})", file, e))?;
        ruby_bootstrap(&content)
    } else if let Some(c) = code {
        ruby_bootstrap(c)
    } else {
        return Err("code または file が必要です".to_string());
    };

    process_run(
        registry,
        &RunParams {
            command: "ruby".to_string(),
            args: vec!["-e".to_string(), ruby_code],
            name: Some(
                name.unwrap_or(file_path.unwrap_or("ruby-daemon"))
                    .to_string(),
            ),
            pane_id: Some(pane_id.to_string()),
            working_dir: None,
            bootstrap: None,
        },
        repo_dir,
        hub,
    )
    .await
}

/// Ruby プロセスを停止（後方互換）
pub async fn ruby_stop(
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
    process_id: &str,
) -> Result<(), String> {
    process_stop(registry, process_id).await
}

/// Ruby プロセス一覧（後方互換）
pub async fn ruby_list(
    registry: &std::sync::Arc<tokio::sync::Mutex<ProcessRegistry>>,
) -> Vec<ProcessInfo> {
    registry.lock().await.list()
}

#[cfg(all(test, unix))]
mod tests {
    //! 棚卸し 9-2 段階 3（doc 63 §8「① 長期 runner が repo stop で止まらない」）。
    //! `sleep` を runner として起こし、repo stop 相当の経路で止まって**終了まで確認できる**ことを固定する。

    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn sleep_params(secs: &str) -> RunParams {
        RunParams {
            command: "sleep".to_string(),
            args: vec![secs.to_string()],
            name: None,
            pane_id: None,
            working_dir: None,
            bootstrap: None,
        }
    }

    fn status_of(reg: &ProcessRegistry, id: &str) -> ProcessStatus {
        reg.list()
            .into_iter()
            .find(|p| p.process_id == id)
            .map(|p| p.status)
            .expect("entry が居る")
    }

    /// 長期 runner が repo stop（`shutdown_repo`）で止まり、registry が Completed になるまで確認できる。
    ///
    /// fix 前は `shutdown_repo` が file watcher しか止めず、`sleep 30` は Running のまま生き残る
    /// （= この assert が赤）。10 秒の外側 timeout は「lock を握ったまま待つと行き詰まる」（停止契約 ②）
    /// の網でもある — `stream_output` の `update_status` が lock を取れないと done が来ない。
    #[tokio::test]
    async fn shutdown_repo_stops_running_runners_and_confirms_exit() {
        let state = crate::repo::state::build_test_app_state().await;
        let id = process_run(
            &state.process_registry,
            &sleep_params("30"),
            "/tmp",
            &state.hub,
        )
        .await
        .expect("sleep 30 を起こす");
        assert!(matches!(
            status_of(&*state.process_registry.lock().await, &id),
            ProcessStatus::Running
        ));

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::repo::server::shutdown_repo(&state),
        )
        .await
        .expect("shutdown_repo が 10 秒以内に返る（lock を握ったまま待っていない）");

        assert!(
            matches!(
                status_of(&*state.process_registry.lock().await, &id),
                ProcessStatus::Completed { .. }
            ),
            "repo stop で runner が止まり、終了が registry に反映される"
        );
    }

    /// 受付を閉じた後の `process_run` は登録できず、spawn した子を kill して Err を返す（停止契約 ①）。
    #[tokio::test]
    async fn process_run_is_refused_after_stop_all() {
        let registry = Arc::new(Mutex::new(ProcessRegistry::new()));
        let hub = crate::repo::hub::Hub::new();
        assert_eq!(stop_all(&registry).await, 0, "runner 0 で閉じる");

        let r = process_run(&registry, &sleep_params("30"), "/tmp", &hub).await;
        assert!(r.is_err(), "閉じた後の起動は Err: {r:?}");
        assert!(
            registry.lock().await.list().is_empty(),
            "登録されていない（子は kill 済み）"
        );
    }

    /// `stop_all` は走っている runner の数を返し、全部 Completed にしてから返る。
    #[tokio::test]
    async fn stop_all_reports_count_and_waits_for_every_runner() {
        let registry = Arc::new(Mutex::new(ProcessRegistry::new()));
        let hub = crate::repo::hub::Hub::new();
        let a = process_run(&registry, &sleep_params("30"), "/tmp", &hub)
            .await
            .unwrap();
        let b = process_run(&registry, &sleep_params("30"), "/tmp", &hub)
            .await
            .unwrap();

        let n = tokio::time::timeout(std::time::Duration::from_secs(10), stop_all(&registry))
            .await
            .expect("10 秒以内");
        assert_eq!(n, 2);
        let reg = registry.lock().await;
        for id in [&a, &b] {
            assert!(matches!(
                status_of(&reg, id),
                ProcessStatus::Completed { .. }
            ));
        }
    }
}
