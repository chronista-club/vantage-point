# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## プロジェクト概要

Vantage Pointは「開発行為を拡張する」プラットフォーム。
AIと協働しながら、デバイス・場所に縛られずシームレスに開発を継続できる環境を提供する。

### コアコンセプト

- **AI主導の選択肢UI**: AIが選択肢を提示 → ユーザーが選ぶ
- **協調モード**: 協調 / 委任 / 自律 の3段階
- **P2P同期**: どのデバイスもサーバー/クライアント両方になれる

詳細: [docs/spec/01-core-concept.md](docs/spec/01-core-concept.md)

## アーキテクチャ

### 技術スタック

| レイヤー | 技術 |
|---------|------|
| Frontend (Web) | SolidJS |
| Frontend (Native) | Swift (Phase 2: Vision Pro) |
| Backend (Core) | Rust |
| Backend (Agent) | TypeScript → WASM |
| Data | SurrealDB (namespace: vantage) |
| 通信 | Unison Protocol (QUIC/KDL) |
| P2P同期 | Loro (CRDT) |

### システム構成

```
┌─────────────────────────────────────────────────────┐
│                   P2P Network                        │
│   Mac ←→ Vision Pro ←→ iPad/iPhone                  │
│   (CRDT)    (CRDT)       (CRDT)                     │
│              ↓                                       │
│         Loro同期                                    │
│              ↓                                       │
│       Unison Protocol                               │
└─────────────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────────────┐
│                Agent Server                         │
│   ┌────────┐  ┌────────────────────┐               │
│   │  Rust  │──│  Agent層 (WASM)    │               │
│   │  Core  │  │  TypeScript/MCP    │               │
│   └────────┘  └────────────────────┘               │
│              ↓                                       │
│   ┌─────────────────────────────────┐              │
│   │    SurrealDB (SSoT)             │              │
│   │    Live Query / namespace       │              │
│   └─────────────────────────────────┘              │
└─────────────────────────────────────────────────────┘
```

詳細: [docs/design/01-architecture.md](docs/design/01-architecture.md)

## プロジェクト構造

```
vantage-point/          # このリポジトリ（母艦）
├── docs/
│   ├── spec/           # 仕様書 (SDG: Spec)
│   ├── design/         # 設計書 (SDG: Design)
│   └── development/    # 開発ガイド
├── working/            # Git Worktree用
└── .claude/            # Claude Code設定
```

> **Note**: 言語・プラットフォームごとに別リポジトリに分離予定。
> 仕様・設計ドキュメントは母艦に集約。

## 開発フェーズ

| Phase | 内容 | Frontend |
|-------|------|----------|
| Phase 1 | コア機能・対話スタイル確立 | SolidJS (Web) |
| Phase 2 | Vision Pro空間体験最適化 | Swift Native |

## 関連リポジトリ

| リポジトリ | 役割 |
|-----------|------|
| [chronista-club/unison-protocol](https://github.com/chronista-club/unison-protocol) | QUIC/KDL通信プロトコル |
| creo-memories | 同構成の姉妹プロジェクト |

## 開発コマンド（準備中）

```bash
# Rustバックエンド
cargo build
cargo test

# SolidJSフロントエンド
bun install
bun dev
```

## 開発時の注意点

1. **設計優先**: 実装前に docs/spec/, docs/design/ を確認
2. **P2P/CRDT**: 状態管理はLoroを通じて行う
3. **Unison Protocol**: 通信はKDLスキーマで型定義
4. **Agent SDK**: Claude Agent SDKとMCPを活用

## ドキュメント構成 (SDG)

本プロジェクトはSDG（Spec-Design-Guide）方式でドキュメント管理:

- **Spec** (`docs/spec/`): 何を作るか - 要件・コンセプト
- **Design** (`docs/design/`): どう作るか - アーキテクチャ・技術設計
- **Guide** (`docs/development/`): どう使うか - 開発ガイド
