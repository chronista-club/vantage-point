# Vantage Point

> 開発行為を拡張する

AIと協働しながら、デバイス・場所に縛られずシームレスに開発を継続できるプラットフォーム。

## クイックスタート

### 必要条件

- macOS 13.0 (Ventura) 以降
- [Claude CLI](https://docs.anthropic.com/en/docs/build-with-claude/claude-code) がインストール済み

### インストール

```bash
# 1. vpコマンドをインストール
curl -L https://github.com/chronista-club/vantage-point/releases/latest/download/vp-aarch64-apple-darwin -o /usr/local/bin/vp
chmod +x /usr/local/bin/vp

# 2. 設定ファイルを作成
mkdir -p ~/.config/vantage
cat > ~/.config/vantage/config.toml << 'EOF'
[[projects]]
name = "my-project"
path = "/path/to/your/project"
EOF

# 3. Standを起動
vp start

# 4. WebUIを開く
vp open
```

### VantagePoint.app（メニューバーアプリ）

Standをメニューバーから操作できるMacアプリ。

1. [VantagePoint.app.zip](https://github.com/chronista-club/vantage-point-mac/releases/latest/download/VantagePoint.app.zip) をダウンロード
2. 解凍して `/Applications` フォルダに移動
3. アプリを起動

メニューバーのアイコンから Stand の起動・停止が可能。

### アップデート

vpとVantagePoint.appは自動で更新をチェックします。

手動アップデート:
```bash
# 最新版をダウンロードして上書き
curl -L https://github.com/chronista-club/vantage-point/releases/latest/download/vp-aarch64-apple-darwin -o /usr/local/bin/vp
```

## コマンド一覧

```bash
vp start [N]      # プロジェクトN番のStandを起動
vp ps             # 稼働中Stand一覧
vp open [N]       # WebUIを開く
vp stop           # Stand停止
vp config         # 設定確認
vp conductor      # Conductorモードで起動（VantagePoint.app用）
```

## コンセプト

### AI主導の選択肢UI

従来のチャットUIではなく、AIが選択肢を提示してユーザーが選ぶスタイル。
移動中でもタップだけで開発を継続できる。

```
AI: 次のステップはどうしますか？
    [A] テストを書く
    [B] リファクタリング
    [C] 次の機能へ
```

### 協調モード

| モード | 説明 |
|--------|------|
| **協調** | ユーザーと一緒に進める |
| **委任** | 任せて、途中経過・結果を確認 |
| **自律** | 完全に任せる |

### シームレスな継続

```
起床 → MIDIパッド → Mac → Vision Pro → 移動中(iPhone) → カフェ(iPad) → 帰宅(Mac)
```

すべて一つのワークスペース上で継続。デバイス間でP2P同期。

## アーキテクチャ

```
┌─────────────────────────────────────────┐
│   VantagePoint.app (Menu Bar)           │
│   メニューバーからStandを操作             │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│   Conductor Stand (vp conductor)        │
│   - VantagePoint.appと通信              │
│   - 複数プロジェクトを管理               │
│   - オートアップデート                   │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│   Project Stand (vp start)              │
│   - Claude Agent SDK                    │
│   - MCP Tools                           │
│   - WebView UI                          │
│   - MIDI入力                            │
└─────────────────────────────────────────┘
```

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Stand | Rust (Tokio, Axum, Clap) |
| Menu Bar App | Swift (AppKit) |
| WebView | wry + tao |
| Agent | Claude CLI + MCP |
| MIDI | midir |

## ドキュメント

| ドキュメント | 内容 |
|-------------|------|
| [docs/spec/01-core-concept.md](docs/spec/01-core-concept.md) | コアコンセプト |
| [docs/spec/02-user-journey.md](docs/spec/02-user-journey.md) | ユーザージャーニー |
| [docs/design/01-architecture.md](docs/design/01-architecture.md) | アーキテクチャ設計 |

## リポジトリ構成

```
vantage-point/              # このリポジトリ（vp CLI）
├── crates/
│   ├── vantage-point/      # メインCLI
│   └── vantage-core/       # 共通ライブラリ
├── web/                    # WebView HTML/JS
└── docs/                   # 仕様・設計

vantage-point-mac/          # メニューバーアプリ
└── VantagePoint/           # Swift Package
```

## 関連リポジトリ

- [chronista-club/vantage-point-mac](https://github.com/chronista-club/vantage-point-mac) - メニューバーアプリ

## ステータス

🚧 **アルファ版 - 開発中**

## ライセンス

Private
