# Vantage Point Documentation

「開発行為を拡張する」プラットフォーム、Vantage Pointの技術ドキュメントです。

## ドキュメント構成 (SDG)

### 📋 [Spec](./spec/) - 仕様書
何を作るか - 要件・コンセプト

| ドキュメント | 内容 |
|-------------|------|
| [01-core-concept.md](./spec/01-core-concept.md) | コアコンセプト・ビジョン |
| [02-user-journey.md](./spec/02-user-journey.md) | ユーザージャーニー |

### 🏗️ [Design](./design/) - 設計書
どう作るか - アーキテクチャ・技術設計

| ドキュメント | 内容 |
|-------------|------|
| [01-architecture.md](./design/01-architecture.md) | システムアーキテクチャ |

### 📘 [Development](./development/) - 開発ガイド
どう使うか - 開発者向けガイド（準備中）

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| Frontend (Web) | SolidJS |
| Frontend (Native) | Swift (Vision Pro) |
| Backend (Core) | Rust |
| Backend (Agent) | TypeScript → WASM |
| Data | SurrealDB |
| 通信 | Unison Protocol (QUIC/KDL) |
| P2P同期 | Loro (CRDT) |

## クイックスタート（準備中）

```bash
# Rustバックエンド
cargo build
cargo test

# SolidJSフロントエンド
bun install
bun dev
```

## プロジェクト情報

- **リポジトリ**: [chronista-club/vantage-point](https://github.com/chronista-club/vantage-point)
- **Issue管理**: GitHub Projects
- **関連**: [chronista-club/unison-protocol](https://github.com/chronista-club/unison-protocol)

## ステータス

🚧 **設計フェーズ完了、実装準備中**
