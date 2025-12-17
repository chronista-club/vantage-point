# Vantage Point Documentation

「開発行為を拡張する」プラットフォーム、Vantage Pointの技術ドキュメントです。

## ドキュメント構成 (SDG)

### Spec - 仕様書
何を作るか - 要件・コンセプト

| ドキュメント | 内容 |
|-------------|------|
| [01-core-concept.md](./spec/01-core-concept.md) | コアコンセプト・ビジョン |
| [02-user-journey.md](./spec/02-user-journey.md) | ユーザージャーニー |

### Design - 設計書
どう作るか - アーキテクチャ・技術設計

| ドキュメント | 内容 |
|-------------|------|
| [01-architecture.md](./design/01-architecture.md) | システムアーキテクチャ |

### Development - 開発ガイド
どう使うか - 開発者向けガイド

| ドキュメント | 内容 |
|-------------|------|
| [setup.md](./development/setup.md) | 環境構築 |
| [gitflow-next.md](./development/gitflow-next.md) | ブランチ戦略 |

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Daemon | Rust (Tokio, Axum, Clap) |
| WebView | wry + tao |
| Frontend | HTML/JS (WebSocket) |
| Agent | Claude CLI + MCP |
| MIDI | midir |

## クイックスタート

```bash
# ビルド＆インストール
cargo install --path crates/vantage-point

# デーモン起動
vp start

# 設定確認
vp config

# 稼働中インスタンス一覧
vp ps
```

## CLIコマンド

| コマンド | 説明 |
|---------|------|
| `vp start [N]` | プロジェクトN番のデーモンを起動 |
| `vp start -d simple` | デバッグモードで起動 |
| `vp ps` | 稼働中インスタンス一覧 |
| `vp open [N]` | WebUIを開く |
| `vp config` | 設定と登録プロジェクト表示 |
| `vp status` | 接続状態確認 |
| `vp stop` | デーモン停止 |
| `vp mcp` | MCPサーバーモード（stdio） |
| `vp tray` | システムトレイモード |
| `vp midi` | MIDI入力監視 |

## プロジェクト情報

- **リポジトリ**: [chronista-club/vantage-point](https://github.com/chronista-club/vantage-point)
- **バージョン**: 0.2.0
- **ライセンス**: MIT

## ステータス

✅ **v0.2.0 - Rust CLI実装完了**

- Agent Service（Claude CLI連携）
- MIDI Service（MIDIコントローラー入力）
- WebView / HTTP Server
- セッション管理
