# Git Worktree 管理ガイド

## 概要

VP プロジェクトでは、複数の Issue を並行開発するために Git worktree を活用する。
ブランチ切り替え不要で、物理的に異なるディレクトリで複数の作業を同時進行できる。

> 現在は `cw` (Claude Worker) ツールが worktree 管理を自動化している。
> 手動 worktree は `cw` が使えない場合のフォールバック。

## cw によるワーカー管理（推奨）

```bash
# Issue ごとの隔離環境を作成
cw new <name> <branch>

# ワーカー一覧
cw ls

# マージ済みワーカーの削除
cw cleanup
```

ワーカーは `~/.cache/cw/workers/` に配置される。

## 手動 Worktree 管理

### ディレクトリ構造

```
~/repos/vantage-point/           # メインリポジトリ
└── .worktrees/                  # worktree 格納（gitignore 対象）
    ├── issue-98-tui-header/     # Issue #98 の作業
    └── issue-101-session-fix/   # Issue #101 の作業
```

### 基本コマンド

```bash
# worktree 作成
git worktree add .worktrees/issue-98-tui-header -b makoto/issue-98-tui-header

# 一覧表示
git worktree list

# 削除
git worktree remove .worktrees/issue-98-tui-header
```

### 命名規則

| 要素 | フォーマット | 例 |
|------|------------|-----|
| ディレクトリ | `issue-{番号}-{slug}` | `issue-98-tui-header` |
| ブランチ | `makoto/issue-{番号}-{slug}` | `makoto/issue-98-tui-header` |
| slug | 英単語 2〜4 個、ハイフン区切り | `tui-header`, `session-fix` |

## ワークフロー

### 1. 新しい Issue の作業開始

```bash
cd ~/repos/vantage-point

# worktree 作成
git worktree add .worktrees/issue-98-tui-header -b makoto/issue-98-tui-header

# 移動して開発開始
cd .worktrees/issue-98-tui-header
```

### 2. 作業完了後のクリーンアップ

```bash
# PR マージ後
cd ~/repos/vantage-point
git worktree remove .worktrees/issue-98-tui-header

# リモートブランチも削除
git push origin --delete makoto/issue-98-tui-header
```

## ベストプラクティス

1. **1 worktree = 1 Issue** — スコープを明確に
2. **定期的なクリーンアップ** — マージ済み worktree は即削除
3. **main の最新を取り込む** — `git fetch origin && git rebase origin/main`

## トラブルシューティング

```bash
# 強制削除
git worktree remove --force .worktrees/<issue-id>

# それでも削除できない場合
rm -rf .worktrees/<issue-id>
git worktree prune
```
