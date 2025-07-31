# Vantage Point CLI 設計ドキュメント

## 概要

Vantage Point CLIは、既存のVantage Point macOSアプリケーションのコンソール機能を独立したコマンドラインツールとして提供します。これにより、ターミナルから直接プロジェクト管理、アセット操作、Vision Pro連携、AI機能を利用できるようになります。

## アーキテクチャ

### 全体構造

```
VantagePointCLI/
├── Package.swift
├── Sources/
│   ├── VantagePointCLI/
│   │   ├── main.swift              # エントリーポイント
│   │   ├── Commands/               # コマンド実装
│   │   │   ├── RootCommand.swift
│   │   │   ├── ProjectCommands.swift
│   │   │   ├── AssetCommands.swift
│   │   │   ├── VisionProCommands.swift
│   │   │   └── AICommands.swift
│   │   ├── Core/                   # コア機能
│   │   │   ├── ConsoleManager.swift
│   │   │   ├── ProjectManager.swift
│   │   │   └── ConfigManager.swift
│   │   └── Utils/                  # ユーティリティ
│   │       ├── FileUtils.swift
│   │       └── NetworkUtils.swift
└── Tests/
    └── VantagePointCLITests/
```

### 依存関係

1. **Swift Argument Parser** - コマンドライン引数の処理
2. **ClaudeIntegration** - AI機能の統合（既存ライブラリ）
3. **Foundation** - 基本的なファイル操作とネットワーク
4. **Combine** - 非同期処理とイベント処理

### コンポーネント設計

#### 1. Command Layer
- ArgumentParserプロトコルを使用したコマンド定義
- サブコマンドの階層構造
- バリデーションとエラーハンドリング

#### 2. Core Layer
- **ConsoleManager**: ログ出力とフォーマット管理
- **ProjectManager**: プロジェクトファイルの読み書き
- **ConfigManager**: 設定ファイルの管理（~/.vantage/config.json）

#### 3. Integration Layer
- 既存のVantage Point共有コードの活用
- ClaudeIntegrationライブラリとの統合
- Vision Pro通信プロトコル

## ユーザーインターフェース設計

### コマンド構造

```bash
vantage [global-options] <command> [command-options] [arguments]
```

### グローバルオプション

- `--verbose, -v`: 詳細なログ出力
- `--quiet, -q`: 最小限の出力
- `--config <path>`: カスタム設定ファイルのパス
- `--no-color`: カラー出力を無効化
- `--version`: バージョン情報表示
- `--help, -h`: ヘルプ表示

### プログレス表示

```
Importing asset: model.usdz
[████████████████████████░░░░░░] 80% | 4.2MB/5.3MB | ETA: 2s
```

### インタラクティブモード

特定のコマンドでは、インタラクティブな入力を求める：

```bash
$ vantage new
Project name: MyVisionProject
Project type:
  1) AR Experience
  2) VR Application
  3) Mixed Reality
Select [1-3]: 1
Creating AR Experience project...
✓ Project created at ~/Documents/Vantage/MyVisionProject
```

## セキュリティとプライバシー

### 認証情報の管理

- Keychain Servicesを使用したAPI キーの安全な保存
- 環境変数のサポート（CLAUDE_API_KEY等）
- 設定ファイルの適切なパーミッション管理

### ネットワーク通信

- Vision Proとの通信はTLS暗号化
- API通信時のレート制限対応
- タイムアウトとリトライ処理

## エラーハンドリング

### エラー分類

1. **ユーザーエラー**: 不正な引数、ファイルが見つからない等
2. **システムエラー**: ネットワーク障害、権限不足等
3. **内部エラー**: 予期しない状態、バグ等

### エラー表示フォーマット

```
Error: Failed to import asset
  Reason: File format not supported
  Details: Only .usdz, .reality, and .fbx formats are supported
  
Try: vantage import --help for more information
```

## 拡張性

### プラグインシステム

将来的な拡張のため、プラグインアーキテクチャを考慮：

```swift
protocol VantagePlugin {
    var name: String { get }
    var version: String { get }
    func registerCommands(to app: CommandConfiguration)
}
```

### カスタムコマンドの追加

ユーザーが独自のコマンドを追加できる仕組み：

```bash
~/.vantage/commands/
├── custom-export.swift
└── batch-process.swift
```

## パフォーマンス考慮事項

1. **起動時間の最適化**
   - 遅延ロード
   - 必要最小限の初期化

2. **メモリ使用量**
   - 大規模アセットのストリーミング処理
   - 適切なメモリ管理

3. **並列処理**
   - 複数アセットの同時処理
   - バックグラウンドタスクの実装