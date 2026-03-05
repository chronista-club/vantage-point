//! Ruby VM プロセス管理
//!
//! Stand が ruby コマンドを子プロセスとして spawn し、
//! stdout/stderr を Canvas にストリーミングする。

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::hub::Hub;
use crate::protocol::StandMessage;

/// Ruby プロセスの状態
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RubyProcessStatus {
    Running,
    Completed { exit_code: Option<i32> },
    Failed { error: String },
}

/// 実行中の Ruby プロセス情報（外部公開用）
#[derive(Debug, Clone, Serialize)]
pub struct RubyProcessInfo {
    pub process_id: String,
    pub name: String,
    pub pane_id: String,
    pub status: RubyProcessStatus,
}

/// 内部管理用の Ruby プロセスエントリ
struct RubyProcessEntry {
    pub info: RubyProcessInfo,
    /// Graceful shutdown 用のシグナル送信
    pub shutdown_tx: Option<mpsc::Sender<()>>,
}

/// Ruby プロセスレジストリ
pub struct RubyRegistry {
    processes: HashMap<String, RubyProcessEntry>,
    counter: u32,
}

impl RubyRegistry {
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            counter: 0,
        }
    }

    /// 新しいプロセスIDを生成
    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("rb-{:04}", self.counter)
    }

    /// 実行中プロセス一覧
    pub fn list(&self) -> Vec<RubyProcessInfo> {
        self.processes.values().map(|e| e.info.clone()).collect()
    }

    /// プロセスを登録
    fn register(
        &mut self,
        process_id: String,
        name: String,
        pane_id: String,
        shutdown_tx: mpsc::Sender<()>,
    ) {
        self.processes.insert(
            process_id.clone(),
            RubyProcessEntry {
                info: RubyProcessInfo {
                    process_id,
                    name,
                    pane_id,
                    status: RubyProcessStatus::Running,
                },
                shutdown_tx: Some(shutdown_tx),
            },
        );
    }

    /// プロセス状態を更新
    pub fn update_status(&mut self, process_id: &str, status: RubyProcessStatus) {
        if let Some(entry) = self.processes.get_mut(process_id) {
            entry.info.status = status;
            entry.shutdown_tx = None;
        }
    }

    /// Graceful shutdown シグナルを送信
    pub async fn send_shutdown(&self, process_id: &str) -> bool {
        if let Some(entry) = self.processes.get(process_id) {
            if let Some(tx) = &entry.shutdown_tx {
                return tx.send(()).await.is_ok();
            }
        }
        false
    }
}

/// Ruby 実行結果
#[derive(Debug, Clone, Serialize)]
pub struct RubyEvalResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
}

/// Ruby コードを即座に実行（短命）
pub async fn ruby_eval(
    code: Option<&str>,
    file_path: Option<&str>,
    pane_id: &str,
    project_dir: &str,
    hub: &Hub,
) -> Result<RubyEvalResult, String> {
    let start = std::time::Instant::now();

    let mut cmd = Command::new("ruby");
    cmd.current_dir(project_dir);

    if let Some(file) = file_path {
        let full_path = Path::new(project_dir).join(file);
        if !full_path.exists() {
            return Err(format!("ファイルが見つかりません: {}", file));
        }
        cmd.arg(&full_path);
    } else if let Some(c) = code {
        cmd.arg("-e").arg(c);
    } else {
        return Err("code または file が必要です".to_string());
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("ruby コマンド実行失敗: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Canvas に結果を表示
    let display = format_output_html(&stdout, &stderr, output.status.code());
    hub.broadcast(StandMessage::Show {
        pane_id: pane_id.to_string(),
        content: crate::protocol::Content::Html(display),
        append: false,
        title: Some("Ruby".to_string()),
    });

    Ok(RubyEvalResult {
        stdout,
        stderr,
        exit_code: output.status.code(),
        elapsed_ms,
    })
}

/// デーモンプロセスとして Ruby を起動
pub async fn ruby_run(
    registry: &std::sync::Arc<tokio::sync::Mutex<RubyRegistry>>,
    code: Option<&str>,
    file_path: Option<&str>,
    name: Option<&str>,
    pane_id: &str,
    project_dir: &str,
    hub: &Hub,
) -> Result<String, String> {
    // Graceful shutdown ラッパーコード生成
    let ruby_code = if let Some(file) = file_path {
        let full_path = Path::new(project_dir).join(file);
        if !full_path.exists() {
            return Err(format!("ファイルが見つかりません: {}", file));
        }
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| format!("ファイル読み込み失敗: {}", e))?;
        wrap_with_shutdown_handler(&content)
    } else if let Some(c) = code {
        wrap_with_shutdown_handler(c)
    } else {
        return Err("code または file が必要です".to_string());
    };

    let process_id = registry.lock().await.next_id();
    let display_name = name.unwrap_or(file_path.unwrap_or("daemon")).to_string();

    // プロセス起動
    let mut child = Command::new("ruby")
        .arg("-e")
        .arg(&ruby_code)
        .current_dir(project_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("ruby プロセス起動失敗: {}", e))?;

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

    registry.lock().await.register(
        process_id.clone(),
        display_name,
        pane_id.to_string(),
        shutdown_tx,
    );

    // 出力ストリーミングタスクを起動
    let hub_clone = hub.clone();
    let pane_id_owned = pane_id.to_string();
    let process_id_clone = process_id.clone();
    let registry_clone = registry.clone();

    tokio::spawn(async move {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();

        stream_daemon_output(
            &mut child,
            stdout,
            stderr,
            stdin,
            shutdown_rx,
            &hub_clone,
            &pane_id_owned,
            &process_id_clone,
            &registry_clone,
        )
        .await;
    });

    Ok(process_id)
}

/// デーモンプロセスの出力をストリーミング
async fn stream_daemon_output(
    child: &mut tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    mut stdin: Option<tokio::process::ChildStdin>,
    mut shutdown_rx: mpsc::Receiver<()>,
    hub: &Hub,
    pane_id: &str,
    process_id: &str,
    registry: &std::sync::Arc<tokio::sync::Mutex<RubyRegistry>>,
) {
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
            _ = shutdown_rx.recv() => {
                // stdin 経由で shutdown フラグを設定
                // NOTE: eval() は graceful shutdown のために意図的に使用
                if let Some(ref mut handle) = stdin {
                    let _ = handle.write_all(b"$shutdown_requested = true\n").await;
                }

                // タイムアウト後に強制 kill
                tokio::select! {
                    status = child.wait() => {
                        let exit_code = status.ok().and_then(|s| s.code());
                        registry.lock().await.update_status(
                            process_id,
                            RubyProcessStatus::Completed { exit_code },
                        );
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                        let _ = child.kill().await;
                        registry.lock().await.update_status(
                            process_id,
                            RubyProcessStatus::Completed { exit_code: Some(-9) },
                        );
                    }
                }

                hub.broadcast(StandMessage::Show {
                    pane_id: pane_id.to_string(),
                    content: crate::protocol::Content::Log("[process stopped]\n".to_string()),
                    append: true,
                    title: None,
                });
                return;
            }

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
                        hub.broadcast(StandMessage::Show {
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
                            RubyProcessStatus::Completed { exit_code },
                        );
                        hub.broadcast(StandMessage::Show {
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

/// $shutdown_requested でグレースフルシャットダウンするラッパー
fn wrap_with_shutdown_handler(code: &str) -> String {
    // stdin から受け取った行を Ruby 文脈で実行する（shutdown フラグ設定用）
    // セキュリティ: ローカル実行環境のため、Claude CLI と同じ信頼レベル
    format!(
        r#"$shutdown_requested = false
$stdin_thread = Thread.new do
  while (line = $stdin.gets)
    begin; binding.eval(line.strip); rescue => e; $stderr.puts e.message; end
  end
end

begin
{}
ensure
  $stdin_thread.kill if $stdin_thread
end"#,
        code
    )
}

/// Graceful stop
pub async fn ruby_stop(
    registry: &std::sync::Arc<tokio::sync::Mutex<RubyRegistry>>,
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

/// HTML エスケープ
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// eval 結果を HTML フォーマット
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

    if let Some(code) = exit_code {
        if code != 0 {
            html.push_str(&format!(
                "<div style=\"color:#e06060;font-size:11px;margin-top:8px\">exit code: {}</div>",
                code
            ));
        }
    }

    html.push_str("</div>");
    html
}
