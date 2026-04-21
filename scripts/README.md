# scripts/

運用補助スクリプト集。

## Daily Journal (L1 Steward dogfood)

VP-77 Lane-as-Process 規約 §6.1 で定義した **Daily Journal Meta Lane** の手動運用版。
Lead Autonomy L1 Steward (daily summary 自動化) を Rust 実装する前段。

### 朝開始

```bash
scripts/daily-journal-open.sh              # 今日
scripts/daily-journal-open.sh 2026-04-22   # 指定日
```

- `docs/journals/YYYY-MM-DD.md` を template から生成
- 現在 branch、開始時刻、open PRs を header に埋める
- 既存ファイルがあれば上書きせず exit

### 日暮れに閉じる

```bash
scripts/daily-journal-close.sh              # 今日
scripts/daily-journal-close.sh 2026-04-22   # 指定日
```

- footer (commits 数 / branches / open PRs state) を追記
- CC session で `remember` を叩いて creo-memories に永続化するのが次ステップ

### 将来の Rust 化

VP-80 (Daily Journal Meta Lane 実装) で本 shell を StandActor 化する。
それまでは shell で運用 + feedback loop を形成。

## 依存

- `bash` 4+
- `git`
- `gh` (GitHub CLI、PR list 用)
- `jq`

## 関連

- 運用 feedback: `~/.claude/projects/-Users-makoto-repos-vantage-point/memory/feedback_dev_journal.md`
- spec: `docs/design/07-lane-as-process.md` §6.1
- Linear: [VP-80](https://linear.app/chronista/issue/VP-80)
