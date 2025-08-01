# Vantage アーキテクチャ概要

## システム概要

Vantageは、Apple Vision Pro向けの没入型開発環境として設計されたvisionOSアプリケーションです。空間コンピューティングの利点を活かし、従来の2D画面の制約を超えた新しいコーディング体験を提供します。

## コアコンポーネント

### 1. レンダリングエンジン

**Metal + CompositorServices**を基盤とした高性能レンダリングパイプライン

```mermaid
graph TD
    A[CompositorServices] --> B[Metal API]
    B --> C[Custom Shaders]
    C --> D[Renderer.swift]
    C --> E[Shaders.metal]
    C --> F[ShaderTypes.h]
    
    style A fill:#ff9999
    style B fill:#66b3ff
    style C fill:#99ff99
```

- **Renderer.swift**: カスタムレンダリングループの実装
- **Shaders.metal**: 頂点・フラグメントシェーダー
- **ShaderTypes.h**: Swift/Metal間の共有型定義

### 2. 空間トラッキング

**ARKit WorldTrackingProvider**による高精度な空間認識

- デバイスの位置・姿勢トラッキング
- ハンドトラッキングによるジェスチャー入力
- 視線トラッキングによる自然なインタラクション

### 3. AI統合レイヤー

プラットフォーム適応型のClaude AI統合

```mermaid
classDiagram
    class ClaudeServiceProtocol {
        <<interface>>
        +sendMessage()
        +streamMessage()
        +setWorkingDirectory()
    }
    
    class ClaudeCodeService {
        +macOS専用
        +XPC通信
        +ファイル操作
    }
    
    class ClaudeAPIService {
        +visionOS対応
        +HTTP通信
        +軽量実装
    }
    
    ClaudeServiceProtocol <|-- ClaudeCodeService
    ClaudeServiceProtocol <|-- ClaudeAPIService
```

## アプリケーション構造

### エントリーポイント

1. **VantageApp.swift** - アプリケーションのライフサイクル管理
2. **AppModel.swift** - グローバル状態管理（@Observable）
3. **ContentView.swift** - メインUIとナビゲーション

### モジュール構成

```mermaid
graph LR
    subgraph "Vantage Platform"
        VV[Vantage Vision<br/>visionOSアプリ]
        VP[Vantage Point<br/>macOS開発ツール]
        CLI[VantagePointCLI<br/>コマンドライン]
        CI[ClaudeIntegration<br/>AI連携モジュール]
    end
    
    VV --> CI
    VP --> CI
    CLI --> CI
    
    style VV fill:#ff6b6b
    style VP fill:#4ecdc4
    style CLI fill:#45b7d1
    style CI fill:#96ceb4
```

## データフロー

### 1. ユーザー入力データフロー

```mermaid
flowchart TD
    A[ユーザージェスチャー/音声] --> B[ARKit/RealityKit]
    B --> C[AppModel<br/>状態更新]
    C --> D[View更新/レンダリング]
    
    style A fill:#ffeb3b
    style B fill:#ff9800
    style C fill:#2196f3
    style D fill:#4caf50
```

### 2. AI処理フロー

```mermaid
flowchart TD
    A[ユーザーリクエスト] --> B[ClaudeServiceFactory]
    B --> C{プラットフォーム判定}
    C -->|macOS| D[ClaudeCodeService]
    C -->|visionOS| E[ClaudeAPIService]
    D --> F[レスポンス処理]
    E --> F
    F --> G[UI更新]
    
    style A fill:#ffeb3b
    style B fill:#ff9800
    style C fill:#9c27b0
    style D fill:#2196f3
    style E fill:#4caf50
    style F fill:#ff5722
    style G fill:#795548
```

## セキュリティ設計

### APIキー管理
- **Keychain Services**による安全な保存
- プラットフォーム別のアクセス制御
- 環境変数からの読み込みサポート

### サンドボックス
- macOS App Sandboxによる制限付きファイルアクセス
- Security Scoped Bookmarksによる永続的アクセス権

## パフォーマンス最適化

### レンダリング最適化
- Metal Performance Shadersの活用
- フレームレート適応制御（60/90/120 FPS）
- GPU並列処理の最大化

### メモリ管理
- ARC + Swift Concurrencyによる自動管理
- 大規模データのストリーミング処理
- テクスチャ/メッシュの動的ロード/アンロード

## 拡張性

### プラグインシステム（計画中）
- 言語サーバープロトコル（LSP）サポート
- カスタムレンダラーの追加
- サードパーティツール連携

### マルチプラットフォーム展開
- visionOS (ネイティブ)
- macOS (Catalyst/ネイティブ)
- iOS/iPadOS (将来対応)