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
vp start [N]      # プロジェクトN番のStandを起動
vp start -d simple # デバッグモードで起動
vp ps             # 稼働中インスタンス一覧
vp open [N]       # WebUIを開く
vp config         # 設定と登録プロジェクト表示
vp status         # 接続状態確認
vp stop           # Stand停止
vp mcp            # MCPサーバーモード（stdio）
vp tray           # システムトレイモード
vp midi [N]       # MIDI入力監視
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
