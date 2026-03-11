//! Unison QUIC ターミナルブリッジ
//!
//! TUI ↔ Process サーバー間の PTY 通信を管理する。
//! mpsc チャネルでコマンド/イベントをやり取りし、
//! 別スレッドで QUIC 接続を維持する。

use std::time::Duration;

/// ブリッジスレッドへのコマンド
pub enum BridgeCommand {
    /// PTY への入力データ
    Input(Vec<u8>),
    /// PTY リサイズ
    Resize { cols: u16, rows: u16 },
    /// 新規セッション作成
    CreateSession {
        cols: u16,
        rows: u16,
        command: Vec<String>,
    },
    /// セッション切替
    SwitchSession(String),
    /// tmux ペイン分割（Process の TmuxActor 経由）
    TmuxSplit {
        horizontal: bool,
        command: Option<String>,
    },
}

/// ブリッジスレッドからのイベント
pub enum BridgeEvent {
    /// PTY 出力データ
    Output(Vec<u8>),
    /// セッション作成完了
    SessionCreated { session_id: String },
    /// セッション切替完了
    SessionSwitched { session_id: String },
    /// エラー
    Error(String),
    /// 接続切断
    Disconnected,
}

/// Unison terminal ブリッジスレッドを起動
///
/// Process サーバーの "terminal" チャネルに QUIC 接続し、
/// PTY の入出力を中継する。
pub fn spawn_terminal_bridge(
    port: u16,
    terminal_token: String,
    cmd_rx: std::sync::mpsc::Receiver<BridgeCommand>,
    event_tx: std::sync::mpsc::Sender<BridgeEvent>,
) {
    std::thread::Builder::new()
        .name("tui-terminal-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(async move {
                let quic_port = port + crate::process::unison_server::QUIC_PORT_OFFSET;
                let addr = format!("[::1]:{}", quic_port);

                // 接続（リトライ付き）
                let client = match unison::ProtocolClient::new_default() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = event_tx
                            .send(BridgeEvent::Error(format!("QUIC client 作成失敗: {}", e)));
                        return;
                    }
                };

                let mut attempts = 0;
                loop {
                    match client.connect(&addr).await {
                        Ok(_) => break,
                        Err(_) => {
                            attempts += 1;
                            if attempts >= 10 {
                                let _ = event_tx
                                    .send(BridgeEvent::Error("QUIC 接続失敗".to_string()));
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                    }
                }

                let channel = match client.open_channel("terminal").await {
                    Ok(ch) => ch,
                    Err(e) => {
                        let _ = event_tx.send(BridgeEvent::Error(format!(
                            "terminal チャネル開設失敗: {}",
                            e
                        )));
                        return;
                    }
                };

                // 認証
                match channel
                    .request(
                        "auth",
                        serde_json::json!({"token": terminal_token}),
                    )
                    .await
                {
                    Ok(resp) => {
                        if resp.get("error").is_some() {
                            let _ = event_tx
                                .send(BridgeEvent::Error(format!("認証失敗: {:?}", resp)));
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(BridgeEvent::Error(format!("認証リクエスト失敗: {}", e)));
                        return;
                    }
                }

                // sync → tokio ブリッジ
                let (bridge_tx, mut bridge_rx) =
                    tokio::sync::mpsc::channel::<BridgeCommand>(256);
                std::thread::Builder::new()
                    .name("tui-cmd-bridge".into())
                    .spawn(move || {
                        while let Ok(cmd) = cmd_rx.recv() {
                            if bridge_tx.blocking_send(cmd).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("tui-cmd-bridge spawn failed");

                // メインループ
                loop {
                    tokio::select! {
                        data = channel.recv_raw() => {
                            match data {
                                Ok(bytes) => {
                                    if event_tx.send(BridgeEvent::Output(bytes)).is_err() {
                                        break;
                                    }
                                }
                                Err(_) => {
                                    let _ = event_tx.send(BridgeEvent::Disconnected);
                                    break;
                                }
                            }
                        }
                        evt = channel.recv() => {
                            match evt {
                                Ok(msg) => {
                                    if msg.method == "session_ended" {
                                        tracing::info!("TUI bridge: session_ended 受信");
                                        let _ = event_tx.send(BridgeEvent::Disconnected);
                                        break;
                                    }
                                }
                                Err(_) => {
                                    let _ = event_tx.send(BridgeEvent::Disconnected);
                                    break;
                                }
                            }
                        }
                        cmd = bridge_rx.recv() => {
                            match cmd {
                                Some(BridgeCommand::Input(data)) => {
                                    if channel.send_raw(&data).await.is_err() {
                                        let _ = event_tx.send(BridgeEvent::Disconnected);
                                        break;
                                    }
                                }
                                Some(BridgeCommand::Resize { cols, rows }) => {
                                    let _ = channel.request(
                                        "resize",
                                        serde_json::json!({"cols": cols, "rows": rows}),
                                    ).await;
                                }
                                Some(BridgeCommand::CreateSession { cols, rows, command }) => {
                                    match channel.request(
                                        "create_session",
                                        serde_json::json!({
                                            "cols": cols,
                                            "rows": rows,
                                            "command": command,
                                        }),
                                    ).await {
                                        Ok(resp) => {
                                            if let Some(sid) = resp.get("session_id").and_then(|v| v.as_str()) {
                                                let _ = event_tx.send(BridgeEvent::SessionCreated {
                                                    session_id: sid.to_string(),
                                                });
                                            } else {
                                                let _ = event_tx.send(BridgeEvent::Error(
                                                    format!("セッション作成失敗: {:?}", resp),
                                                ));
                                            }
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(BridgeEvent::Error(
                                                format!("セッション作成リクエスト失敗: {}", e),
                                            ));
                                        }
                                    }
                                }
                                Some(BridgeCommand::SwitchSession(session_id)) => {
                                    match channel.request(
                                        "switch_session",
                                        serde_json::json!({"session_id": session_id}),
                                    ).await {
                                        Ok(resp) => {
                                            if let Some(sid) = resp.get("session_id").and_then(|v| v.as_str()) {
                                                let _ = event_tx.send(BridgeEvent::SessionSwitched {
                                                    session_id: sid.to_string(),
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(BridgeEvent::Error(
                                                format!("セッション切替失敗: {}", e),
                                            ));
                                        }
                                    }
                                }
                                Some(BridgeCommand::TmuxSplit { horizontal, command }) => {
                                    let payload = serde_json::json!({
                                        "horizontal": horizontal,
                                        "command": command,
                                    });
                                    match channel.request("tmux_split", payload).await {
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::warn!("tmux_split 失敗: {}", e);
                                        }
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                }
            });
        })
        .expect("tui-terminal-bridge スレッドの起動に失敗");
}
