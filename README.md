# Vantage Point

**AI ネイティブ開発環境** — Claude CLI をエンジンとして、TUI コンソール + Canvas (WebView) + 外部コントロール (MIDI / MCP) を統合した、Rust 製の開発環境。

**プロジェクト起点**で開発フローを設計し、Claude との対話と Canvas 表示を並列に動かす。前回の続きから再開できるセッション永続化を備え、Mac / Windows native で動作する。

## Status

Private alpha (v0.14.0)。dogfooding を通じて体験を磨きながら使用感を確かめる段階。
README は work in progress、API・内部構造は活発に変化中。

## コアコンセプト

- **VP が主、Claude Code はそのエンジン** — VP はあなたの開発フローの "視点" を提供し、Claude を駆動する
- **TUI で操る、Canvas で視る** — TUI コンソールから Claude と対話し、HTML / Mermaid / 画像 / ログ等は Canvas WebView で同時に視る
- **セッション永続化** — プロジェクト + Pane 構成 + Lead Agent を再開できる、開発の "場" を残す環境
- **deterministic ports** — TheWorld 32000 / Project 33000-33010 / Unison 33100-33110 で透過的固定。`vp port` で確認

## アーキテクチャ (JoJo メタファー)

外向けは普通の用語、内部 codename は JoJo's Bizarre Adventure のスタンド。命名定義は [`crates/vantage-point/src/stands.rs`](crates/vantage-point/src/stands.rs) に集約。

```
TheWorld 👑 (常駐デーモン / Process Manager)
  └── Star Platinum ⭐ (Project Core / TUI 統合ビュー)
        ├── Heaven's Door 📖 (Coding Assistant / Claude CLI orchestrator)
        ├── Paisley Park 🧭 (Information Navigator / Canvas)
        ├── Gold Experience 🌿 (Code Runner / 動的生命注入)
        └── Hermit Purple 🍇 (External Control / MIDI / tmux / MCP)
```

| Stand | 役割 |
|-------|------|
| **TheWorld 👑** | 全 Project Process を統括する常駐デーモン (port 32000)。Push (QUIC self-register) + Pull (port scan) の二重パスで自律復帰 |
| **Star Platinum ⭐** | プロジェクトごとの Process。HTTP + WebSocket + QUIC を持ち、各 Stand の同居場 |
| **Heaven's Door 📖** | Claude CLI の orchestration 層 (OneShot / Interactive / PTY 3 mode) |
| **Paisley Park 🧭** | Canvas / WebView による情報提示と関連検索 |
| **Gold Experience 🌿** | コード実行・スクリプト評価 (Ruby 等) |
| **Hermit Purple 🍇** | MIDI / tmux / MCP の外部コントロール |

## ターゲット環境

| 配布形態 | 状態 | 経路 |
|---------|-----|-----|
| **VantagePoint.app** (macOS Swift app, メニューバー) | 開発中 | `apple/VantagePoint/` (SwiftUI + vp-bridge staticlib) |
| **vp-app** (cross-platform, tao + wry + WebView) | 開発中 | `crates/vp-app/` (Mac / Windows native) |
| **`vp` CLI** | active | `crates/vp-cli/` |

将来的には Mac App Store 配布予定 (有料 + Free プランの可能性あり)。

## クイックスタート

### 前提

- **macOS 13+** (VantagePoint.app) または **Windows 11** (vp-app)
- **Rust 1.94+** (workspace で固定、[mise](https://mise.jdx.dev/) で auto install)
- **Claude CLI** ([インストール手順](https://docs.anthropic.com/en/docs/build-with-claude/claude-code))
- ([mise](https://mise.jdx.dev/) があると tool / env / task が一括管理される — 推奨)

### Mac (Swift VantagePoint.app)

```bash
# mise で rust + node + bun を install してから
mise run mac          # vp-bridge build → xcodegen → xcodebuild → /Applications に install → 起動
mise run mac:build    # ビルドのみ
mise run mac:release  # DMG + 署名 + Notarize (要 keychain profile)
```

### Mac (cross-platform vp-app — tao + wry + WebView)

Swift 版とは別経路。Windows 版と同じコードを Mac で動かす。

```bash
# 別 pane で daemon 起動
cargo run -p vp-cli -- world

# vp-app 起動
cargo run -p vp-app
```

### Windows (vp-app)

`mise run win` 一発でビルド + 配置 + 起動 + ログ tail まで行う。**Git Bash (MINGW64) で実行**。

```bash
# 前提 (初回のみ)
scoop install mingw nasm
rustup target add x86_64-pc-windows-gnu

# ビルド + 起動 + log 監視
mise run win

# その他
mise run win:build    # ビルドのみ
mise run win:logs     # 起動済 vp-app + daemon の log を tail
mise run win:release  # release build
```

詳細は [`docs/guide/setup.md`](docs/guide/setup.md) と [`.mise.toml`](.mise.toml) を参照。

## CLI

```bash
# Core
vp                    # 稼働中インスタンス一覧 (= vp ps)
vp world              # TheWorld 起動 (port 32000)
vp ps                 # Process 一覧
vp config             # 設定と登録プロジェクト表示
vp restart-all        # 全 Process + TheWorld を一括再起動
vp update [--check]   # セルフアップデート

# Project Process
vp sp <subcmd>        # SP (Project Core) 管理
vp pane <subcmd>      # ペイン操作

# Agent / 外部
vp hd <subcmd>        # HD (Claude CLI) 管理
vp mcp                # MCP サーバーモード (stdio JSON-RPC)
vp port <subcmd>      # deterministic port layout 表示
vp ws <subcmd>        # Stone Free 🧵 worker workspace 管理
vp midi <subcmd>      # MIDI ハードウェア操作 (要 midi feature)
vp tmux <subcmd>      # tmux ペイン操作 / capture / dashboard
vp file <subcmd>      # ファイル監視
vp db <subcmd>        # SurrealDB デーモン管理
```

設定ファイル: `~/.config/vantage/config.toml`

## Claude Code との統合

`vp` は MCP サーバーとして Claude CLI に登録できる。

```bash
claude mcp add vp -- vp mcp
```

Claude のセッション中から、Canvas に Markdown / HTML / 画像を表示する `show`、Pane の分割・close、tmux pane の dashboard / capture、Ruby スクリプト評価、deterministic port lookup、TheWorld 上の他 actor との messaging (`msg_*`) など多数の tool が呼べる。

具体的な tool 一覧は `vp mcp` 起動後の MCP capability か [`crates/vantage-point/src/mcp.rs`](crates/vantage-point/src/mcp.rs) を参照。

## プロジェクト構造

```
vantage-point/
├── crates/
│   ├── vantage-point/   # コアロジック (lib)
│   ├── vp-cli/          # `vp` CLI バイナリ
│   ├── vp-app/          # cross-platform native app (tao + wry + WebView)
│   ├── vp-bridge/       # Swift / Rust ブリッジ (staticlib for VantagePoint.app)
│   ├── vp-ccws/         # Stone Free worker workspace
│   ├── vp-db/           # SurrealDB 統合 (embed mode / surrealkv)
│   ├── vp-mdast/        # Markdown → mdast パーサー + TS 型自動生成
│   └── vp-mdast-wasm/   # vp-mdast の WASM ターゲット (Canvas 用)
├── apple/VantagePoint/  # macOS Swift app (SwiftUI, メニューバー + サイドバー)
├── web/                 # WebView HTML / JS
├── docs/                # 仕様 / 設計 / ガイド
├── scripts/             # 補助 script
└── .mise.toml           # tool / env / task 統合管理
```

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Process | Rust (Tokio, Axum, Clap) |
| Native app (cross-platform) | tao + wry + WebView (WebView2 / WKWebView) |
| Native app (macOS) | SwiftUI + AppKit + vp-bridge |
| Agent | Claude CLI + MCP (rmcp) |
| QUIC | unison (in-house) |
| Database | SurrealDB / SQLite |
| MIDI | midir |
| Tool / env / task | mise |

## ドキュメント

- [`CLAUDE.md`](CLAUDE.md) — Claude Code (および AI agent) 向けプロジェクト概要
- [`docs/`](docs/README.md) — Spec / Design / Guide
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — 開発参加ガイド

## ライセンス

**MIT OR Apache-2.0** dual license.

詳細は [LICENSE-MIT](LICENSE-MIT) / [LICENSE-APACHE](LICENSE-APACHE)、コントリビュートは [CONTRIBUTING.md](CONTRIBUTING.md)、セキュリティ報告は [SECURITY.md](SECURITY.md) を参照。
