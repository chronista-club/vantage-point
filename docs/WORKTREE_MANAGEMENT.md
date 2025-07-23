# Worktree管理システム

時として、開発者は複数のブランチを同時に扱う必要に迫られる。一つのブランチで新機能を実装しながら、別のブランチで緊急のバグ修正を行う。まるで複数の作業空間を行き来する職人のように。

VANTAGEプロジェクトでは、この並行開発の管理にGit worktreeという仕組みを採用している。これにより、同一リポジトリの異なるブランチを、物理的に異なるディレクトリで同時に操作できる。

## 概念と設計

worktreeとは、同一のGitリポジトリから派生した、独立した作業ディレクトリである。本を読むとき、複数の栞を挟んで異なるページを同時に開いておくように、worktreeは複数の開発コンテキストを物理的に分離して保持する。

VANTAGEにおいて、すべてのworktreeは`working/`ディレクトリ配下に作成される。このディレクトリは`.gitignore`によってGit管理から除外されており、各worktree内の変更が親リポジトリに影響を与えることはない。

## ディレクトリ構造

プロジェクトの構造は精緻に設計されている。各ファイルが明確な役割を持ち、全体として一貫性のあるシステムを構成している。

```
VANTAGE/
├── .mise.toml                    # worktree管理コマンドの定義（Lua実装）
├── working/                      # worktree格納ディレクトリ（.gitignoreで除外）
│   ├── README.md                 # 使用方法の説明
│   ├── .gitkeep                  # ディレクトリをGitに保持するためのファイル
│   └── wt.lua                    # ディレクトリ移動用のヘルパースクリプト
└── docs/
    └── WORKTREE_MANAGEMENT.md    # このドキュメント
```

## コマンドリファレンス

以下に、worktree管理システムで使用可能なコマンドを記す。各コマンドは`mise`タスクランナーを通じて実行され、内部的にはLuaスクリプトとして実装されている。

### `mise wt list` - worktree一覧の表示

現在存在するすべてのworktreeを一覧表示する。このコマンドは内部的に`git worktree list`を実行し、その結果を整形して表示する。

```bash
$ mise wt list
📁 Available worktrees:
  /Users/username/VANTAGE (edge)
  /Users/username/VANTAGE/working/unison-core (feature/node-comm)
```

出力形式:
- 各行がworktreeのパスとチェックアウトされているブランチ名を表示
- メインのワークツリー（プロジェクトルート）も含まれる

### `mise wt add <name> [branch]` - 新しいworktreeの作成

新たな作業環境を作成するコマンド。このコマンドにより、開発者は既存のリポジトリから独立した作業ディレクトリを構築できる。

**パラメータ:**
- `name`: worktreeのディレクトリ名。`working/<name>`として作成される
- `branch`: チェックアウトするブランチ名。省略時は`name`と同じブランチ名を使用

**使用例:**
```bash
# 新しいブランチと共にworktreeを作成
$ mise wt add unison-core feature/node-communication
🌿 Creating new branch: feature/node-communication
✅ Worktree created at: /Users/username/VANTAGE/working/unison-core

# 既存のブランチでworktreeを作成
$ mise wt add bugfix-123 bugfix/issue-123
🔗 Using existing branch: bugfix/issue-123
✅ Worktree created at: /Users/username/VANTAGE/working/bugfix-123
```

内部的には`git worktree add`コマンドを実行し、ブランチの存在確認とエラーハンドリングを行う。各worktreeは独立したGitリポジトリとして機能し、他のworktreeに影響を与えることなく作業を進められる。

### `mise wt cd <name>` - worktreeへの移動

指定したworktreeディレクトリへ移動するコマンド。このコマンドは二段階の実行が必要となる。まずコマンドがcdコマンドを出力し、次に`eval`を使用して実際のディレクトリ移動を実行する。

**実行手順:**
```bash
# 第一段階：cdコマンドの出力を確認
$ mise wt cd unison-core
📂 Switching to worktree: unison-core
cd '/Users/username/VANTAGE/working/unison-core'

# 第二段階：evalを使用して実際に移動
$ eval $(mise wt cd unison-core)
```

この仕組みは、Luaスクリプトが現在のシェルセッション内で直接ディレクトリを変更できないため必要となる。`eval`コマンドによって、出力されたcdコマンドが現在のシェルで実行される。

### `mise wt remove <name>` - worktreeの削除

不要になったworktreeを削除するコマンド。作業が完了し、変更がメインブランチにマージされた後に使用する。

**削除の実行:**
```bash
$ mise wt remove unison-core
🗑️  Removing worktree: unison-core
✅ Worktree removed successfully
```

内部的には`git worktree remove`を実行する。削除前に未コミットの変更がないか確認し、必要に応じて警告を表示する。worktreeで行われた作業は、適切にコミット・プッシュされていればリポジトリの履歴として保持される。

## 効率的な使い方

### シェル関数の設定

作業効率を向上させるため、シェルの設定ファイル（`.bashrc`や`.zshrc`）に便利な関数を定義できる。これらの関数により、より素早くworktreeを操作できるようになる。

```bash
# worktreeへの高速移動
wtcd() {
    if [ -z "$1" ]; then
        echo "使い方: wtcd <worktree名>"
        echo "利用可能なworktree:"
        ls -1 "${VANTAGE_ROOT:-$(git rev-parse --show-toplevel)}/working" 2>/dev/null | \
            grep -v -E '(README.md|\.gitkeep|wt\.lua)' | sed 's/^/  - /'
        return 1
    fi
    eval $(mise wt cd "$1")
}

# worktree作成と移動を同時実行
wtadd() {
    local name="${1:-}"
    local branch="${2:-$name}"
    if [ -z "$name" ]; then
        echo "使い方: wtadd <名前> [ブランチ]"
        return 1
    fi
    mise wt add "$name" "$branch" && eval $(mise wt cd "$name")
}

# worktree一覧表示
wtls() {
    mise wt list
}

# worktree削除
wtrm() {
    mise wt remove "$1"
}
```

これらの関数を使用することで、worktreeの操作が格段に速くなる：

```bash
# 新しいworktreeを作成して即座に移動
$ wtadd unison-core feature/new-horizon

# 既存のworktreeへ素早く移動
$ wtcd unison-core

# すべてのworktreeを一覧表示
$ wtls
```

## 実装の詳細

### Luaスクリプトの構造

worktree管理システムは、`.mise.toml`に定義されたLuaスクリプトとして実装されている。以下の技術要素で構成される：

- **実行環境**: miseのLuaランタイム
- **主要な機能**:
  - `io.popen` - Gitコマンドの実行と結果の取得
  - ディレクトリの存在確認 - worktreeの有効性検証
  - ブランチの存在確認 - 指定されたブランチの妥当性チェック
  - エラーハンドリング - 各種エラー状況への適切な対応

### ディレクトリ構造の設計原則

worktreeの管理には、以下の設計原則が適用される：

- すべてのworktreeは`working/`ディレクトリ配下に配置
- `working/`ディレクトリは`.gitignore`によりGit管理から除外
- 各worktreeは独立したGitリポジトリとして機能し、他のworktreeと干渉しない

## ベストプラクティス

### 1. 命名規則

worktreeには、その目的が明確にわかる名前を付けることが重要である。適切な命名により、複数のworktreeを効率的に管理できる：

- 推奨される命名: `unison-core`（コア機能）、`bugfix-auth`（認証バグ修正）、`feature-ui`（UI機能追加）
- 避けるべき命名: `test`、`tmp`、`work`（目的が不明確）

### 2. ブランチ戦略

各worktreeは特定のブランチと紐付けられる。一貫したブランチ命名により、プロジェクト全体の見通しが良くなる：

- 機能開発: `feature/<機能名>`
- バグ修正: `bugfix/<issue番号>`
- 実験的作業: `experiment/<実験内容>`

### 3. worktreeの整理

作業が完了したworktreeは、適時削除することで作業環境をクリーンに保てる。不要なworktreeの放置は、ディスク容量の無駄遣いや混乱の元となる：

```bash
# 作業完了後のworktree削除
$ mise wt remove feature-xyz
```

### 4. 並行開発の活用

worktreeの真価は、複数の開発タスクを並行して進められることにある。異なる端末やIDEウィンドウで別々のworktreeを開くことで、コンテキストスイッチのコストを削減できる：

```bash
# 端末1：unison-coreで通信機能を開発
$ wtcd unison-core
$ cargo test

# 端末2：unison-claudeでAI統合を実装
$ wtcd unison-claude
$ cargo build
```

この手法により、ビルド待ち時間を有効活用し、開発効率を大幅に向上させられる。

## トラブルシューティング

worktree操作中に発生する可能性のある問題と、その解決方法を以下に示す。

### worktree作成エラー

**エラーメッセージ**: `fatal: '<branch>' is already checked out at '<path>'`

このエラーは、指定したブランチが既に別のworktreeでチェックアウトされていることを示す。Gitでは、同一ブランチを複数のworktreeで同時にチェックアウトすることはできない。

**解決方法**: 
- 別のブランチ名を指定する
- 既存のworktreeを削除してから再作成する
- `git worktree list`で現在のworktree状態を確認する

### worktreeが見つからない

**エラーメッセージ**: `Worktree '<name>' not found`

指定した名前のworktreeが存在しない場合に発生する。

**確認手順**:
```bash
# 現在のworktree一覧を確認
$ mise wt list

# workingディレクトリの内容を直接確認
$ ls -la working/
```

worktree名のタイプミスや、既に削除済みのworktreeを指定していないか確認する。

### 未コミットの変更がある場合

**エラーメッセージ**: `fatal: '<path>' contains modified or untracked files, use --force to delete anyway`

worktreeに未コミットの変更やトラックされていないファイルが存在する場合に発生する。

**対処方法**:
1. 変更を確認してコミットする
   ```bash
   $ cd working/<name>
   $ git status
   $ git add .
   $ git commit -m "作業内容をコミット"
   ```

2. 変更を破棄する場合（注意：データが失われる）
   ```bash
   $ git worktree remove --force working/<name>
   ```

強制削除を行う前に、必ず変更内容を確認し、本当に破棄してよいか慎重に判断する。

## さらなる学びのために

- [Git Worktreeの原典](https://git-scm.com/docs/git-worktree) - より深い理解を求める者のために
- [miseの魔導書](https://mise.jdx.dev/) - 呪文システムの全貌
- `working/README.md` - 日常使いのための簡潔な手引き

---

*このドキュメントは、VANTAGEプロジェクトにおけるGit worktreeを活用した並行開発の手法を記したものである。複数の作業環境を効率的に管理することで、開発者はより生産的で、より創造的な開発を実現できる。技術的な正確性を保ちながら、開発という営みの本質的な価値を追求する — それがこのシステムの目指すところである。*