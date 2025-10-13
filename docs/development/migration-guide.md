# ディレクトリ構造リファクタリング移行ガイド

## 概要

VAN-4: プロジェクトのディレクトリ構造をモノレポ形式に再編成しました。

## 変更内容

### ディレクトリマッピング

| 旧パス | 新パス |
|--------|--------|
| `Vantage Vision/` | `apps/visionos/` |
| `Vantage Point/` | `apps/macos/` |
| `Vantage Point for iPad/` | `apps/macos/` (統合) |
| `VantagePointCLI/` | `apps/cli/` |
| `Sources/ClaudeIntegration/` | `packages/claude-integration/` |
| `Packages/RealityKitContent/` | `packages/reality-kit-content/` |
| `Vantage Tests/` | `tests/vantage/` |
| `Tests/ClaudeIntegrationTests/` | `tests/claude-integration/` |
| `repos/unison-claude/` | `tools/unison-claude/` |

## Xcodeプロジェクトの更新手順

### 1. Xcodeでプロジェクトを開く

```bash
open Vantage.xcodeproj
```

### 2. 欠落ファイルの確認

プロジェクトナビゲータで赤く表示されているファイルがあります。これらは移動されたファイルです。

### 3. ファイルパスの再リンク

各欠落ファイルに対して：

1. ファイルを右クリック → "Show File Inspector"
2. Location セクションで新しいパスを選択
3. または、プロジェクトナビゲータから古いファイル参照を削除し、新しいパスから再追加

### 4. ビルド設定の確認

#### Vantage Vision (visionOS)
- **Info.plist パス**: `apps/visionos/Info.plist`
- **ソースルート**: `apps/visionos/`

#### Vantage Point (macOS)
- **Info.plist パス**: `apps/macos/Info.plist`
- **ソースルート**: `apps/macos/`
- **Entitlements**: `apps/macos/Vantage_Mac.entitlements`

#### Vantage Point for iPad
- **Info.plist パス**: `apps/macos/Info-iPad.plist`
- **ソースルート**: `apps/macos/`
- **Entitlements**: `apps/macos/Vantage_iPad.entitlements`

### 5. パッケージ依存関係の更新

#### ClaudeIntegration パッケージ
- 新しいパス: `packages/claude-integration/`
- Package.swift の location を確認

#### RealityKitContent パッケージ
- 新しいパス: `packages/reality-kit-content/`
- Package.swift の location を確認

### 6. テストターゲットの更新

- **VantageTests**: `tests/vantage/` を参照
- **ClaudeIntegrationTests**: `tests/claude-integration/` を参照

## ビルドとテスト

### 1. クリーンビルド

```bash
xcodebuild clean -scheme Vantage
```

### 2. ビルド確認

```bash
# visionOS
xcodebuild -scheme "Vantage Vision" -configuration Debug

# macOS
xcodebuild -scheme "Vantage Point for Mac" -configuration Debug
```

### 3. テスト実行

```bash
xcodebuild test -scheme Vantage -destination 'platform=visionOS Simulator,name=Apple Vision Pro'
```

## トラブルシューティング

### ビルドエラー: "No such module"

**原因**: パッケージ参照が正しく更新されていない

**解決策**:
1. File → Packages → Resolve Package Versions
2. Package Dependencies を削除して再追加

### ビルドエラー: "File not found"

**原因**: Info.plist や Entitlements のパスが古い

**解決策**:
1. Build Settings で "Info.plist File" を検索
2. 新しいパスに更新

### Xcodeが新しいファイル構造を認識しない

**解決策**:
1. Xcodeを終了
2. DerivedData を削除
   ```bash
   rm -rf ~/Library/Developer/Xcode/DerivedData/Vantage-*
   ```
3. Xcodeを再起動

## 確認事項チェックリスト

- [ ] すべてのソースファイルが正しくリンクされている
- [ ] Info.plist パスが更新されている
- [ ] Entitlements パスが更新されている
- [ ] パッケージ依存関係が解決されている
- [ ] ビルドが成功する
- [ ] テストが実行できる
- [ ] アプリが起動する

## 参考リンク

- [GitHub Issue #4](https://github.com/chronista-club/vantage/issues/4)
- [プロジェクト構造ドキュメント](../architecture/overview.md)
