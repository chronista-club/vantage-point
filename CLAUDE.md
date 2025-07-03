# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## プロジェクト概要

VantageはApple Vision Pro向けのvisionOSアプリケーションで、カスタムMetalレンダリングを使用した没入型複合現実体験を提供します。

## 開発コマンド

### ビルド
```bash
# デバッグビルド
xcodebuild -scheme Vantage -configuration Debug

# リリースビルド
xcodebuild -scheme Vantage -configuration Release

# 特定のデバイス向けビルド（Vision Pro Simulator）
xcodebuild -scheme Vantage -destination 'platform=visionOS Simulator,name=Apple Vision Pro'
```

### テスト
```bash
# 全テストの実行
xcodebuild test -scheme Vantage -destination 'platform=visionOS Simulator,name=Apple Vision Pro'

# 特定のテストクラスの実行
xcodebuild test -scheme Vantage -destination 'platform=visionOS Simulator,name=Apple Vision Pro' -only-testing:VantageTests/VantageTests
```

### クリーン
```bash
xcodebuild clean -scheme Vantage
```

## アーキテクチャ

### コア構造
- **VantageApp.swift** - アプリのエントリーポイント。ImmersiveSpaceを設定し、AppModelを環境オブジェクトとして提供
- **AppModel.swift** - アプリケーション状態管理。immersiveSpaceStateとARKitセッションを管理
- **ContentView.swift** - メインUI。3Dモデル表示とToggleImmersiveSpaceButtonを含む
- **Renderer.swift** - Metal/CompositorServicesを使用したカスタムレンダリングパイプライン実装

### レンダリングパイプライン
1. **CompositorServices** - 低レベルレンダリングフレームワーク
2. **Metal** - GPU計算とシェーダー処理
3. **ARKit** - WorldTrackingProviderを通じた空間トラッキング
4. **RealityKit** - 3Dコンテンツとアセット管理

### 重要な型定義
- **ShaderTypes.h** - SwiftとMetal間で共有される型（Uniforms、InstanceData等）
- **Shaders.metal** - 頂点・フラグメントシェーダー実装

### RealityKitContentパッケージ
- **Package.swift** - visionOS 2.0+、macOS 15+、iOS 18+をサポート
- **Sources/RealityKitContent/** - RealityKitアセットとシーン
- **Immersive.usda** - メインの没入型シーン定義

## 開発時の注意点

1. **プラットフォーム要件**: visionOS 2.0以上が必要
2. **Swift 6.0**: 厳格な並行性チェックが有効
3. **Metal Performance**: レンダリングループは高頻度で実行されるため、パフォーマンスに注意
4. **ARKit Session**: デバイストラッキングにはARKit権限が必要