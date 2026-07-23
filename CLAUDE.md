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
        └── Hermit Purple 🍇 (External Control / MIDI・MCP)
```

## 技術スタック

| レイヤー | 技術 |
|---------|------|
| CLI / Process | Rust (Tokio, Axum, Clap) |
| WebView | wry + tao |
| Frontend | SolidJS + xterm.js + creo-ui (vp-app webview) |
| Agent | Claude CLI + MCP |
| MIDI | midir |

> **Process**: プロジェクトの開発プロセスを表す本体。JoJo の Stand（能力）を保持し、ユーザーの開発を支援する。

### 依存境界（runtime dependency 方針、2026-07-04 確定）

**基準 = 「user のマシンで要求されるか」**。 brew cask を入れた user の環境で必要になるものだけが
product の依存。 maintainer の repo にある道具は project の道具（各 lane の project が何を使うかも自由）。

- **vp runtime は外部ツールに依存しない**: tmux / mise は不使用（tmux decoupling PR1-2 で撤去）。
  lane の依存は「user 自身の login shell」と「claude 本体」のみ。 DB も embedded surrealkv
  （外部 `surreal` バイナリ不要）。
- **許容 ≠ 依存**: `spawn_env` が mise shims を PATH に足すのは「user が mise で claude を
  管理していても見つかる」ための許容。 mise 不在でも全機能が動く。
- **repo の dev tooling**（`.mise/tasks` = release/build/daemon 系 + toolchain pin）は
  maintainer 専用で product に同梱されない — この層の mise 使用は「1 project の task runner」
  として許容側。

### システム構成

```
vp-app (GUI: wry+tao)   vp (CLI)
        └────────┬───────┘
                 │ HTTP + QUIC（listener は TheWorld のみ）
        TheWorld 👑 :32000          ← Process Manager (常駐 daemon)
                 │ spawn ↓ ／ SP→World QUIC registry 自己登録 ↑（reconcile = Push 一本）
     ┌───────────┼───────────┐
   SP[33000]   SP[33001]  ...      ← Star Platinum ⭐ (project ごと、portless = outbound-only)
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
vp ps                  # 稼働中 project 一覧（PROJECT / LANES 数 / STATUS active|idle）。詳細は vp lane list
vp config              # 設定と登録プロジェクト表示
vp projects            # 登録 project 管理（add/remove/rename/enable/disable/reorder/list）
vp sync                # projects.kdl を現実と同期（ghost project 除去）
vp mcp                 # MCPサーバーモード（stdio）
vp update [--check]    # セルフアップデート
vp restart-all         # TheWorld を再起動（= 全 project 再起動。fold-in 後は daemon restart と等価）
                       # ⚠️ 復元されるのは「enabled な project」で「再起動前に動いていた project」ではない。
                       #    停止を永続させたいなら vp projects disable（stop だけでは再起動で生き返る）。

# TheWorld（Daemon）/ Project
vp daemon start|stop|status  # TheWorld 管理（alias: vp world）
vp daemon restart [--if-running]  # ownership-agnostic 再起動（実 port holder を停止 → LaunchAgent 優先で起動。--if-running = 不在なら no-op、brew cask postflight 用）
vp daemon install|uninstall  # LaunchAgent 常駐化（macOS、login always-on + crash 自動再起動）
vp projects start|stop <name>  # 単一 project の起動/停止（doc 44 P1 fold-in で `vp sp` から移設）
# ⚠️⚠️ doc 44 P1 (fold-in) で daemon 停止の意味論が変わった:
#   project は World プロセス内の Arc<AppState> になったため、**daemon を止めると
#   全 project が必ず一緒に落ちる**（= lane claude も全部落ちる）。旧「gentle（daemon だけ
#   止めて SP は温存）」は SP が別プロセスだった時代の挙動で、fold-in 後は成立しない。
#   → lane の中から daemon を再起動すると自分が死ぬ。実機検証は VP の外（kitty 等）で行うこと。
#   会話は cc_session の `--resume` で次回 spawn 時に継がれる（「プロセスは死ぬがコンテキストは蘇る」）。

# App（GUI）
vp app start           # vp-app GUI 起動（spawn + 即 exit、 cwd を起点に開く）
vp app stop            # vp-app を停止
# 再起動は `vp app stop && vp app start` で合成 (restart は意図的に CLI に持たない)
vp shot                # vp-app window の screenshot を PNG 保存

# Lane / dev-flow / messaging
vp lane                # performer Lane 管理（Stone Free 🧵）
vp flow handoff|progress  # Conductor × Performer orchestration
vp wire send|recv|inbox|thread|ack|watch|hook-check  # wire messaging（store は TheWorld :32000 に中央化。hook-check は claude hook 実体、R2-c）
vp lane history [--limit N]  # 見送りの記録（いつ何を見送ったか / 判断待ちの滞留、doc 44 §7.5 の帳簿）
vp lane capture <lane> [--session N]  # lane console の現在画面を読む（旧 vp tmux capture の後継、tmux 非依存）
vp lane slots <lane>   # lane が持つ console slot 一覧（doc 46 P5 — slot は session ごと）
vp lane slot-new <lane> [--stand <engine>]  # console をもう 1 枚立てる（新 session を採番。root は動かない）
vp lane nudge <lane> <text> [--session N]  # lane の claude に text+Enter を注入（旧 vp tmux send-keys / directmsg の後継）
# ⚠️ `--session` 省略は **root**（lane の代表 slot）。chat 系 API の session 省略が focused なのと既定が違う

# その他
vp port                # deterministic port layout の計算・表示
vp db init|path|status # embedded SurrealDB 管理
vp auth me|login|logout  # Creo ID 認証
vp pane / vp file      # ペイン操作 / ファイル監視
vp midi monitor|ports  # MIDI（feature = "midi" ビルドのみ）
vp midi lpd8 write|switch|ports|demo  # demo = mk2 フル RGB pad 投影
vp midi xtouch demo|wave  # X-Touch (MCU) 実機 smoke / フェーダー wave
vp midi roto demo|anim|probe  # ROTO-CONTROL 実機 smoke / BPM 同期アニメ / handshake 観察
```

> ⚠️ `vp start` / `vp stop` / `vp open` / `vp tray` / `vp sp` は**存在しない**（旧体系。start/stop は `vp projects` / `vp daemon` / `vp app` に分散。`vp sp` は doc 44 P1 fold-in で退役 — project がプロセスでなくなったため）。UI は native vp-app（旧 localhost browser canvas は未使用のため撤去済）。
> ⚠️ `vp hd` / `vp tmux` / `vp directmsg` も**存在しない**（tmux decoupling PR1-2 で退役。console の read/write は `vp lane capture` / `vp lane nudge`）。**VP は tmux に依存しない**（lane = SP の PtySlot が claude を直接ホスト、design doc `docs/design/tmux-decoupling.md`）。

## 開発コマンド

```bash
cargo build --release -p vantage-point   # ビルド
cargo test --workspace                    # テスト
cargo install --path crates/vp-cli --locked  # インストール（codesign 自動付与。--locked 必須 — install は Cargo.lock を無視して最新依存を解決するため、未検証の新リリース（例: time 0.3.48 × ratatui-widgets の E0119）を踏む）
cargo fmt --all -- --check                # フォーマットチェック
cargo clippy --workspace --all-targets    # Lint

# dogfood: 普段使いの .app を作業ツリーの build で差し替えて触る（GUI 変更の実機確認の正）
mise run app:swap                          # DRY build → /Applications/VantagePoint.app 差し替え → 起動
VP_SWAP_RESTART_DAEMON=1 mise run app:swap # server (crates/vantage-point) も効かせる（lane が全部落ちる）

# webview bundle は生成物（commit しない、2026-07-19 転換 — docs/guide/webview.md が SSOT）
mise run app:bundle                        # bun install --frozen-lockfile + bun run build（swap / release は内部で自動実行）
```

> **`app:swap` を使う理由**: dev profile（`VP_PROFILE=dev`）は state を別 namespace に切るため daemon / SP / GUI を三点セットで立て直す要があり、素の `~/.cargo/bin/vp-app` は `.app` bundle でないので macOS の app として扱えない（screenshot 許可対象にすらならない）。`app:swap` は本番と同じ `.app` 形のまま notarize の待ち時間だけを落とす（quarantine xattr が付かない自前 build に notarization ticket は不要 — Developer ID 署名で足りる）。
> ⚠️ **GUI と server で反映タイミングが違う**: `.app` 差し替えで入れ替わるのは GUI（vp-app）だけ。daemon / SP は既に memory 上の旧 binary で走っているので、`crates/vantage-point` を触ったなら `VP_SWAP_RESTART_DAEMON=1` が要る（= SP の子である lane の claude が全部落ちる。会話は `cc_session` の `--resume` で復帰）。
> webview（tsx/ts）変更は swap が内部で `app:bundle` を回すので手動 `bun run build` / `touch main_area.rs` は不要（旧儀式は build.rs の rerun-if-changed で根治）。webview 依存は npm semver pin（`file:` sibling 依存と bundle commit は 2026-07-19 に廃止）。creoui / club-unison の同時開発は `bun link`（docs/guide/webview.md）。
> ⚠️ **swap 後は brew と現実が乖離する**: `app:swap` は brew cask 管理下の `.app` を dev build で上書きするが、Caskroom のメタデータは触らない。しかも swap した dev build は作業ツリーの version をそのまま名乗るため、**`brew upgrade --cask vantage-point` は version 一致で no-op になり dev build が居座り続ける**（Caskroom が持つのは実体コピーではなく `/Applications` への symlink なので brew は中身の差分を検知できない）。公式 release に戻すのは **`brew reinstall --cask vantage-point`**（`upgrade` では戻らない）。今どちらが入っているかは `spctl -a -t exec -vvv /Applications/VantagePoint.app` で判別できる（`Notarized Developer ID` = 公式 release / `Developer ID` = swap 済の dev build）。

## 設定・ポート

- config / data / state パスは **XDG Base Directory 準拠の 3 zone に統一**（VP-189 / #460、全 OS 共通、ディレクトリ名は `vp`）。定義は **`crates/vp-paths`**（vantage-point + vp-app 共有の SSOT。`vantage_point::config` は `pub use vp_paths::{...}` で re-export、vp-app は直接依存）。

  | zone | env | default | 用途 |
  |------|-----|---------|------|
  | **config** (`vp_config_dir()`) | `$XDG_CONFIG_HOME` | `~/.config/vp/` | 人が編集（`config.kdl` / `projects.kdl` / `addresses.toml`） |
  | **data** (`vp_data_dir()`) | `$XDG_DATA_HOME` | `~/.local/share/vp/` | 永続 data store（db / discs） |
  | **state** (`vp_state_dir()`) | `$XDG_STATE_HOME` | `~/.local/state/vp/` | runtime state + log（`session.json` / `sessions/` / `log/`） |

  - 設定ファイルは **KDL**（`vp_config_dir()/config.kdl`、人が編集する read-only global 設定）。登録プロジェクトの SSOT は `projects.kdl`（VP-188、config.kdl には出さない）。
  - federation opt-in は config.kdl の `hub-addr "hub.chronista.club:12879"`（常設 SSOT — launchd daemon は shell env を持たない）。env `CHRONISTA_HUB_ADDR` は dev override として優先される。未設定 = federation off（machine-local）。状態確認は `vp daemon status` の `Hub:` 行 or `/api/health` の `hub` field。
  - 起動時に旧パス（Application Support / Library/Logs / `dirs::config_dir()/vantage/` 等）から新 XDG zone へ冪等にデータ移行（`migrate_legacy_paths()`、旧データは残す）。
- ポート割り当て:
  - TheWorld: 32000 (HTTP + QUIC) — **唯一の listener**
  - Project (SP): 33000〜（`PORT_RANGE` 33000-33024 の deterministic slot）— **portless**。SP は listen せず、この番号は registry 上の論理 identity（停止/特定に使う）
  - SP → World の QUIC は **outbound のみ**（registry / canvas-ingest / control の自己登録接続）。SP 自身は per-process な QUIC listener を持たない
- `vp ps` は TheWorld registry（:32000）に問い合わせて一覧化（ポートスキャンは廃止）

### VP_PROFILE — dev / brew の state 分離（#643）

dev binary（`~/.cargo/bin/vp`、`cargo install` 由来）と release（brew cask / `.app`、`/opt/homebrew/bin/vp`）を混在させると **state を全共有して衝突**する（sp_LOCK 奪い合い / port 衝突）。`VP_PROFILE` 環境変数で state を完全 namespace 分離してこれを構造的に防ぐ。SSOT は `vp-paths`（`vp_profile()` / `app_dir_name()` / `default_world_port()`）。

| レバー | 未設定 = **brew**（一般ユーザ・従来通り） | `VP_PROFILE=dev`（開発者） |
|---|---|---|
| config/data/state/db dir | `vp`（`~/.local/share/vp/` 等） | `vp-dev`（`~/.local/share/vp-dev/` 等） |
| world port | 32000 | 32100 |
| daemon pidfile | `$TMPDIR/vp/` | `$TMPDIR/vp-dev/` |

> （旧レバー「tmux socket `-L vp` / `-L vp-dev`」は tmux decoupling PR2 で退役 — lane は SP の PtySlot 直ホストで tmux server を持たない）

- env は継承で伝播する（dev shell → daemon → SP → lane claude）ので **`export VP_PROFILE=dev` 一発**で以降の全 vp が dev namespace になる。`vp switch` command / 起動時 guard / LaunchAgent 処理は不要。brew は LaunchAgent 起動で env を持たないため自然に brew namespace。
- **dev 起動は専用 alias（`.zprofile`）**: `alias vpd='VP_PROFILE=dev ~/.cargo/bin/vp'`。素の `vp`（release）と混ざらないよう cargo dev binary を明示指定する。
  ```zsh
  vpd daemon start   # → TheWorld :32100 / ~/.local/share/vp-dev
  vpd daemon status  # → Port: 32100 で確認 / vpd db path → .../vp-dev/db/...
  vpd app start      # dev GUI（要 `cargo install --path crates/vp-app` で dev vp-app）
  ```
- release（brew）は素の `vp`（= `.app` 同梱 CLI への symlink）/ GUI は `VantagePoint.app`。dev(32100) と release(32000) は完全並列で常駐でき、互いに衝突しない。
- ⚠️ `VP_PROFILE` を honor するのは **#643 を含む binary のみ**。未対応 binary に `VP_PROFILE=dev` を渡しても無視され brew namespace(32000) に落ちる（混在再発）ので、dev alias は feature 込みで `cargo install` した `~/.cargo/bin/vp` を明示指定する。

### プロセス管理（Reconciliation）

TheWorld が **QUIC registry（Push）** でプロセスを管理する。SP-portless 化に伴い旧 Pull（ポートスキャン）は撤去され、registry が**単一の真実源**になった（portless SP は listen しないためポートスキャンでは発見できない）。

| パス | 仕組み | 用途 |
|------|--------|------|
| **Push (QUIC Registry)** | SP が TheWorld に QUIC 永続接続で自己登録（outbound）。heartbeat 15s + 再接続時の snapshot replace で reconcile。切断 = 即時除去 | リアルタイム検出 + 自律復帰 |

- `running_processes` / `projects` の HashMap キーは正規化パス（`normalize_path_key()`）。`project_name` は表示用ラベル
- `/api/health` レスポンスに `stands` フィールドを含む（各 Stand の状態をリアルタイムで返す）

## Agent モジュール

Claude CLI統合の実装（`crates/vantage-point/src/agent.rs`）。2つの実行モードを提供:

| モード | CLI形式 | 用途 |
|--------|---------|------|
| **OneShot**（`ClaudeAgent`） | `claude -p "prompt"` | 単発プロンプト |
| **Interactive**（`InteractiveClaudeAgent`、デフォルト） | `claude -p --input-format stream-json` | 持続プロセス、複数ターン |

> 対話モードの claude（TUI）は Agent モジュールではなく、 **lane の PtySlot 直ホスト**（`stand_spawner::build_stand_command` が Act I slot（login shell）に `claude --resume … || claude` を type-ahead 注入）が担う（tmux decoupling PR2、design doc `docs/design/tmux-decoupling.md` §13）。

### Stream-JSON 入力フォーマット

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"メッセージ"}]}}
```

## コーディング規約

- **コメントは日本語で記述する**
- data / calculations / actions を明確に分離

## デバッグ / ログ

> ⚠️ 旧「デバッグモード」（`-d simple|detail` + `send_debug` + WebUI 右パネル）は
> doc 44 P1 fold-in で撤去（§5.4）。生産側（`send_debug`）は fold-in で常に無効化され、
> 消費側（WebUI デバッグパネル）は旧 localhost browser UI ごと撤去済で、end-to-end で
> dead だった。現在のログ手段は下記の 2 本。

### tracing ログ詳細度（`VANTAGE_DEBUG`）

| 値 | tracing レベル | 用途 |
|----|---------------|------|
| （未設定 / `none`） | warn 相当（default EnvFilter） | 本番運用 |
| `simple` | `vantage_point=info` | 基本ログ |
| `detail` | `vantage_point=debug` | 詳細ログ |

`VP_LOG=debug|info|warn|error` が設定されていれば `VANTAGE_DEBUG` より優先。
（`cli::parse_debug_env` → `init_tracing`）

### 問題調査フロー

1. `VANTAGE_DEBUG=detail` で daemon を起動（or `VP_LOG=debug`）
2. daemon ログ（`~/.local/state/vp/log/daemon.kdl.log`、dev は `vp-dev`）を tail
3. lane console は `vp lane capture <lane>` で読む
4. 必要に応じて `tracing::debug!` を追加 → 再ビルド

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

## GitNexus index 更新コマンド（正）

> ⚠️ index 更新は **`bunx gitnexus analyze`** を使う（この repo / mako 環境の JS runtime は bun。 node・npm・npx は使わない）。
> 下の `<!-- gitnexus:start -->` ブロックは `gitnexus analyze` が**毎回再生成**するため `node .gitnexus/run.cjs analyze` 表記に戻るが、 それは tool 自動生成なので無視してよい。 **正はこの行（`bunx gitnexus analyze`）**。 関連: memory `js-runtime-bun`。
>
> **symbol 数などの volatile な統計は `.gitnexusrc`（`analyze.stats: false`）で md から外してある**（gitnexus #1706 の正規機能。 コマンドに `--no-stats` を足す必要は無い）。 数字は index には正しく入るので MCP tools の精度は落ちない。 これが無いと analyze のたびに AGENTS.md / CLAUDE.md が書き換わり、 branch 間で無価値な diff / conflict を生む。 `.gitnexusrc` は **fail closed**（未知キー・型違いは即エラー）なので typo が silent no-op にならない。

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **vantage-point**. Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

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
