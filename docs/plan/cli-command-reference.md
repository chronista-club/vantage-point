# Vantage Point CLI コマンドリファレンス

## 概要

このドキュメントは、Vantage Point CLIで利用可能なすべてのコマンドの詳細な仕様を提供します。

## グローバルオプション

すべてのコマンドで利用可能なオプション：

```bash
vantage [global-options] <command> [command-options]
```

| オプション | 短縮形 | 説明 | デフォルト |
|-----------|--------|------|-----------|
| `--verbose` | `-v` | 詳細なログ出力を有効化 | false |
| `--quiet` | `-q` | 最小限の出力のみ表示 | false |
| `--config <path>` | `-c` | カスタム設定ファイルのパス | ~/.vantage/config.json |
| `--no-color` | | カラー出力を無効化 | false |
| `--json` | | JSON形式で出力 | false |
| `--version` | | バージョン情報を表示 | - |
| `--help` | `-h` | ヘルプを表示 | - |

## プロジェクト管理コマンド

### vantage new

新しいプロジェクトを作成します。

```bash
vantage new [options] <project-name>
```

**引数:**
- `<project-name>`: プロジェクト名（必須）

**オプション:**
- `--template <type>`: プロジェクトテンプレート
  - `ar-experience` (デフォルト)
  - `vr-application`
  - `mixed-reality`
  - `custom`
- `--path <directory>`: プロジェクトの作成場所（デフォルト: ~/Documents/Vantage/）
- `--no-git`: Gitリポジトリを初期化しない
- `--open`: 作成後にプロジェクトを開く

**例:**
```bash
# 基本的な使用
vantage new MyProject

# テンプレートとパスを指定
vantage new --template vr-application --path ~/Projects MyVRApp

# 作成後に開く
vantage new --open MyARProject
```

### vantage open

既存のプロジェクトを開きます。

```bash
vantage open [options] <project-path>
```

**引数:**
- `<project-path>`: プロジェクトのパス（必須）

**オプション:**
- `--editor`: エディタで開く
- `--validate`: プロジェクトの整合性を検証

**例:**
```bash
vantage open ~/Documents/Vantage/MyProject
vantage open --validate ./current-project
```

### vantage list

すべてのVantageプロジェクトを一覧表示します。

```bash
vantage list [options]
```

**オプション:**
- `--filter <pattern>`: 名前でフィルタリング
- `--sort <field>`: ソート基準
  - `name` (デフォルト)
  - `modified`
  - `created`
  - `size`
- `--reverse`: 逆順でソート
- `--limit <n>`: 表示件数を制限

**例:**
```bash
# すべてのプロジェクトを表示
vantage list

# 最近更新された5つのプロジェクト
vantage list --sort modified --limit 5

# "Demo"を含むプロジェクト
vantage list --filter Demo
```

### vantage info

プロジェクトの詳細情報を表示します。

```bash
vantage info [options] [project-path]
```

**引数:**
- `[project-path]`: プロジェクトパス（省略時は現在のディレクトリ）

**オプション:**
- `--assets`: アセット情報を含める
- `--stats`: 統計情報を表示

**例:**
```bash
vantage info
vantage info --assets ~/Documents/Vantage/MyProject
```

## アセット管理コマンド

### vantage import

3Dアセットやリソースをプロジェクトにインポートします。

```bash
vantage import [options] <file-path> [destination]
```

**引数:**
- `<file-path>`: インポートするファイルのパス（必須）
- `[destination]`: プロジェクト内の保存先（省略時は自動決定）

**オプション:**
- `--type <asset-type>`: アセットタイプを明示的に指定
  - `model`
  - `texture`
  - `animation`
  - `scene`
  - `auto` (デフォルト)
- `--optimize`: インポート時に最適化
- `--preview`: プレビューを生成
- `--batch`: バッチモード（複数ファイル）

**サポート形式:**
- 3Dモデル: `.usdz`, `.reality`, `.fbx`, `.obj`, `.gltf`, `.glb`
- テクスチャ: `.png`, `.jpg`, `.jpeg`, `.tiff`, `.exr`
- シーン: `.reality`, `.rcproject`

**例:**
```bash
# 基本的なインポート
vantage import model.usdz

# 最適化してインポート
vantage import --optimize --preview character.fbx Models/Characters/

# バッチインポート
vantage import --batch ~/Assets/*.usdz
```

### vantage assets

プロジェクト内のアセットを管理します。

```bash
vantage assets [subcommand] [options]
```

**サブコマンド:**
- `list`: アセット一覧を表示（デフォルト）
- `search <query>`: アセットを検索
- `delete <asset-id>`: アセットを削除
- `optimize <asset-id>`: アセットを最適化
- `export <asset-id>`: アセットをエクスポート

**list オプション:**
- `--type <type>`: タイプでフィルタ
- `--tag <tag>`: タグでフィルタ
- `--unused`: 未使用アセットのみ表示

**例:**
```bash
# すべてのアセットを表示
vantage assets list

# モデルのみ表示
vantage assets list --type model

# アセットを検索
vantage assets search "character"

# アセットを最適化
vantage assets optimize asset_001
```

## Vision Pro連携コマンド

### vantage devices

利用可能なVision Proデバイスを管理します。

```bash
vantage devices [subcommand] [options]
```

**サブコマンド:**
- `list`: デバイス一覧を表示（デフォルト）
- `pair <device-id>`: デバイスをペアリング
- `unpair <device-id>`: ペアリングを解除
- `info <device-id>`: デバイス情報を表示

**例:**
```bash
# デバイス一覧
vantage devices list

# デバイスをペアリング
vantage devices pair VP-001

# デバイス情報
vantage devices info VP-001
```

### vantage connect

Vision Proデバイスに接続します。

```bash
vantage connect [options] [device-id]
```

**引数:**
- `[device-id]`: デバイスID（省略時は最後に接続したデバイス）

**オプション:**
- `--timeout <seconds>`: 接続タイムアウト（デフォルト: 30）
- `--retry <count>`: リトライ回数（デフォルト: 3）

**例:**
```bash
vantage connect
vantage connect VP-001 --timeout 60
```

### vantage sync

プロジェクトをVision Proと同期します。

```bash
vantage sync [options] [project-path]
```

**オプション:**
- `--watch`: ファイル変更を監視して自動同期
- `--force`: すべてのファイルを強制同期
- `--dry-run`: 実際には同期せずに変更内容を表示
- `--exclude <pattern>`: 除外パターン

**例:**
```bash
# 現在のプロジェクトを同期
vantage sync

# ウォッチモードで同期
vantage sync --watch

# ドライラン
vantage sync --dry-run
```

## AI アシスタントコマンド

### vantage ask

AI アシスタントに質問します。

```bash
vantage ask [options] <question>
```

**引数:**
- `<question>`: 質問内容（必須）

**オプション:**
- `--context`: プロジェクトコンテキストを含める
- `--model <model>`: 使用するAIモデル
  - `claude-3-opus` (デフォルト)
  - `claude-3-sonnet`
  - `claude-3-haiku`
- `--max-tokens <n>`: 最大トークン数
- `--stream`: ストリーミング出力

**例:**
```bash
# 基本的な質問
vantage ask "How do I optimize my 3D models?"

# プロジェクトコンテキスト付き
vantage ask --context "What's wrong with my shader code?"

# ストリーミングモード
vantage ask --stream "Explain the rendering pipeline"
```

### vantage generate

AIを使用してコードやアセットを生成します。

```bash
vantage generate [options] <type> <description>
```

**引数:**
- `<type>`: 生成タイプ（必須）
  - `shader`
  - `script`
  - `config`
  - `documentation`
- `<description>`: 生成内容の説明（必須）

**オプション:**
- `--output <path>`: 出力先パス
- `--language <lang>`: プログラミング言語（script の場合）
- `--review`: 生成前にレビュー

**例:**
```bash
# シェーダーを生成
vantage generate shader "Holographic effect with rainbow colors"

# Swiftスクリプトを生成
vantage generate script --language swift "Animation controller for character movement"

# レビュー付きで生成
vantage generate --review config "Performance optimization settings"
```

## ユーティリティコマンド

### vantage config

CLI設定を管理します。

```bash
vantage config [subcommand] [options]
```

**サブコマンド:**
- `get <key>`: 設定値を取得
- `set <key> <value>`: 設定値を設定
- `list`: すべての設定を表示
- `reset`: デフォルトに戻す

**例:**
```bash
# API キーを設定
vantage config set claude.api_key "sk-..."

# 設定を表示
vantage config list

# 特定の値を取得
vantage config get project.default_path
```

### vantage stats

パフォーマンス統計を表示します。

```bash
vantage stats [options] [project-path]
```

**オプション:**
- `--realtime`: リアルタイム更新
- `--export <format>`: エクスポート形式
  - `csv`
  - `json`
  - `html`

**例:**
```bash
vantage stats
vantage stats --realtime
vantage stats --export csv > stats.csv
```

## エラーハンドリング

### 一般的なエラーコード

| コード | 説明 | 対処法 |
|-------|------|--------|
| 1 | 一般的なエラー | エラーメッセージを確認 |
| 2 | ファイルが見つからない | パスを確認 |
| 3 | 権限がない | sudo で実行または権限を確認 |
| 4 | ネットワークエラー | 接続を確認 |
| 5 | 無効な引数 | --help でコマンドの使用法を確認 |
| 10 | プロジェクトエラー | プロジェクトの整合性を確認 |
| 11 | アセットエラー | アセットの形式とパスを確認 |
| 20 | デバイス接続エラー | デバイスの状態を確認 |
| 30 | AI APIエラー | APIキーと制限を確認 |

### エラー時の詳細情報

```bash
# 詳細なエラー情報を表示
vantage --verbose <command>

# デバッグモード
VANTAGE_DEBUG=1 vantage <command>
```