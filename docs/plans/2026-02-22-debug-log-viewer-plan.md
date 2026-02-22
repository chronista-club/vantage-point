# Debug Log Viewer 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** MCP → QUIC → Stand のメッセージフローをリアルタイムに追跡できるデバッグログビュワーを構築する

**Architecture:** 両プロセス（`vp mcp` / `vp start`）から共通ログファイルに JSON Lines 形式で構造化ログを書き出し、Stand がファイル監視で Canvas WebSocket にストリーム配信する。各リクエストに `trace_id` を付与し、1リクエストの全ステップをフィルタリング可能にする。

**Tech Stack:** tracing + tracing-appender（ログ書き出し）、notify（ファイル監視）、Canvas HTML/JS（ビュワーUI）

---

### Task 1: 共通ログ構造体とヘルパーの定義

**Files:**
- Create: `crates/vantage-point/src/trace_log.rs`
- Modify: `crates/vantage-point/src/lib.rs` (モジュール追加)

**Step 1: trace_log.rs を作成**

```rust
//! デバッグトレースログ
//!
//! MCP / Stand 両プロセスで共通のログファイルに書き出す。
//! trace_id でリクエスト単位のフィルタリングが可能。

use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// trace_id を生成（プロセス内でユニーク）
pub fn new_trace_id() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("t-{:04x}", n)
}

/// ログファイルのパスを返す
pub fn log_file_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vantage")
        .join("logs");
    std::fs::create_dir_all(&config_dir).ok();
    config_dir.join("debug.log")
}

/// 構造化ログエントリ
#[derive(Debug, Serialize)]
pub struct TraceEntry {
    pub ts: String,
    pub process: &'static str,
    pub trace_id: String,
    pub step: String,
    pub level: &'static str,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}
```

コンストラクタ、ファイル初期化、write 関数も含む。

**Step 2: lib.rs にモジュール追加**

**Step 3: Cargo.toml に chrono 依存追加**

**Step 4: ビルド確認**

Run: `cargo build -p vantage-point 2>&1 | tail -5`

**Step 5: コミット**

```
feat: trace_log モジュール追加（デバッグログ基盤）
```

---

### Task 2: MCP プロセスにトレースログを追加

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs`

**Step 1: run_mcp_server() でログ初期化**

**Step 2: ensure_channels() にログ追加**

各ステップ（connect, open_channel）に write_trace を挿入。

**Step 3: stand_call() にログ追加**

trace_id を生成し、request 送信 / response 受信 / error をログ。elapsed_ms も記録。

**Step 4: canvas_call() にも同様のログ追加**

**Step 5: ビルド確認**

**Step 6: コミット**

```
feat: MCP プロセスにトレースログ追加
```

---

### Task 3: Stand プロセスにトレースログを追加

**Files:**
- Modify: `crates/vantage-point/src/stand/unison_server.rs`
- Modify: `crates/vantage-point/src/stand/server.rs`

**Step 1: server.rs の run() でログ初期化**

**Step 2: unison_server.rs の stand チャネルにログ追加**

リクエスト受信(receive)、処理完了(respond)をログ。

**Step 3: canvas チャネルにも同様のログ追加**

**Step 4: QUIC サーバー起動時のログ**

**Step 5: ビルド確認**

**Step 6: 動作テスト — curl でログファイル確認**

**Step 7: コミット**

```
feat: Stand プロセスにトレースログ追加
```

---

### Task 4: ログファイル監視 → WebSocket 配信

**Files:**
- Modify: `crates/vantage-point/src/stand/server.rs`
- Modify: `crates/vantage-point/src/protocol/messages.rs`
- Modify: `crates/vantage-point/src/trace_log.rs`
- Modify: `Cargo.toml` (notify 追加)

**Step 1: Cargo.toml に notify 追加**

**Step 2: StandMessage に TraceLog バリアントを追加**

**Step 3: trace_log.rs に watch_and_broadcast() を追加**

notify クレートでログファイルを監視し、新しい行を Hub 経由で WebSocket broadcast。

**Step 4: server.rs で debug_mode 有効時に監視タスク起動**

**Step 5: ビルド確認**

**Step 6: コミット**

```
feat: ログファイル監視 → WebSocket broadcast
```

---

### Task 5: Canvas ログビュワー UI

**Files:**
- Modify: `web/canvas.html`

**Step 1: ログパネル HTML/CSS 追加**

- 画面下部にトグル式ログパネル（高さ40%）
- プロセスごとに色分け（mcp: 青系、stand: 緑系）
- monospace フォント、12px

**Step 2: フィルタバー実装**

- trace_id / process / level でフィルタリング
- リアルタイム絞り込み

**Step 3: WebSocket ハンドラ追加**

trace_log メッセージを受信して addTraceLog() で DOM に追加。
テキストコンテンツは textContent で安全に挿入。data は JSON.stringify + textContent で展開。

**Step 4: トグルボタンと自動スクロール**

**Step 5: ビルド・インストール・動作確認**

**Step 6: コミット**

```
feat: Canvas ログビュワー UI
```

---

### Task 6: 統合テスト & MCP 経由テスト

**Step 1: フルフロー確認（VP再起動 → Canvas → MCP show → ログ確認）**

**Step 2: trace_id フィルタ確認**

**Step 3: tail -f でファイルログ確認**

**Step 4: 微調整があればコミット**
