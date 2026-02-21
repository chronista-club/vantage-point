# Daemon + PTY直接管理 設計書

> tmux依存を排除し、VP独自のDaemonベースプロセス管理を実現する

**日付**: 2026-02-21
**ステータス**: 承認済み

---

## 背景と動機

### 現行アーキテクチャの課題

- **pipe-pane不安定**: macOSでFIFOのブロッキングセマンティクスにより pipe-pane が動作しない
- **200msポーリング**: capture-pane ポーリングによるレイテンシ
- **CLIオーバーヘッド**: 毎回 `tmux` プロセスを起動するコスト（30箇所以上）
- **外部依存**: tmuxのインストールが前提

### 目標

1. **外部依存排除**: tmuxなしでVP単体で完結
2. **セッション生存**: VPクラッシュ時もプロセス存続
3. **マルチウィンドウ**: 複数プロセス並列管理
4. **低レイテンシ**: ポーリングではなくリアルタイムPTY出力転送
5. **メッシュ拡張**: 将来のStand間通信への拡張ポイント

---

## UXビジョン

```
$ vp start
  → Daemonに接続（なければ自動起動）
  → セッションなし → プロジェクト名で自動作成
  → セッションあり → タブ表示で再接続
  → ウィンドウ閉じる → プロセスはDaemonで生存
  → vp start 再実行 → 既存セッションに再接続
```

### タブとペインの概念

- **タブ = セッション**: プロジェクト単位
- **ペイン = プロセス**: 各タブ内にPTY（shell/claude cli）やStand TUI、showコンテンツが分割表示

```
vp start（ビューア/コンソール）
  ├── tab: vantage-point
  │     ├── pane: zsh (PTY)
  │     ├── pane: Stand TUI
  │     └── pane: show content (markdown/HTML)
  └── tab: creo-memories
        ├── pane: claude cli (PTY)
        └── pane: Stand TUI
```

### 操作

- セッション/ペインの追加: MCP or 手動
- Cmd+T: 新規ペイン（shell）
- Cmd+W: ペイン終了
- Cmd+1-9: タブ切替

---

## 全体アーキテクチャ

```
┌─ VP Daemon (常駐) ──────────────────────────────────┐
│                                                     │
│  SessionRegistry                                    │
│  ├── Session "vantage-point"                        │
│  │     ├── PtySlot 0: zsh                           │
│  │     └── PtySlot 1: claude cli                    │
│  └── Session "creo-memories"                        │
│        └── PtySlot 0: zsh                           │
│                                                     │
│  Unison Server (QUIC)                               │
│  └── listen [::1]:34000                             │
│       ├── "terminal" channel                        │
│       │    ├── RPC: create/kill/resize/list          │
│       │    ├── Event: PTY output (raw bytes 0x01)   │
│       │    └── RPC: write_input                     │
│       ├── "session" channel                         │
│       │    ├── RPC: create/list/attach/detach       │
│       │    └── Event: session状態変更通知            │
│       └── "system" channel                          │
│            ├── RPC: health/shutdown                  │
│            └── Event: daemon状態                     │
│                                                     │
│  ※ 将来: "messaging" channel (Stand間通信)           │
└─────────────────────────────────────────────────────┘
         │
         │ QUIC (Unison Protocol)
         │
┌────────┴────────────────────────────────────────────┐
│  VP Console (vp start)                              │
│  Unison Client                                      │
│  ├── "session" channel → セッション一覧・タブ管理     │
│  ├── "terminal" channel → PTY I/O (リアルタイム)     │
│  └── CoreText ネイティブレンダラー                    │
└─────────────────────────────────────────────────────┘
```

### 核心的な分離

- **Daemon**: プロセスの所有者。PTYのmaster fdを保持。常駐
- **Console**: 表示と入力のみ。何度でも接続/切断可能

### 通信: Unison Protocol

- **トランスポート**: QUIC (quinn) + TLS 1.3
- **通信パターン**: Unified Channel（RPC + Event Push + Raw Bytes）
- **PTY出力**: Raw Bytes (type tag 0x01) でリアルタイム転送
- **参考実装**: cplp-sound-systemのメッシュP2Pネットワーク

---

## Daemonライフサイクル

```
$ vp start
     │
     ▼
  Daemon接続試行 (QUIC → [::1]:34000)
     ├─ 失敗 → fork で Daemon 自動起動
     │          PIDファイル: /tmp/vantage-point/daemon.pid
     │          接続リトライ → 成功
     └─ 成功
          │
          ▼
     session.list() RPC
     ├─ セッションあり → タブ復元 + attach
     └─ セッションなし → プロジェクト名で自動作成 + attach
```

- `vp daemon stop`: 全セッションのPTYにSIGHUP → クリーンアップ → 終了
- 異常終了時: PTY子プロセスは孤児化して生存。Daemon再起動時にrunning.jsonからPID復元

---

## データモデル

```rust
SessionRegistry {
    sessions: HashMap<SessionId, Session>,
    default_session: Option<SessionId>,
}

Session {
    id: SessionId,          // "vantage-point", "creo-memories"
    panes: Vec<Pane>,
    layout: PaneLayout,     // 分割情報（将来）
    created_at: Timestamp,
}

Pane {
    id: PaneId,             // 連番 0, 1, 2...
    kind: PaneKind,
        // Pty { pid, master_fd, shell_cmd }
        // Content { content_type, body }
    cols: u16,
    rows: u16,
    active: bool,
}
```

---

## Unison チャネル設計

| チャネル | メソッド | 方向 | 用途 |
|---------|---------|------|------|
| `session` | `create` | RPC | セッション作成 |
| `session` | `list` | RPC | セッション一覧 |
| `session` | `attach` | RPC → Event push開始 | セッションに接続 |
| `session` | `detach` | RPC | 接続解除 |
| `terminal` | `create_pane` | RPC | 新規ペイン（shell起動） |
| `terminal` | `write` | RPC | PTY入力送信 |
| `terminal` | `resize` | RPC | リサイズ |
| `terminal` | `kill_pane` | RPC | ペイン終了 |
| `terminal` | (event push) | Event | PTY出力バイナリ (raw bytes) |
| `system` | `health` | RPC | ヘルスチェック |
| `system` | `shutdown` | RPC | Daemon停止 |

### attach フロー

1. Console → `session.attach("vantage-point")` RPC
2. Daemon → 全ペインの現在スクリーンバッファ送信
3. Daemon → 以降、PTY出力を Event push でリアルタイム転送
4. Console → `terminal.write(pane_id, data)` RPCで入力送信

---

## CLIコマンド対応

| 現行 | 新設計 |
|------|--------|
| `vp start` | Daemon接続 + Console表示 |
| `vp stop` | Console閉じるだけ（Daemon生存） |
| `vp daemon start` | Daemon明示的起動 |
| `vp daemon stop` | Daemon + 全セッション終了 |
| `vp ps` | `session.list()` RPC |
| `vp mcp` | MCP → Daemon RPC |

---

## 既存コードからの移行

### Daemon側（新規モジュール）

| 新モジュール | 元になるコード | 内容 |
|---|---|---|
| `daemon/mod.rs` | 新規 | Daemonエントリーポイント、fork/PID管理 |
| `daemon/registry.rs` | 新規 | SessionRegistry |
| `daemon/pty_slot.rs` | `stand/pty.rs` | PtyManager拡張、マルチペイン対応 |
| `daemon/channels.rs` | 新規 | Unisonチャネルハンドラ |

### Console側（terminal_window.rsの進化）

| 変更箇所 | 現行 | 新設計 |
|---|---|---|
| PTY出力取得 | tmux capture-pane 200msポーリング | Unison Event push |
| 入力送信 | WebSocket → Stand → tmux send-keys | Unison `terminal.write` RPC |
| リサイズ | WebSocket → Stand → tmux resize-window | Unison `terminal.resize` RPC |
| ウィンドウ切替 | tmux select-window CLI | Console側タブ切替 |
| ステータスバー | tmux list-windows 2秒ポーリング | `session.attach` メタデータ + Event通知 |

### 削除対象

- `stand/tmux.rs` — tmux依存を完全排除
- `terminal_window.rs` のtmux関連30箇所
- `start_status_poller` — Event pushで不要

### 存続

- `stand/pty.rs` — PtySessionのPTY生成ロジックを活用
- `terminal/renderer.rs` — CoreTextレンダラーは変更なし
- `terminal/mod.rs` — TerminalStateは変更なし

---

## フェーズ分け

```
Phase 1: Daemon基盤
  ├── daemonプロセス（fork, PID管理, シグナル処理）
  ├── SessionRegistry（セッション/ペイン CRUD）
  ├── PtySlot（portable-ptyでPTY管理、output read loop）
  ├── Unison Server（session/terminal/system channel）
  └── vp daemon start / stop / status

Phase 2: Console接続
  ├── vp start → Daemon自動起動 + Unison接続
  ├── terminal_window.rs からtmux依存を除去
  ├── PTY出力受信 → CoreText描画
  ├── タブ/ペイン操作（Cmd+T, Cmd+W, Cmd+1-9）
  └── コピー&ペースト維持

Phase 3: MCP統合
  ├── vp mcp → Daemon経由でセッション操作
  ├── show / split_pane のペイン表示
  └── tmux.rs 削除、完全移行

Phase 4: メッセージング拡張（将来）
  ├── "messaging" channel追加
  ├── ccwire/cwflow連携
  └── Stand間通信・メッシュ対応
```

---

## 拡張ポイント

### Stand間メッセージング（Phase 4）

DaemonのUnison Serverに `messaging` channelを追加:

```
"messaging" channel
  ├── RPC: send(target_session, message)
  ├── RPC: broadcast(message)
  └── Event: message_received
```

ccwire/cwflowとの統合:
- Daemon内のStand同士はUnison channel経由で直接通信
- 別マシンのDaemon同士はUnison P2P接続（CPLPメッシュと同パターン）
