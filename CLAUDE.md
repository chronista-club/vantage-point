# CLAUDE.md

## プロジェクト概要

Vantage Point（`vp`）は Rust製の **AI ネイティブ開発環境**。
Claude CLI をエンジンとして、TUI コンソール・Canvas（WebView）・外部コントロールを統合した開発体験を提供する。
OSS（MIT/Apache-2.0 dual ライセンス）として公開。配布は `.dmg` 直配布（GitHub Releases）/ Homebrew tap（`chronista-club/tap`）/ `cargo install` の三本柱。Mac/Win/Linux 対応。

<!--
配布方針メモ — Mac App Store ではなく直接配布（2026-04-18 OSS 化決定で App Store 配布を見送り）:

VP のような Claude Code / Gemini CLI 連携アプリは Mac App Store で配布できない。
理由 = 外部プロセスの spawn・任意コマンドの実行・ファイルシステム全域へのアクセスが
App Sandbox 要件に反するため（iTerm2 / VS Code 等の開発ツールが軒並み App Store 外
なのと同じ）。加えて OSS ライセンスと App Store 規約・IAP（アプリ内課金）の相性も悪い。
→ Developer ID 署名 + notarization 済みの `.dmg` を直接配布するのが定石。

旧方針（2026-03-13）は App Store + サブスク課金だったが、2026-04-18 の OSS 化決定で
転換。詳細は creo-memories `mem_1CaB5PmdWNfPPVR1UkFYLC`（配布戦略の転換）を参照。
-->

### プロジェクト方針

**VP は焦らず、使用感を確かめながら、熟慮・議論を重ねて進化させるプロジェクト。**
Creo Memories（サービス）とは異なり、「自分のような開発フロー」のためのアプリ。
dogfooding を通じて体験を磨き、納得できる完成度でリリースする。

### コアコンセプト

- **AI ネイティブ開発環境**: VP が主、Claude Code はそのエンジン
- **プロジェクト起点**: プロジェクト選択 → TUI コンソール → Claude との対話が 1st ビュー
- **Canvas + TUI**: TUI で操る、Canvas で視る。両者が並列に動く
- **セッション永続化**: 前回の続きから再開できる開発環境

### アーキテクチャ命名体系（JoJo メタファー）

外向けは普通の用語メイン + JoJo 名を小さく併記（機能イメージを伝える目的）。
命名定義は `crates/vantage-point/src/stands.rs` に集約。

```
TheWorld 👑 (Process Manager / 常駐デーモン)
  └── Star Platinum ⭐ (Project Core / TUI 統合ビュー + 各 Stand が同居する場)
        ├── Echoes 💬 (Coding Assistant / Claude CLI オーケストレーター、 旧 Heaven's Door 📖)
        ├── Paisley Park 🧭 (Information Navigator / Canvas・情報提供)
        ├── Gold Experience 🌿 (Code Runner / 動的生命注入エンジン)
        └── Hermit Purple 🍇 (External Control / MIDI・tmux・MCP)
```

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Process | Rust (Tokio, Axum, Clap) |
| WebView | wry + tao |
| Frontend | SolidJS + xterm.js + creo-ui (vp-app web-bundle) |
| Agent | Claude CLI + MCP |
| MIDI | midir |

> **Process**: プロジェクトの開発プロセスを表す本体。JoJo の Stand（能力）を保持し、ユーザーの開発を支援する。

### システム構成

```
vp-app (GUI: wry+tao)   vp (CLI)
        └────────┬───────┘
                 │ HTTP + QUIC
        TheWorld 👑 :32000          ← Process Manager (常駐 daemon)
                 │ spawn + reconcile (Push/Pull 二重パス)
     ┌───────────┼───────────┐
   SP :33000   SP :33001   ...      ← Star Platinum ⭐ (project ごと)
     └ Stands: Echoes 💬 / Paisley Park 🧭 / Gold Experience 🌿 / Hermit Purple 🍇
```

## プロジェクト構造

```
vantage-point/
├── crates/
│   ├── vantage-point/   # server lib (TheWorld + SP の HTTP/WS server)
│   ├── vp-paths/        # config/data/state path 解決 (XDG SSOT、 vantage-point + vp-app 共有)
│   ├── vp-app/          # Rust GUI (wry + tao + xterm.js + creo-ui) — Mac 主軸 (2026-04-26 移行)
│   ├── vp-cli/          # CLI binary (vp、 lane lib も内包)
│   └── vp-mdast{,-wasm}/ # Markdown AST parser (+ wasm binding)
├── docs/
│   ├── spec/            # 仕様書
│   ├── design/          # 設計書
│   └── guide/           # 開発ガイド
└── .claude/             # Claude Code設定
```

## CLIコマンド

```bash
# Core
vp start [N]           # プロジェクトN番のProcessを起動
vp start -d simple     # デバッグモードで起動
vp stop [--port]       # Process停止
vp restart [--port]    # Process再起動
vp ps                  # 稼働中インスタンス一覧
vp open [N]            # WebUIを開く
vp config              # 設定と登録プロジェクト表示
vp mcp                 # MCPサーバーモード（stdio）
vp update [--check]    # セルフアップデート

# TheWorld（Daemon 統合）
vp daemon              # TheWorld 起動（プロジェクト管理 + PTY管理、 alias: vp world）
vp daemon start|stop|status  # subcommand 形式

# App
vp app start           # vp-app GUI 起動（spawn + 即 exit、 cwd を起点に開く）
vp app stop            # vp-app を停止
# 再起動は `vp app stop && vp app start` で合成 (restart は意図的に CLI に持たない)
vp tray                # システムトレイモード

# MIDI
vp midi monitor|ports
vp midi lpd8 write|switch|ports
```

## 開発コマンド

```bash
cargo build --release -p vantage-point   # ビルド
cargo test --workspace                    # テスト
cargo install --path crates/vp-cli        # インストール（codesign 自動付与）
cargo fmt --all -- --check                # フォーマットチェック
cargo clippy --workspace --all-targets    # Lint
```

## 設定・ポート

- config / data / state パスは **XDG Base Directory 準拠の 3 zone に統一**（VP-189 / #460、全 OS 共通、ディレクトリ名は `vp`）。定義は **`crates/vp-paths`**（vantage-point + vp-app 共有の SSOT。`vantage_point::config` は `pub use vp_paths::{...}` で re-export、vp-app は直接依存）。

  | zone | env | default | 用途 |
  |------|-----|---------|------|
  | **config** (`vp_config_dir()`) | `$XDG_CONFIG_HOME` | `~/.config/vp/` | 人が編集（`config.kdl` / `projects.kdl` / `addresses.toml`） |
  | **data** (`vp_data_dir()`) | `$XDG_DATA_HOME` | `~/.local/share/vp/` | 永続 data store（db / discs） |
  | **state** (`vp_state_dir()`) | `$XDG_STATE_HOME` | `~/.local/state/vp/` | runtime state + log（`session.json` / `sessions/` / `log/`） |

  - 設定ファイルは **KDL**（`vp_config_dir()/config.kdl`、人が編集する read-only global 設定）。登録プロジェクトの SSOT は `projects.kdl`（VP-188、config.kdl には出さない）。
  - 起動時に旧パス（Application Support / Library/Logs / `dirs::config_dir()/vantage/` 等）から新 XDG zone へ冪等にデータ移行（`migrate_legacy_paths()`、旧データは残す）。
- ポート割り当て:
  - TheWorld: 32000 (HTTP + QUIC)
  - Project: 33000〜33010 (HTTP + WS)
  - Unison per Process: 33100〜33110 (QUIC, +100)
- `vp ps` で 33000-33010 をスキャン

### プロセス管理（Reconciliation）

TheWorld が **Push + Pull の二重パス** でプロセスを管理。どちらが落ちても自律復帰する。

| パス | 仕組み | 用途 |
|------|--------|------|
| **Push (QUIC Registry)** | SP が TheWorld に QUIC 永続接続で自己登録。切断 = 即時除去 | リアルタイム検出 |
| **Pull (ポートスキャン)** | TheWorld が 33000-33010 を 30秒間隔でスキャン | QUIC 障害時の自律復帰 |

- `running_processes` / `projects` の HashMap キーは正規化パス（`normalize_path_key()`）。`project_name` は表示用ラベル
- `/api/health` レスポンスに `stands` フィールドを含む（各 Stand の状態をリアルタイムで返す）

## Agent モジュール

Claude CLI統合の実装。3つの実行モードを提供:

| モード | CLI形式 | 用途 |
|--------|---------|------|
| **OneShot** | `claude -p "prompt"` | 単発プロンプト |
| **Interactive** | `claude -p --input-format stream-json` | 持続プロセス、複数ターン |
| **PTY** | `claude` (対話モード) | PTY経由の対話モード、Multiplexer Orchestration用 |

### Stream-JSON 入力フォーマット

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"メッセージ"}]}}
```

### PTYモード API

`pty-process` クレートを使用:

- `PtyClaudeAgent::start()` - PTY付きでClaude CLI起動
- `PtyClaudeAgent::send()` / `send_raw()` - テキスト / 制御シーケンス送信
- `PtyClaudeAgent::resize()` - ターミナルサイズ変更
- `PtyClaudeAgent::events()` - 出力イベント受信

## コーディング規約

- **コメントは日本語で記述する**
- data / calculations / actions を明確に分離

## デバッグモード

| モード | 用途 | 起動方法 |
|--------|------|----------|
| `none` | 本番運用 | `vp start` |
| `simple` | 基本的なイベントログ | `vp start -d simple` |
| `detail` | 詳細なデータ・タイミング | `vp start -d detail` |

### ログ出力

```rust
// Simple
state.send_debug("category", "メッセージ", None);

// Detail
state.send_debug_detail("category", "メッセージ", serde_json::json!({"key": "value"}));
```

カテゴリ: `connection`, `pty`, `permission`, `agent`, `timing`, `tool`

### 問題調査フロー

1. `vp start -d detail` で起動
2. WebUIデバッグパネル（右パネル）でログ確認
3. ブラウザコンソールで `Received:` ログ確認
4. 必要に応じてログ追加 → 再ビルド

## MCP ツール補足

### capture_terminal

- `CGWindowListCopyWindowInfo`（`swift -e`）でウィンドウ ID を取得
- プロセス名は `"Vantage Point"`（スペースあり）で照合

## プロジェクト管理（creo-memories）

task 管理は creo-memories に一本化（Linear は不使用、2026-05-19 確定）。GitHub Issues も使わない。

### ルール

- **task = memory**: `remember` で起票（atlas は `CLAUDE.local.md` 参照）。`status`（active=TODO/進行中, done=完了）で lifecycle 管理、priority は tag（`priority:high|medium|low`）
- ブランチ名: `mako/{slug}` 形式（task memory の slug から推論）
- PR: `gh` で作成。関連 task memory の ID を PR 本文に記載
- 他プロジェクト横断の task は creo-memories の shared context に集約

### ブランチ運用 — nightly / main 二段（2026-05-29 確定）

開発の最新は **nightly**、 公開 release のみ **main** が進む二段運用。
**GitHub default branch は `main`**（= 公開の顔、 visitor / cloner が安定版を見る。 2026-06-03 に nightly→main へ変更、 公開 OSS 慣習に合わせた）。 一方 **day-to-day の dev trunk（= lane base / PR base）は `nightly`** で不変。 この 2 つ（公開 default と dev trunk）は **意図的に decouple** している。

> ⚠️ default 変更の副作用: `gh pr create` の base 既定が `main` になった。 **feature PR は必ず `--base nightly` を明示**すること（lane フロー規約と一致、 下記）。 dev work を誤って main に向けない。

| branch | 役割 | 直 push | PR | 更新元 |
|---|---|---|---|---|
| **nightly** | **dev trunk**（day-to-day 積み上げ・lane base・**PR base**） | 可（force / deletion 禁止） | 任意 | lane → PR or 直 push |
| **main** | **GitHub default**（公開の顔）+ 公開 release の単位（= 「ここを参照すれば最新安定」） | **禁止** | 必須（force / deletion 禁止） | nightly → release PR → tag cut |
| **lane / performer** | 単一タスク隔離 | 自由 | 必須 | from nightly |

#### lane 作業フロー（lead session = メインセッション向け）

1. `git fetch origin nightly && git checkout -b mako/{slug} origin/nightly` で lane 開始
2. lane 上で commit、 PR は **base = nightly** で `gh pr create --base nightly` で作る
3. PR merge / 直 push で nightly が進む
4. nightly が一定量積み上がったら release PR (nightly → main) を切って tag cut

> 上記 step 1 の `checkout -b` は **lead checkout を占有する単独セッション専用**。並列 worker（wing / 他 agent）は worktree lane（`vp lane new` or `git worktree add`）を使う — cross-agent な lane 規約（公認入口 / raw-git fallback / discovery）の SSOT は **`AGENTS.md`**。

#### release flow（= nightly → main）

```
nightly  ───────────────────────────────────►
            │                  │
            │ release PR       │ tag cut（vX.Y.Z）
            ▼                  ▼
main    ───●──────────────────●──────────────►
                              │
                              ▼
                     GitHub Release（.dmg / homebrew / cargo install）
```

- release PR は `release: vX.Y.Z` のような形で nightly → main を merge
- merge 後に `git tag vX.Y.Z` + `mise run release:mac` で notarized `.dmg` を build → GitHub Release publish → Homebrew cask 自動更新
- Phase 2 で `release-please` 等の自動化を検討

#### 配布チャネル / Install

| チャネル | コマンド | 中身 |
|---|---|---|
| **Homebrew cask**（推奨） | `brew tap chronista-club/tap && brew install --cask vantage-point` | notarized `.dmg`（GUI + `vp` CLI 同梱、 arm64） |
| **.dmg 直 DL** | [GitHub Releases](https://github.com/chronista-club/vantage-point/releases/latest) の `VantagePoint-<ver>-arm64.dmg` | 同上（portal DL 動線の source） |
| **cargo install** | `cargo install --path crates/vp-cli` | `vp` CLI のみ（開発者向け） |

- **Homebrew tap**: `chronista-club/homebrew-tap`（`Casks/vantage-point.rb`）。release 時に `release:mac` → `release:cask`（mise）が version/sha256 を自動反映して push する。cask だけ手動更新するなら `mise run release:cask`。
- App Store は CC 依存で sandbox 不可のため非対象（上部「配布方針メモ」参照）。

## クロスプロジェクト協業（MARU x VP）

MARU（ESP32-S3物理コントローラ）との連携開発。設計・経緯は creo-memories に記録（`category: "cross-project"` + `from: "vp"`）。

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **vantage-point** (11394 symbols, 24983 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/vantage-point/context` | Codebase overview, check index freshness |
| `gitnexus://repo/vantage-point/clusters` | All functional areas |
| `gitnexus://repo/vantage-point/processes` | All execution flows |
| `gitnexus://repo/vantage-point/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
