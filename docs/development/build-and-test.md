# ビルドとテストガイド

## ビルド設定

### スキーム構成

Vantageプロジェクトには以下のスキームが含まれています：

| スキーム | ターゲット | プラットフォーム | 用途 |
|---------|-----------|---------------|------|
| Vantage Vision | visionOSアプリ | visionOS 2.0+ | メインアプリ |
| Vantage Point | macOSアプリ | macOS 14.0+ | 開発ツール |
| VantagePointCLI | CLIツール | macOS 14.0+ | コマンドライン |
| ClaudeIntegration | フレームワーク | iOS/macOS/visionOS | AI連携 |

### ビルド構成

- **Debug**: 開発・デバッグ用（最適化なし、デバッグシンボル付き）
- **Release**: リリース用（最適化あり、デバッグシンボル別ファイル）
- **Profile**: パフォーマンス計測用（最適化あり、Instrumentsサポート）

## ビルドコマンド

### 基本的なビルド

```bash
# macOSアプリのビルド
xcodebuild -scheme "Vantage Point" -configuration Debug build

# visionOSアプリのビルド（シミュレータ）
xcodebuild -scheme "Vantage Vision" \
  -destination 'platform=visionOS Simulator,name=Apple Vision Pro' \
  build

# CLIツールのビルド
xcodebuild -scheme VantagePointCLI -configuration Release build
```

### アーカイブとエクスポート

```bash
# アーカイブの作成
xcodebuild -scheme "Vantage Vision" \
  -configuration Release \
  -archivePath ./build/Vantage.xcarchive \
  archive

# IPAファイルのエクスポート
xcodebuild -exportArchive \
  -archivePath ./build/Vantage.xcarchive \
  -exportPath ./build/export \
  -exportOptionsPlist ExportOptions.plist
```

### Swift Packageのビルド

```bash
# ClaudeIntegrationパッケージのビルド
cd Sources/ClaudeIntegration
swift build

# リリースビルド
swift build -c release

# 特定のプラットフォーム向けビルド
swift build --platform macos
```

## テスト実行

### ユニットテスト

```bash
# 全テストの実行
xcodebuild test -scheme Vantage \
  -destination 'platform=macOS' \
  -resultBundlePath TestResults.xcresult

# 特定のテストクラスのみ実行
xcodebuild test -scheme Vantage \
  -destination 'platform=macOS' \
  -only-testing:ClaudeIntegrationTests/ClaudeServiceTests

# テストの並列実行
xcodebuild test -scheme Vantage \
  -destination 'platform=macOS' \
  -parallel-testing-enabled YES \
  -maximum-concurrent-test-device-destinations 4
```

### UIテスト

```bash
# UIテストの実行
xcodebuild test -scheme "Vantage Vision" \
  -destination 'platform=visionOS Simulator,name=Apple Vision Pro' \
  -only-testing:VantageUITests
```

### テストカバレッジ

```bash
# カバレッジ付きでテスト実行
xcodebuild test -scheme Vantage \
  -destination 'platform=macOS' \
  -enableCodeCoverage YES \
  -resultBundlePath TestResults.xcresult

# カバレッジレポートの生成
xcrun xccov view --report TestResults.xcresult
```

## CI/CD設定

### GitHub Actions設定例

```yaml
name: Build and Test

on:
  push:
    branches: [ main, edge ]
  pull_request:
    branches: [ main ]

jobs:
  build:
    runs-on: macos-14
    
    steps:
    - uses: actions/checkout@v4
    
    - name: Select Xcode
      run: sudo xcode-select -s /Applications/Xcode_16.0.app
    
    - name: Build
      run: |
        xcodebuild -scheme Vantage \
          -destination 'platform=macOS' \
          build
    
    - name: Test
      run: |
        xcodebuild test -scheme Vantage \
          -destination 'platform=macOS' \
          -resultBundlePath TestResults.xcresult
    
    - name: Upload Coverage
      uses: codecov/codecov-action@v3
      with:
        xcode: true
        xcode_archive_path: TestResults.xcresult
```

## デバッグ

### LLDBデバッグ

```bash
# デバッグビルドの実行
xcodebuild -scheme "Vantage Point" \
  -configuration Debug \
  -derivedDataPath ./DerivedData \
  build

# LLDBでアプリを起動
lldb ./DerivedData/Build/Products/Debug/Vantage\ Point.app/Contents/MacOS/Vantage\ Point
```

### ログ出力

```swift
// デバッグログの設定
#if DEBUG
let logger = Logger(subsystem: "com.chronista.vantage", category: "Claude")
logger.debug("API Request: \(request)")
#endif
```

### メモリデバッグ

```bash
# Address Sanitizerを有効化
xcodebuild -scheme Vantage \
  -configuration Debug \
  -enableAddressSanitizer YES \
  build
```

## パフォーマンス計測

### Instrumentsの使用

```bash
# Time Profilerでの計測
instruments -t "Time Profiler" \
  -D trace.trace \
  ./DerivedData/Build/Products/Debug/Vantage\ Point.app
```

### ビルド時間の最適化

```bash
# ビルド時間の計測
xcodebuild -scheme Vantage \
  -showBuildTimingSummary \
  build

# 並列ビルドの有効化
defaults write com.apple.dt.XCBuild EnableSwiftBuildSystemIntegration 1
```

## トラブルシューティング

### よくあるビルドエラー

#### 1. 依存関係の解決エラー
```bash
# Package.resolvedをリセット
rm Package.resolved
xcodebuild -resolvePackageDependencies
```

#### 2. コード署名エラー
```bash
# 証明書の確認
security find-identity -v -p codesigning

# プロビジョニングプロファイルの更新
xcodebuild -allowProvisioningUpdates
```

#### 3. SwiftUIプレビューのクラッシュ
```bash
# プレビューキャッシュのクリア
rm -rf ~/Library/Developer/Xcode/UserData/Previews
```

### デバッグ用環境変数

```bash
# SQLiteデバッグ
export SQLITE_ENABLE_DEBUG_LOGGING=1

# URLSessionログ
export CFNETWORK_DIAGNOSTICS=3

# SwiftUIデバッグ
export SWIFTUI_PROFILE_DRAW=1
```

## ベストプラクティス

### 1. インクリメンタルビルド
モジュール化により、変更箇所のみの再ビルドで時間を短縮

### 2. テストの独立性
各テストは他のテストに依存せず、単独で実行可能に

### 3. CI統合
プルリクエストごとに自動ビルド・テストを実行

### 4. ビルド設定の共有
`.xcconfig`ファイルで設定を一元管理

## 関連ドキュメント

- [開発環境セットアップ](./setup.md)
- [アーキテクチャ概要](../architecture/overview.md)
- [CI/CD設定ガイド](./ci-cd.md)