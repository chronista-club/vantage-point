# Vantage Point Documentation

「開発行為を拡張する」プラットフォーム、Vantage Pointの技術ドキュメントです。

## ドキュメント構成 (SDG)

### Spec - 仕様書
何を作るか - 要件・コンセプト

| ドキュメント | 内容 |
|-------------|------|
| [01-core-concept.md](./spec/01-core-concept.md) | コアコンセプト・ビジョン |
| [02-user-journey.md](./spec/02-user-journey.md) | ユーザージャーニー |
| [03-lpd8-integration.md](./spec/03-lpd8-integration.md) | LPD8 MIDI統合 |
| [04-ag-ui-requirements.md](./spec/04-ag-ui-requirements.md) | AG-UI要件 |
| [05-stand-capability.md](./spec/05-stand-capability.md) | Stand Capability仕様 |
| [06-auto-update.md](./spec/06-auto-update.md) | セルフアップデート |
| [06-user-prompt.md](./spec/06-user-prompt.md) | ユーザープロンプト |

### Design - 設計書
どう作るか - アーキテクチャ・技術設計

| ドキュメント | 内容 |
|-------------|------|
| [01-architecture.md](./design/01-architecture.md) | システムアーキテクチャ |
| [02-stand-capability-evolution.md](./design/02-stand-capability-evolution.md) | Capability進化システム |
| [03-agent-protocol-unification.md](./design/03-agent-protocol-unification.md) | エージェントプロトコル統一 |

### Development - 開発ガイド
どう使うか - 開発者向けガイド

| ドキュメント | 内容 |
|-------------|------|
| [setup.md](./development/setup.md) | 環境構築 |
| [gitflow-next.md](./development/gitflow-next.md) | ブランチ戦略 |
| [release-flow.md](./development/release-flow.md) | リリースフロー |
| [testing-strategy.md](./development/testing-strategy.md) | テスト戦略 |
| [stand-capability-params-guide.md](./development/stand-capability-params-guide.md) | Capabilityパラメータガイド |

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Stand | Rust (Tokio, Axum, Clap) |
| WebView | wry + tao |
| Frontend | HTML/JS (WebSocket) |
| Agent | Claude CLI + MCP |
| MIDI | midir |

## CLIコマンド

```bash
# Core
vp start [N]           # プロジェクトN番のStandを起動
vp stop [--port]       # Stand停止
vp restart [--port]    # Stand再起動
vp ps                  # 稼働中インスタンス一覧
vp open [N]            # WebUIを開く
vp config              # 設定と登録プロジェクト表示
vp mcp                 # MCPサーバーモード（stdio）
vp update [--check]    # セルフアップデート

# Canvas
vp canvas open|close|capture

# Pane
vp pane split|close|toggle|show|clear

# File
vp file watch|unwatch

# Daemon
vp daemon start|stop|status

# MIDI
vp midi monitor|ports
```

## プロジェクト情報

- **バージョン**: 0.7.0
- **ライセンス**: Proprietary
