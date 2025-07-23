# Working Directory

このディレクトリは一時ファイルや実験用のワークスペースです。

## Git Worktreeの使用方法

### miseコマンドを使う方法（推奨）

```bash
# worktreeの一覧を表示
mise wt list

# 新しいworktreeを作成
mise wt add unison-core feature/node-comm

# worktreeに切り替え（cdコマンドを出力）
mise wt cd unison-core

# 実際にディレクトリを切り替える
eval $(mise wt cd unison-core)

# worktreeを削除
mise wt remove unison-core
```

### ショートカット関数（.bashrc/.zshrcに追加）

```bash
# Worktree quick switch function
wtcd() {
    if [ -z "$1" ]; then
        echo "Usage: wtcd <worktree-name>"
        echo "Available worktrees:"
        ls -1 "${VANTAGE_ROOT:-$(git rev-parse --show-toplevel)}/working" 2>/dev/null | \
            grep -v -E '(README.md|\.gitkeep|wt\.lua)' | sed 's/^/  - /'
        return 1
    fi
    eval $(mise wt cd "$1")
}

# Worktree create and switch
wtadd() {
    local name="${1:-}"
    local branch="${2:-$name}"
    if [ -z "$name" ]; then
        echo "Usage: wtadd <name> [branch]"
        return 1
    fi
    mise wt add "$name" "$branch" && eval $(mise wt cd "$name")
}
```

### 直接git worktreeを使う方法

```bash
# 新しいworktreeを作成
git worktree add working/feature-xyz feature-xyz

# または新しいブランチを作成して同時にworktreeも作成
git worktree add -b new-feature working/new-feature

# worktreeの一覧を確認
git worktree list

# 不要になったworktreeを削除
git worktree remove working/feature-xyz
```

## 注意事項

- このディレクトリ内のファイルは`.gitignore`によってgitから除外されています
- worktreeを作成する場合も、`working/`以下に作成することで自動的に無視されます
- 重要なファイルは適切なディレクトリに移動してください