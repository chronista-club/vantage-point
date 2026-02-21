# Canvas 復活 + QUIC Readiness 設計

## 概要

MCP `open_canvas` / `close_canvas` が動作しない問題 (#51) を解決する。
根本原因は2つ: (1) `vp webview` コマンドが未定義、(2) QUIC サーバー起動前に running.json 登録。

## 変更方針

### 1. `vp canvas` サブコマンド追加

`canvas.rs` の既存 `run_canvas(port)` をサブコマンドとして公開。

```
vp canvas --port 33000
```

Stand の canvas handler が `vp canvas --port <port>` を spawn する。

**ファイル:**
- `main.rs`: `Canvas` variant 追加
- `commands/canvas.rs` (新規): `execute(port)` → `canvas::run_canvas(port)`
- `stand/unison_server.rs`: `handle_canvas_open` の spawn コマンドを `vp canvas` に変更

### 2. QUIC Readiness Signal

`start_unison_server()` が `oneshot::Sender` で listen 完了を通知。
`server.rs` が `await` してから `RunningStands::register()` を呼ぶ。

**変更前:**
```rust
// server.rs — fire-and-forget
tokio::spawn(start_unison_server(state, port));
RunningStands::register(port, ...);  // QUIC未起動の可能性
```

**変更後:**
```rust
// server.rs — readiness 待ち
let (ready_tx, ready_rx) = oneshot::channel();
tokio::spawn(start_unison_server(state, port, ready_tx));
let _ = ready_rx.await;  // バインド完了を待つ
RunningStands::register(port, ...);  // QUIC 確実に起動済み
```

**unison_server.rs の変更:**

`spawn_listen()` を使って `ServerHandle` を取得し、ready signal を送信。
```rust
pub async fn start_unison_server(
    state: Arc<AppState>,
    http_port: u16,
    ready_tx: oneshot::Sender<()>,
) {
    // ... register_channel ...

    let handle = server.spawn_listen(&addr).await;
    match handle {
        Ok(handle) => {
            let _ = ready_tx.send(());  // バインド完了通知
            // handle を保持してシャットダウンに備える
        }
        Err(e) => {
            tracing::error!("QUIC server failed: {}", e);
            let _ = ready_tx.send(());  // エラーでも通知（ブロック防止）
        }
    }
}
```

## 影響ファイル

| ファイル | 変更内容 |
|---------|---------|
| `main.rs` | `Canvas { port }` コマンド追加 |
| `commands/canvas.rs` (新規) | canvas コマンドの execute 関数 |
| `stand/unison_server.rs` | `vp webview` → `vp canvas` + readiness signal 引数追加 |
| `stand/server.rs` | QUIC ready を await してから register |

## データフロー

```
MCP open_canvas
  → canvas_call("open")
  → QUIC "canvas" channel
  → handle_canvas_open()
  → spawn "vp canvas --port 33000"
  → wry WebView 起動
```

## 検証

1. `cargo build` — ビルド通過
2. `cargo test --workspace` — テスト通過
3. `vp start` → MCP `open_canvas` → Canvas ウィンドウが開く
4. MCP `close_canvas` → Canvas ウィンドウが閉じる
5. Stand 起動直後の `open_canvas` が QUIC 接続に成功する（レース解消）
