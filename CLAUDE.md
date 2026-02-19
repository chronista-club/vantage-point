# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## プロジェクト概要

Vantage Point（`vp`）は Rust製のAI協働開発プラットフォーム。
Claude CLIをバックエンドとして、WebView UIとMIDI入力を統合した開発体験を提供する。

### コアコンセプト

- **AI主導の選択肢UI**: AIが選択肢を提示 → ユーザーが選ぶ
- **協調モード**: 協調 / 委任 / 自律 の3段階
- **CLI-First**: Rust CLIをコアとして段階的に拡張

詳細: [docs/spec/01-core-concept.md](docs/spec/01-core-concept.md)

## アーキテクチャ

### 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Stand | Rust (Tokio, Axum, Clap) |
| WebView | wry + tao |
| Frontend | HTML/JS (WebSocket) |
| Agent | Claude CLI + MCP |
| MIDI | midir |

> **Stand**: JoJoの奇妙な冒険のスタンドにちなんだ命名。
> AIエージェントが動作するサーバーの総称で、ユーザーの「傍らに立ち」能力を発揮する存在。

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

詳細: [docs/design/01-architecture.md](docs/design/01-architecture.md)

## プロジェクト構造

```
vantage-point/
├── crates/
│   ├── vantage-point/   # メインCLI (vp)
│   └── vantage-core/    # 共通ライブラリ
├── web/                 # WebView HTML/JS
├── docs/
│   ├── spec/            # 仕様書 (SDG: Spec)
│   ├── design/          # 設計書 (SDG: Design)
│   └── development/     # 開発ガイド
└── .claude/             # Claude Code設定
```

## CLIコマンド

```bash
# Core（日常使い）
vp start [N]           # プロジェクトN番のStandを起動（デフォルト）
vp start -d simple     # デバッグモードで起動
vp stop [--port]       # Stand停止
vp restart [--port]    # Stand再起動
vp ps                  # 稼働中インスタンス一覧
vp open [N]            # WebUIを開く
vp config              # 設定と登録プロジェクト表示
vp mcp                 # MCPサーバーモード（stdio）
vp update [--check]    # セルフアップデート

# Daemon（常駐プロセス）
vp daemon start        # デーモン起動（Stand管理 + ヘルスチェック）
vp daemon stop         # デーモン停止
vp daemon status       # デーモン状態確認

# App（起動モード）
vp app                 # VantagePoint.app起動（Daemon自動起動）
vp tray                # システムトレイモード

# MIDI（ハードウェア）
vp midi monitor        # MIDI入力モニタリング
vp midi ports          # 利用可能なMIDIポート一覧
vp midi lpd8 write     # LPD8に設定書き込み
vp midi lpd8 switch N  # LPD8プログラム切り替え
vp midi lpd8 ports     # MIDI出力ポート一覧
```

## 開発コマンド

```bash
# ビルド
cargo build --release -p vantage-point

# テスト
cargo test --workspace

# インストール
cargo install --path crates/vantage-point

# Lint
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

## 設定ファイル

**場所**: `~/.config/vantage/config.toml`

```toml
[[projects]]
name = "vantage-point"
path = "/path/to/vantage-point"

[[projects]]
name = "creo-memories"
path = "/path/to/creo-memories"
```

## ポート管理

- Project 0 → Port 33000
- Project 1 → Port 33001
- `vp ps` で 33000-33010 をスキャン

## ドキュメント構成 (SDG)

- **Spec** (`docs/spec/`): 何を作るか
- **Design** (`docs/design/`): どう作るか
- **Guide** (`docs/development/`): どう使うか

## Agent モジュール

Claude CLI統合の実装。3つの実行モードを提供:

### 実行モード

| モード | CLI形式 | 用途 |
|--------|---------|------|
| **OneShot** | `claude -p "prompt"` | 単発プロンプト → 応答 |
| **Interactive** | `claude -p --input-format stream-json` | 持続プロセス、複数ターン（Stream-JSON I/O） |
| **PTY** | `claude` (対話モード) | PTY経由の真の対話モード、Multiplexer Orchestration用 |

### Stream-JSON 入力フォーマット

`--input-format stream-json` 使用時のJSONL形式:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"メッセージ"}]}}
```

### PTYモード

`pty-process` クレートを使用してPTY（疑似端末）経由でClaude CLIを起動。
tmuxのようなMultiplexer Orchestrationに対応:

- `PtyClaudeAgent::start()` - PTY付きでClaude CLI起動
- `PtyClaudeAgent::send()` - テキスト入力送信
- `PtyClaudeAgent::send_raw()` - 制御シーケンス送信（Ctrl+C等）
- `PtyClaudeAgent::resize()` - ターミナルサイズ変更
- `PtyClaudeAgent::events()` - 出力イベント受信

### コーディング規約

- **コメントは日本語で記述する**
- 設計思想に従い、data / calculations / actions を明確に分離

## デバッグモード開発方針

開発時は常にデバッグモード（`simple` または `detail`）を使用し、適切なログを配置しながら実装を進める。

### デバッグモードの種類

| モード | 用途 | 起動方法 |
|--------|------|----------|
| `none` | 本番運用 | `vp start` |
| `simple` | 基本的なイベントログ | `vp start -d simple` |
| `detail` | 詳細なデータ・タイミング情報 | `vp start -d detail` |

### ログ出力方針

サーバー側（Rust）では `state.send_debug()` を使用:

```rust
// Simple: 基本イベント
state.send_debug("category", "メッセージ", None);

// Detail: 詳細データ付き
state.send_debug_detail("category", "メッセージ", serde_json::json!({
    "key": "value"
}));
```

**カテゴリ例**:
- `connection`: WebSocket接続関連
- `pty`: PTYイベント
- `permission`: パーミッションリクエスト
- `agent`: エージェント動作
- `timing`: 処理時間計測
- `tool`: ツール実行

### 問題調査時のフロー

1. `vp start -d detail` でStandを起動
2. WebUIのデバッグパネル（右パネル）でログを確認
3. ブラウザコンソールで `Received:` ログを確認
4. 必要に応じてコードにログを追加して再ビルド

## クロスプロジェクト協業（MARU × VP）

本プロジェクトはMARU（ESP32-S3物理コントローラ）と連携開発を行っている。
creo-memoriesを共有データベースとして、CC間でデータ共有・議論を行う。

### セッション開始時

- `recall("cross-project discussion")` で未読の議論を確認する

### 記録規約

- 他プロジェクトに関わる設計決定時: `remember()` で `category: "cross-project"` に記録
- メタデータに `from: "vp"` を必ず付与
- 議論は `type: proposal / question / answer / decision` で分類
- タグ例: `wire-protocol`, `atlas`, `decision`, `discussion`

### 参照

- 設計ドキュメント: [docs/plans/2026-02-15-cross-project-collab-design.md](docs/plans/2026-02-15-cross-project-collab-design.md)
