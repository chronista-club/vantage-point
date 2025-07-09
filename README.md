# Vantage

Apple Vision Pro向けの没入型複合現実アプリケーション。カスタムMetalレンダリングとClaude AI統合を特徴とする空間コンピューティング体験を提供します。

## 機能

- 🥽 **visionOS対応** - Apple Vision Pro向けに最適化された空間UI
- 🤖 **AIアシスタント** - Claude APIを使用したインテリジェントなコーディング支援
- 🎨 **カスタムレンダリング** - Metal/CompositorServicesによる高度な視覚表現
- 🌐 **空間トラッキング** - ARKitによるワールドトラッキング

## プロジェクト構成

```
Vantage/
├── Package.swift           # ClaudeAPIパッケージ定義
├── Vantage.xcodeproj      # 統合プロジェクト
├── Sources/ClaudeAPI/     # Claude API統合ライブラリ
├── Vantage/              # visionOSアプリソース
│   ├── AI/               # AIアシスタント機能
│   ├── Console/          # デバッグコンソール
│   ├── ClaudeTestApp/    # macOSテストアプリ
│   └── Shaders/          # Metalシェーダー
└── ClaudeTestApp/        # macOSテストアプリソース（移行中）
```

## セットアップ

### 1. ClaudeAPIパッケージの追加

1. Xcodeで`Vantage.xcodeproj`を開く
2. File → Add Package Dependencies...
3. "Add Local..."をクリック
4. 現在のディレクトリ（Vantageフォルダ）を選択
5. "Add Package"をクリック

### 2. Claude APIキーの設定

アプリ初回起動時にAPIキー入力画面が表示されます。[Anthropic Console](https://console.anthropic.com/)で取得したAPIキーを入力してください。

### 3. ビルドと実行

```bash
# デバッグビルド
xcodebuild -scheme Vantage -configuration Debug

# Vision Proシミュレータで実行
xcodebuild test -scheme Vantage -destination 'platform=visionOS Simulator,name=Apple Vision Pro'
```

## 開発

### AI アシスタント

VantageにはClaude APIを使用したAIアシスタントが統合されています：

- **空間UI** - ガラスモーフィズム効果を持つ3Dウィンドウ
- **ストリーミング** - リアルタイムレスポンス表示
- **セキュア** - Keychainによる安全なAPIキー管理

### テスト環境

`ClaudeTestApp`はmacOS上でClaude APIの動作を確認するためのテストアプリです。

#### ClaudeTestAppターゲットの追加方法

1. Xcodeで`Vantage.xcodeproj`を開く
2. File → New → Target...
3. macOS → App を選択
4. 以下の設定で作成：
   - Product Name: ClaudeTestApp
   - Interface: SwiftUI
   - Language: Swift
5. 作成後、`Vantage/ClaudeTestApp/`内のファイルをターゲットに追加
6. Build Phases → Link Binary With Libraries で ClaudeAPI を追加

## 技術スタック

- **visionOS 2.0+** - Apple Vision Pro SDK
- **Swift 6.0** - 厳格な並行性チェック
- **Metal** - カスタムGPUレンダリング
- **RealityKit** - 3Dコンテンツ管理
- **Claude API** - AI言語モデル統合

## ライセンス

[ライセンス情報を追加]