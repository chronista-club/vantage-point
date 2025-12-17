# 開発環境セットアップガイド

## 必要なソフトウェア

### 基本要件

- **macOS**: 14.0 (Sonoma) 以上
- **Rust**: 1.82 以上（Edition 2024）
- **Git**: 2.39 以上
- **Claude CLI**: 最新版

### 推奨環境

- **macOS**: 15.0 (Sequoia)
- **メモリ**: 16GB RAM 以上
- **ストレージ**: 10GB 以上の空き容量

## セットアップ手順

### 1. リポジトリのクローン

```bash
git clone git@github.com:chronista-club/vantage-point.git
cd vantage-point
```

### 2. Rustツールチェーンのインストール

```bash
# rustupのインストール
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 必要なコンポーネント
rustup component add rustfmt clippy
```

### 3. ビルドとインストール

```bash
# ビルド
cargo build --release

# ローカルにインストール
cargo install --path crates/vantage-point
```

### 4. Claude CLIのセットアップ

```bash
# Claude CLIのインストール（npm）
npm install -g @anthropic-ai/claude-code

# 認証
claude auth
```

### 5. 設定ファイルの作成

```bash
# ディレクトリ作成
mkdir -p ~/.config/vantage

# 設定ファイル作成
cat > ~/.config/vantage/config.toml << 'EOF'
default_port = 33000

[[projects]]
name = "vantage-point"
path = "/path/to/vantage-point"
EOF
```

## 開発コマンド

### ビルド

```bash
# デバッグビルド
cargo build

# リリースビルド
cargo build --release

# 特定クレートのみ
cargo build -p vantage-point
```

### テスト

```bash
# 全テスト実行
cargo test --workspace

# 特定テストのみ
cargo test test_name
```

### Lint

```bash
# フォーマットチェック
cargo fmt --all -- --check

# Clippy
cargo clippy --workspace --all-targets
```

### 実行

```bash
# 開発時（ビルド後すぐ実行）
cargo run -p vantage-point -- start

# デバッグモード
cargo run -p vantage-point -- start -d simple
```

## MIDIコントローラー設定（オプション）

AKAI LPD8等のMIDIコントローラーを使用する場合：

```bash
# 利用可能なMIDIポート確認
vp midi-ports

# MIDI入力監視（ポート0）
vp midi 0
```

## トラブルシューティング

### ビルドエラー

```bash
# クリーンビルド
cargo clean && cargo build

# ロックファイル更新
cargo update
```

### ポート使用中エラー

```bash
# 稼働中インスタンス確認
vp ps

# 全インスタンス停止
pkill -f vp
```

### Claude CLI認証エラー

```bash
# 再認証
claude auth logout
claude auth
```

## 関連ドキュメント

- [アーキテクチャ](../design/01-architecture.md)
- [ブランチ戦略](./gitflow-next.md)
