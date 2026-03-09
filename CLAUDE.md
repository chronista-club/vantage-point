# CLAUDE.md

## プロジェクト概要

Vantage Point（`vp`）は Rust製の **AI ネイティブ開発環境**。
Claude CLI をエンジンとして、TUI コンソール・Canvas（WebView）・外部コントロールを統合した開発体験を提供する。
Mac App Store で配布予定（有料 + Free プランの可能性あり）。

### プロジェクト方針

**VP は焦らず、使用感を確かめながら、熟慮・議論を重ねて進化させるプロジェクト。**
Creo Memories（サービス）とは異なり、「自分のような開発フロー」のためのアプリ。
dogfooding を通じて体験を磨き、納得できる完成度でリリースする。

### コアコンセプト

- **AI ネイティブ開発環境**: VP が主、Claude Code はそのエンジン
- **プロジェクト起点**: プロジェクト選択 → TUI コンソール → Claude との対話が 1st ビュー
- **Canvas + TUI**: TUI で操る、Canvas で視る。両者が並列に動く
- **セッション永続化**: 前回の続きから再開できる開発環境

### アーキテクチャ命名体系（JoJo メタファー）

外向けは普通の用語メイン + JoJo 名を小さく併記（機能イメージを伝える目的）。
命名定義は `crates/vantage-point/src/stands.rs` に集約。

```
TheWorld 👑 (Process Manager / 常駐デーモン)
  └── Star Platinum ⭐ (Project Server / 各プロジェクトの開発プロセス)
        ├── Gold Experience 🌿 (AI Agent / Claude CLI オーケストレーター)
        ├── Paisley Park 🧭 (Display Engine / Canvas WebView)
        ├── Heaven's Door 📖 (Code Runner / ProcessRunner)
        └── Hermit Purple 🍇 (External Control / MIDI・tmux・MCP)
```

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Process | Rust (Tokio, Axum, Clap) |
| WebView | wry + tao |
| Frontend | HTML/JS (WebSocket) |
| Agent | Claude CLI + MCP |
| MIDI | midir |

> **Process**: プロジェクトの開発プロセスを表す本体。JoJo の Stand（能力）を保持し、ユーザーの開発を支援する。

### システム構成

```
┌─────────────────────────────────────────────────────┐
│                    VP CLI (vp)                       │
├─────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │
│  │   Agent     │  │    MIDI     │  │   WebView   │ │
│  │  Service    │  │   Service   │  │   Server    │ │
│  │ Claude CLI  │  │   midir     │  │ Axum + wry  │ │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘ │
│         └────────────────┼────────────────┘         │
│                   Session Manager                    │
└─────────────────────────────────────────────────────┘
```

## プロジェクト構造

```
vantage-point/
├── crates/
│   ├── vantage-point/   # メインCLI (vp)
│   └── vantage-core/    # 共通ライブラリ
├── web/                 # WebView HTML/JS
├── docs/
│   ├── spec/            # 仕様書
│   ├── design/          # 設計書
│   └── development/     # 開発ガイド
└── .claude/             # Claude Code設定
```

## CLIコマンド

```bash
# Core
vp start [N]           # プロジェクトN番のProcessを起動
vp start -d simple     # デバッグモードで起動
vp stop [--port]       # Process停止
vp restart [--port]    # Process再起動
vp ps                  # 稼働中インスタンス一覧
vp open [N]            # WebUIを開く
vp config              # 設定と登録プロジェクト表示
vp mcp                 # MCPサーバーモード（stdio）
vp update [--check]    # セルフアップデート

# Daemon
vp daemon start|stop|status

# App
vp app                 # VantagePoint.app起動（Daemon自動起動）
vp tray                # システムトレイモード

# MIDI
vp midi monitor|ports
vp midi lpd8 write|switch|ports
```

## 開発コマンド

```bash
cargo build --release -p vantage-point   # ビルド
cargo test --workspace                    # テスト
cargo install --path crates/vantage-point # インストール
cargo fmt --all -- --check                # フォーマットチェック
cargo clippy --workspace --all-targets    # Lint
```

## 設定・ポート

- 設定ファイル: `~/.config/vantage/config.toml`
- ポート割り当て: Project 0 → 33000, Project 1 → 33001, ...
- `vp ps` で 33000-33010 をスキャン

## Agent モジュール

Claude CLI統合の実装。3つの実行モードを提供:

| モード | CLI形式 | 用途 |
|--------|---------|------|
| **OneShot** | `claude -p "prompt"` | 単発プロンプト |
| **Interactive** | `claude -p --input-format stream-json` | 持続プロセス、複数ターン |
| **PTY** | `claude` (対話モード) | PTY経由の対話モード、Multiplexer Orchestration用 |

### Stream-JSON 入力フォーマット

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"メッセージ"}]}}
```

### PTYモード API

`pty-process` クレートを使用:

- `PtyClaudeAgent::start()` - PTY付きでClaude CLI起動
- `PtyClaudeAgent::send()` / `send_raw()` - テキスト / 制御シーケンス送信
- `PtyClaudeAgent::resize()` - ターミナルサイズ変更
- `PtyClaudeAgent::events()` - 出力イベント受信

## コーディング規約

- **コメントは日本語で記述する**
- data / calculations / actions を明確に分離

## デバッグモード

| モード | 用途 | 起動方法 |
|--------|------|----------|
| `none` | 本番運用 | `vp start` |
| `simple` | 基本的なイベントログ | `vp start -d simple` |
| `detail` | 詳細なデータ・タイミング | `vp start -d detail` |

### ログ出力

```rust
// Simple
state.send_debug("category", "メッセージ", None);

// Detail
state.send_debug_detail("category", "メッセージ", serde_json::json!({"key": "value"}));
```

カテゴリ: `connection`, `pty`, `permission`, `agent`, `timing`, `tool`

### 問題調査フロー

1. `vp start -d detail` で起動
2. WebUIデバッグパネル（右パネル）でログ確認
3. ブラウザコンソールで `Received:` ログ確認
4. 必要に応じてログ追加 → 再ビルド

## クロスプロジェクト協業（MARU x VP）

MARU（ESP32-S3物理コントローラ）との連携開発。creo-memoriesで `category: "cross-project"` + `from: "vp"` で記録。

設計ドキュメント: [docs/plans/archive/2026-02-15-cross-project-collab-design.md](docs/plans/archive/2026-02-15-cross-project-collab-design.md)
