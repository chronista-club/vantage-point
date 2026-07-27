# 開発環境セットアップガイド

## Prerequisites

### 必須

| ツール | バージョン | インストール |
|--------|-----------|-------------|
| macOS | 11.0+ (Big Sur) | — |
| Rust | 1.96 (Edition 2024) | mise が管理（`mise install`）。 手動なら `rustup` |
| rustfmt + clippy | — | `rustup component add rustfmt clippy` |
| Git | 2.39+ | Xcode CLI Tools |
| Claude Code | 最新版 | `npm install -g @anthropic-ai/claude-code` |
| Node.js | 18+ | Claude Code に必要 |

> ツール版の SSOT は `.mise.toml` の `[tools]`（`rust = "1.96"` / node / bun / ruby）。
> `mise install` でrepoに必要な toolchain が一括で揃う（基本 stable 追随、minor pin / patch float）。

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

- macOS 14.0 (Sonoma) 以降（最低は 11.0 Big Sur）
- 16GB RAM 以上
- 10GB 以上の空き容量

## セットアップ

### 1. クローン

```bash
git clone git@github.com:chronista-club/vantage-point.git
cd vantage-point
```

### 2. toolchain を揃える

```bash
mise install              # .mise.toml の rust / node / bun / ruby を一括導入
```

### 3. ビルド & インストール

```bash
cargo build --release
cargo install --path crates/vp-cli   # CLI `vp` をインストール
```

### 4. Claude Code 認証

```bash
claude auth
```

### 5. 設定ファイル

設定は KDL 形式。config / data / state は XDG Base Directory 準拠の 3 zone に分かれる
（全 OS 統一、ディレクトリ名は `vp`）:

| zone | 環境変数 | default | 用途 |
|------|----------|---------|------|
| config | `$XDG_CONFIG_HOME` | `~/.config/vp/` | 人が編集（`config.kdl` / `repos.kdl`） |
| data | `$XDG_DATA_HOME` | `~/.local/share/vp/` | 永続 data store（db / discs） |
| state | `$XDG_STATE_HOME` | `~/.local/state/vp/` | runtime state + log |

登録 repoの SSOT は `~/.config/vp/repos.kdl`:

```bash
mkdir -p "$HOME/.config/vp"
cat > "$HOME/.config/vp/repos.kdl" << 'EOF'
repo "vantage-point" path="/path/to/vantage-point" slot=0
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
cargo run -p vp-cli -- start                # Process（repo）起動
cargo run -p vp-cli -- start -d simple      # デバッグモード

# インストール（バイナリ更新）
cargo install --path crates/vp-cli
```

## MIDI 設定（オプション）

```bash
vp midi ports              # ポート一覧
vp midi monitor            # 入力監視
vp midi lpd8 write         # LPD8 に VP 設定書込み
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
