# 開発環境セットアップガイド

## 必要なソフトウェア

### 基本要件

- **macOS**: 14.0 (Sonoma) 以上
- **Xcode**: 15.0 以上
- **Git**: 2.39 以上
- **mise**: 最新版（ランタイムバージョン管理）

### 推奨環境

- **macOS**: 15.0 (Sequoia) 
- **Xcode**: 16.0
- **メモリ**: 16GB RAM 以上
- **ストレージ**: 50GB 以上の空き容量

## セットアップ手順

### 1. リポジトリのクローン

```bash
# メインリポジトリをクローン
git clone git@github.com:chronista-club/vantage.git ~/Documents/GitHub/vantage
cd ~/Documents/GitHub/vantage
```

### 2. miseのインストールと設定

```bash
# miseのインストール（Homebrew使用）
brew install mise

# miseの初期化
mise install

# 環境変数の設定
eval "$(mise env)"
```

### 3. ワークスペースの準備

```bash
# VANTAGEワークスペースディレクトリを作成
mkdir -p ~/Workspaces/VANTAGE/worktrees

# ワークスペースに移動
cd ~/Workspaces/VANTAGE

# vantage-spaceリポジトリをクローン（オプション）
git clone git@github.com:chronista-club/vantage-space.git .
```

### 4. Xcodeプロジェクトの設定

```bash
# Xcodeでプロジェクトを開く
open ~/Documents/GitHub/vantage/Vantage.xcodeproj
```

Xcodeで以下を確認：
1. **Team ID**が正しく設定されている
2. **Bundle Identifier**が適切に設定されている
3. **Signing & Capabilities**でエラーがない

### 5. APIキーの設定（Claude API使用時）

#### Keychainに保存（推奨）

```bash
# macOSテストアプリで設定画面から入力
# または、プログラムで設定
```

#### 環境変数で設定（開発時）

```bash
# ~/.zshrcまたは~/.bashrcに追加
export CLAUDE_API_KEY="your-api-key-here"
```

### 6. 開発ツールの設定

#### SwiftLint（コード品質）

```bash
brew install swiftlint

# プロジェクトルートに.swiftlint.ymlを作成
```

#### SwiftFormat（コードフォーマット）

```bash
brew install swiftformat
```

## ビルドとテスト

### デバッグビルド

```bash
# macOSアプリ
xcodebuild -scheme "Vantage Point" -configuration Debug

# visionOSアプリ
xcodebuild -scheme "Vantage Vision" -destination 'platform=visionOS Simulator,name=Apple Vision Pro'
```

### テストの実行

```bash
# 全テストを実行
xcodebuild test -scheme Vantage -destination 'platform=macOS'

# 特定のテストを実行
xcodebuild test -scheme Vantage -only-testing:ClaudeIntegrationTests
```

### リリースビルド

```bash
xcodebuild -scheme Vantage -configuration Release archive
```

## 開発ワークフロー

### 1. 新しいIssueの作業開始

```bash
# 1. LinearでIssueを確認（例：UNI-123）

# 2. worktreeを作成
cd ~/Documents/GitHub/vantage
git worktree add ~/Workspaces/VANTAGE/worktrees/UNI-123-feature -b mito/uni-123-feature

# 3. worktreeに移動して開発
cd ~/Workspaces/VANTAGE/worktrees/UNI-123-feature
```

### 2. コミットとプッシュ

```bash
# 変更をステージング
git add .

# コミット（Conventional Commits形式）
git commit -m "feat: 新機能の実装 (UNI-123)"

# リモートにプッシュ
git push -u origin mito/uni-123-feature
```

### 3. Pull Requestの作成

```bash
# GitHub CLIを使用
gh pr create --title "feat: 新機能の実装 (UNI-123)" --body "説明..."
```

## トラブルシューティング

### Xcodeビルドエラー

1. **Clean Build Folder**: `Cmd + Shift + K`
2. **Derived Data削除**: 
   ```bash
   rm -rf ~/Library/Developer/Xcode/DerivedData
   ```
3. **Package依存関係の更新**: `File > Packages > Update to Latest Package Versions`

### mise関連のエラー

```bash
# miseのバージョン確認
mise --version

# 設定の再読み込み
mise trust .mise.toml
mise install --force
```

### Git worktreeのエラー

```bash
# worktreeの状態確認
git worktree list

# 不整合の修正
git worktree prune
```

## 便利な設定

### Xcodeカスタムビヘイビア

1. **Preferences > Behaviors**で設定
2. 「Build Succeeds」→ 「Play Sound」
3. 「Build Fails」→ 「Show Navigator」→ 「Issue Navigator」

### エディタ設定

```bash
# .editorconfig をプロジェクトルートに配置
root = true

[*.swift]
indent_style = space
indent_size = 4
end_of_line = lf
charset = utf-8
trim_trailing_whitespace = true
insert_final_newline = true
```

## 関連ドキュメント

- [Worktree管理ガイド](./worktree-management.md)
- [ビルドとテストガイド](./build-and-test.md)
- [アーキテクチャ概要](../architecture/overview.md)