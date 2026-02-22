//! 構造化トレースログ基盤
//!
//! MCP プロセスと Stand プロセスの両方から同一ファイルに
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
    /// プロセス種別（"mcp" or "stand"）
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
pub fn log_file_path() -> Option<PathBuf> {
    let config_dir = dirs::config_dir()?;
    let log_dir = config_dir.join("vantage").join("logs");

    // ディレクトリがなければ作成
    if !log_dir.exists() {
        if let Err(e) = fs::create_dir_all(&log_dir) {
            eprintln!("[trace_log] ログディレクトリ作成失敗: {e}");
            return None;
        }
    }

    Some(log_dir.join("debug.log"))
}

/// ログファイルを初期化（append モード）
///
/// OnceLock により、プロセス内で1回だけ実行される。
fn init_log_file() -> Option<&'static Mutex<File>> {
    Some(LOG_FILE.get_or_init(|| {
        let path = log_file_path().expect("ログファイルパス取得失敗");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("ログファイルオープン失敗: {path:?}: {e}"));
        Mutex::new(file)
    }))
}

/// トレースエントリを JSON 1行としてファイルに書き出す
///
/// 書き出し後に flush を実行し、クラッシュ時のデータ損失を最小化する。
/// エラー時は stderr に出力するが、呼び出し元には伝播しない
/// （ログ書き込み失敗でアプリを止めない設計）。
pub fn write_trace(entry: &TraceEntry) {
    let Some(file_mutex) = init_log_file() else {
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
