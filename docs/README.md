# Vantage Point Documentation

「開発行為を拡張する」AI ネイティブ開発環境の技術ドキュメント。

## ドキュメント構成 (SDG)

ID 体系: `VP-SPEC-NNN` / `VP-DESIGN-NNN` / `VP-GUIDE-NNN`

### Spec — 要件定義 (What & Why)

| ID | ドキュメント | 内容 |
|----|-------------|------|
| VP-SPEC-001 | [01-core-concept.md](./spec/01-core-concept.md) | コア要件 (REQ1〜REQ7) + ビジョン |
| VP-SPEC-002 | [02-capability.md](./spec/02-capability.md) | Capability / MIDI 仕様 |
| VP-SPEC-003 | [03-update.md](./spec/03-update.md) | セルフアップデート |

### Design — 設計 (How)

| ID | ドキュメント | 内容 |
|----|-------------|------|
| VP-DESIGN-001 | [01-architecture.md](./design/01-architecture.md) | システムアーキテクチャ |
| VP-DESIGN-002 | [02-capability-evolution.md](./design/02-capability-evolution.md) | Capability 進化システム |

### Guide — ガイド (Usage)

| ドキュメント | 内容 |
|-------------|------|
| [setup.md](./guide/setup.md) | 環境構築 + Prerequisites |
| [release.md](./guide/release.md) | リリースフロー |
| [testing.md](./guide/testing.md) | テスト戦略 |

> 開発フロー（ブランチ戦略・コミット規約）は chronista-style `codeflow` スキルに準拠。

### Archive

| ドキュメント | 理由 |
|-------------|------|
| [04-ag-ui-requirements.md](./archive/04-ag-ui-requirements.md) | AG-UI 未採用 |
| [03-agent-protocol-unification.md](./archive/03-agent-protocol-unification.md) | AG-UI 前提の設計 |
| [08-viewport-semantic-split.md](./archive/08-viewport-semantic-split.md) | self-superseded |
| [16-worker-lane-msgbox-recv.md](./archive/16-worker-lane-msgbox-recv.md) | msgbox → wiremsg 再設計で陳腐化 |
| [18-msg-lifecycle-state.md](./archive/18-msg-lifecycle-state.md) | msgbox → wiremsg 再設計で陳腐化 |
| [19-msgbox-whitesnake-primary.md](./archive/19-msgbox-whitesnake-primary.md) | msgbox → wiremsg 再設計で陳腐化（historical reference） |
| [20-spike-report.md](./archive/20-spike-report.md) | spike 完了・self-superseded |
| [dogfooding-v0.13.0.md](./archive/dogfooding-v0.13.0.md) | 旧バージョンの dogfooding 記録 |

## プロジェクト情報

- **バージョン**: 0.19.0
- **ライセンス**: MIT OR Apache-2.0（dual）
