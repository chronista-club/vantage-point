# AGENTS.md — vantage-point agent 規約

> 対象: この repo で作業するすべての coding agent（Claude Code / Cursor / Antigravity / Codex 等）。
> 本ファイルは cross-agent な最小規約の SSOT。Claude Code 向けの詳細（開発コマンド・アーキテクチャ等）は `CLAUDE.md` を参照。

## branch / lane 規約

dev trunk は **nightly**（GitHub default の `main` は公開 release 専用・直 push 禁止）。
並列に作業する agent は lane（= git worktree）単位で隔離する。**並列 lane で作業する agent は lead checkout（この repo 本体）の branch を切り替えない**こと — branch の checkout は自分の worktree 内でのみ行う（lead checkout の branch 操作は lead session だけが行う）。

### branch 名 = 段（branch-step、2026-10-01 適用）

branch 名は **「今どの段にいるか」だけ**を語る（chronista-style plugin の `branch-step` スキル。設計は plugin-chronista-style の `docs/design/02-branch-step-naming.md`）。

| 段 | 名前 | 意味 |
|---|---|---|
| 探っている | `spike/<slug>` | **ローカル専用**（pre-push hook が push を拒否）。捨ててよい |
| 作っている | `wip/<slug>` | lane の既定。origin/nightly 起点 |
| 見せている | `review/<slug>` | PR が開いている（base nightly） |
| 生かしておく実験 | `exp/<slug>` | 掃除・停滞検知の対象外 |
| main 起点の緊急修正 | `hotfix/<slug>` | main へ PR、tag、nightly へ back-merge |

- **slug** = 起票 memory の Branch slug と同じ `[a-z0-9-]+`。**lane 名 = slug**、worktree は `.vp/lanes/<slug>`。type（feat / fix）は commit message の仕事で枝名には載せない
- 段を進める操作は `git next`（spike/exp → wip、wip → review。review に入る時だけ push + PR）/ `git keep`（spike → exp、初 push）/ `git drop`（spike を削除）/ `git board`（一覧）
- merge は `gh pr merge --squash --delete-branch` → worktree を畳む（`git worktree remove`）→ `git worktree prune`。`review/` 以降は rename しない（直しは PR の中で）

#### 導入（repo ごとに一度。plugin を更新したら**やり直す** — alias が plugin の版ごとの絶対パスを指すため）

```bash
bash "$HOME/.claude/plugins/cache/chronista-plugins/chronista-style/<ver>/skills/branch-step/scripts/branch-step" install --local
# wip → review の門 = VP の標準チェック（CI と同じ mise run check）。
# ⚠️ env を剥がす形で設定する: git の alias の中で走るため GIT_DIR 等が子プロセスに残り、
#    テストが一時 repo で git を呼ぶと本物の repo に書き込む（plugin 0.33.0。creo-memories の lane で
#    起きて巻き戻した、と nexus lane から 2026-10-01 に警告。plugin 側の修正が入るまでの防御）
git config branch-step.test 'env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_PREFIX -u GIT_CONFIG_PARAMETERS mise run check'
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
```

- PR は `git next --memory <mem_id>` で開く（`review/<slug>` に昇格し、base = **nightly** で `gh pr create`。body 冒頭に memory ID）。手で開くなら base を明示する: `gh pr create --base nightly`（GitHub default が main のため、省略すると main に向いてしまう）

### discovery

lane の一覧は git-native に取得する（manifest ファイルは存在しない）:

- `git worktree list` — live registry（worktree lane の全列挙）
- `git branch --list 'wip/*' 'review/*'` — lane branch の列挙（`git board` は段ごとに並べる）

## wire 規約（inter-agent messaging）

wire messaging 全体の見取り図（store / category / ack 台帳 / federation / flow_state 投影）は
`docs/guide/messaging.md`。wire message の `body.kind` は dev-flow FSM の入力になる（taxonomy と
FSM の詳細 = `docs/guide/dev-flow-primitives.md` §3）。特に:

- **`needs_user`**: 「main では捌けない、**ユーザ本人**の意見が要る」相談を投げる時は
  `body.kind = "needs_user"` + `body.category = "command"` で main 宛に送る。
  受信側は**ユーザの回答を relay してから** `wire_ack` する — 未 ack の間、その sub は
  `awaiting_user`（sidebar の needs-you 表示）のまま。main が自分で判断できる相談は
  `question` を使い、needs_user は乱発しない（needs-you signal の希少性を守る）。

### GitNexus との読み替え

下記 GitNexus block の `detect_changes` 例にある `base_ref: "main"` は、この repo では **`base_ref: "nightly"`** に読み替えること（main は公開 release 専用で dev からの diff が膨らむ）。

<!-- 以下は gitnexus 管理 block。start/end marker の間のみ `gitnexus analyze` が再生成する（外側の本セクションは上書きされない） -->
<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **vantage-point**. Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

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
