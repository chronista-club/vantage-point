# Vantage Point

> 開発行為を拡張する

AIと協働しながら、デバイス・場所に縛られずシームレスに開発を継続できるプラットフォーム。

## コンセプト

### AI主導の選択肢UI

従来のチャットUIではなく、AIが選択肢を提示してユーザーが選ぶスタイル。
移動中でもタップだけで開発を継続できる。

```
AI: 次のステップはどうしますか？
    [A] テストを書く
    [B] リファクタリング
    [C] 次の機能へ
```

### 協調モード

| モード | 説明 |
|--------|------|
| **協調** | ユーザーと一緒に進める |
| **委任** | 任せて、途中経過・結果を確認 |
| **自律** | 完全に任せる |

### シームレスな継続

```
起床 → MIDIパッド → Mac → Vision Pro → 移動中(iPhone) → カフェ(iPad) → 帰宅(Mac)
```

すべて一つのワークスペース上で継続。デバイス間でP2P同期。

## アーキテクチャ

```
┌─────────────────────────────────────────┐
│           P2P Network (Loro/CRDT)       │
│   Mac ←→ Vision Pro ←→ iPad/iPhone     │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│   Agent Server (Rust + WASM)            │
│   - Claude Agent SDK                    │
│   - MCP Tools                           │
│   - Unison Protocol (QUIC/KDL)          │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│   SurrealDB (Single Source of Truth)    │
└─────────────────────────────────────────┘
```

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| Frontend (Web) | SolidJS |
| Frontend (Native) | Swift (Vision Pro) |
| Backend (Core) | Rust |
| Backend (Agent) | TypeScript → WASM |
| Data | SurrealDB |
| 通信 | Unison Protocol |
| P2P同期 | Loro (CRDT) |

## 開発フェーズ

| Phase | 内容 | Frontend |
|-------|------|----------|
| **Phase 1** | コア機能・対話スタイル確立 | SolidJS (Web) |
| **Phase 2** | Vision Pro空間体験最適化 | Swift Native |

## ドキュメント

| ドキュメント | 内容 |
|-------------|------|
| [docs/spec/01-core-concept.md](docs/spec/01-core-concept.md) | コアコンセプト |
| [docs/spec/02-user-journey.md](docs/spec/02-user-journey.md) | ユーザージャーニー |
| [docs/design/01-architecture.md](docs/design/01-architecture.md) | アーキテクチャ設計 |

## リポジトリ構成

このリポジトリ (`vantage-point`) はプロジェクト全体の**母艦**。

```
vantage-point/          ← このリポジトリ（母艦）
├── docs/               # 仕様・設計・ガイド
└── working/            # Git Worktree用

# 将来的に分離予定
vantage-server/         # Rust + WASM (Backend)
vantage-web/            # SolidJS (Frontend)
vantage-native/         # Swift (Vision Pro)
```

言語・プラットフォームごとに分離する可能性あり。
仕様・設計ドキュメントは母艦に集約。

## 関連リポジトリ

- [chronista-club/unison-protocol](https://github.com/chronista-club/unison-protocol) - QUIC/KDL通信プロトコル

## ステータス

🚧 **設計フェーズ完了、実装準備中**

## ライセンス

Private
