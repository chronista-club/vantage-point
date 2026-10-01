# Design 73: lane のブランチ命名 — prefix が段、`wip/<slug>`（branch-step の適用）

> **Status**: 実装済み（2026-10-01、`wip/lane-branch-wip-prefix`）。規約側（AGENTS.md / CLAUDE.md、`git next` の門）は同日の `review/branch-step-adopt`（PR #1156）。
> **正本**: 設計と裁定 = creo `mem_1CfZvzMGQyyyQJLqyZMyR8`（[[branch-step-naming]]）、図 = `mem_1CfZz3aDDKfmVm7uRs6w9Z`。skill = chronista-style plugin 0.33.0 `branch-step`（設計 doc は plugin-chronista-style `docs/design/02-branch-step-naming.md`）。この doc は VP 本体への適用だけを書く。
> **task**: creo `mem_1CfaMskLwpEupufZsHTMxt`（GO: mako 2026-10-01「他のレポにもbranch-step適用しよう」）

## 1. 何を変えたか

| 前 | 後 |
|---|---|
| lane の既定 branch = `<git-user>/<name>`（`git config user.name` を lowercase + sanitize して prefix に） | **`wip/<name>`**（`lane::config::default_branch_for`） |
| lane 名の allowlist `[a-zA-Z0-9_-]`（ブランチ名側で sanitize） | **`[a-z0-9-]+`**、先頭は英数字（`validate_sub_name`）。外れたら**拒否**（丸めない） |
| `vp lane new <name> <branch>`（branch 必須） | `vp lane new <name> [branch]`（省略時 `wip/<name>`。`fork` も同じ） |

- 導出は 1 か所（`lane::config::default_branch_for`）。呼び手は 4 か所 — daemon の `lanes/create`（`control_ops::resolve_create_lane_args`）、repo の `create_sub_orchestrated`（`repo/lane/lifecycle.rs`）、CLI の `lane new` と `lane fork`（`vp-cli/src/main.rs`）
- `derive_default_branch` / `sanitize_for_branch`（`repo/lane/lifecycle.rs`）は撤去。git user を読む subprocess も消えた
- base の解決（`--base` → `.vp/sub-files.kdl` の `base-ref` → origin/HEAD → main）は変えていない
- **既存 lane は触らない**。検証は作成時にしか走らず、既存の `mako/*` の枝と worktree はそのまま生きる（migration しない）

## 2. なぜ

- **ブランチ名は「今どの段にいるか」だけを語る**（branch-step の原理）。`git next` は `wip/<slug>` → `review/<slug>` と prefix だけを rename する前提なので、prefix に人名（`mako/`）が座っていると段を表せない
- **誰が切ったかはブランチ名の仕事ではない**。commit の author が持つ情報で、ブランチ名に載せると同じ課題を別の人が引き継いだときに名前が嘘になる
- **lane 名 = slug = ブランチ名の末尾 = worktree dir 名**を一致させる。旧実装は lane 名をブランチ名側で sanitize（`Feat_API` → `feat_api`）していたので、`git branch --list` と lane 一覧が対応しない余地があった。slug 規約で検証して拒否すれば、変換が要らず、ずれも生まれない
- 大文字と `_` を落としたのは branch-step の slug 規約（`[a-z0-9-]+`、起票 memory の Branch slug と完全一致で探す）に合わせるため。2026-10-01 に repo を数えた範囲（`setup_sub` / `new_sub_in` / `validate_sub_name` の呼び出しと、docs の `vp lane new` / `flow_handoff` の例）で、`_` や大文字を含む lane 名は `validate_sub_name` の test 入力（`feature_login` / `_leading`）だけだった。実在の lane（`.vp/lanes/`）も slug 形のみ

## 3. 影響と見送り

- **拒否される入力が増える**: `vp lane new Foo_bar` / MCP `add_sub` / `flow_handoff` の `name` に大文字や `_` があると error（message に理由）。呼ぶ側が slug で起票していれば当たらない
- **`vp lane new` の第 2 引数が省略可能に**。指定すれば従来どおりその名前で切れる（`exp/` や `hotfix/` を手で切る用途）
- 見送り: 既存 lane の枝の rename、`train/` / stack 形（branch-step 側で未決）、`git board` を VP の lane 一覧に描く（別 task）

## 4. 検証

- unit: `lane::config::tests`（`default_branch_is_wip_slug` / `name_is_a_branch_step_slug`）、`daemon::server` の `create_lane_defaults_are_derived`（`wip/sub` を厳密一致で見る）
- 実機（2026-10-01、この枝の debug build `vp`、使い捨て repo で）: `vp lane new foo` → `.vp/lanes/foo` が `wip/foo` で origin/nightly（HEAD = init）から切れた / `vp lane new Foo_bar` → `invalid sub name … Only [a-z0-9-] are allowed` で exit 1、dir は作られない / `vp lane new bar exp/bar` → 明示 branch はそのまま `exp/bar` / vantage-point 本体で `vp lane list` → 既存 lane `portal`（`mako/portal`）はそのまま
- 未確認: daemon 経由（MCP `add_sub` / `flow_handoff`、GUI）。導出は同じ `default_branch_for` を通るが、daemon の再起動が要るので merge 後の dogfood で

## Status log

- 2026-10-01: 初版。task `mem_1CfaMskLwpEupufZsHTMxt` の loop B。規約側（loop A）は PR #1156
