# 開発環境セットアップ

macOS を主な開発環境としています。Windows は Windows 上で確認し、Linux は未検証です。
配布アプリを使うだけの場合は [README](../../README.md) を参照してください。

## 必要なもの

- Xcode Command Line Tools（Git・C/C++ toolchain）
- Rust と rustfmt / clippy。版は [`.mise.toml`](../../.mise.toml) が正本
- Bun（WebView の依存解決・テスト・bundle）
- `protoc`（club-unison の build script で使用。macOS は `brew install protobuf`）
- mise task を使う場合は mise と Ruby（版も `.mise.toml`）
- AI の動作確認をする場合は、対象 engine の CLI と認証

tmux や外部 DB サーバーは不要です。AI CLI の導入と認証は各 engine の公式手順に従ってください。

## ソースを用意する

```bash
git clone --branch nightly https://github.com/chronista-club/vantage-point.git
cd vantage-point
mise install
rustup component add rustfmt clippy
brew install protobuf
```

この repo の開発 trunk は `nightly`、`main` は公開 release 用です。
作業を分ける場合は [AGENTS.md](../../AGENTS.md) に従って lane を作ります。

## ビルドと検証

WebView bundle は gitignore 対象の生成物で、Rust のビルド前に必要です。

```bash
mise run app:bundle
cargo build -p vp-cli -p vp-app
cargo test --workspace --features vp-nexus/test-utils
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

mise を使わず bundle を作る場合:

```bash
cd crates/vp-app/webview
bun install --frozen-lockfile
bun run test
bun run typecheck
bun run build
cd ../../..
```

Rust のテストは KDL / ts-rs の生成型を更新します。テスト後の `git diff` を確認し、
型や schema を変更した場合は生成物も同じ commit に含めます。CI は生成差分や未追跡ファイルが残れば失敗します。

CLI だけインストールする場合は `cargo install --path crates/vp-cli --locked`。
Homebrew と併用する開発機は [CLAUDE.md](../../CLAUDE.md) の dev profile 手順を参照してください。

## 起動

```bash
cargo run -p vp-cli -- daemon start
cargo run -p vp-cli -- repos add vantage-point /path/to/vantage-point
cargo run -p vp-cli -- repos start vantage-point
cargo run -p vp-app
```

配布アプリと同じ形で GUI を確認する開発用 task は `mise run app:swap` です。
これは `/Applications/VantagePoint.app` を差し替えて起動します。
server 側の変更を反映する `VP_SWAP_RESTART_DAEMON=1 mise run app:swap` は
全 repo と engine を再起動するため、VP の lane の外から実行してください。

## 設定と保存先

| zone | 環境変数 | 既定 | 用途 |
|------|----------|------|------|
| config | `XDG_CONFIG_HOME` | `~/.config/vp/` | `config.kdl`・`settings.kdl`・`repos.kdl`・`vp-app.toml` |
| data | `XDG_DATA_HOME` | `~/.local/share/vp/` | 永続データ |
| state | `XDG_STATE_HOME` | `~/.local/state/vp/` | runtime state・ログ |

repo 登録は `vp repos add` を使います。既存の `repos.kdl` をセットアップ用コマンドで上書きしないでください。
設定の所有と書き手は [設計 59](../design/59-settings-page.md) を参照してください。

## トラブルシューティング

- bundle が見つからない: `mise run app:bundle` を実行してから Rust をビルドします。
- codegen の差分で CI が失敗: `cargo test -p vp-app` を実行し、生成差分を確認して commit します。
- daemon に接続できない: `vp daemon status` と `vp ps`、state ディレクトリのログを確認します。
- repo だけを止める: `vp repos stop <name>`。自動起動も止める場合は `vp repos disable <path>`。
- chat の送信に失敗: 理由を確認して「入力欄に戻す」。受付結果が不明と表示された場合は、会話の応答を確認してから再送します。

## 関連

[現行設計の索引](../README.md) · [WebView 開発](webview.md) · [リリース](release.md) · [テスト戦略](testing.md)
