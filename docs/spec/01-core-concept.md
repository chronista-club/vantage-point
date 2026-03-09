# VP-SPEC-001: Vantage Point コアコンセプト

> **Status**: Active
> **Created**: 2025-12-16
> **Updated**: 2026-03-10
> **Author**: @makoto

---

## Abstract

Vantage Point（VP）は Mac 向けの **AI ネイティブ開発環境**。
Claude Code をエンジンとして、TUI・Canvas（WebView）・外部入力を統合し、
「伝えれば、動く」開発体験を提供する。

本文書は VP の存在意義・スコープ・要件を定義する。

---

## Motivation

### 解決する課題

1. **AI 協働の断片化** — エディタ、ターミナル、ブラウザを行き来する開発フローは、AI との対話を分断する
2. **コンテキストの喪失** — セッションが切れるたびに、AI との共有理解がリセットされる
3. **情報の非対称性** — AI が出力する豊富な情報（図表・コード・ログ）を、ターミナルだけでは活かしきれない
4. **入力手段の制限** — キーボードのみの入力は、開発者の意図表現を制約する

### VP が提供する価値

- **統合**: TUI + Canvas + 外部入力が一体となった開発環境
- **永続性**: プロジェクト起点でセッションを再開できる
- **拡張性**: MIDI・tmux・MCP を通じて、開発環境の境界を広げる
- **可視化**: AI の出力をリッチに表示し、開発者と AI の双方に最適な情報を届ける

---

## Scope

### In Scope

| 領域 | 説明 |
|------|------|
| TUI コンソール | Claude Code との対話インターフェース |
| Canvas (WebView) | リッチコンテンツの表示・可視化 |
| セッション管理 | 永続化・再開・一覧 |
| 外部入力統合 | MIDI コントローラー、tmux、MCP |
| コード実行 | ProcessRunner による動的実行 |
| プロセス管理 | 複数プロジェクトのライフサイクル管理 |
| Mac アプリ | Mac App Store 配布（有料 + Free プラン検討） |

### Out of Scope

| 領域 | 理由 |
|------|------|
| エディタ機能 | VP はエディタではない。編集は既存ツール（Cursor, VS Code 等）に委ねる |
| クロスプラットフォーム | Mac ファースト。wry/tao の macOS API 依存 |
| クラウドサービス | VP はローカルアプリ。クラウド連携は MCP 経由で外部に委譲 |

---

## Requirements

### R1: プロジェクト起点の開発体験

VP の 1st ビューは「プロジェクト選択 → TUI → AI 対話」。

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ1.1 | `vp start [N]` でプロジェクトの Process を起動できる | Must |
| REQ1.2 | TUI 起動時にセッション選択（前回続行 / 新規 / 過去一覧）ができる | Must |
| REQ1.3 | 設定ファイル (`config.toml`) で複数プロジェクトを管理できる | Must |
| REQ1.4 | `vp ps` で稼働中プロセスを一覧できる | Must |

### R2: AI との対話（📖 Heaven's Door — Coding Assistant）

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ2.1 | Claude Code を PTY 経由で対話的に操作できる | Must |
| REQ2.2 | AI の応答状態（応答中 / 入力待ち）を視覚的に表示する | Must |
| REQ2.3 | 複数セッションを切り替えられる（Ctrl+N / Ctrl+←→） | Should |
| REQ2.4 | MCP サーバー経由で外部ツールから AI にプロンプトを送れる | Must |

### R3: 情報の可視化（🧭 Paisley Park — Information Navigator）

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ3.1 | Canvas（WebView）で Markdown / HTML をリッチ表示できる | Must |
| REQ3.2 | Canvas は TUI とは独立したウィンドウとして開閉できる | Must |
| REQ3.3 | ペイン分割で複数コンテンツを同時表示できる | Should |
| REQ3.4 | ファイル監視（watch_file）でログをリアルタイム表示できる | Should |
| REQ3.5 | MCP ツール経由で AI が Canvas にコンテンツを表示できる | Must |

### R4: コード実行（🌿 Gold Experience — Code Runner）

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ4.1 | Ruby スクリプトを動的に実行し、結果を Canvas に表示できる | Must |
| REQ4.2 | 実行中のスクリプトを停止できる | Must |
| REQ4.3 | MCP ツール経由で AI がコード実行を指示できる | Must |

### R5: 外部コントロール（🍇 Hermit Purple — External Control）

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ5.1 | MIDI コントローラー（LPD8 等）からの入力を受け付ける | Should |
| REQ5.2 | tmux ペイン操作（分割・キャプチャ）を MCP 経由で提供する | Should |
| REQ5.3 | MCP サーバーモード (`vp mcp`) で外部ツールと統合できる | Must |

### R6: プロセス管理（👑 TheWorld — Process Manager）

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ6.1 | 常駐デーモンとして全 Process のライフサイクルを管理する | Must |
| REQ6.2 | Process の起動・停止・再起動を API 経由で操作できる | Must |
| REQ6.3 | ポート自動割当（33000〜33010）で複数プロジェクトが共存できる | Must |
| REQ6.4 | プロセス発見はインメモリ管理（ファイルキャッシュ不使用） | Must |

### R7: Mac ネイティブ体験

| ID | 要件 | 優先度 |
|----|------|--------|
| REQ7.1 | メニューバーアプリとしてシステムトレイに常駐できる | Should |
| REQ7.2 | ネイティブ WebView (wry) で Canvas を表示する | Must |
| REQ7.3 | Mac App Store での配布に対応する | Could |

---

## Design Principles

1. **CLI-First** — GUI は CLI の上に構築する。CLI 単体で完結できること
2. **AI が主、ツールは従** — VP は AI の能力を最大化する環境であり、AI の代替ではない
3. **TUI で操る、Canvas で視る** — 操作と表示の関心を分離する
4. **プロジェクト = コンテキスト** — 全ての体験はプロジェクトを起点とする
5. **dogfooding 駆動** — 自ら使い、体験から改善する。納得できる完成度でリリースする

---

## Architecture Overview

```
TheWorld 👑 (Process Manager / 常駐デーモン)
  └── Star Platinum ⭐ (Project Core / TUI 統合ビュー)
        ├── Heaven's Door 📖 (Coding Assistant / Claude CLI)
        ├── Paisley Park 🧭 (Information Navigator / Canvas)
        ├── Gold Experience 🌿 (Code Runner / 動的実行)
        └── Hermit Purple 🍇 (External Control / MIDI・tmux・MCP)
```

- **Process**: プロジェクトの開発プロセス本体。Star Platinum が主人公として各 Stand を束ねる
- **Stand（能力）**: Process が保持する Capability の総称
- **TheWorld**: 常駐デーモン。全 Process のライフサイクルを管理

> 詳細な技術設計は `design/01-architecture.md` を参照。

---

## Input Triggers

| 入力 | 状態 | Stand |
|------|------|-------|
| テキスト入力（TUI） | 実装済み | 📖 Heaven's Door |
| MIDI コントローラー | 実装済み（LPD8） | 🍇 Hermit Purple |
| tmux 連携 | 実装済み | 🍇 Hermit Purple |
| MCP サーバー | 実装済み | 🍇 Hermit Purple |

---

## Platform

| プラットフォーム | 位置づけ |
|-----------------|---------|
| **Mac** | メイン開発環境（Mac App Store 配布予定） |

---

## Vision: 理想の開発体験

```
Mac に向かう → MIDI パッド → TUI 起動 → AI と協働 → Canvas で確認 → 成果物
```

- **ワンアクション起動**: LPD8 の Pad を叩く → TUI が前回セッションから再開
- **AI + 可視化**: TUI で対話、Canvas に設計図・ログ・実行結果をリアルタイム表示
- **マルチプロジェクト**: Pad 1〜4 で 4 プロジェクトを瞬時に切替、状態は独立保持

---

## Open Questions

- [ ] Free プランのスコープ（どこまで無料で使えるか）
- [ ] セッション永続化の長期ストレージ戦略

---

## References

- `design/01-architecture.md` (VP-DESIGN-001) — 技術アーキテクチャ設計
- `spec/02-capability.md` (VP-SPEC-002) — Capability / MIDI 仕様
- `crates/vantage-point/src/stands.rs` — Stand 命名定義

---

*VP は焦らず、使用感を確かめながら、熟慮・議論を重ねて進化させるプロジェクト。*
