# 開発環境セットアップガイド

## Prerequisites

### 必須

| ツール | バージョン | インストール |
|--------|-----------|-------------|
| macOS | 14.0+ (Sonoma) | — |
| Rust | 1.82+ (Edition 2024) | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| rustfmt + clippy | — | `rustup component add rustfmt clippy` |
| Git | 2.39+ | Xcode CLI Tools |
| Claude Code | 最新版 | `npm install -g @anthropic-ai/claude-code` |
| Node.js | 18+ | Claude Code に必要 |

### 推奨（Rust 製 CLI ツール）

| ツール | 用途 | インストール |
|--------|------|-------------|
| `fd` | ファイル検索 | `cargo install fd-find` |
| `rg` | テキスト検索 | `cargo install ripgrep` |
| `bat` | ファイル表示 | `cargo install bat` |
| `lsd` | ファイル一覧 | `cargo install lsd` |
| `delta` | diff 表示 | `cargo install git-delta` |
| `tokei` | コード統計 | `cargo install tokei` |
| `hyperfine` | ベンチマーク | `cargo install hyperfine` |

### オプション

| ツール | 用途 |
|--------|------|
| AKAI LPD8 | MIDI コントローラー |
| tmux | ターミナル多重化 |
| kitty | TUI 推奨ターミナル |

### 推奨環境

- macOS 15.0 (Sequoia)
- 16GB RAM 以上
- 10GB 以上の空き容量

## セットアップ

### 1. クローン

```bash
git clone git@github.com:chronista-club/vantage-point.git
cd vantage-point
```

### 2. ビルド & インストール

```bash
cargo build --release
cargo install --path crates/vp-cli
```

### 3. Claude Code 認証

```bash
claude auth
```

### 4. 設定ファイル

```bash
mkdir -p ~/.config/vantage
cat > ~/.config/vantage/config.toml << 'EOF'
default_port = 33000

[[projects]]
name = "vantage-point"
path = "/path/to/vantage-point"
EOF
```

## 開発コマンド

```bash
# ビルド
cargo build                              # デバッグ
cargo build --release                    # リリース

# テスト
cargo test --workspace

# Lint
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# 実行
cargo run -p vp-cli -- start             # TUI 起動
cargo run -p vp-cli -- start -d simple   # デバッグモード

# インストール（バイナリ更新）
cargo install --path crates/vp-cli
```

## MIDI 設定（オプション）

```bash
vp midi ports              # ポート一覧
vp midi monitor            # 入力監視
vp lpd8 write              # LPD8 に VP 設定書込み
```

## トラブルシューティング

```bash
# クリーンビルド
cargo clean && cargo build

# ポート使用中
vp ps                      # 稼働中プロセス確認
pkill -f vp                # 全停止

# Claude Code 再認証
claude auth logout && claude auth
```

## References

- [アーキテクチャ](../design/01-architecture.md) (VP-DESIGN-001)
- [リリースフロー](./release.md)
- [テスト戦略](./testing.md)
