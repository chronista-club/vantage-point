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
use crate::protocol::ProcessMessage;

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
    /// 作業ディレクトリ（省略時はプロジェクトディレクトリ）
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
}

/// プロセスレジストリ
pub struct ProcessRegistry {
    processes: HashMap<String, ProcessEntry>,
    counter: u32,
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
    ) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

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
            },
        );
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
            entry.info.status = status;
            entry.shutdown_tx = None;
            entry.inject_tx = None;
        }
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
    project_dir: &str,
    hub: &Hub,
) -> Result<RunResult, String> {
    let start = std::time::Instant::now();
    let pane_id = params.pane_id.as_deref().unwrap_or("main");
    let work_dir = params.working_dir.as_deref().unwrap_or(project_dir);

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
    hub.broadcast(ProcessMessage::Show {
        pane_id: pane_id.to_string(),
        content: crate::protocol::Content::Html(display),
        append: false,
        title: Some(params.command.clone()),
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
    project_dir: &str,
    hub: &Hub,
) -> Result<String, String> {
    // 新プロセス起動前に完了済みエントリを掃除（5分経過分）
    registry.lock().await.cleanup_completed(300);

    let pane_id = params.pane_id.as_deref().unwrap_or("main");
    let work_dir = params.working_dir.as_deref().unwrap_or(project_dir);

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

    registry.lock().await.register(
        process_id.clone(),
        display_name,
        command_display,
        pane_id.to_string(),
        shutdown_tx,
        inject_tx,
    );

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

    if let Some(out) = stdout {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send(("stdout".to_string(), line)).await.is_err() {
                    break;
                }
            }
        });
    }

    if let Some(err) = stderr {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send(("stderr".to_string(), line)).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(line_tx);

    loop {
        tokio::select! {
            // Shutdown シグナル
            _ = shutdown_rx.recv() => {
                // stdin を閉じてプロセスに EOF 通知
                drop(stdin.take());

                // タイムアウト後に強制 kill
                tokio::select! {
                    status = child.wait() => {
                        let exit_code = status.ok().and_then(|s| s.code());
                        registry.lock().await.update_status(
                            process_id,
                            ProcessStatus::Completed { exit_code },
                        );
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                        let _ = child.kill().await;
                        registry.lock().await.update_status(
                            process_id,
                            ProcessStatus::Completed { exit_code: Some(-9) },
                        );
                    }
                }

                hub.broadcast(ProcessMessage::Show {
                    pane_id: pane_id.to_string(),
                    content: crate::protocol::Content::Log("[process stopped]\n".to_string()),
                    append: true,
                    title: None,
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
                        hub.broadcast(ProcessMessage::Show {
                            pane_id: pane_id.to_string(),
                            content,
                            append: true,
                            title: None,
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
                        hub.broadcast(ProcessMessage::Show {
                            pane_id: pane_id.to_string(),
                            content: crate::protocol::Content::Log(
                                format!("[process exited: {:?}]\n", exit_code),
                            ),
                            append: true,
                            title: None,
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
    project_dir: &str,
    hub: &Hub,
) -> Result<RunResult, String> {
    let mut args = Vec::new();

    if let Some(file) = file_path {
        let full_path = Path::new(project_dir).join(file);
        let canonical = full_path
            .canonicalize()
            .map_err(|e| format!("パス解決エラー: {}", e))?;
        let project_canonical = Path::new(project_dir)
            .canonicalize()
            .map_err(|e| format!("プロジェクトディレクトリ解決エラー: {}", e))?;
        if !canonical.starts_with(&project_canonical) {
            return Err(format!(
                "プロジェクトディレクトリ外のファイルにはアクセスできません: {}",
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
        project_dir,
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
    project_dir: &str,
    hub: &Hub,
) -> Result<String, String> {
    let ruby_code = if let Some(file) = file_path {
        let full_path = Path::new(project_dir).join(file);
        let canonical = full_path
            .canonicalize()
            .map_err(|e| format!("パス解決エラー: {}", e))?;
        let project_canonical = Path::new(project_dir)
            .canonicalize()
            .map_err(|e| format!("プロジェクトディレクトリ解決エラー: {}", e))?;
        if !canonical.starts_with(&project_canonical) {
            return Err(format!(
                "プロジェクトディレクトリ外のファイルにはアクセスできません: {}",
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
        project_dir,
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
