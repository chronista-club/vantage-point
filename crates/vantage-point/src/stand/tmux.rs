//! tmuxセッション管理モジュール
//!
//! portable-ptyの代わりに tmux CLI をラップし、
//! Stand と tmux セッションのライフサイクルを融合する。
//!
//! ## 設計思想
//! - 1 Stand = 1 tmux session
//! - VP crash しても tmux セッションは生存
//! - 再起動時に既存セッションへ再アタッチ
//! - pipe-pane (named pipe) で出力ストリーミング

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast;

use crate::protocol::StandMessage;

/// tmuxセッションマネージャー
pub struct TmuxManager {
    /// tmuxセッション名（"vp-{port}" or "vp-{project_name}"）
    session_name: String,
    /// プロジェクトディレクトリ
    project_dir: String,
    /// named pipe のパス
    pipe_path: PathBuf,
    /// セッションがアクティブか
    active: bool,
}

impl TmuxManager {
    pub fn new() -> Self {
        Self {
            session_name: String::new(),
            project_dir: String::new(),
            pipe_path: PathBuf::new(),
            active: false,
        }
    }

    /// tmuxが利用可能かチェック
    pub async fn is_available() -> bool {
        tokio::process::Command::new("tmux")
            .arg("-V")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// セッション作成（既存なら再利用）
    ///
    /// named pipe を作成し、tmux pipe-pane で出力を流す。
    /// 既にセッションが存在する場合はそのまま再利用する。
    pub async fn start(
        &mut self,
        project_dir: &str,
        session_name: &str,
        cols: u16,
        rows: u16,
        tx: broadcast::Sender<StandMessage>,
    ) -> Result<()> {
        self.session_name = session_name.to_string();
        self.project_dir = project_dir.to_string();

        // named pipe のパスを決定
        let pipe_dir = std::env::temp_dir().join("vantage-point");
        tokio::fs::create_dir_all(&pipe_dir).await.ok();
        self.pipe_path = pipe_dir.join(format!("{}.pipe", session_name));

        // 既存パイプがあれば削除
        if self.pipe_path.exists() {
            tokio::fs::remove_file(&self.pipe_path).await.ok();
        }

        // named pipe (FIFO) を作成
        let pipe_path_str = self.pipe_path.to_string_lossy().to_string();
        let mkfifo_status = tokio::process::Command::new("mkfifo")
            .arg(&pipe_path_str)
            .status()
            .await
            .context("mkfifo の実行に失敗")?;

        if !mkfifo_status.success() {
            bail!("named pipe の作成に失敗: {}", pipe_path_str);
        }

        // セッションが既に存在するかチェック
        let has_session = self.has_session().await;

        if has_session {
            tracing::info!("既存の tmux セッション '{}' に再アタッチ", session_name);
            // リサイズ
            self.resize(cols, rows).await.ok();
        } else {
            tracing::info!(
                "新規 tmux セッション '{}' を作成 ({}x{})",
                session_name,
                cols,
                rows
            );

            // 新規セッション作成（デタッチド）
            let status = tmux_cmd(&[
                "new-session",
                "-d",
                "-s",
                session_name,
                "-c",
                project_dir,
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ])
            .await
            .context("tmux new-session の実行に失敗")?;

            if !status.success() {
                bail!("tmux セッション '{}' の作成に失敗", session_name);
            }
        }

        // pipe-pane で出力ストリーミング開始
        self.start_pipe_pane().await?;

        // named pipe リーダータスクを起動
        self.start_reader_task(tx);

        self.active = true;

        Ok(())
    }

    /// tmux にキー入力を送信（バイナリデータ対応）
    ///
    /// base64デコード済みの生バイトを受け取り、
    /// tmux send-keys の `-l` (literal) フラグで送信する。
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        if !self.active {
            bail!("tmux セッションがアクティブではありません");
        }

        // バイナリデータをtmuxに送信
        // send-keys -l はリテラル文字として送信（キーバインド解釈なし）
        // 制御文字（Enter, Ctrl+C等）も正しく送信される
        let text = String::from_utf8_lossy(data);

        let status = tmux_cmd(&["send-keys", "-t", &self.session_name, "-l", &text])
            .await
            .context("tmux send-keys の実行に失敗")?;

        if !status.success() {
            tracing::warn!("tmux send-keys 失敗 (session: {})", self.session_name);
        }

        Ok(())
    }

    /// セッションが存在するか
    pub async fn has_session(&self) -> bool {
        tmux_cmd(&["has-session", "-t", &self.session_name])
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// ウィンドウリサイズ
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if !self.active {
            return Ok(());
        }

        // resize-window を使用（tmux 2.9+）
        let status = tmux_cmd(&[
            "resize-window",
            "-t",
            &self.session_name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ])
        .await
        .context("tmux resize-window の実行に失敗")?;

        if !status.success() {
            tracing::warn!(
                "tmux resize-window 失敗 (session: {}, {}x{})",
                self.session_name,
                cols,
                rows
            );
        }

        Ok(())
    }

    /// セッション停止
    pub async fn kill(&mut self) -> Result<()> {
        if self.session_name.is_empty() {
            return Ok(());
        }

        // pipe-pane を停止
        self.stop_pipe_pane().await.ok();

        // named pipe をクリーンアップ
        if self.pipe_path.exists() {
            tokio::fs::remove_file(&self.pipe_path).await.ok();
        }

        // tmux セッション終了
        let _ = tmux_cmd(&["kill-session", "-t", &self.session_name]).await;

        self.active = false;
        tracing::info!("tmux セッション '{}' を終了", self.session_name);

        Ok(())
    }

    /// セッションがアクティブか
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// セッション名を取得
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    // --- Private ---

    /// pipe-pane を開始
    async fn start_pipe_pane(&self) -> Result<()> {
        let pipe_path_str = self.pipe_path.to_string_lossy().to_string();

        // pipe-pane: tmux の出力を named pipe に流す
        // -O: 出力のみ（入力は流さない）
        // 注意: `cat` はFIFO出力時にフルバッファリングを使うため、
        // perlの $|=1 (autoflush) + sysread/print で即座にフラッシュする
        let pipe_cmd = format!(
            "perl -e '$|=1; while(sysread(STDIN,$buf,4096)){{print $buf}}' > {}",
            pipe_path_str
        );
        let status = tmux_cmd(&["pipe-pane", "-O", "-t", &self.session_name, &pipe_cmd])
            .await
            .context("tmux pipe-pane の実行に失敗")?;

        if !status.success() {
            bail!("tmux pipe-pane の開始に失敗");
        }

        tracing::info!("tmux pipe-pane 開始: {}", pipe_path_str);
        Ok(())
    }

    /// pipe-pane を停止
    async fn stop_pipe_pane(&self) -> Result<()> {
        // 引数なしの pipe-pane で停止
        let _ = tmux_cmd(&["pipe-pane", "-t", &self.session_name]).await;
        Ok(())
    }

    /// named pipe からの読み取りタスクを起動
    fn start_reader_task(&self, tx: broadcast::Sender<StandMessage>) {
        let pipe_path = self.pipe_path.clone();
        let session_name = self.session_name.clone();

        tokio::spawn(async move {
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;

            // named pipe をオープン（ブロッキングなので spawn_blocking 内ではなく
            // tokio の File で非同期に読む）
            let file = match tokio::fs::File::open(&pipe_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(
                        "named pipe のオープンに失敗 (session: {}): {}",
                        session_name,
                        e
                    );
                    return;
                }
            };

            let mut reader = tokio::io::BufReader::new(file);
            let mut buf = [0u8; 4096];

            // TerminalReady を通知
            let _ = tx.send(StandMessage::TerminalReady);

            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => {
                        // パイプがクローズされた
                        tracing::info!("tmux pipe reader: EOF (session: {})", session_name);
                        break;
                    }
                    Ok(n) => {
                        let encoded = engine.encode(&buf[..n]);
                        match tx.send(StandMessage::TerminalOutput { data: encoded }) {
                            Ok(_) => {}
                            Err(_) => {
                                tracing::debug!("tmux pipe broadcast: 受信者なし");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("tmux pipe reader error (session: {}): {}", session_name, e);
                        break;
                    }
                }
            }
        });
    }
}

impl Drop for TmuxManager {
    fn drop(&mut self) {
        // named pipe のクリーンアップ（同期的に）
        if self.pipe_path.exists() {
            std::fs::remove_file(&self.pipe_path).ok();
        }
        // 注意: tmux セッション自体は kill しない（VP再起動時に再アタッチするため）
    }
}

/// tmux コマンドを実行
async fn tmux_cmd(args: &[&str]) -> Result<std::process::ExitStatus> {
    let status = tokio::process::Command::new("tmux")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("tmux コマンドの実行に失敗")?;

    Ok(status)
}

/// セッション名を生成: "vp-{port}"
pub fn session_name_for_port(port: u16) -> String {
    format!("vp-{}", port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_name_for_port() {
        assert_eq!(session_name_for_port(33000), "vp-33000");
        assert_eq!(session_name_for_port(33001), "vp-33001");
    }

    #[tokio::test]
    async fn test_tmux_availability_check() {
        // tmux がインストールされていれば true
        let available = TmuxManager::is_available().await;
        // CI環境では false の可能性があるので assert しない
        tracing::info!("tmux available: {}", available);
    }

    #[test]
    fn test_tmux_manager_new() {
        let mgr = TmuxManager::new();
        assert!(!mgr.is_active());
        assert!(mgr.session_name().is_empty());
    }
}
