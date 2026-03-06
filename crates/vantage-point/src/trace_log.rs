//! 構造化トレースログ基盤
//!
//! MCP プロセスと Process プロセスの両方から同一ファイルに
//! JSON Lines 形式でログを書き出す。
//!
//! ログファイル: `~/.config/vantage/logs/debug.log`

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use serde::Serialize;

/// トレース ID 用のグローバルカウンター
static TRACE_COUNTER: AtomicU32 = AtomicU32::new(0);

/// ログファイルハンドル（プロセスごとに1つ）
static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// 構造化ログエントリ
///
/// 各フィールドは JSON Lines の1行として書き出される。
#[derive(Debug, Serialize)]
pub struct TraceEntry {
    /// タイムスタンプ（RFC3339 ミリ秒精度 UTC）
    pub ts: String,
    /// プロセス種別（"mcp" or "process"）
    pub process: &'static str,
    /// トレース ID（`t-0001` 形式）
    pub trace_id: String,
    /// ステップ名（処理の段階を示す）
    pub step: String,
    /// ログレベル（"DEBUG", "INFO", "WARN", "ERROR"）
    pub level: &'static str,
    /// メッセージ本文
    pub msg: String,
    /// 任意の付加データ
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// 経過時間（ミリ秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl TraceEntry {
    /// 新しいトレースエントリを生成
    ///
    /// タイムスタンプは呼び出し時点の UTC 時刻で自動設定される。
    pub fn new(
        process: &'static str,
        trace_id: impl Into<String>,
        step: impl Into<String>,
        level: &'static str,
        msg: impl Into<String>,
    ) -> Self {
        Self {
            ts: Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            process,
            trace_id: trace_id.into(),
            step: step.into(),
            level,
            msg: msg.into(),
            data: None,
            elapsed_ms: None,
        }
    }

    /// 付加データを設定（ビルダーパターン）
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// 経過時間を設定（ビルダーパターン）
    pub fn with_elapsed(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = Some(elapsed_ms);
        self
    }
}

/// 新しいトレース ID を生成（`t-0001` 形式）
///
/// AtomicU32 カウンターによりスレッドセーフ。
/// プロセス内でユニークな連番を返す。
pub fn new_trace_id() -> String {
    let n = TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("t-{:04}", n)
}

/// ログファイルのパスを返す
///
/// ディレクトリが存在しない場合は自動作成する。
/// パス: `~/.config/vantage/logs/debug.log`
/// （macOS でも `~/.config` を使用し、VP の設定パスと統一する）
pub fn log_file_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let log_dir = home.join(".config").join("vantage").join("logs");

    // ディレクトリがなければ作成
    if !log_dir.exists() {
        if let Err(e) = fs::create_dir_all(&log_dir) {
            eprintln!("[trace_log] ログディレクトリ作成失敗: {e}");
            return None;
        }
    }

    Some(log_dir.join("debug.log"))
}

/// ログファイルを早期に初期化する（プロセス起動時呼び出し用）
///
/// `write_trace()` 内でも遅延初期化されるが、プロセス起動直後に
/// 呼び出すことで初期化タイミングを明示的に制御できる。
pub fn init_log_file() {
    ensure_log_file();
}

/// ログファイルを初期化（append モード）
///
/// OnceLock により、プロセス内で1回だけ実行される。
/// パス取得失敗時は None を返す。
fn ensure_log_file() -> Option<&'static Mutex<File>> {
    let path = log_file_path()?;
    Some(LOG_FILE.get_or_init(|| {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("ログファイルオープン失敗: {path:?}: {e}"));
        Mutex::new(file)
    }))
}

/// ログファイルを監視し、新しいエントリを Hub 経由で WebSocket にブロードキャストする
///
/// `notify` クレートでファイル変更を検知し、追記された JSON Lines を
/// `ProcessMessage::TraceLog` として配信する。
/// 既存のエントリはスキップし、監視開始後の新規行のみ配信する。
pub async fn watch_and_broadcast(hub: crate::process::hub::Hub) {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    use notify::{EventKind, RecursiveMode, Watcher};

    use crate::protocol::ProcessMessage;

    let Some(path) = log_file_path() else {
        tracing::warn!("トレースログファイルパスが取得できません");
        return;
    };

    // ファイルが存在しない場合は作成を待つ
    if !path.exists() {
        tracing::info!("トレースログファイルが未作成、作成を待機: {:?}", path);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if path.exists() {
                break;
            }
        }
    }

    // ファイルを開いて末尾へシーク（既存エントリをスキップ）
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("トレースログファイルを開けません: {e}");
            return;
        }
    };
    let mut reader = BufReader::new(file);
    if let Err(e) = reader.seek(SeekFrom::End(0)) {
        tracing::error!("トレースログファイルのシーク失敗: {e}");
        return;
    }

    // notify の同期コールバックから async へブリッジするチャネル
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);

    // ファイル変更ウォッチャーを起動
    let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, _>| {
        if let Ok(event) = res
            && matches!(event.kind, EventKind::Modify(_))
        {
            // バッファに空きがなければドロップ（次回検知で読み取れる）
            let _ = tx.try_send(());
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("ファイルウォッチャーの作成に失敗: {e}");
            return;
        }
    };

    if let Err(e) = watcher.watch(&path, RecursiveMode::NonRecursive) {
        tracing::error!("トレースログファイルの監視開始に失敗: {e}");
        return;
    }

    tracing::info!("トレースログ監視を開始: {:?}", path);

    let mut line_buf = String::new();

    // 変更通知を受け取るたびに新規行を読み取ってブロードキャスト
    while rx.recv().await.is_some() {
        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf) {
                Ok(0) => break, // 新しい行がない
                Ok(_) => {
                    let trimmed = line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // JSON パースして ProcessMessage::TraceLog に変換
                    match serde_json::from_str::<serde_json::Value>(trimmed) {
                        Ok(val) => {
                            let msg = ProcessMessage::TraceLog {
                                ts: val["ts"].as_str().unwrap_or_default().to_string(),
                                process: val["process"].as_str().unwrap_or_default().to_string(),
                                trace_id: val["trace_id"].as_str().unwrap_or_default().to_string(),
                                step: val["step"].as_str().unwrap_or_default().to_string(),
                                level: val["level"].as_str().unwrap_or_default().to_string(),
                                msg: val["msg"].as_str().unwrap_or_default().to_string(),
                                data: val.get("data").cloned(),
                                elapsed_ms: val["elapsed_ms"].as_u64(),
                            };
                            hub.broadcast(msg);
                        }
                        Err(e) => {
                            tracing::debug!("トレースログ行のJSONパース失敗: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("トレースログファイルの読み取りエラー: {e}");
                    break;
                }
            }
        }
    }

    tracing::info!("トレースログ監視を終了");
}

/// トレースエントリを JSON 1行としてファイルに書き出す
///
/// 書き出し後に flush を実行し、クラッシュ時のデータ損失を最小化する。
/// エラー時は stderr に出力するが、呼び出し元には伝播しない
/// （ログ書き込み失敗でアプリを止めない設計）。
pub fn write_trace(entry: &TraceEntry) {
    let Some(file_mutex) = ensure_log_file() else {
        return;
    };

    let Ok(json) = serde_json::to_string(entry) else {
        eprintln!("[trace_log] JSON シリアライズ失敗");
        return;
    };

    let Ok(mut file) = file_mutex.lock() else {
        eprintln!("[trace_log] ファイルロック取得失敗");
        return;
    };

    if let Err(e) = writeln!(file, "{json}") {
        eprintln!("[trace_log] 書き込み失敗: {e}");
        return;
    }

    if let Err(e) = file.flush() {
        eprintln!("[trace_log] flush 失敗: {e}");
    }
}
