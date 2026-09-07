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

### Design — 現行実装を読む入口

設計書には未実装の後続案も含まれます。各文書の Status / Status log と対象コードを併せて読みます。

| ドキュメント | 現行の責務 |
|-------------|------------|
| [44-world-one-process.md](design/44-world-one-process.md) | daemon 内の repo runtime・lifecycle |
| [45-transport-consolidation.md](design/45-transport-consolidation.md) | Unison 制御・購読と HTTP 診断 |
| [46-lane-pane-model.md](design/46-lane-pane-model.md) | lane / session / pane の関係 |
| [33-console-unification.md](design/33-console-unification.md) | Console の TUI / GUI 表示 |
| [50-pane-chrome-and-session-panes.md](design/50-pane-chrome-and-session-panes.md) | session を単位にした pane / 操作 |
| [53-lane-reconcile.md](design/53-lane-reconcile.md) | lane reconcile・供給と購読の境界 |
| [52-board-redesign.md](design/52-board-redesign.md) | board の現行実装と段階的な再設計 |
| [59-settings-page.md](design/59-settings-page.md) | 設定の所有と GUI |

### 提案・歴史を読む

- [設計書一覧](design/): 上表以外も含む全設計。文書内の Draft / 将来フェーズは実装済みの保証ではありません。
- [01-architecture.md](design/01-architecture.md): 初期アーキテクチャの記録。現行のプロセス・通信構成は設計 44 / 45 を参照。
- [11-vp-app-refactor.md](design/11-vp-app-refactor.md): 当時の改善計画。現在の未完タスク一覧としては使いません。
- [Archive](archive/): 撤去済み・置換済みの設計。旧名称や当時の前提は履歴として残します。

### Guide — ガイド (Usage)

| ドキュメント | 内容 |
|-------------|------|
| [setup.md](./guide/setup.md) | 環境構築 + Prerequisites |
| [release.md](./guide/release.md) | リリースフロー |
| [testing.md](./guide/testing.md) | テスト戦略 |
| [stand-smoke-matrix.md](./guide/stand-smoke-matrix.md) | Stand（engine）横断の実機スモーク行列 — 能力表 + 観測ログ |

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

## repo情報

- **バージョン**: [Cargo.toml](../Cargo.toml) の `workspace.package.version` が正本
- **ライセンス**: MIT OR Apache-2.0（dual）
