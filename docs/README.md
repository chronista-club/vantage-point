# Vantage Point Documentation

AI ネイティブ開発環境 Vantage Point (`vp`) の技術ドキュメント。
プロジェクト全体像は [リポジトリ ROOT の README](../README.md)、AI agent 向け概要は [`CLAUDE.md`](../CLAUDE.md) を参照。

## ドキュメント体系

| ID 体系 | 用途 |
|--------|------|
| `VP-SPEC-NNN` | 要件定義 (What & Why) — `spec/` |
| `VP-DESIGN-NNN` | 設計 (How) — `design/` |
| `(無番号)` | ガイド・決定ログ・archive |

> **状態ラベル**: `(草案)` 概念検討中・実装未着手 / `(検討)` 採否未確定だが議論進行中 / `(実装中)` コードに具体化進行中 / `(無印)` 確定・現行仕様。確定後に `VP-DESIGN-NNN` を採番。
>
> 一部の design ドキュメントは Lane / ccwire / AG-UI 等の検討中・廃案を含む過渡的記録 (D2 phase で整理予定)。

### Spec — 要件定義

| ID | ドキュメント | 内容 |
|----|-------------|------|
| VP-SPEC-001 | [01-core-concept.md](spec/01-core-concept.md) | コア要件 (REQ1〜REQ7) + ビジョン |
| VP-SPEC-002 | [02-capability.md](spec/02-capability.md) | Capability / MIDI 仕様 |
| VP-SPEC-003 | [03-update.md](spec/03-update.md) | セルフアップデート |

### Design — 設計

| ID | ドキュメント | 内容 |
|----|-------------|------|
| VP-DESIGN-001 | [01-architecture.md](design/01-architecture.md) | システムアーキテクチャ |
| VP-DESIGN-002 | [02-capability-evolution.md](design/02-capability-evolution.md) | Capability 進化システム |
| (検討) | [03-mailbox-vs-ccwire.md](design/03-mailbox-vs-ccwire.md) | Mailbox / ccwire 役割分離 (2026-04-19) |
| (検討) | [04-ccwire-redesign.md](design/04-ccwire-redesign.md) | ccwire リデザイン (2026-04-19) |
| (検討) | [05-pane-content-lane-smart-canvas.md](design/05-pane-content-lane-smart-canvas.md) | Pane / Content / Lane / Smart Canvas 統合 (2026-04-21) |
| (草案) | [06-creoui-draft.md](design/06-creoui-draft.md) | CreoUI schema draft (VP-73 R0) |
| (草案) | [07-lane-as-process.md](design/07-lane-as-process.md) | Lane-as-Process 規約 (VP-77) |
| (検討) | [08-viewport-semantic-split.md](design/08-viewport-semantic-split.md) | Viewport Semantic Split (VP-83) |
| (実装中) | [vp-native-app.md](design/vp-native-app.md) | VP ネイティブアプリ化 |
| (実装中) | [vp-app-hd-bridge.md](design/vp-app-hd-bridge.md) | vp-app ↔ HD bridge |

### Guide — ガイド

| ドキュメント | 内容 |
|-------------|------|
| [setup.md](guide/setup.md) | 環境構築 + Prerequisites |
| [release.md](guide/release.md) | リリースフロー |
| [testing.md](guide/testing.md) | テスト戦略 |
| [dogfooding-v0.13.0.md](guide/dogfooding-v0.13.0.md) | v0.13.0 dogfooding チェックリスト (バージョン固定) |

> 開発フロー (ブランチ戦略・コミット規約) は chronista-style `codeflow` スキルに準拠。

### Decisions — 決定ログ

| ドキュメント | 内容 |
|-------------|------|
| [2026-04-19-strategy-summary.md](decisions/2026-04-19-strategy-summary.md) | VP 戦略決定総まとめ |

### Archive

| ドキュメント | 理由 |
|-------------|------|
| [03-agent-protocol-unification.md](archive/03-agent-protocol-unification.md) | AG-UI 前提の設計 (採用見送り) |
| [04-ag-ui-requirements.md](archive/04-ag-ui-requirements.md) | AG-UI 未採用 |

## プロジェクト情報

- **バージョン**: workspace で一元管理 — [`Cargo.toml`](../Cargo.toml) 参照
- **ライセンス**: MIT OR Apache-2.0 dual ([LICENSE-MIT](../LICENSE-MIT) / [LICENSE-APACHE](../LICENSE-APACHE))
- **コントリビュート**: [CONTRIBUTING.md](../CONTRIBUTING.md)
- **セキュリティ報告**: [SECURITY.md](../SECURITY.md)
