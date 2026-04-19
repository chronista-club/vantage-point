# Mailbox vs ccwire — 役割分離 (2026-04-19)

## 結論

| 機能 | 担当 | 場所 |
|------|------|------|
| **Actor 間 messaging（cross-Process 含む）** | VP Mailbox | `crates/vantage-point/src/capability/mailbox*.rs` |
| **tmux session のライフサイクル追跡** | ccwire | `crates/vantage-point/src/ccwire.rs` + `~/repos/claude-plugin-ccwire` |
| **tmux pane operation（split/capture/send-keys）** | VP `tmux_actor` | `crates/vantage-point/src/process/tmux_actor.rs` |

## 経緯

2026-04-18 当初は「ccwire 完全削除 + mailbox に統一」を計画
（決定: `mem_1CaB6nfYtxWhmpKemXWoUw`、Path C: `mem_1CaBDETcyDY5YKeYpXBf2j`）。

しかし ccwire の **tmux session tracking** には独自価値があり、
役割分離 + 進化させる方針に転換（2026-04-19、ccwire = tmux power-tool ビジョン）。

## 役割境界

### Mailbox（messaging primitive）

- **scope**: actor 間の point-to-point messaging
- **transport**: 同一 Process 内 = mpsc、cross-Process = TheWorld registry + HTTP forward
- **特徴**: 永続化（Whitesnake）、TTL、manual_ack、retry、auth、address parser
- **Phase 完了状況**:
  - Phase 1 (#140): opt-in persistent
  - Phase 2 (#144): TTL + manual_ack + GC
  - Phase 3 Step 1 (#146): TheWorld actor registry
  - Phase 3 Step 2a (#147): RemoteRoutingClient + receive route + 5 改善
  - Phase 3 Step 2b (#148): runtime wiring（startup register / shutdown unregister）

### ccwire（tmux session tracker → 進化方向: tmux power-tool）

- **現 scope**: tmux session の register / unregister / heartbeat / list
- **DB**: `~/.cache/ccwire/ccwire.db` (SQLite WAL)
- **連携**: `~/repos/claude-plugin-ccwire`（外部プラグイン）と DB 共有
- **進化方向**: tmux 機能フル活用（pane orchestration / metadata / monitor / hooks）
- **削除対象**:
  - `claude-plugin-ccwire` の `wire-send` / `wire-receive` / `wire-status`（messaging 機能）
  - これらは VP mailbox に統合済み

### tmux_actor / `vp tmux`（tmux primitive）

- **scope**: tmux pane operation（split / capture / send-keys / dashboard）
- **境界線**: 単一 VP Process 内の pane 操作
- **ccwire 進化版との関係**: spec 起こし時に決定（次セッション）

## ユーザー操作のマッピング

| ユーザーの目的 | 旧方式 | 新方式（推奨） |
|---------------|--------|--------------|
| 別 CC セッションにメッセージ送信 | `wire-send` | **`vp mailbox send`** または MCP `mailbox_send` |
| tmux session の一覧確認 | `wire-sessions` | `vp ccwire sessions` (将来) or 進化版 ccwire CLI |
| pane 分割 | tmux 直接 / `vp tmux split` | `vp tmux split` |
| エージェントへの状態通知 | （未対応） | `vp mailbox send to=notify` |

## 移行方針

1. **本 PR (Step 3)**: ccwire.rs の docstring 更新、本 doc 追加
2. **次 PR (別リポ)**: `claude-plugin-ccwire` の messaging 系コマンド削除
3. **次セッション**: ccwire 進化 spec 起こし（pane orchestration 範囲決定）

## 関連 PR

- #140 / #144: Mailbox Phase 1 / Phase 2
- #146 / #147 / #148: Mailbox Phase 3 Step 1 / 2a / 2b
- (本 PR): ccwire 役割明示
