# Daemon + PTY直接管理 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** tmux依存を排除し、VP独自のDaemonベースプロセス管理でリアルタイムターミナル体験を実現する

**Architecture:** 常駐Daemon がPTYプロセスを所有し、Unison Protocol (QUIC) 経由でConsole（ビューア）にPTY出力をリアルタイム転送する。Consoleは何度でも接続/切断可能で、Daemon生存中はプロセスが存続する。

**Tech Stack:** Rust, Tokio, portable-pty, Unison Protocol (QUIC/quinn), tao + CoreText (macOS native)

**設計書:** `docs/plans/2026-02-21-daemon-process-management-design.md`

---

## Phase 1: Daemon基盤

### Task 1: Daemonデータモデル定義

**Files:**
- Create: `crates/vantage-point/src/daemon/mod.rs`
- Create: `crates/vantage-point/src/daemon/registry.rs`
- Modify: `crates/vantage-point/src/main.rs:22` (mod daemon 追加)
- Test: `crates/vantage-point/src/daemon/registry.rs` (インラインテスト)

**Step 1: daemon モジュール作成**

`daemon/mod.rs`:
```rust
//! VP Daemon — プロセス管理デーモン
//!
//! PTYプロセスを所有し、Unison Protocol経由でConsoleに出力を転送する。
//! Daemon生存中はプロセスが存続し、Console（vp start）は何度でも接続/切断可能。

pub mod registry;
```

**Step 2: SessionRegistry / Session / Pane のデータモデル**

`daemon/registry.rs` に以下を実装:

```rust
//! セッション・ペインの管理レジストリ
//!
//! Daemon が管理するセッション（タブ）とペイン（プロセス）のデータ構造。

use std::collections::HashMap;
use serde::{Serialize, Deserialize};

pub type SessionId = String;
pub type PaneId = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PaneKind {
    /// PTYプロセス（shell / claude cli等）
    Pty {
        pid: u32,
        shell_cmd: String,
    },
    /// コンテンツ表示（show コマンド用）
    Content {
        content_type: String,
        body: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: PaneId,
    pub kind: PaneKind,
    pub cols: u16,
    pub rows: u16,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub panes: Vec<PaneInfo>,
    pub created_at: u64,
}

/// セッション・ペインの管理レジストリ
pub struct SessionRegistry {
    sessions: HashMap<SessionId, Session>,
    default_session: Option<SessionId>,
}

/// セッション（内部状態）
struct Session {
    info: SessionInfo,
    next_pane_id: PaneId,
}

impl SessionRegistry {
    pub fn new() -> Self { ... }
    pub fn create_session(&mut self, id: &str) -> &SessionInfo { ... }
    pub fn remove_session(&mut self, id: &str) -> bool { ... }
    pub fn get_session(&self, id: &str) -> Option<&SessionInfo> { ... }
    pub fn list_sessions(&self) -> Vec<SessionInfo> { ... }
    pub fn add_pane(&mut self, session_id: &str, kind: PaneKind, cols: u16, rows: u16) -> Option<PaneId> { ... }
    pub fn remove_pane(&mut self, session_id: &str, pane_id: PaneId) -> bool { ... }
    pub fn default_session(&self) -> Option<&str> { ... }
    pub fn set_default_session(&mut self, id: &str) { ... }
}
```

**Step 3: テストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_list_sessions() { ... }

    #[test]
    fn test_add_and_remove_panes() { ... }

    #[test]
    fn test_default_session() { ... }

    #[test]
    fn test_remove_session() { ... }
}
```

**Step 4: テスト実行**

Run: `cargo test -p vantage-point registry`
Expected: PASS

**Step 5: main.rs に mod daemon 追加**

`main.rs` の mod 宣言に `mod daemon;` を追加。

**Step 6: コミット**

```bash
git add crates/vantage-point/src/daemon/ crates/vantage-point/src/main.rs
git commit -m "feat: daemon モジュールと SessionRegistry データモデル追加"
```

---

### Task 2: PtySlot — PTYプロセスの直接管理

**Files:**
- Create: `crates/vantage-point/src/daemon/pty_slot.rs`
- Modify: `crates/vantage-point/src/daemon/mod.rs` (pub mod pty_slot 追加)
- Reference: `crates/vantage-point/src/stand/pty.rs` (PtySession のロジックを活用)

**Step 1: PtySlot 構造体を実装**

`daemon/pty_slot.rs` に以下を実装:

```rust
//! PTYスロット — 個々のPTYプロセスの管理
//!
//! portable-pty で PTY を作成し、master fd からの出力を
//! broadcast channel 経由で配信する。
//! 既存の `stand/pty.rs` の PtySession を基に、Daemon用に再設計。

use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::broadcast;

pub struct PtySlot {
    writer: Box<dyn std::io::Write + Send>,
    pair: portable_pty::PtyPair,
    pid: u32,
    shell_cmd: String,
    output_tx: broadcast::Sender<Vec<u8>>,
}

impl PtySlot {
    /// PTYプロセスを起動
    pub fn spawn(
        cwd: &str,
        shell_cmd: &str,
        cols: u16,
        rows: u16,
    ) -> Result<Self> { ... }

    /// PTY に入力を書き込む
    pub fn write(&mut self, data: &[u8]) -> Result<()> { ... }

    /// PTY をリサイズ
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> { ... }

    /// 出力ストリームを購読（broadcast receiver）
    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>> { ... }

    /// プロセスID
    pub fn pid(&self) -> u32 { ... }

    /// シェルコマンド
    pub fn shell_cmd(&self) -> &str { ... }
}
```

**重要な違い（stand/pty.rs との差分）:**
- output は broadcast channel 経由（WebSocketではなくDaemonのIPC層が消費）
- reader task は tokio::task::spawn_blocking でバイト列をそのまま broadcast
- base64エンコードはしない（IPC層の責務）

**Step 2: PTY読み取りタスク**

```rust
/// PTY出力読み取りタスクを起動
fn start_reader_task(
    reader: Box<dyn std::io::Read + Send>,
    tx: broadcast::Sender<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => { let _ = tx.send(buf[..n].to_vec()); }
                Err(_) => break,
            }
        }
    })
}
```

**Step 3: テスト**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pty_spawn_and_output() {
        // echo コマンドで PTY 起動 → 出力を受信できることを確認
    }

    #[tokio::test]
    async fn test_pty_write_input() {
        // PTY にコマンド送信 → 出力が返ることを確認
    }
}
```

**Step 4: テスト実行**

Run: `cargo test -p vantage-point pty_slot`
Expected: PASS

**Step 5: コミット**

```bash
git add crates/vantage-point/src/daemon/pty_slot.rs crates/vantage-point/src/daemon/mod.rs
git commit -m "feat: PtySlot — PTYプロセスの直接管理（broadcast output）"
```

---

### Task 3: Daemon Unison Server — チャネルハンドラ

**Files:**
- Create: `crates/vantage-point/src/daemon/server.rs`
- Create: `crates/vantage-point/src/daemon/protocol.rs`
- Modify: `crates/vantage-point/src/daemon/mod.rs`
- Reference: `crates/vantage-point/src/stand/unison_server.rs` (既存のUnison使い方)

**Step 1: プロトコルメッセージ定義**

`daemon/protocol.rs`:
```rust
//! Daemon IPC プロトコルのメッセージ型定義

use serde::{Serialize, Deserialize};

// --- Session Channel ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AttachRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DetachRequest {
    pub session_id: String,
}

// --- Terminal Channel ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePaneRequest {
    pub session_id: String,
    #[serde(default = "default_shell")]
    pub shell_cmd: String,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WriteRequest {
    pub session_id: String,
    pub pane_id: u32,
    pub data: String, // base64
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResizeRequest {
    pub session_id: String,
    pub pane_id: u32,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KillPaneRequest {
    pub session_id: String,
    pub pane_id: u32,
}
```

**Step 2: Daemon Unison Server 実装**

`daemon/server.rs`:
```rust
//! Daemon の Unison QUIC サーバー
//!
//! session / terminal / system の3チャネルを提供。
//! Console (vp start) からの接続を受け付け、PTY I/Oを中継する。

use std::sync::Arc;
use tokio::sync::RwLock;
use unison::network::{ProtocolServer, UnisonServer, UnisonServerExt};

use super::registry::SessionRegistry;

pub struct DaemonState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub pty_slots: Arc<RwLock<HashMap<(SessionId, PaneId), PtySlot>>>,
}

pub async fn start_daemon_server(state: Arc<DaemonState>, port: u16) {
    let mut server = ProtocolServer::new();

    // session.create ハンドラ
    // session.list ハンドラ
    // session.attach ハンドラ (→ PTY output ストリーム開始)
    // session.detach ハンドラ

    // terminal.create_pane ハンドラ (→ PtySlot::spawn)
    // terminal.write ハンドラ (→ PtySlot::write)
    // terminal.resize ハンドラ (→ PtySlot::resize)
    // terminal.kill_pane ハンドラ

    // system.health ハンドラ
    // system.shutdown ハンドラ

    server.listen(&format!("[::1]:{}", port)).await;
}
```

**Step 3: テスト**

```rust
#[cfg(test)]
mod tests {
    // Unison Server + Client の結合テスト
    // 1. サーバー起動
    // 2. クライアント接続
    // 3. session.create RPC
    // 4. session.list RPC → 作成したセッションが返る
    // 5. terminal.create_pane RPC
    // 6. terminal.write → PTY output event 受信
}
```

**Step 4: テスト実行**

Run: `cargo test -p vantage-point daemon::server`
Expected: PASS

**Step 5: コミット**

```bash
git add crates/vantage-point/src/daemon/
git commit -m "feat: Daemon Unison Server — session/terminal/system チャネル"
```

---

### Task 4: Daemon プロセス管理（fork / PID / シグナル）

**Files:**
- Create: `crates/vantage-point/src/daemon/process.rs`
- Modify: `crates/vantage-point/src/commands/daemon.rs` (新しいDaemon起動ロジック)
- Modify: `crates/vantage-point/src/daemon/mod.rs`

**Step 1: Daemon プロセス管理**

`daemon/process.rs`:
```rust
//! Daemon プロセスのライフサイクル管理
//!
//! fork でバックグラウンド化、PIDファイルで生存確認、
//! シグナルハンドリングでグレースフル停止。

use anyhow::Result;
use std::path::PathBuf;

/// Daemon の PID/ソケットファイルのパス
pub fn daemon_dir() -> PathBuf {
    std::env::temp_dir().join("vantage-point")
}

pub fn pid_file() -> PathBuf {
    daemon_dir().join("daemon.pid")
}

/// Daemon が生きているか確認
pub fn is_daemon_running() -> Option<u32> {
    let pid_path = pid_file();
    if !pid_path.exists() { return None; }
    let pid_str = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
    if alive { Some(pid) } else { None }
}

/// PIDファイルを書き出す
pub fn write_pid_file() -> Result<()> { ... }

/// PIDファイルを削除
pub fn remove_pid_file() { ... }

/// Daemon をフォアグラウンドで起動（vp daemon start から呼ばれる）
pub async fn run_daemon(port: u16) -> Result<()> {
    write_pid_file()?;
    // シグナルハンドラ登録 (SIGTERM, SIGINT)
    // DaemonState 初期化
    // Unison Server 起動
    // シャットダウン待機
    // クリーンアップ
    remove_pid_file();
    Ok(())
}

/// Daemon をバックグラウンドで自動起動（vp start から呼ばれる）
pub fn ensure_daemon_running(port: u16) -> Result<u32> {
    if let Some(pid) = is_daemon_running() {
        return Ok(pid);
    }
    // fork して daemon 起動
    // `vp daemon start --port {port}` をバックグラウンドで実行
    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["daemon", "start", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(child.id())
}
```

**Step 2: commands/daemon.rs を更新**

```rust
DaemonCommands::Start { port } => {
    println!("Starting VP Daemon on port {}...", port);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::daemon::process::run_daemon(port))
}
```

**Step 3: テスト**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_is_daemon_running_no_pid_file() {
        // PIDファイルなし → None
    }

    #[test]
    fn test_daemon_dir_paths() {
        // パスが正しいことを確認
    }
}
```

**Step 4: テスト実行**

Run: `cargo test -p vantage-point daemon::process`
Expected: PASS

**Step 5: コミット**

```bash
git add crates/vantage-point/src/daemon/process.rs crates/vantage-point/src/commands/daemon.rs
git commit -m "feat: Daemon プロセス管理（PIDファイル、自動起動、シグナル処理）"
```

---

## Phase 2: Console接続

### Task 5: vp start → Daemon接続 + Unison Client

**Files:**
- Modify: `crates/vantage-point/src/commands/start.rs`
- Create: `crates/vantage-point/src/daemon/client.rs`
- Modify: `crates/vantage-point/src/daemon/mod.rs`

**Step 1: Daemon Client 実装**

`daemon/client.rs`:
```rust
//! Daemon への Unison クライアント
//!
//! Console (vp start) から Daemon に接続し、
//! セッション操作・PTY I/O を行う。

use anyhow::Result;
use unison::network::ProtocolClient;

pub struct DaemonClient {
    client: ProtocolClient,
}

impl DaemonClient {
    /// Daemon に接続（リトライ付き）
    pub async fn connect(port: u16, retries: u32) -> Result<Self> { ... }

    // Session operations
    pub async fn create_session(&self, id: &str) -> Result<SessionInfo> { ... }
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> { ... }
    pub async fn attach(&self, id: &str) -> Result<()> { ... }
    pub async fn detach(&self, id: &str) -> Result<()> { ... }

    // Terminal operations
    pub async fn create_pane(&self, session_id: &str, shell: &str, cols: u16, rows: u16) -> Result<PaneId> { ... }
    pub async fn write_input(&self, session_id: &str, pane_id: u32, data: &[u8]) -> Result<()> { ... }
    pub async fn resize_pane(&self, session_id: &str, pane_id: u32, cols: u16, rows: u16) -> Result<()> { ... }
    pub async fn kill_pane(&self, session_id: &str, pane_id: u32) -> Result<()> { ... }

    // System
    pub async fn health(&self) -> Result<()> { ... }
    pub async fn shutdown(&self) -> Result<()> { ... }
}
```

**Step 2: commands/start.rs を更新**

起動フローを変更:
```rust
// 現行: Stand サーバー起動 → WebView/TerminalWindow
// 新: Daemon 接続 → セッション確認/作成 → TerminalWindow
```

```rust
pub fn execute(opts: StartOptions) -> Result<()> {
    // ... (ポート・プロジェクト解決は既存ロジック維持) ...

    let daemon_port = 34000; // Daemon の QUIC ポート

    // 1. Daemon 接続確認・自動起動
    daemon::process::ensure_daemon_running(daemon_port)?;

    // 2. Daemon Client 接続
    let rt = tokio::runtime::Runtime::new()?;
    let client = rt.block_on(DaemonClient::connect(daemon_port, 30))?;

    // 3. セッション確認・作成
    let sessions = rt.block_on(client.list_sessions())?;
    let project_name = resolved_project_dir.rsplit('/').next().unwrap_or("default");
    let session = if let Some(s) = sessions.iter().find(|s| s.id == project_name) {
        s.clone()
    } else {
        rt.block_on(client.create_session(project_name))?
    };

    // 4. Native Terminal Window 起動（Daemon Client を渡す）
    terminal_window::run_terminal_with_daemon(client, session)?;

    Ok(())
}
```

**Step 3: テスト**

```rust
// 結合テスト: Daemon起動 → Client接続 → セッション作成 → リスト確認
```

**Step 4: コミット**

```bash
git add crates/vantage-point/src/daemon/client.rs crates/vantage-point/src/commands/start.rs
git commit -m "feat: vp start → Daemon 自動起動 + Unison Client 接続"
```

---

### Task 6: terminal_window.rs — tmux依存除去 + Daemon IPC

**Files:**
- Modify: `crates/vantage-point/src/terminal_window.rs`

**Step 1: tmux ポーリングを Daemon Event push に置き換え**

現行の `start_status_poller`（tmux capture-pane 200ms ポーリング）を削除し、Daemon の output broadcast を受信するスレッドに置き換え:

```rust
// 現行: start_status_poller (tmux capture-pane 200ms polling)
// 新: start_daemon_receiver (Unison event push)

fn start_daemon_receiver(
    client: Arc<DaemonClient>,
    session_id: String,
    proxy: EventLoopProxy<TerminalEvent>,
) {
    std::thread::Builder::new()
        .name("daemon-receiver".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // attach して PTY output stream を受信
                let mut rx = client.attach_output(&session_id).await.unwrap();
                loop {
                    match rx.recv().await {
                        Ok(PtyOutput { pane_id, data }) => {
                            let _ = proxy.send_event(TerminalEvent::Output(data));
                        }
                        Err(_) => break,
                    }
                }
            });
        })
        .expect("daemon-receiver スレッドの起動に失敗");
}
```

**Step 2: 入力送信を Daemon RPC に置き換え**

現行の `WsBridgeCommand::Input` → WebSocket → Stand → tmux send-keys を:

```rust
// 現行: input_tx.send(WsBridgeCommand::Input(encoded))
// 新: daemon_client.write_input(session_id, pane_id, &bytes)
```

**Step 3: ステータスバーを Daemon メタデータに置き換え**

現行の `refresh_status`（tmux list-windows CLI）を:

```rust
// 現行: tmux list-windows → StatusBarInfo パース
// 新: client.list_sessions() / get_session() → PaneInfo からステータス構築
```

**Step 4: tmux_command_and_refresh を削除**

ウィンドウ切替・作成・削除のtmuxコマンドを Daemon RPC に置き換え:

```rust
// Cmd+T: client.create_pane(session_id, shell, cols, rows)
// Cmd+W: client.kill_pane(session_id, pane_id)
// Cmd+1-9: Console側タブ切替（Daemon不要、active pane変更のみ）
```

**Step 5: WebSocket bridge (start_terminal_bridge) を削除**

Daemon IPC が代替するため、Stand WebSocket 経由のブリッジは不要。

**Step 6: テスト**

手動テスト:
1. `vp daemon start` → Daemon 起動確認
2. `vp start` → ネイティブウィンドウ表示、シェルプロンプト表示
3. コマンド入力 → 即座に出力表示（ポーリング遅延なし）
4. Cmd+T → 新しいペイン
5. ウィンドウ閉じる → `vp start` → 再接続

**Step 7: コミット**

```bash
git add crates/vantage-point/src/terminal_window.rs
git commit -m "feat: terminal_window tmux依存除去、Daemon IPC に完全移行"
```

---

### Task 7: コピー&ペースト・選択の維持

**Files:**
- Verify: `crates/vantage-point/src/terminal_window.rs` (既存ロジック維持)
- Verify: `crates/vantage-point/src/terminal/renderer.rs` (変更なし)

**Step 1: 動作確認**

以下が引き続き動作することを確認:
- マウスドラッグでテキスト選択
- Cmd+C でクリップボードにコピー
- Cmd+V でペースト（pbpaste → Daemon write_input）
- Escape で選択解除

**Step 2: pbpaste → Daemon write_input に修正**

```rust
// 現行: input_tx.send(WsBridgeCommand::Input(encoded))
// 新: daemon_client.write_input(session_id, active_pane_id, &bytes)
```

**Step 3: コミット（変更があれば）**

```bash
git commit -m "fix: コピー&ペーストを Daemon IPC 経由に更新"
```

---

## Phase 3: MCP統合 + tmux完全削除

### Task 8: MCP → Daemon IPC ブリッジ

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs`
- Modify: `crates/vantage-point/src/stand/unison_server.rs`

**Step 1: MCP ツールを Daemon 経由に更新**

MCP の `show` / `split_pane` / `close_pane` 等のツールが Daemon のセッション/ペインを操作するように更新。

**Step 2: コミット**

```bash
git commit -m "feat: MCP ツールを Daemon IPC 経由に更新"
```

---

### Task 9: tmux.rs 削除 + クリーンアップ

**Files:**
- Delete: `crates/vantage-point/src/stand/tmux.rs`
- Modify: `crates/vantage-point/src/stand/mod.rs` (pub mod tmux 削除)
- Modify: `crates/vantage-point/src/stand/state.rs` (tmux_manager, use_tmux 削除)
- Modify: `crates/vantage-point/src/stand/server.rs` (TmuxManager 依存削除)
- Modify: `crates/vantage-point/src/stand/routes/ws.rs` (tmux 分岐削除)

**Step 1: tmux.rs 削除**

```bash
rm crates/vantage-point/src/stand/tmux.rs
```

**Step 2: 参照箇所をすべて除去**

- `stand/mod.rs`: `pub mod tmux;` 削除
- `stand/state.rs`: `TmuxManager` / `use_tmux` フィールド削除
- `stand/server.rs`: `TmuxManager::is_available()` / `tmux_manager` 初期化削除
- `stand/routes/ws.rs`: `if state.use_tmux { ... }` 分岐削除、Daemon経由に統一

**Step 3: ビルド確認**

Run: `cargo build -p vantage-point`
Expected: 成功（tmux参照エラーなし）

**Step 4: テスト実行**

Run: `cargo test --workspace`
Expected: 全テスト PASS

**Step 5: コミット**

```bash
git add -A
git commit -m "refactor: tmux.rs 削除、tmux依存を完全排除"
```

---

### Task 10: CI確認 + リリース準備

**Step 1: フルCI確認**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release -p vantage-point
```

**Step 2: 動作確認チェックリスト**

- [ ] `vp daemon start` → Daemon 起動
- [ ] `vp start` → Daemon 自動起動 + ネイティブウィンドウ表示
- [ ] シェルプロンプト表示（リアルタイム）
- [ ] コマンド入力・出力（レイテンシ改善確認）
- [ ] Cmd+T → 新規ペイン
- [ ] Cmd+W → ペイン終了
- [ ] Cmd+1-9 → タブ切替
- [ ] Cmd+C/V → コピー&ペースト
- [ ] ウィンドウ閉じる → `vp start` → 再接続
- [ ] `vp daemon stop` → 全プロセス終了
- [ ] `vp ps` → セッション一覧表示
- [ ] MCP show/split_pane 動作

**Step 3: コミット**

```bash
git commit -m "chore: CI確認、tmux→daemon移行完了"
```

---

## 依存関係

```
Task 1 (データモデル)
  └── Task 2 (PtySlot)
       └── Task 3 (Unison Server)
            └── Task 4 (Daemon プロセス管理)
                 └── Task 5 (vp start + Client)
                      └── Task 6 (terminal_window 移行)
                           └── Task 7 (コピペ維持)
                                └── Task 8 (MCP統合)
                                     └── Task 9 (tmux削除)
                                          └── Task 10 (CI + リリース)
```

全タスクが直列依存。Phase 1 (Task 1-4) が土台、Phase 2 (Task 5-7) が体験、Phase 3 (Task 8-10) がクリーンアップ。
