# ccwire リデザイン仕様 (2026-04-19)

## 背景

2026-04-18 に messaging を VP Mailbox に完全委譲する決定（[`03-mailbox-vs-ccwire.md`](03-mailbox-vs-ccwire.md)）。
ccwire は単なる「セッション登録簿」から **並列開発メッシュの semantic 管理層** に役割を再定義する。

2026-04-21 から VP を外部ユーザにも使ってもらい始めるため、新 identity を spec として固定する（実装は段階的）。

## identity

> **ccwire = 並列開発メッシュの semantic 管理層**
>
> tmux pane に role（lead / worker / log / repl 等）を付与し、role-based query / spawn / dispatch を提供する。
> `vp tmux` が **素の tmux 操作** に対し、ccwire は **「役割を持った pane 群」** を扱う。

## scope（今回 IN）

### A) tmux mesh orchestrator
- lead / worker constellation の構築・破棄
- pane 生成と semantic role の自動付与
- ccws との連動（worker spawn 時に Issue ブランチで隔離環境作成）

### B) declarative tmux config
- `ccwire.toml`（プロジェクト直下）で lead/worker 構成を宣言
- `vp ccwire apply` で実体化 / `vp ccwire diff` で差分表示

### C) tmux DSL / workflow scripting
- 高頻度操作（split → send → capture）をマクロ化
- Hooks 連携（tmux session 起動時に role 自動付与）

### D) tmux pane registry + role 管理
- 各 pane に role + metadata（branch, task id, parent）を登録
- semantic query: `vp ccwire panes --role worker --branch mako/vp-XX`

## scope（今回 OUT）

- **E) cross-tool tmux 共有 layer** — claude-plugin-ccwire 等との view 共有は当面 ccwire DB 互換維持で対応、専用 API 化は後回し
- **F) tmux 抽象化** — zellij / WSL 対応は後回し、当面 tmux 専用

## CLI 表面（spec）

```bash
# Mesh ライフサイクル
vp ccwire init                   # 現セッションを mesh として初期化、現 pane = lead
vp ccwire status                 # mesh 状態（lead / worker 数 / role 分布）
vp ccwire teardown               # mesh 解体（pane は残す、role 登録のみ解除）

# Worker spawn
vp ccwire spawn-worker --task VP-XX --branch mako/vp-XX [--mode relay|autonomous]
vp ccwire kill-worker <name>      # ccws cleanup と連動

# Pane query
vp ccwire panes                   # 全 pane（role 付き）
vp ccwire panes --role worker
vp ccwire panes --role worker --branch mako/vp-55

# Role 管理（手動）
vp ccwire assign <pane-id> --role <role> [--metadata key=val]
vp ccwire unassign <pane-id>

# Declarative
vp ccwire apply [--config ccwire.toml]   # 宣言と現状の差分を実体化
vp ccwire diff
vp ccwire export > ccwire.toml          # 現状を toml で書き出し

# Workflow macro
vp ccwire macro split-send <pane-id> "<keys>"
vp ccwire macro capture-after <pane-id> "<keys>" --wait 500ms

# Dispatch（mailbox との橋渡し、convenience）
vp ccwire dispatch worker-1 "wire_send で質問"   # → mailbox_send to: "worker-1@..."
```

## ccwire.toml フォーマット（spec）

```toml
schema_version = 1

[mesh]
name = "vantage-point"

[[panes]]
role = "lead"
session = "vp"
window = 0
pane = 0

[[panes]]
role = "worker"
session = "vp"
window = 0
pane = 1
metadata = { task = "VP-55", branch = "mako/vp-55" }

[[panes]]
role = "log"
session = "vp"
window = 0
pane = 2
follow = "tail -f /tmp/vp.log"
```

## Mailbox actor address との関係

- `worker-1` (semantic role + 連番) ↔ mailbox actor `worker-1@vantage-point`
- ccwire spawn 時に actor 名を自動採番、mailbox に register
- `vp ccwire dispatch worker-1 "..."` は内部で `mailbox_send(to: "worker-1@vantage-point", payload: ...)`

## 既存 ccwire DB との互換

- 現状の `~/.cache/ccwire/ccwire.db` は **そのまま維持**
- 新 ccwire は **role 拡張 column（または別テーブル）** を追加（既存 schema は temper しない）
- claude-plugin-ccwire（外部）は既存 schema を読み続ける、role 拡張は VP 内のみ参照

## 実装フェーズ

| Phase | 内容 | リリース | breaking |
|-------|------|---------|---------|
| **0** | この spec doc を main に置く | v0.11.x | なし |
| **1** | D 実装: pane registry + role 付与 + query | v0.12.0 | なし（追加） |
| **2** | A 実装: spawn-worker + ccws 連動 + Mailbox 統合 | v0.12.0 / v0.13.0 | なし |
| **3** | B 実装: ccwire.toml + apply/diff | v0.13.0 | なし |
| **4** | C 実装: workflow macro + Hooks 連携 | v0.13.0 / v0.14.0 | なし |

## 既存「ccwire セッション登録簿」との関係

VP 内の `crates/vantage-point/src/ccwire.rs` は **既存機能（register / heartbeat / list）を維持**。
新 ccwire 機能は `crates/vantage-point/src/ccwire/` モジュール化して内部に追加（並存）。

## 関連

- [03-mailbox-vs-ccwire.md](03-mailbox-vs-ccwire.md) — 役割分離の前提
- [`docs/decisions/2026-04-19-strategy-summary.md`](../decisions/2026-04-19-strategy-summary.md) — 戦略総まとめ
