# Vantage Point Documentation

「開発行為を拡張する」AI ネイティブ開発環境、Vantage Point の技術ドキュメントです。

## ドキュメント構成 (SDG)

ID 体系: `VP-SPEC-NNN` / `VP-DESIGN-NNN` / `VP-GUIDE-NNN`
要件 ID: `REQ{セクション}.{連番}` — spec で発番、design/guide は参照

### Spec - 要件定義 (What & Why)

| ID | ドキュメント | 内容 |
|----|-------------|------|
| VP-SPEC-001 | [01-core-concept.md](./spec/01-core-concept.md) | コアコンセプト・要件定義 |
| — | [02-user-journey.md](./spec/02-user-journey.md) | ユーザージャーニー (Draft) |
| — | [03-lpd8-integration.md](./spec/03-lpd8-integration.md) | LPD8 MIDI 統合 |
| — | [05-capability.md](./spec/05-capability.md) | Capability 仕様 |
| — | [06-auto-update.md](./spec/06-auto-update.md) | セルフアップデート |

### Design - 設計書 (How)

| ID | ドキュメント | 内容 |
|----|-------------|------|
| VP-DESIGN-001 | [01-architecture.md](./design/01-architecture.md) | システムアーキテクチャ |
| — | [02-capability-evolution.md](./design/02-capability-evolution.md) | Capability 進化システム |

### Development - 開発ガイド

| ドキュメント | 内容 |
|-------------|------|
| [setup.md](./development/setup.md) | 環境構築 |
| [gitflow-next.md](./development/gitflow-next.md) | ブランチ戦略 |
| [release-flow.md](./development/release-flow.md) | リリースフロー |
| [testing-strategy.md](./development/testing-strategy.md) | テスト戦略 |
| [capability-params-guide.md](./development/capability-params-guide.md) | Capability パラメータガイド |

### Archive

| ドキュメント | 理由 |
|-------------|------|
| [04-ag-ui-requirements.md](./archive/04-ag-ui-requirements.md) | AG-UI 未採用 |
| [03-agent-protocol-unification.md](./archive/03-agent-protocol-unification.md) | AG-UI 前提の設計 |

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Process | Rust (Tokio, Axum, Clap) |
| TUI | ratatui + crossterm + alacritty_terminal |
| WebView | wry + tao |
| QUIC | quinn + Unison |
| Agent | Claude CLI (PTY) + MCP |
| MIDI | midir |

## CLI コマンド

```bash
# Core
vp start [N]           # プロジェクト N 番の Process を起動（TUI）
vp stop [--port]       # Process 停止
vp restart [--port]    # Process 再起動
vp ps                  # 稼働中プロセス一覧
vp config              # 設定と登録プロジェクト表示
vp mcp                 # MCP サーバーモード（stdio）
vp update [--check]    # セルフアップデート

# TheWorld（デーモン）
vp world               # TheWorld 起動
vp daemon start|stop|status  # 後方互換エイリアス

# App
vp app                 # VantagePoint.app 起動

# MIDI
vp midi monitor|ports
vp midi lpd8 write|switch|ports
```

## プロジェクト情報

- **バージョン**: 0.8.2
- **ライセンス**: Proprietary
