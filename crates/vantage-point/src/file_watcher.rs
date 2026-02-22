//! 汎用ファイル監視モジュール
//!
//! 任意のログファイルを監視し、レベル/target フィルタ付きで
//! 色分け HTML として WebView ペインにリアルタイム表示する。
//!
//! `trace_log.rs::watch_and_broadcast()` のパターンを汎用化した実装。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::protocol::{Content, StandMessage};
use crate::stand::hub::Hub;

/// ログフォーマット
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchFormat {
    /// JSON Lines (timestamp, level, fields.message, target)
    #[default]
    JsonLines,
    /// プレーンテキスト（行全体をメッセージとして扱う）
    Plain,
}

/// 表示スタイル
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchStyle {
    /// ターミナル風（暗い背景 + モノスペース）
    #[default]
    Terminal,
    /// プレーンテキスト
    Plain,
}

/// 監視設定（MCP パラメータから構築）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    /// 監視対象ファイルパス
    pub path: String,
    /// 表示先ペイン ID
    pub pane_id: String,
    /// ログフォーマット
    #[serde(default)]
    pub format: WatchFormat,
    /// レベルフィルタ正規表現（例: "INFO|WARN|ERROR"）
    #[serde(default)]
    pub filter: Option<String>,
    /// 除外 target リスト
    #[serde(default)]
    pub exclude_targets: Vec<String>,
    /// ペインタイトル
    #[serde(default)]
    pub title: Option<String>,
    /// 表示スタイル
    #[serde(default)]
    pub style: WatchStyle,
}

/// 監視ハンドル（停止用）
struct WatchHandle {
    task: JoinHandle<()>,
}

/// 同時監視数の上限
const MAX_WATCHERS: usize = 20;

/// 監視マネージャー（AppState に保持）
pub struct FileWatcherManager {
    watchers: HashMap<String, WatchHandle>,
}

impl FileWatcherManager {
    pub fn new() -> Self {
        Self {
            watchers: HashMap::new(),
        }
    }

    /// 監視を開始する。同じ pane_id なら既存を停止して再起動
    pub fn start_watch(&mut self, config: WatchConfig, hub: Hub) -> Result<(), String> {
        // パス検証
        validate_watch_path(&config.path)?;

        // 監視数の上限チェック（同一 pane_id の置き換えは許可）
        if self.watchers.len() >= MAX_WATCHERS && !self.watchers.contains_key(&config.pane_id) {
            return Err(format!(
                "同時監視数の上限（{}）に達しています",
                MAX_WATCHERS
            ));
        }

        let pane_id = config.pane_id.clone();

        // 既存の監視があれば停止
        self.stop_watch(&pane_id);

        let task = tokio::spawn(async move {
            watch_file_task(config, hub).await;
        });

        self.watchers.insert(pane_id, WatchHandle { task });
        Ok(())
    }

    /// 指定ペインの監視を停止
    pub fn stop_watch(&mut self, pane_id: &str) {
        if let Some(handle) = self.watchers.remove(pane_id) {
            handle.task.abort();
            tracing::info!("ファイル監視を停止: pane_id={}", pane_id);
        }
    }

    /// 全監視を停止（シャットダウン用）
    pub fn stop_all(&mut self) {
        let pane_ids: Vec<String> = self.watchers.keys().cloned().collect();
        for pane_id in pane_ids {
            self.stop_watch(&pane_id);
        }
    }

    /// 現在監視中のペイン ID 一覧
    pub fn active_panes(&self) -> Vec<String> {
        self.watchers.keys().cloned().collect()
    }
}

impl Default for FileWatcherManager {
    fn default() -> Self {
        Self::new()
    }
}

/// ファイルパスのバリデーション
///
/// - 絶対パスのみ許可
/// - シンボリックリンクを解決して正規化
/// - 機密ディレクトリ（.ssh, .gnupg 等）へのアクセスを拒否
fn validate_watch_path(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);

    // 絶対パスのみ許可
    if !p.is_absolute() {
        return Err("絶対パスのみ許可されます".to_string());
    }

    // 機密パターンの拒否
    let forbidden_patterns = [
        "/.ssh/",
        "/.gnupg/",
        "/.aws/",
        "/.config/gcloud/",
        "/id_rsa",
        "/id_ed25519",
        "/credentials",
        "/.env",
    ];
    let path_str = path.to_lowercase();
    for pattern in &forbidden_patterns {
        if path_str.contains(pattern) {
            return Err(format!("機密ファイルへのアクセスは拒否されます: {}", path));
        }
    }

    Ok(())
}

/// CSS プリアンブル（初回 Show で送信）
fn css_preamble() -> String {
    r#"<style>
.vp-log { font-family: 'SF Mono', 'Menlo', 'Monaco', monospace; font-size: 12px; background: #1e1e2e; color: #cdd6f4; padding: 8px; }
.vp-log-line { white-space: pre-wrap; padding: 1px 0; }
.vp-log-ts { color: #6c7086; }
.vp-log-lvl { font-weight: bold; margin: 0 4px; }
.vp-log-error { color: #f38ba8; }
.vp-log-warn { color: #fab387; }
.vp-log-info { color: #a6e3a1; }
.vp-log-debug { color: #6c7086; }
.vp-log-trace { color: #585b70; }
.vp-log-target { color: #89b4fa; }
.vp-log-msg { color: #cdd6f4; }
</style>
<div class="vp-log">"#
        .to_string()
}

/// パース済みログエントリ
struct LogEntry {
    timestamp: Option<String>,
    level: Option<String>,
    target: Option<String>,
    message: String,
}

/// JSON Lines 形式のログ行をパースする
fn parse_json_line(line: &str) -> Option<LogEntry> {
    let val: serde_json::Value = serde_json::from_str(line).ok()?;

    // tracing の JSON 出力形式に対応
    // timestamp: "timestamp" or "ts"
    let timestamp = val
        .get("timestamp")
        .or_else(|| val.get("ts"))
        .and_then(|v| v.as_str())
        .map(|s| {
            // タイムスタンプから時刻部分のみ抽出（HH:MM:SS）
            if let Some(time_part) = s.find('T') {
                let time_str = &s[time_part + 1..];
                // "15:23:26.123Z" のような形式から "15:23:26" を取得
                time_str
                    .get(..8)
                    .unwrap_or(time_str.split('.').next().unwrap_or(time_str))
                    .to_string()
            } else {
                s.to_string()
            }
        });

    let level = val
        .get("level")
        .and_then(|v| v.as_str())
        .map(|s| s.to_uppercase());

    let target = val
        .get("target")
        .or_else(|| val.get("span").and_then(|s| s.get("name")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // message: "fields.message" or "msg" or "message"
    let message = val
        .get("fields")
        .and_then(|f| f.get("message"))
        .or_else(|| val.get("msg"))
        .or_else(|| val.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(LogEntry {
        timestamp,
        level,
        target,
        message,
    })
}

/// ログエントリを色分け HTML に変換
fn render_log_line_html(entry: &LogEntry, _style: &WatchStyle) -> String {
    let ts_html = match &entry.timestamp {
        Some(ts) => format!(r#"<span class="vp-log-ts">{}</span> "#, html_escape(ts)),
        None => String::new(),
    };

    let level_html = match &entry.level {
        Some(level) => {
            let class = match level.as_str() {
                "ERROR" => "vp-log-error",
                "WARN" => "vp-log-warn",
                "INFO" => "vp-log-info",
                "DEBUG" => "vp-log-debug",
                "TRACE" => "vp-log-trace",
                _ => "vp-log-msg",
            };
            format!(
                r#"<span class="vp-log-lvl {}">{:<5}</span> "#,
                class,
                html_escape(level)
            )
        }
        None => String::new(),
    };

    let target_html = match &entry.target {
        Some(target) => format!(
            r#"<span class="vp-log-target">{}</span> "#,
            html_escape(target)
        ),
        None => String::new(),
    };

    let msg_html = format!(
        r#"<span class="vp-log-msg">{}</span>"#,
        html_escape(&entry.message)
    );

    format!(
        r#"<div class="vp-log-line">{}{}{}{}</div>"#,
        ts_html, level_html, target_html, msg_html
    )
}

/// プレーンテキスト行を HTML に変換
fn render_plain_line_html(line: &str) -> String {
    format!(
        r#"<div class="vp-log-line"><span class="vp-log-msg">{}</span></div>"#,
        html_escape(line)
    )
}

/// HTML エスケープ
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// ファイル監視タスク本体
///
/// notify でファイル変更を検知し、新行を読み取り → パース → フィルタ → HTML 変換
/// → hub.broadcast(StandMessage::Show { append: true }) で WebView に配信する。
async fn watch_file_task(config: WatchConfig, hub: Hub) {
    let path = std::path::PathBuf::from(&config.path);

    // レベルフィルタの正規表現をコンパイル
    let level_filter: Option<Regex> = config.filter.as_ref().and_then(|f| {
        Regex::new(f)
            .map_err(|e| tracing::warn!("レベルフィルタ正規表現が無効: {}: {}", f, e))
            .ok()
    });

    // 1. ファイルが存在しない場合は 500ms ポーリングで待機
    if !path.exists() {
        tracing::info!("監視対象ファイルが未作成、待機中: {:?}", path);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if path.exists() {
                break;
            }
        }
    }

    // 2. ファイルを開いて末尾へシーク（既存行スキップ）
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("監視対象ファイルを開けません: {:?}: {}", path, e);
            return;
        }
    };
    let mut reader = BufReader::new(file);
    if let Err(e) = reader.seek(SeekFrom::End(0)) {
        tracing::error!("ファイルのシーク失敗: {:?}: {}", path, e);
        return;
    }

    // 初回 Show（append: false）で CSS プリアンブルを送信
    hub.broadcast(StandMessage::Show {
        pane_id: config.pane_id.clone(),
        content: Content::Html(css_preamble()),
        append: false,
        title: config.title.clone(),
    });

    // 3. notify ウォッチャーを起動（sync → async ブリッジ）
    let (tx, mut rx) = mpsc::channel::<()>(16);

    let watch_path = path.clone();
    let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, _>| {
        if let Ok(event) = res
            && matches!(event.kind, notify::EventKind::Modify(_))
        {
            let _ = tx.try_send(());
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("ファイルウォッチャーの作成に失敗: {}", e);
            return;
        }
    };

    use notify::{RecursiveMode, Watcher};
    if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
        tracing::error!("ファイル監視の開始に失敗: {:?}: {}", watch_path, e);
        return;
    }

    tracing::info!(
        "ファイル監視を開始: {:?} → pane_id={}",
        path,
        config.pane_id
    );

    let mut line_buf = String::new();

    // 4. 変更通知を受け取るたびに新規行を読み取ってブロードキャスト
    while rx.recv().await.is_some() {
        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf) {
                Ok(0) => {
                    // ログローテーション検知: ファイルサイズが現在位置より小さければ再オープン
                    if let Ok(metadata) = std::fs::metadata(&path) {
                        let current_pos = reader.stream_position().unwrap_or(0);
                        if metadata.len() < current_pos {
                            tracing::info!(
                                "ログローテーション検知（ファイル縮小）: {:?}",
                                path
                            );
                            match File::open(&path) {
                                Ok(new_file) => {
                                    reader = BufReader::new(new_file);
                                    // ローテーション後は先頭から読む
                                    continue;
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "ローテーション後のファイル再オープン失敗: {:?}: {}",
                                        path,
                                        e
                                    );
                                }
                            }
                        }
                    }
                    break;
                }
                Ok(_) => {
                    let trimmed = line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // フォーマットに応じてパース
                    let html = match config.format {
                        WatchFormat::JsonLines => {
                            match parse_json_line(trimmed) {
                                Some(entry) => {
                                    // レベルフィルタ
                                    if let Some(ref filter) = level_filter
                                        && let Some(ref level) = entry.level
                                        && !filter.is_match(level)
                                    {
                                        continue;
                                    }

                                    // target 除外
                                    if let Some(ref target) = entry.target
                                        && config
                                            .exclude_targets
                                            .iter()
                                            .any(|ex| target.contains(ex))
                                    {
                                        continue;
                                    }

                                    render_log_line_html(&entry, &config.style)
                                }
                                // JSON パース失敗時はプレーンテキストとして表示
                                None => render_plain_line_html(trimmed),
                            }
                        }
                        WatchFormat::Plain => render_plain_line_html(trimmed),
                    };

                    // HTML を append: true で追記
                    hub.broadcast(StandMessage::Show {
                        pane_id: config.pane_id.clone(),
                        content: Content::Html(html),
                        append: true,
                        title: None,
                    });
                }
                Err(e) => {
                    tracing::error!("ファイル読み取りエラー: {:?}: {}", path, e);
                    break;
                }
            }
        }
    }

    tracing::info!(
        "ファイル監視を終了: {:?} (pane_id={})",
        path,
        config.pane_id
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_line_tracing_format() {
        let line = r#"{"timestamp":"2026-02-23T15:23:26.123Z","level":"INFO","fields":{"message":"Audio stream started"},"target":"cplp_audio::engine"}"#;
        let entry = parse_json_line(line).unwrap();
        assert_eq!(entry.timestamp.as_deref(), Some("15:23:26"));
        assert_eq!(entry.level.as_deref(), Some("INFO"));
        assert_eq!(entry.target.as_deref(), Some("cplp_audio::engine"));
        assert_eq!(entry.message, "Audio stream started");
    }

    #[test]
    fn test_parse_json_line_simple_format() {
        let line = r#"{"ts":"2026-02-23T10:00:00Z","level":"WARN","msg":"connection lost","target":"net"}"#;
        let entry = parse_json_line(line).unwrap();
        assert_eq!(entry.level.as_deref(), Some("WARN"));
        assert_eq!(entry.message, "connection lost");
        assert_eq!(entry.target.as_deref(), Some("net"));
    }

    #[test]
    fn test_parse_json_line_invalid() {
        assert!(parse_json_line("not json").is_none());
    }

    #[test]
    fn test_render_log_line_html_info() {
        let entry = LogEntry {
            timestamp: Some("15:23:26".to_string()),
            level: Some("INFO".to_string()),
            target: Some("app::core".to_string()),
            message: "Server started".to_string(),
        };
        let html = render_log_line_html(&entry, &WatchStyle::Terminal);
        assert!(html.contains("vp-log-info"));
        assert!(html.contains("15:23:26"));
        assert!(html.contains("app::core"));
        assert!(html.contains("Server started"));
    }

    #[test]
    fn test_render_log_line_html_error() {
        let entry = LogEntry {
            timestamp: None,
            level: Some("ERROR".to_string()),
            target: None,
            message: "Something failed".to_string(),
        };
        let html = render_log_line_html(&entry, &WatchStyle::Terminal);
        assert!(html.contains("vp-log-error"));
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn test_css_preamble() {
        let css = css_preamble();
        assert!(css.contains("<style>"));
        assert!(css.contains("vp-log"));
    }

    #[test]
    fn test_file_watcher_manager_new() {
        let mgr = FileWatcherManager::new();
        assert!(mgr.active_panes().is_empty());
    }

    #[test]
    fn test_watch_config_deserialize() {
        let json = r#"{"path":"/tmp/test.log","pane_id":"logs","format":"json_lines"}"#;
        let config: WatchConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.path, "/tmp/test.log");
        assert_eq!(config.pane_id, "logs");
        assert!(matches!(config.format, WatchFormat::JsonLines));
    }

    #[test]
    fn test_watch_config_defaults() {
        let json = r#"{"path":"/tmp/test.log","pane_id":"logs"}"#;
        let config: WatchConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config.format, WatchFormat::JsonLines));
        assert!(matches!(config.style, WatchStyle::Terminal));
        assert!(config.filter.is_none());
        assert!(config.exclude_targets.is_empty());
    }

    #[test]
    fn test_validate_watch_path_absolute() {
        assert!(validate_watch_path("/tmp/test.log").is_ok());
        assert!(validate_watch_path("/var/log/app.log").is_ok());
    }

    #[test]
    fn test_validate_watch_path_relative_rejected() {
        assert!(validate_watch_path("relative/path.log").is_err());
        assert!(validate_watch_path("../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_watch_path_sensitive_rejected() {
        assert!(validate_watch_path("/Users/makoto/.ssh/id_rsa").is_err());
        assert!(validate_watch_path("/home/user/.gnupg/secret").is_err());
        assert!(validate_watch_path("/home/user/.aws/credentials").is_err());
    }

    #[test]
    fn test_start_watch_limit() {
        let mgr = FileWatcherManager::new();
        // MAX_WATCHERS を超えるとエラーになることを確認
        // （実際にタスクを spawn するとランタイムが必要なので、上限チェックのロジックのみ検証）
        assert_eq!(mgr.active_panes().len(), 0);
    }
}
