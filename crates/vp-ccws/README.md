# ccws

Claude Code Workspace — Git clone ベースのワークスペースマネージャー。リポジトリの独立コピーを作り、並列で作業できるようにする。

## インストール

```bash
cargo install --git https://github.com/chronista-club/ccws
```

---

## ccws new すると何が起こるか

`ccws new issue-42 fix/login-bug` を実行すると、以下の流れでワーカー環境が作られる。

```mermaid
flowchart LR
    A["ccws new issue-42\nfix/login-bug"] --> B["リポジトリを\nshallow clone"]
    B --> C["remote を\nGitHub URL に変更"]
    C --> D[".claude/worker-files.kdl\nに従いファイル配置"]
    D --> E["ブランチ作成\nfix/login-bug"]
    E --> F["post-setup 実行"]
```

1. カレントリポジトリを `~/.local/share/ccws/<リポ名>-issue-42` に `--depth 1` で clone する
2. clone 先の remote を GitHub の URL に差し替える（push 先を正しく設定）
3. `.claude/worker-files.kdl` を読み、指定されたファイルを symlink / copy する
4. `fix/login-bug` ブランチを作成する
5. `post-setup` が定義されていれば実行する（`bun install` など）

ワーカー名にはリポジトリ名が自動で prefix される（`issue-42` → `vantage-point-issue-42`）。

---

## 設定ファイル

プロジェクトの `.claude/worker-files.kdl` にワーカー環境へ配置するファイルを定義する。

```kdl
// symlink: 元リポジトリと共有（.env 等の変更が即反映）
symlink ".env"
symlink ".mcp.json"
symlink ".claude/settings.local.json"

// symlink-pattern: パターンで一括 symlink
symlink-pattern "**/*.local.*"
symlink-pattern "**/*.local"

// copy: 独立コピー（ワーカー側で自由に変更可能）
copy "config/dev.toml"

// post-setup: clone 後に実行するコマンド
post-setup "bun install"
```

| 種類 | 動作 | 用途 |
|------|------|------|
| `symlink` | 元ファイルへのシンボリックリンク | `.env`, `.mcp.json` など共有したいファイル |
| `copy` | 独立コピー | ワーカー側で変更が必要なファイル |
| `symlink-pattern` | glob パターンで一括 symlink | `*.local.*` など gitignore 対象のローカルファイル |
| `post-setup` | clone 後に実行するコマンド | `bun install`, `cargo build` など |

---

## コマンド

```bash
ccws new <name> <branch>   # ワーカー環境を作成
ccws fork <name> <branch>  # 未コミット変更ごとワーカーにフォーク
ccws ls                     # 一覧表示（名前・ブランチ・パス）
ccws status                 # 全ワーカーの状態表示（変更数・ahead/behind・最新コミット）
ccws path <name>            # ワーカーのパスを出力
ccws rm <name>              # 削除
ccws rm --all --force       # 全ワーカーを削除
ccws cleanup                # マージ済みワーカーを表示（dry-run）
ccws cleanup --force        # マージ済みワーカーを削除
```

### ccws fork — 作業途中をフォーク

`ccws fork experiment feature/alt-approach` を実行すると:

1. 現在の**未コミット変更**（staged + unstaged + untracked files）を diff としてキャプチャ
2. `ccws new` と同じ手順でワーカー環境を作成
3. キャプチャした diff をワーカーに適用

元のワーキングツリーは**一切変更されない**。作業を中断せずに dirty state をフォークできる。

`ccws path` は stdout にパスだけを出力するので、他のコマンドと組み合わせられる:

```bash
cd $(ccws path issue-42)
```

ワーカーのデフォルト保存先は `~/.local/share/ccws/`（XDG準拠）。`CCWS_WORKERS_DIR` 環境変数で変更できる。

## ライセンス

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
