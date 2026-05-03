# `vp:stand:*` ─ Lane Stand task 群

VP の Lane spawn 機構が呼び出す mise file-based task。 各 Stand を **1 ファイル 1 task** で定義。

設計の根拠は [`docs/design/11-stand-init-script-system.md`](../../../../docs/design/11-stand-init-script-system.md) を参照。

## 命名規則

| Path | Task 名 | Command |
|------|---------|---------|
| `.mise/tasks/vp/stand/hd` | `vp:stand:hd` | `mise run vp:stand:hd` |
| `.mise/tasks/vp/stand/shell` | `vp:stand:shell` | `mise run vp:stand:shell` |
| `.mise/tasks/vp/stand/tmux` | `vp:stand:tmux` | `mise run vp:stand:tmux` |

mise はディレクトリ名を `:` separator として task 名を組み立てる。 ファイル名 (拡張子なし、 拡張子 `.rb` `.py` 等は shebang から interpreter 判定) が Stand 名。

## VP env 規約

VP (caller) が以下の環境変数を設定して mise task を起動:

| ENV | 意味 | 例 |
|-----|------|-----|
| `VP_CWD` | Lane の working directory (project_dir) | `/Users/makoto/repos/vantage-point` |
| `VP_SESSION` | tmux session 名 (sanitize 済) | `vp-vantage-point-lead-hd` |
| `VP_PROJECT` | project 識別子 | `vantage-point` |
| `VP_LANE` | lane label (lead / worker name) | `lead` / `sub` |

quoting 規約: task 内では `"$VP_CWD"` のように **必ず double-quote**で囲む。

## metadata convention

各ファイルの先頭コメントに以下を含める:

- `#MISE description="..."` ─ mise が parse、 `mise tasks ls --json` の description field に出る
- `#VP icon="📖"` ─ VP が parse、 sidebar 表示に使う (1 文字 emoji)
- `#VP tier=N` ─ VP が parse、 PTY tier 区分 (0=shell / 1=tmux / 2=hd)

> **用語注**: `tier` は **PTY 階層** を表す (LSCM Layer = World/Project/Lane の container とは別概念、 [doc 12 §0 Glossary](../../../../docs/design/12-stand-architecture.md) 参照)。 旧 `#VP layer=N` は VP-110 で `#VP tier=N` に rename された。

## Tier 区分 (JoJo 演目 metaphor)

| Tier | task | 動作 | 比喩 |
|------|------|------|------|
| 0 | `vp:stand:shell` | bare login shell | 舞台の床 |
| 1 | `vp:stand:tmux` | tmux session attached、 LLM なし | 副舞台を仕込む |
| 2 | `vp:stand:hd` | tmux + Claude auto-launch | 役者を呼ぶ |

## Standalone test

各 task は VP を経由せずに単独実行できる:

```sh
# Tier 0: 単に shell を起動 (現在の shell が exec で置き換わる)
VP_CWD=/tmp VP_SESSION=test VP_PROJECT=test VP_LANE=lead \
  mise run vp:stand:shell

# Tier 1: tmux session に attach
VP_CWD=/tmp VP_SESSION=vp-test-lead-tx VP_PROJECT=test VP_LANE=lead \
  mise run vp:stand:tmux

# Tier 2: tmux + claude auto-launch
VP_CWD=$(pwd) VP_SESSION=vp-test-lead-hd VP_PROJECT=test VP_LANE=lead \
  mise run vp:stand:hd
```

実行後、 起動した tmux session を抜ける時は通常通り `Ctrl-b d` で detach、 kill する場合は別 terminal から `tmux kill-session -t vp-test-lead-tx` 等。

## per-project override

mise の cascade を活用、 VP 側の改修不要:

```
~/repos/creo-memories/.mise/tasks/vp/stand/hd  ← project-local override (もしあれば優先)
~/repos/vantage-point/.mise/tasks/vp/stand/hd  ← workspace default (本ディレクトリ)
~/.config/mise/tasks/vp/stand/hd               ← global fallback (もしあれば)
```

各 project の workflow に合わせて Stand を override できる。 例えば creo-memories project で「HD lane では rails console + claude を一緒に起動」 したい場合、 project の `.mise/tasks/vp/stand/hd` を作ってそこで rails+claude を起動する script に置き換える。

## 追加 Stand の作り方

新しい Stand を追加する手順:

1. `.mise/tasks/vp/stand/{name}` でファイル作成 (拡張子は任意、 shebang で interpreter 指定可)
2. shebang (`#!/usr/bin/env bash` / `ruby` / `python`)
3. 先頭に `#MISE description=...` `#VP icon="..."` `#VP tier=N` を記述
4. VP env (`VP_CWD`, `VP_SESSION`, `VP_PROJECT`, `VP_LANE`) を読み取って必要な処理を実行
5. `chmod +x .mise/tasks/vp/stand/{name}` で実行可能に
6. `mise run vp:stand:{name}` で standalone 動作確認

例: Claude Opus 4.7 xhigh で起動する Stand (`opus.rb`):

```ruby
#!/usr/bin/env ruby
#MISE description="Claude with Opus 4.7 xhigh thinking"
#VP icon="🧠"
#VP tier=2

cwd     = ENV.fetch('VP_CWD')
session = ENV.fetch('VP_SESSION')
model   = ENV['CLAUDE_MODEL'] || 'claude-opus-47-xhigh'
xhigh   = ENV['CC_XHIGH'] == '1'

claude_args = ["--model #{model}", ("--thinking-effort xhigh" if xhigh)].compact.join(' ')
exec %Q{tmux new-session -A -c "#{cwd}" -s "#{session}" "claude #{claude_args} || claude"}
```

VP 起動 (Phase 2) 後は sidebar の dropdown に「opus」 が現れて選択可能。
