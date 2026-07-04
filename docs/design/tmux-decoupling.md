# tmux decoupling — 設計メモ（草案）

> lane: `mako/tmux-decouple` / 起票: 2026-07-04 / status: 議論中（草案）

## 1. 背景・動機

- **駆動力 = #2（console/PTY の env 注入の痛み）**。TERM/LANG/PATH 注入税・CJK blackout・console 制御バグ（memory `tmux-client-utf8-cjk-blackout` / `sp-console-term-blackout` / `console-control-fix-handoff`）は全て tmux 層由来。
- **#1（detach 永続）は de-prioritize**。VP が安定してきたので「daemon/SP と独立した tmux server で claude を生存させる」必要性が下がった（user 判断 2026-07-04）。
- **方針 = 依存を少しずつ減らす**（user）。SP が既に常駐しているので、tmux という別プロセス依存を段階的に外す。

## 2. 現状トポロジと痛みの根

```
TheWorld(daemon) → SP(常駐) → PtySlot(portable-pty, env 注入済) → tmux new-session -A → claude
                                                                    ↑ 冗長な二重 env 層
```

- **`crates/vantage-point/src/daemon/pty_slot.rs` の `PtySlot` が既に native PTY host**:
  portable-pty ベース・cross-platform（Windows ConPTY 対応）、`TERM=xterm-256color` / `LANG` / `LC_CTYPE` / `PATH` を spawn 時に自前注入（`crate::spawn_env`）、出力を tokio broadcast → xterm.js、resize、DSR auto-answer、Drop で kill+wait、ユニットテスト済み。
- **痛みの根（確定）**: PtySlot は env を正しく注入している。しかしその PTY 内で `tmux new-session -A` を起動すると、tmux server/client が **nested な env を再導出**し utf8=0 / TERM 取りこぼしを起こす。今までの修正（.mise task の LANG guard、launchd plist への TERM/LANG 焼き込み）は全て「tmux 層に env を注入して回る」対症療法だった。
- 結論: **tmux は「既に正しく動いている native PTY host」の中に挟まった冗長な二重 env 層**。

## 3. 洞察 — native PTY host は既にある

除去は「新規に PTY host を作る」ではなく **「PtySlot の中の tmux nesting を外す」**。

```
TheWorld(daemon) → SP(常駐) → PtySlot → (login shell →) claude
```

PtySlot の正しい env が claude に直接届き、tmux の env 再導出が消える → #2 の痛みが構造的に消滅する。

## 4. トレードオフ

- **claude の寿命 = SP の寿命**になる（tmux 独立 server の生存を失う）。#1 de-prioritize 済みで許容。
- **復帰 = claude `--resume <session-id>`**。`crates/vantage-point/src/lane/cc_session.rs` / `vp lane last-session` が CC session id を追跡済み。daemon/SP 再起動時に direct-spawn へ `--resume` を渡すのが pilot の唯一の新規実装。
- 「プロセスは死ぬがコンテキストは蘇る」= AI-native な永続モデル。

## 5. tmux 依存 surface 棚卸し（grep grounding, 2026-07-04）

**中核 wrapper**
- `src/tmux.rs`（`tmux_bin` / `is_tmux_available` / `tmux_command` / `spawn_or_adopt`）
- `src/process/tmux_actor.rs`（async 制御 actor）

**面（surface）別**
| surface | 主なファイル | 役割 |
|---|---|---|
| console / echoes spawn | `.mise/tasks/vp/stand/echoes`, `process/stand_spawner.rs`, `process/stand_metadata.rs`, `daemon/pty_slot.rs` | claude を tmux 内で起動 |
| lane | `process/lanes_state.rs`(81), `process/routes/lanes.rs`, `mcp/lane.rs`, `process/lane_spawn_actor.rs`, `lane/cc_session.rs`, `process/lane_cmd.rs` | lane = tmux session 前提 |
| CLI 制御 | `commands/hd.rs`(48), `commands/tmux.rs`, `commands/directmsg.rs`, `commands/flow.rs`, `commands/restart{,_all}.rs`, `commands/sp.rs`, `commands/daemon.rs` | send-keys / capture / attach |
| wire / delivery | `process/delivery_actor.rs`, `process/delegation.rs`, `commands/wire.rs` | 通知の tmux passthrough |
| infra | `process/state.rs`(48), `process/unison_server.rs`(53), `vp-paths`(socket `-L vp` / `-L vp-dev`), `vp-paths/spawn_env.rs` | socket 名 / spawn env |

## 6. 制御面の対応（tmux → PtySlot）

| 現状（tmux） | 移行先（PtySlot） |
|---|---|
| `send-keys`（入力注入） | `PtySlot::write(stdin)` |
| `capture-pane`（出力取得） | `PtySlot::subscribe_output`（broadcast） |
| `new-session -A`（attach-or-create） | PtySlot spawn + `--resume` |
| directmsg / wire hook passthrough | PtySlot.write（別 surface で移行） |

## 7. 移行戦略 → 2 PR strangler（§11 scoping で確定）

当初 one-shot 全撤去を lean としたが、scoping（§11）で **2 PR strangler に確定**。ただし incremental の dual-backend limbo とは違い、**各 PR が clean cut**（`pre-mvp-development-stance` / `vp-rebuild-epic-dev-policy` の「中間状態を残さない」と両立）。決め手は atomic 性: 制御面（nudge/wake）を先に tmux 非依存化すると **tmux 存置のまま独立検証でき**、PR2 の risk surface を host swap だけに絞れる。詳細は §11。

- **一時的な二重バックエンド（tmux + native の常時共存）は作らない**。dev を claude-on-kitty にすることで「自分のセッションを守るために旧経路を残す」制約が消え、直切替が安全に取れる（§10）。
- 面ごとの依存関係（参考、§11 の PR 分割の根拠）:
  1. **pilot: echoes / console**（下記 §8）
  2. 制御面: `directmsg` / wire injection を `PtySlot::write` へ
  3. `vp tmux` / `vp hd` の撤去 or 再定義
  4. lane: tmux session 前提を PtySlot slot 前提へ
  5. `tmux.rs` / `tmux_actor.rs` / vp-paths socket の撤去

## 8. Pilot — SP console の native PTY 化

- **変更点**: echoes 起動を `PtySlot → mise run vp:stand:echoes（内部で tmux new-session）→ claude` から **`PtySlot → claude`（必要なら login shell 経由）** に直結。
- **新規実装**: restart 復帰の `--resume <session-id>` 配線。
- **成功基準**:
  - (a) console が tmux なしで xterm.js に描画される
  - (b) CJK / 日本語が化けない（PtySlot の LANG 注入のみで足りる）
  - (c) TERM/LANG/PATH の「tmux への注入して回る」保守が消える
  - (d) daemon gentle restart 後に `--resume` で会話継続
- **非対象（pilot では tmux 温存）**: directmsg / wire / `vp hd` / lane。これらの為に tmux は残る（依存は縮小するが未撤去）。

## 9. Open questions

- login shell を挟むか（`zsh -l → claude`）。env の source を login shell に委譲する `echoes-act1-primary-design`（Act1 primary）と統合すべきか。
- `--resume` の mid-turn 復帰の堅牢性（restart タイミング次第で会話が途中状態になる）。
- 既存の adopt 経路（稼働中 tmux session の adopt）は pilot で消える → 既存稼働 session の移行 UX をどうするか。
- `vp tmux` / `vp hd` は「便利ツール」として残す価値があるか、完全撤去か。

## 10. dev 環境・検証戦略（claude-on-kitty）

- **実装は claude-on-kitty で行う**（rebuild Epic 同様、`vp-rebuild-epic-dev-policy`）。理由: tmux/lane 経路を引き剥がす作業を、その tmux/lane を**通らない**独立セッション（kitty 直の claude）でやる → 途中で console/lane が壊れても dev セッションが生き残る（dogfood-critical path で自分の枝を切らない）。
- これが one-shot 直切替を安全にする: 自セッション保護のために旧 tmux 経路を残す必要がなくなる。
- **検証は dev/brew profile 分離で**（`vp-profile-env-isolation`）:
  - canonical(brew) VP は素の tmux 経路のまま常駐継続（無傷）。
  - 新 `PtySlot → claude` 経路は dev-profile VP（`vpd` / `VP_PROFILE=dev` / :32100 / tmux-less）で実機検証。
  - 壊しても brew canonical に影響しない。
- 成功後に release cut で brew canonical へ降ろす。

## 11. Scoping 結果 → 判定: 2 PR（strangler）

Explore agent が全 surface（44 の .rs + mise task）を実読。**判定 = 2 PR。理由は「量」ではなく atomic 性**: 制御面（nudge/wake）は先に tmux 非依存化しても **tmux 存置のまま検証できる**（`write_to_lane` は既存で、現 PtySlot が tmux をラップしているため今でも claude に届く）ので、PR2 の risk surface を host swap だけに絞れる。dual-backend limbo は作らない（各 PR が clean cut）。

### 分類（要約）
- **CORE（host swap の不可分集合）**: `.mise/tasks/vp/stand/echoes`, `process/stand_spawner.rs`(spawn_or_adopt/build_stand_command), `process/lanes_state.rs`(TmuxLaneAddress/TmuxMode/tmux field/restart_lane/with_conductor), `process/lane_spawn_actor.rs`, `process/routes/lanes.rs`
- **制御面（runtime 結合、PR1 対象）**: `process/delivery_actor.rs`(send_keys_to_session), `process/state.rs`(nudge_lane/resolve_lane_session/ensure_tmux), `process/delegation.rs`, `commands/directmsg.rs`, `mcp/lane.rs`(flow_handoff nudge), `commands/flow.rs`(try_nudge)
- **PERIPHERAL（独立削除）**: `process/tmux_actor.rs`, `unison_server.rs`(handle_tmux_*), `commands/tmux.rs`, `commands/hd.rs`, `tmux.rs`(leaf、最後に削除), `commands/restart{,_all}.rs`, `commands/sp.rs`, `.mise/tasks/vp/stand/tmux`, `.mise/tasks/daemon/stop`, vp-paths socket 名
- **TRIVIAL（コメント/表示 field）**: process_manager_capability, protocol(tmux_session wire field), discovery, commands/daemon(ps 表示), stand_metadata(`is_tmux_hosted` は prod 呼出 0 = dead), config/stands/agent/renderer, generated/agent_tools, vp-app webview

### PR 分割
- **PR1（制御面 → tmux 非依存、低リスク・tmux 存置で独立検証可）**: nudge/wake/directmsg/flow を `send-keys` → SP process-proxy `terminal_write`→`write_to_lane` に移す。`pick_nudge_target` の `TmuxMode::Tmux` gate 除去。directmsg の去就（SP 経由存続 or retire）を決める。
- **PR2（host swap、高リスク・不可分）**: echoes → 直 `claude --resume`、adopt 撤去 + duplicate-SP dedup 代替、restart_lane 簡素化、TmuxLaneAddress/TmuxMode/tmux field 退役、tmux.rs/TmuxActor/`vp tmux`/`vp hd`/tmux stand + mise cascade 削除。

### hidden assumptions（実装前判断・PR2 の難所）
1. **tmux session 名 = cross-process IPC namespace** — TheWorld daemon(delivery_actor) と `vp directmsg`(別プロセス) が SP 非経由で `send-keys -t <session>` 注入。撤去後は SP proxy `write_to_lane` 必須（TheWorld nudge は「SP 到達可能」前提に）。→ PR1 で解消する対象。
2. **directmsg の SP/DB 非依存 emergency channel が原理的に消える** — 製品判断（存続 or retire）。
3. **adopt = reconcile の要**（duplicate-SP Dead 化バグ 2026-06-30 の防波堤）— 単純削除で再発。dedup 代替を PR2 に同梱必須。
4. **crash recovery 意味論変更** — tmux detached 生存 → PtySlot Drop で claude kill、復帰は `--resume`（会話復元）。#1 で合意済。
5. **描画/keys** — truecolor(`Tc`)/extended-keys が tmux shim 無しで xterm.js に出るか要実機検証。per-session `VP_*` env の cross-project leak 防止は PtySlot 直 env で自然消滅（簡素化）。
