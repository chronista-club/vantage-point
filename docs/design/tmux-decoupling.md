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

## 12. PR1 実機検証で判明した点（2026-07-04, dev-profile :32100）

- **配送は成立・CJK 無破損**: `vp wire send --category command` → delivery_actor(World) → `forward_to_sp_control`（新 control_channels 配線）→ SP `handle_lane_nudge` → PtySlot → tmux → conductor claude、で nudge text が実配送で `❯` に着弾。日本語/emoji も PtySlot の LANG 注入だけで化けない（§8-b 満たす）。最高リスクだった「同一 control_channels Arc を daemon + 両 World loop に配る」配線が実機で機能。
- **⚠️ submit されない → 2 phase 化で対処（PR1 内で修正済）**: 当初 `write_nudge` は `text + \r` を **1 回**で PtySlot に write していたが、submit されず input に留まる事象を確認。根因 = **PtySlot は tmux client の stdin を wrap**しており、burst 入力を tmux が **bracketed paste** 扱いにして paste 内 CR が改行化する（Enter keystroke の意味論が失われる）。旧 `send-keys -l text` + 別 `send-keys Enter` は tmux server が **pane PTY へ直接**注入するため submit が効いていた。
  - **対処**: `LanePool::write_nudge`（bundled）を廃し、`lanes_state::deliver_nudge`（async, 2 phase）に置換。① text を write（末尾 CR/LF 落とし）② `assume-paste-time`(既定1ms)を跨ぐ猶予(50ms) ③ Enter を**単独** write。paste の外の keystroke として Enter を成立させ submit させる。SP-local `nudge_lane` / proxy `handle_lane_nudge` の双方が同 sink に収束。
  - **PR2 への含意**: tmux 撤去（PtySlot→claude 直結）後は bracketed paste の wrap 主体（tmux client）が消えるため、この 2 phase は**中間状態の互換層**。PR2 で claude 直結にした際、`text\r` の 1 発 submit が効くか（= 2 phase を畳めるか）を再検証する。逆に効かない場合は claude 側の入力意味論に依存するので 2 phase を残す。

## 13. PR2 ゼロベース再設計 — 「1 つの lane に、1 つの名前、1 つの所有者」（2026-07-04 確定）

> user 指示「tmux のせいで交通渋滞を起こしてる実装かもだから、ゼロベースでリデザイン。強く美しい構造を」。
> 当初 PR2 案（§11 = echoes script から tmux を抜く）を破棄し、**script 層ごと消す** Rust-native 化に拡大。

### 13.1 診断 — 渋滞の実体

lane には現在 **4 つの名前**（LaneAddress / tmux session 名 / socket 名 / pane id）と
**4 層の env 中継**（launchd plist → SP → bash script → tmux -e → claude）があり、全サブシステムが
この翻訳に税を払う。LANG guard / plist 焼き込み / `-e VP_*` / SOCK 二重導出 / adopt / 800ms peek /
2-phase nudge / allow-passthrough / Tc override は全てこの翻訳層の瘢痕。

### 13.2 目標構造

| 層 | 所有者（唯一） | 消えるもの |
|---|---|---|
| Identity | `LaneAddress` | tmux session 名 / socket 名 / pane id / `tmux_session_name()` / TmuxLaneAddress / TmuxMode |
| Host | `PtySlot`（env 注入は spawn の 1 箇所） | bash → tmux server → tmux client の env 中継、LANG guard |
| Program | Rust の command builder（**bash script 層ごと削除**） | `.mise/tasks/vp/stand/*` 3 本、mise fallback、install-root cwd ダンス（PR-D Z 系統）、Windows shebang 回避 |
| Control | `deliver_nudge`（PR1 の形を維持、内部は要再検証） | send-keys / paste 意味論の補正 |
| View | broadcast → xterm.js ＋ per-lane grid `lane_capture` | TmuxActor / capture-pane / handle_tmux_* |
| Lifecycle | spawn / Drop=kill / restart=Drop+respawn(`--resume`) | adopt / kill_session / orphan reconcile |

Program の中身は **Act1-layered**（`echoes-act1-primary-design` と合流）:

```
PtySlot → $LOGIN_SHELL -l                    ← Act1: 常に生きる「床」（self-healing、/exit 後も prompt）
   ↓ initial_input（spawn 後に PTY へ type-ahead 注入）
   claude --resume 'ID' --settings 'HOOKS' || claude --settings 'HOOKS'   ← Act3（|| fallback は shell が native 処理）
```

- **死んでいた機構の復権**: `StandCommand.initial_input` と early-exit fallback（「PR-B 以降 dead、防御的維持」）が主機構に戻る。新しい状態機械は不要。
- **層の逆転の解消**: bash が `vp lane last-session` を子→親 CLI 呼び出ししていたのを、spawn 前に Rust が `lane::cc_session` を直読み。
- **VP_SESSION は lane display 形（`<project>/conductor` 等）に再定義**（tmux の「/ 禁止」制約で生まれた `vp-...-echoes` 形は不要に。env 変数名は互換のため維持）。
- **Windows unlock**: tmux が Windows lane の最大障壁だった。可搬点は LOGIN_SHELL 解決（git-bash）1 点に集約 — この再設計がそのまま Windows lane 戦略。

### 13.3 dedup 確定（Explore 裏取り、2026-07-04）

- DB-LOCK abort（`server.rs:120→137→144`）は conductor spawn（`server.rs:206`）より**上流** → 重複 SP は lane に触る前に死ぬ。adopt は tmux 層の二次防御にすぎず、tmux 撤去と同時に無意味化。**代替 dedup 不要**。
- 残る window = DB-less SP（LOCK 以外の DB 失敗で継続）のみ。だが tmux 撤去後は重複 SP が衝突する共有資源（tmux session）自体が無く、各自の PtySlot を持って並走するだけ（2026-06-30 バグの同形再発は構造的に不可能）。受容。
- `restart_lane` は元々 adopt を通らない（`lanes_state.rs:727` → spawn_with_fallback 直）。

### 13.4 新規実装（ここだけ新コード）

1. **`lane_capture`**: `read_pane` MCP / capture は実利用機能（conductor が performer console を読む）。per-lane Term grid（TermAttach）を全 lane に張り、grid→text で置換。tmux 撤去で唯一「消すと機能が失われる」箇所。
2. **resume 配線**: spawn/restart 時に (project, lane) → cc_session id を読み command に埋める。`VP_FRESH` env → spawn パラメータ化。
3. **legacy stand 吸収**: DB descriptor の `stand="tmux"/"hd"` → 床 shell に graceful mapping（warn log）。

### 13.4b mise 境界（user 決定 2026-07-04: 「project の task runner としては可、vp 自体は依存しない」）

- **product runtime は mise-free**（PR2 で達成）: stand script 層 / `stands_list` の
  `mise tasks ls` scan / builder の mise degraded fallback が全て消え、 product code に
  mise を exec する箇所はゼロ。 lane の runtime 依存 = login shell + claude のみ。
- `spawn_env` の mise-shims PATH 追加は**許容であって依存ではない**（mise 不在でも全機能動作）。
- repo の dev tooling（`.mise/tasks` + mise.toml toolchain pin）は maintainer 専用 = 「1 project
  の task runner」として存続。 script 化は user 価値ゼロのため非対象（やるなら別 chore）。
- 基準の一般化: **「user のマシンで要求されるか」が product 依存の定義**（CLAUDE.md 依存境界に明文化）。

### 13.5 実機確認項目（PR2 後の検証フェーズへ）

- extended-keys（Shift+Enter）/ truecolor が tmux shim 無しで xterm.js↔claude 間で成立するか（対処点は vp-app の xterm.js 設定側 = 端点交渉）
- initial_input の type-ahead（重い rc でも PTY line discipline がバッファするはず）
- 2-phase nudge → 1 write に畳めるか（§12）
- 移行 UX: 旧 tmux session は orphan 生存 → 会話は `--resume` で新 lane に継続、orphan は手動掃除（brew canonical は release cut まで無傷）

### 13.6 実機検証結果（2026-07-04、dev :32100、brew 終始無傷）

**✅ 検証済み:**
1. **spawn**: `Lane spawned: program=/bin/zsh args=["-l"]` — 床 + claude 注入、mise/tmux 無し
2. **`--resume` 会話継続 ×3**: 旧 tmux 世代 → 新 PtySlot lane、SP kill → 再起動、の各遷移で
   conductor の会話が継続（「プロセスは死ぬがコンテキストは蘇る」実証）。旧世代からの移行も
   cc_session 経由で無縫合
3. **nudge submit（tmux 無し）**: conductor claude（`✶ Synthesizing…` = 送信成立）+
   performer 床 shell（`command not found` = text+Enter 実行の直接証拠）の両方で 2-phase 成立
4. **lane_capture**: grid render 成立。発見 → 修正 2 件: wide-char spacer 混入（CJK 1 文字ごと空白）、
   TermAttach 初期 dims 80x24 ≠ PtySlot 120x48（headless capture が再 wrap で崩れ）
5. **flow handoff 全通**: performer 作成（worktree）+ wire + nudge、delete 経路も完走
6. **⚠️ mise trust footgun（発見 → env-only 修正）**: 床 = login shell 化で user rc の mise activate
   が新 worktree の未 trust config に interactive prompt → 床を塞ぐ。`MISE_TRUSTED_CONFIG_PATHS` に
   lane cwd を注入して抑止（mise 不 exec = 依存境界維持）。`echoes-act1-primary-design` の予見が的中
7. **CJK**: 配送・console 描画とも無破損

### 13.6b brew 本番 dogfood + GUI 検証（2026-07-04、:32000 namespace で新コード実行）

release cut 前に **brew namespace(:32000) を新 binary で起動**（launchd を bootout → `~/.cargo/bin/vp daemon start`）して実プロジェクト 5 本で dogfood。

- ✅ **全 conductor が PtySlot 直ホストで起動**（`program=/bin/zsh args=["-l"]`）、`tmux -L vp` は「no server running」= 本番で tmux ゼロ。実 conductor の会話は `--resume` で無縫合継続（handoff 元セッションが履歴ごと復活）
- ✅ **truecolor 復活**（GUI 実機）: 旧 echoes は tmux の `Tc` override で truecolor を交渉していた → tmux 撤去で 256 色退行。**PtySlot が端点として `COLORTERM=truecolor` を宣言**する fix（`daemon/pty_slot.rs`）で解消。全 conductor claude env に注入確認 + vp-app xterm.js で色描画確認
- ✅ **CJK 完璧**（GUI）: 日本語が `_` 化も spacer 混入もなく描画
- ⏳ **Shift+Enter**: xterm.js に custom handler なし（copy/paste のみ）。daemon 側でなく vp-app 端点の話で、tmux 撤去の回帰ではない。GUI 全体は良好、必要なら vp-app に key handler 追加（follow-up）
- **中間状態の注意**: shell の `vp`（.app symlink）は旧版のまま → CLI は `~/.cargo/bin/vp` を使う。release cut で解消

### 13.6c deliver_nudge 1-write 畳み込み（2026-07-04、release 前 polish で決着）

§12/§13.5 の宿題を empirical に決着。 brew :32000（新 binary）で throwaway echoes performer に
`text + \r` の **1 回 write** で nudge → claude が submit（`⏺ Calling… Synthesizing…`）を確認。
tmux 撤去後は PtySlot が claude の PTY master を直接持ち paste-wrap する主体が居ないため、
2-phase（text → 50ms → Enter）は不要だった。 1 write に畳み → レビュー B5 の「50ms 窓での
並行 nudge / user 入力との interleave」も構造的に消滅、 write は `write_to_lane` 1 回のみ。

**既知挙動（PR2 起因でない）:** daemon は SP 死亡を検知すると `run_health_monitor` が
30s × 2-strike（= 60s）で auto-respawn する（crash recovery、仕様 doc 15 §3.1）。
ただし *意図的な* `vp sp stop` も crash と同一視されて respawn される（在/不在の binary しか
持たず「user 意図 = Stopped」を表現できないため）— これが本当の gap で、解は Stage B の
`SpDesiredState` enum + supervisor（doc 15 §5、未着手）。
（注: 旧版はここを「auto-respawn しない」と誤記していた。health monitor の respawn は
#615 以前から存在するため、doc 15 と実装が正。2026-07-04 PR3 spec review で訂正。）

### 13.7b dev tooling footgun 修正（release 前 polish）

- `.mise/tasks/daemon/stop` の `pgrep -f 'vp sp start'` は **profile-blind**（dev で回すと brew SP も
  巻き込んで kill）。 各 SP の `ps eww` env の `VP_PROFILE` を現プロファイルと照合して対象を絞る。
  tmux 撤去で「SP kill = lane claude kill」になったため profile 跨ぎ誤爆の被害が大きくなった対策。

### 13.7 Follow-up ideas（PR3 候補、本 PR 非対象）

1. **SP spawn の CPU コアベース cap**（user mito 発案 2026-07-04、creo `mem_1CcgqCxDrcNfuhLZbJ9vcS`）:
   PR2 で lane claude = SP の子になったため、同時 spawn が CPU を圧迫すると claude 群の起動が
   団子になる。`start_process`（全 trigger の sink、doc 15 §2）を `cores − 2`（floor 1）の
   semaphore でゲートし、一度に走る `vp sp start` を平滑化する（**semantics A** = 総稼働数は
   縛らず spawn 同時数だけ制限、Workflow の `min(16, cores-2)` と同発想）。
   → **実装済み**（`spawn_semaphore` / `spawn_cap()`、2026-07-04）。
2. ~~**SP auto-respawn**~~: 上の「既知挙動」の訂正どおり **respawn は既に存在**する
   （`run_health_monitor`、doc 15 §3.1）。残るのは *意図的 stop* を respawn しない区別で、
   それは doc 15 §5 の `SpDesiredState`（Stage B）の領分。この follow-up 項目は誤認だった。
