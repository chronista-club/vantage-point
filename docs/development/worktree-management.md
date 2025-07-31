# Git Worktree管理ガイド

## 概要

Vantageプロジェクトでは、複数のIssueを並行して開発するためにGit worktreeを活用しています。これにより、ブランチの切り替えなしに、物理的に異なるディレクトリで複数の作業を同時に進められます。

## ディレクトリ構造

```
$HOME/Workspaces/VANTAGE/
└── worktrees/           # すべてのworktreeを格納（gitignore対象）
    ├── UNI-118-workdir-support/   # Issue UNI-118の作業
    ├── UNI-119-claude-platform/   # Issue UNI-119の作業
    └── issue-456-bug-fix/         # GitHub Issue #456の修正
```

## 基本コマンド

### worktreeの作成

```bash
# 基本形式
git worktree add worktrees/<issue-id>-<slug> -b <username>/<branch-name>

# 例：Linear Issue用
git worktree add worktrees/UNI-120-file-browser -b mito/uni-120-file-browser

# 例：GitHub Issue用
git worktree add worktrees/issue-123-auth-fix -b mito/issue-123-auth-fix
```

### worktreeの一覧表示

```bash
git worktree list
```

出力例：
```
/Users/mito/Documents/GitHub/vantage              d0275fa [edge]
/Users/mito/Workspaces/VANTAGE/worktrees/UNI-118  1234567 [mito/uni-118-workdir-support]
/Users/mito/Workspaces/VANTAGE/worktrees/UNI-119  abcdefg [mito/uni-119-claude-platform]
```

### worktreeの削除

```bash
# worktreeディレクトリに移動してから削除
cd /Users/mito/Workspaces/VANTAGE
git worktree remove worktrees/<issue-id>-<slug>
```

## 命名規則

### worktreeディレクトリ名
- **フォーマット**: `<Issue番号>-<説明slug>`
- **Issue番号**: 
  - Linear: `UNI-XXX`
  - GitHub: `issue-XXX`
- **説明slug**: 
  - 英単語2〜4個
  - ハイフン区切り
  - 全て小文字

### 例
- `UNI-118-workdir-support` - 作業ディレクトリサポート
- `UNI-119-claude-platform` - Claude連携プラットフォーム
- `issue-123-auth-fix` - 認証バグ修正

## ワークフロー

### 1. 新しいIssueの作業開始

```bash
# 1. Linearで新しいIssueを確認（例：UNI-121）
# 2. worktreeを作成
cd $HOME/Documents/GitHub/vantage
git worktree add $HOME/Workspaces/VANTAGE/worktrees/UNI-121-new-feature -b mito/uni-121-new-feature

# 3. worktreeに移動
cd $HOME/Workspaces/VANTAGE/worktrees/UNI-121-new-feature

# 4. 開発開始
```

### 2. 複数Issueの並行作業

各worktreeは独立しているため、自由に切り替えて作業できます：

```bash
# Issue UNI-118の作業
cd ~/Workspaces/VANTAGE/worktrees/UNI-118-workdir-support
# ... 作業 ...

# Issue UNI-119に切り替え
cd ~/Workspaces/VANTAGE/worktrees/UNI-119-claude-platform
# ... 作業 ...
```

### 3. 作業完了後のクリーンアップ

```bash
# 1. PRがマージされたら
# 2. worktreeを削除
cd ~/Workspaces/VANTAGE
git worktree remove worktrees/UNI-121-new-feature

# 3. リモートブランチも削除（オプション）
git push origin --delete mito/uni-121-new-feature
```

## ベストプラクティス

### 1. 一つのworktree = 一つのIssue
各worktreeは特定のIssueに対応させ、スコープを明確に保ちます。

### 2. 定期的なクリーンアップ
完了したIssueのworktreeは削除し、ディスク容量を節約します。

### 3. ブランチ名の一貫性
worktreeディレクトリ名とブランチ名を対応させることで、管理を簡単にします。

### 4. メインリポジトリの更新
定期的にメインリポジトリで`git fetch`を実行し、各worktreeで最新の変更を取り込みます：

```bash
# メインリポジトリで
cd ~/Documents/GitHub/vantage
git fetch origin

# 各worktreeで
cd ~/Workspaces/VANTAGE/worktrees/UNI-XXX
git rebase origin/edge  # または merge
```

## トラブルシューティング

### worktreeが削除できない場合

```bash
# 強制削除
git worktree remove --force worktrees/<issue-id>

# それでも削除できない場合
rm -rf worktrees/<issue-id>
git worktree prune
```

### ブランチの不整合

```bash
# worktree一覧を更新
git worktree prune

# 詳細情報の確認
git worktree list --verbose
```

## 関連リンク

- [Git Worktree公式ドキュメント](https://git-scm.com/docs/git-worktree)
- [Linear Issue管理ガイド](../../.claude/LINEAR.md)
- [開発環境セットアップ](./setup.md)