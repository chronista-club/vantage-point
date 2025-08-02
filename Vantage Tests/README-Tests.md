# Working Directory機能のテスト

このディレクトリには、作業ディレクトリ機能のユニットテストと統合テストが含まれています。

## テストファイル一覧

### 1. WorkingDirectoryManagerTests.swift
- `WorkingDirectoryManager`クラスの単体テスト
- Security Scoped Bookmarkの作成と復元
- ブックマークの永続化機能
- ディレクトリアクセス権限の管理

### 2. BookmarkedDirectoryTests.swift
- `BookmarkedDirectory`構造体のテスト
- Codableプロトコルの実装検証
- プロパティの初期化と計算プロパティ

### 3. ChatViewModelWorkingDirectoryTests.swift
- `ChatViewModel`の作業ディレクトリ関連機能
- ファイル内容の読み込みと送信
- エラーハンドリング
- システムプロンプトへの統合

### 4. FileItemTests.swift
- `FileItem`モデルのテスト
- ファイルタイプの判定とアイコン選択
- ファイルサイズのフォーマット
- ソート機能の検証

### 5. WorkingDirectoryIntegrationTests.swift
- 完全なワークフローの統合テスト
- 複数インスタンス間での永続化
- 複数ファイルの処理

### 6. AllTests.swift
- テストスイートの概要
- カバレッジサマリー

## テストの実行方法

### Xcodeから実行
1. Xcodeでプロジェクトを開く
2. Product > Test (⌘U) を選択
3. または、Test Navigatorから個別のテストを実行

### コマンドラインから実行
```bash
# すべてのテストを実行
xcodebuild test -project Vantage.xcodeproj -scheme "Vantage Mac" -destination 'platform=macOS'

# 特定のテストクラスを実行
xcodebuild test -project Vantage.xcodeproj -scheme "Vantage Mac" -destination 'platform=macOS' \
    -only-testing:VantageTests/WorkingDirectoryManagerTests
```

## テストカバレッジ

### カバーされている機能
- ✅ ディレクトリの選択と保存
- ✅ Security Scoped Bookmarkの管理
- ✅ ブックマークの永続化（UserDefaults）
- ✅ ファイル一覧の取得
- ✅ ファイル内容の読み込み
- ✅ Claude APIとの統合
- ✅ エラーハンドリング
- ✅ UI初期化

### 今後追加可能なテスト
- [ ] ファイル監視機能
- [ ] 大量ファイルのパフォーマンステスト
- [ ] 権限エラーのエッジケース
- [ ] UI操作のインテグレーションテスト

## 注意事項

1. **サンドボックス環境**: テストは App Sandbox が有効な環境で実行されます
2. **一時ディレクトリ**: テストは `FileManager.default.temporaryDirectory` を使用
3. **クリーンアップ**: 各テストは終了時に作成したファイルを削除します
4. **非同期テスト**: `@MainActor` を使用した非同期テストが含まれています

## トラブルシューティング

### テストが失敗する場合
1. Xcodeのスキーム設定を確認
2. テストターゲットのビルド設定を確認
3. 必要な権限（ファイルアクセス）が設定されているか確認

### ビルドエラーが発生する場合
1. 新しいファイルがXcodeプロジェクトに追加されているか確認
2. `@testable import Vantage_Point_for_Mac` が正しいか確認
3. Swift バージョンの互換性を確認