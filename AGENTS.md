# AGENTS.md — vantage-point agent 規約

> 対象: この repo で作業するすべての coding agent（Claude Code / Cursor / Antigravity / Codex 等）。
> 本ファイルは cross-agent な最小規約の SSOT。Claude Code 向けの詳細（開発コマンド・アーキテクチャ等）は `CLAUDE.md` を参照。

## branch / lane 規約

dev trunk は **nightly**（GitHub default の `main` は公開 release 専用・直 push 禁止）。
並列に作業する agent は lane（= git worktree）単位で隔離する。**並列 lane で作業する agent は lead checkout（この repo 本体）の branch を切り替えない**こと — branch の checkout は自分の worktree 内でのみ行う（lead checkout の branch 操作は lead session だけが行う）。

### ブランチ命名（branch-step、2026-10-01 適用）— prefix が段、slug は不変

ブランチ名は **「今どの段にいるか」だけ**を語る（chronista-style plugin の `branch-step` スキル。設計は plugin-chronista-style の `docs/design/02-branch-step-naming.md`）。

| 段 | 名前 | 意味 |
|---|---|---|
| 探っている | `spike/<slug>` | **ローカル専用**（pre-push hook が push を拒否）。捨ててよい |
| 作っている | `wip/<slug>` | lane の既定。origin/nightly 起点 |
| 見せている | `review/<slug>` | PR が開いている（base nightly） |
| 生かしておく実験 | `exp/<slug>` | 掃除・停滞検知の対象外 |
| main 起点の緊急修正 | `hotfix/<slug>` | main へ PR、tag、nightly へ back-merge |

- **slug** = 起票 memory の Branch slug と同じ `[a-z0-9-]+`。**lane 名 = slug**、worktree は `.vp/lanes/<slug>`。type（feat / fix）は commit message の仕事でブランチ名には載せない
- 段を進める操作は `git next`（spike/exp → wip、wip → review。review に入る時だけ push + PR）/ `git keep`（spike → exp、初 push）/ `git drop`（spike を削除）/ `git board`（一覧）
- merge は `gh pr merge --squash --delete-branch` → worktree を畳む（`git worktree remove`）→ `git worktree prune`。`review/` 以降は rename しない（直しは PR の中で）

#### 呼び方と導入 — 正本は plugin の skill

`git next` などの呼び方と、新しい clone での導入（`branch-step install`）は、chronista-style plugin の
`branch-step` skill（「呼び方」「導入」の節）に従う。**ここには写さない** — 写しは plugin が変わっても
古いまま残る（実例: 0.36.0 で入口が git alias から plugin の `bin/` に移り、ここにあった
「plugin を更新したら install をやり直す」が誤りになった）。Codex など plugin の `bin/` が PATH に入らない
engine での呼び方も、同じ skill にある。理由と経緯は plugin-chronista-style の
`docs/design/02-branch-step-naming.md`。

この repo に固有なのは、wip → review の門の設定だけ:

```bash
# 門 = VP の標準チェック（CI と同じ mise run check）。app:bundle を先に挟むのは、新しい worktree には
# webview bundle（生成物、commit しない）が無く vp-app の build.rs が止まるため。
# env -u は、git が子プロセスに渡す GIT_DIR 等を剥がす防御（2026-10-01、テストが一時 repo で git を呼ぶと
# 本物の repo に書き込んだ）。plugin の script も 1.0.1 から自分で剥がすが、設定はそのまま残している。
git config branch-step.test 'env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_PREFIX -u GIT_CONFIG_PARAMETERS sh -c "mise run app:bundle && mise run check"'
```

### 公認入口: `vp lane new`

lane の作成は `vp lane new <name>` を使う。branch 命名・base 解決・worktree 配置・並列作成時の一意性担保を VP が行う（`<name>` は slug。branch は `wip/<slug>` — VP 本体側の対応は別 PR、それまでは下の raw-git 手順で `wip/` を切る）。

### raw-git fallback（`vp` CLI が使えない agent 向け）

`vp` が無い環境で lane 相当を作る場合は以下の規約に従う:

- worktree 配置: `<repo>/.vp/lanes/<slug>`（`.vp/` は gitignore 済み）
- branch 名: `wip/<slug>`
- base: **origin/nightly**（作成前に `git fetch origin nightly`）

```bash
git fetch origin nightly
git worktree add -b wip/<slug> .vp/lanes/<slug> origin/nightly
git -C .vp/lanes/<slug> submodule update --init --recursive   # vendor/agent-chat（共有チャット基盤）を埋める
```

- submodule: `vendor/agent-chat` は worktree add だけでは空のまま（`cargo` が path 依存を見つけられず落ちる）。raw-git で作ったら上の `submodule update --init --recursive` を一度流す。`vp lane new` は作成時に自動で流す（失敗は warn で続行、その時は手で）

- PR は `git next --memory <mem_id>` で開く（`review/<slug>` に昇格し、base = **nightly** で `gh pr create`。body 冒頭に memory ID）。手で開くなら base を明示する: `gh pr create --base nightly`（GitHub default が main のため、省略すると main に向いてしまう）

### discovery

lane の一覧は git-native に取得する（manifest ファイルは存在しない）:

- `git worktree list` — live registry（worktree lane の全列挙）
- `git branch --list 'wip/*' 'review/*'` — lane branch の列挙（`git board` は段ごとに並べる）

### sub lane の権限 — task の段（2026-10-17、mako 裁定）

sub lane が「どこまで自分で判断してよいか」は lane ではなく **task ごと**に決まる。lead が handoff の
spec の頭に `段: A | B | C | 調査` を書く。書き忘れは **B 扱い**（安全側）。lane は段を**上げる**のは
自由（「触ってみたら daemon に及ぶので C で進めます」と wire で一言）、**下げる**のは lead に聞く。

決め方の軸は 1 つ — **壊れたときに戻せるか、誰を巻き込むか**。

| 段 | 対象の目安 | Ready 前の自己レビュー | PR | merge |
|---|---|---|---|---|
| **A: 直し** | 1 か所の bug 修正、doc、設定値。自分の worktree で完結し revert 1 発で戻る | なし（gate と claims で足りる） | `git next` で出す | lead（CI 緑で） |
| **B: 機能** | 複数ファイル、新しい挙動、UI 変更。他の人が触って初めて分かる | **moody-blues を 1 回**。指摘と対応（直した / 見送った理由）を PR 本文に書く | 出す | lead（lead 側のレビューも残す） |
| **C: 危ないもの** | daemon の lifecycle、安全装置（guard）、protocol / wire の形、plugin の release、実機まわり、2 repo にまたがるもの。戻すのに他人や再起動が要る | **santa**（moody-blues × sticky-fingers） | **draft** で出し、lead レビュー後に ready へ | lead レビュー後、**mako が可否** |
| **調査** | 読むだけ・実測だけ・設計案。何も変えない | — | **出さない**。`spike/`（local のみ）か `exp/`（push 可）。報告は wire + creo の annotation | — |

全段で変わらないもの:

- TDD、gate（`cargo fmt` / `mise run check` / clippy `-D warnings` / `mise run test` / `mise run claims`）、
  Ready は証拠付き（何を検証し、何を未検証で残したか、実機で見るべき手順）
- 禁止: daemon 再起動 / `app:swap`、merge、他 lane の worktree を触る、実機の書き込み系（MIDI 設定・Serial 書き込み）
- **repo をまたぐ契約**（コマンド名 / wire の形 / API の口 / mise の task 名）は、相手側の**実物**（`mise tasks ls`、
  実際の JSON、実際のエラー文）で突き合わせてから Ready。spec に書かれた名前を正としない
  （2026-10-17 の実例: lead が `mise tasks ls` の出力だけを見て「task 名は `app:swap-detached` で、guard の
  `app:swap:detached` と食い違う」と判断し、直しの lane まで出した。実際は task ファイルの `#MISE alias` で
  `app:swap:detached` が実在し、`mise tasks info` で見れば分かった。**一覧コマンドは alias や別名を隠すことが
  ある** — 契約の照合は「その名前で実際に呼べるか」まで確かめる）
- 重い build（cargo）は同じ Mac の他 lane と枠を分け合う — 開始前・終了後に wire で一言、並列に走らせない
- lane の build は基本 mise task 経由。cargo / bun build を直接打つ場合も macOS は `taskpolicy -c utility`、他の Unix / taskpolicy 不在時は `nice -n 10` を付ける（Windows は素通し、mise の無効化は `VP_BUILD_PRIORITY=normal`）。

lead 側の規則（進行の見張り）:

- **止まり検知**: 作業中の lane から 30 分 wire が無ければ「次の一手」を具体的に 1 つ渡す。60 分で畳んで
  渡し直す。見るのは `vp lane ls --detail` の最終活動時刻（wire は「来たもの」にしか反応しないので、
  来ないことは lead が定期に見る。2026-10-16 に着手報告のあと 15 時間止まった lane に気づけなかった）
- Ready を受けたら lead がレビュー（B / C は sub の自己レビュー結果を読んでから）→ CI → merge → 次の task。
  C の merge は mako に可否を聞く

## wire 規約（inter-agent messaging）

wire messaging 全体の見取り図（store / category / ack 台帳 / federation / flow_state 投影）は
`docs/guide/messaging.md`。wire message の `body.kind` は dev-flow FSM の入力になる（taxonomy と
FSM の詳細 = `docs/guide/dev-flow-primitives.md` §3）。特に:

- **`needs_user`**: 「main では捌けない、**ユーザ本人**の意見が要る」相談を投げる時は
  `body.kind = "needs_user"` + `body.category = "command"` で main 宛に送る。
  受信側は**ユーザの回答を relay してから** `wire_ack` する — 未 ack の間、その sub は
  `awaiting_user`（sidebar の needs-you 表示）のまま。main が自分で判断できる相談は
  `question` を使い、needs_user は乱発しない（needs-you signal の希少性を守る）。

## code の影響確認 — 道具なしから dogfood し直す（2026-11-17）

影響確認の道具（gitnexus の MCP と skill、編集前の `impact` / commit 前の `detect_changes` の決まり）は
**いったん外した**（mako 2026-11-17「一旦何もなし（sem,sem-coreだけ残し）から、何が必要かを一つ一つ再度dogfoodしていく」）。
残しているのは sem の CLI だけ（`sem setup` 済みの clone では、commit 時に変更の一覧が出る）。

- 編集の前後の確認は、当面 grep と test で行う。`gitnexus analyze` は走らせない（以前は本ファイルと
  `CLAUDE.md` に節を自動で書いていた。印を消した今も書き足すかは未確認）
- **次の 2 種類の場面は、起きたその場で creo の action `mem_1Ch7G3HVai5DDxPsFJk2jQ` に 1 行足す**
  （日付、lane、何が起きたか、代わりに何をしたか）:
  - **取り違えた**: 影響を見落とした、呼び出し元を数え間違えた、古い情報で判断した、など
  - **欲しくなった**: 影響が分かればと思って、grep や手作業で代わりをした場面
- 記録がたまったら、どの場面が何回、どれだけ痛かったかで、最初に入れる 1 つを決める。
  経緯と測った数字は creo `mem_1Ch78gydQzkh9ZAnnRqNhx`
