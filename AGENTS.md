# AGENTS.md — vantage-point agent 規約

> 対象: この repo で作業するすべての coding agent（Claude Code / Cursor / Antigravity / Codex 等）。
> 本ファイルは cross-agent な最小規約の SSOT。Claude Code 向けの詳細（開発コマンド・アーキテクチャ等）は `CLAUDE.md` を参照。

## branch / lane 規約

dev trunk は **nightly**（GitHub default の `main` は公開 release 専用・直 push 禁止）。
並列に作業する agent は lane（= git worktree）単位で隔離する。**並列 lane で作業する agent は lead checkout（この repo 本体）の branch を切り替えない**こと — branch の checkout は自分の worktree 内でのみ行う（lead checkout の branch 操作は lead session だけが行う）。

### 公認入口: `vp lane new`

lane の作成は `vp lane new <name>` を使う。branch 命名・base 解決・worktree 配置・並列作成時の一意性担保を VP が行う。

### raw-git fallback（`vp` CLI が使えない agent 向け）

`vp` が無い環境で lane 相当を作る場合は以下の規約に従う:

- worktree 配置: `<repo>/.vp/lanes/<name>`（`.vp/` は gitignore 済み）
- branch 名: `mako/<slug>`
- base: **origin/nightly**（作成前に `git fetch origin nightly`）

```bash
git fetch origin nightly
git worktree add -b mako/<slug> .vp/lanes/<name> origin/nightly
```

- PR は base = **nightly** を明示する: `gh pr create --base nightly`（GitHub default が main のため、省略すると main に向いてしまう）

### discovery

lane の一覧は git-native に取得する（manifest ファイルは存在しない）:

- `git worktree list` — live registry（worktree lane の全列挙）
- `git branch --list 'mako/*'` — lane branch の列挙

### GitNexus との読み替え

下記 GitNexus block の `detect_changes` 例にある `base_ref: "main"` は、この repo では **`base_ref: "nightly"`** に読み替えること（main は公開 release 専用で dev からの diff が膨らむ）。

<!-- 以下は gitnexus 管理 block。start/end marker の間のみ `gitnexus analyze` が再生成する（外側の本セクションは上書きされない） -->
<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **vantage-point** (11429 symbols, 24992 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

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
