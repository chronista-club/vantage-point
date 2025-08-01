# Vantage Documentation

Apple Vision Pro向けの没入型エディタアプリケーション「Vantage」の技術ドキュメントです。

## 📚 ドキュメント構成

### 🏗️ [Architecture](./architecture/)
システムアーキテクチャと設計に関するドキュメント

- [アーキテクチャ概要](./architecture/overview.md) - システム全体の構成と設計思想
- [Claude統合](./architecture/claude-integration.md) - AI連携機能の実装詳細

### 💻 [Development](./development/)
開発者向けガイドとワークフロー

- [開発環境セットアップ](./development/setup.md) - 初期設定と環境構築
- [Worktree管理](./development/worktree-management.md) - Git worktreeを使った並行開発
- [ビルドとテスト](./development/build-and-test.md) - ビルド手順とテスト実行

### 🔌 [API](./api/)
API仕様とインテグレーションガイド

- [Claude API実装ガイド](./api/claude-api-guide.md) - Swift実装の詳細ガイド
- [サービスプロトコル](./api/service-protocols.md) - プラットフォーム別実装の仕様

### 🖥️ [CLI](./cli/)
Vantage Point CLIツールのドキュメント

- [設計概要](./cli/design.md) - CLIアーキテクチャと設計思想
- [コマンドリファレンス](./cli/commands.md) - 利用可能なコマンド一覧
- [実装ロードマップ](./cli/implementation.md) - 開発計画とマイルストーン
- [技術仕様](./cli/technical-specs.md) - 詳細な技術要件

## 🚀 クイックスタート

1. **開発環境のセットアップ**
   ```bash
   # リポジトリのクローン
   git clone https://github.com/chronista-club/vantage.git
   
   # 依存関係のインストール
   mise install
   ```

2. **ビルドと実行**
   ```bash
   # macOS向けビルド
   xcodebuild -scheme Vantage -configuration Debug
   
   # Vision Pro向けビルド
   xcodebuild -scheme "Vantage Vision" -destination 'platform=visionOS Simulator'
   ```

3. **Worktreeの作成**
   ```bash
   # 新しいIssue用のworktreeを作成
   git worktree add worktrees/UNI-XXX-feature-name -b username/uni-xxx-feature-name
   ```

## 📋 プロジェクト情報

- **リポジトリ**: [chronista-club/vantage](https://github.com/chronista-club/vantage)
- **Issue管理**: [Linear](https://linear.app/chronista/project/vantage)
- **技術スタック**: Swift 6.0, visionOS 2.0+, Metal, RealityKit

## 🤝 貢献方法

1. Linearで適切なIssueを選択または作成
2. Issue専用のworktreeを作成
3. 変更を実装してテストを追加
4. PRを作成してレビューを依頼

詳細は[開発ガイド](./development/)を参照してください。