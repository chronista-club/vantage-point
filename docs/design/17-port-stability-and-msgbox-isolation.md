# 17. Port 安定化 + Msgbox の project 跨ぎ汚染遮断

> **改訂 (2026-05-21)**: 本 doc の **Msgbox 跨ぎ汚染遮断** 部分 (= Whitesnake msgbox の project-keyed 化 / `restore_pending` の project guard / `normalize_from` 汚染) は、 旧 msgbox を全廃した **wiremsg 再設計 (R1〜R6、 PR #406〜#420)** で構造的に置き換わった (wiremsg は per-agent cursor accumulation で msgbox table を持たない)。 一方、 **port 安定化** 部分 (slug-keyed DB ディレクトリ / port reshuffle 対策) は wiremsg と直交しており現行有効。 本 doc は VP-165 時点の設計の historical + 一部現行 reference。

> **対象 Issue**: [VP-165](https://linear.app/chronista/issue/VP-165) — ポート割当が project リスト変更で不安定 + Whitesnake が port-keyed → 永続 msg がプロジェクト跨ぎで継承される / from 汚染
> **親 Epic**: [VP-156](https://linear.app/chronista/issue/VP-156) — Msgbox routing 統一 + 永続化 first-class
> **関連設計**: [14-wire-address-v3.md](14-wire-address-v3.md)（`normalize_from` / address syntax）/ [16-worker-lane-msgbox-recv.md](16-worker-lane-msgbox-recv.md)（決定E の `from` 修正と射程が重なる）/ [15-auto-spawn-triggers.md](15-auto-spawn-triggers.md)（SP spawn 経路 / VP-155）/ [01-architecture.md](01-architecture.md)（Reconciliation / port scheme）
> **Status**: ✅ **Implemented**（2026-05-12、PR-1〜PR-5b/PR-6 全 land。詳細は §Implementation 参照）。残り任意 follow-up は §残り 参照。`port_layout` 階層型 / TheWorld-as-reverse-proxy は本設計のスコープ外＝別 arc

## Abstract

dogfood（2026-05-11、VP-163 後）で「vantage-point conductor の `agent#conductor` を `msg_recv` したら、(1) chore worker からの ping が `from=agent@fleetstage` で届く、(2) 過去の test msg `b774c741` が `from=agent@creo-ui` に化けて再浮上」が観測された。同 session 内で `msg_directory` を 2 回叩いたら全 project の port が一斉シフトしていた（新規 project `nexus` が config に追加され、index ベースの port 割当が雪崩れた）。

### 問題の本質を一行に

VP-165 の全症状は **「port が安定 ID じゃないのに、複数箇所が port を ID として使ってる」** に還元される。そして port が不安定な理由はただ一つ —— **`port = f(config リストの位置)`**（`find_project_index()` → `33000 + idx`）。config に project を 1 つ足すと全 index がシフトし、全 SP の port が雪崩れる。これ以外の症状は全部その下流:

| | 症状の源 | 修正方向 | 本設計の決定 |
| -- | -- | -- | -- |
| **(A)** | worker の `vp mcp` が SP port を **stale な `VP_PROCESS_PORT` env** から取る → reshuffle 後は別 project の SP に msg を投げ、その SP の `local_project` で `from` がスタンプされる | worker は cwd から「自分は project X の worker」を確実に知れる → **毎回 discovery（= TheWorld）で X の SP を引き直す**。env は project_dir 照合付きの fast path に格下げ | 決定A |
| **(B)** | `Whitesnake::file_backed_for_port(port)` が `discs/{port}/` でディレクトリを切る → port を継いだ別 project が前 project の `msg/*` DISC を読む | Whitesnake を **project-keyed**（`discs/p_{slug}/`）に。port が変わっても永続データは正しい project に紐づく | 決定B |

> **後続改訂 note**: 決定B で導入した `discs/p_{slug}/` レイアウトは、 その後 **VP-182 (PR #367)** で SP 専用 DB ディレクトリ `db/sp_{slug}/` に再編された（surrealkv の single-writer 排他ロック対策で World daemon の `db/world/` と物理分離する必要が生じたため。 詳細は [doc 19 §6](19-msgbox-whitesnake-primary.md)）。 さらに **VP-188 (PR #371)** で projects の SSOT が embedded DB から `projects.kdl` に移行した。 本 doc の `discs/p_{slug}/` 表記は VP-165 時点の設計であり、 現行コードでは `db/sp_{slug}/` を参照すること。
| **(C)** | SP 起動 port = `find_project_index()` ベース（位置依存）。しかも実 spawn 主体（TheWorld の `start_process`）は `vp sp start` を `-p` 無しで起動し、子が選んだ port を `wait_for_process_port` で**後追い discover** している | **TheWorld が port allocation の唯一の authority**になる。`start_process` が `Config::sp_port_for_project(name)`（= 既存 `ensure_slot` 機構 + `port` override）で port を決め、`-p <port>` で spawn、`/api/health` を直 poll。位置ベース経路（`port_for_configured` / `wait_for_process_port` の range scan）は削除。外部プロセスが port を握ってたら `start_process` 内で次の空き slot に 1 回きり退避 + 永続 | 決定C |
| **(D)** | `restore_pending` が DISC を無条件で `router_tx.send` → 異 project 宛/発の msg も再投入 → `normalize_from` で `from` 汚染が拡大 | restore 時に「自 project 宛 or 自 project 発 or bare」のみ復元。それ以外は skip + warn | 決定D |
| (ε) | LAN federation の `AddressBook.record_sp_port(host, project, port)` も project ごとに port をキャッシュ（cross-machine 版の (A)） | **本設計スコープ外**（VP-154/161 系の別 issue）。ただし「TheWorld を machine の front door にする」arc（§将来拡張）に進めば (ε) は**消滅する**（remote が SP port を一切 cache しなくなる）。メモのみ | — |

(C) が landed すれば reshuffle 自体が止まるので (A)(B)(D) は「外部衝突 auto-reassign が走った時 / migration 残骸 / 手動編集」に対する安全弁になる。だが (A)(B)(D) はいずれも *正しくする*だけの小修正（元々 (A) の env-cache は永久 snapshot を信じてたバグ、(B) の port-keying は workaround）なので、4 つ全部入れる。

## 現状 — コード上の事実

### (A) worker MCP の SP port 解決

`resolve_process_port`（`mcp.rs:2924`）の優先度: ①明示 port 引数（この経路では未使用）→ ②環境変数 `VP_PROCESS_PORT` → ③`find_for_cwd()`（TheWorld API 経由）→ ④フォールバック 33000。

`VP_PROCESS_PORT` は `create_tmux_session()`（`commands/start.rs:370`）が tmux セッション作成時に「その時点の親 SP port」を注入する。ccws worker の tmux セッションは worker 作成時に作られ、env はその時の親 port を焼き込んだまま（TUI reconnect 時のみ `set-environment` で上書き — `start.rs:567`）。port が reshuffle されても worker の `VP_PROCESS_PORT` は古いまま → 古い port を*別の project の SP* が掴んでたら（reshuffle で fleetstage が 33000 に来た等）、worker の `msg_send to=agent@vantage-point` は fleetstage の SP に届く。fleetstage の `routing_loop`: `parse_address` → not local（`local_project="fleetstage"` ≠ `"vantage-point"`）→ remote forward → `normalize_from("agent")` = **`agent@fleetstage`** → TheWorld registry で `agent@vantage-point` を lookup → http_forward → vantage-point が `from=agent@fleetstage` で受信。→ dogfood 症状 (1)。

ccws worker dir（`~/.local/share/ccws/<parent>-<name>`）は登録 project の path 配下ではないので `find_for_cwd()` は None（フォールバック 33000 行き）→ worker にとって `VP_PROCESS_PORT` は事実上 mandatory で、stale-prone。`vp mcp` には既に「自分が worker か」を cwd から判定する経路がある（`self_register_if_worker()` / `resolve_parent_project()` が `~/.local/share/ccws/<parent>-<name>` を parse して `<parent>` を `discovery::find_by_project_blocking()` で引く）。**この parent→discovery 経路を port 解決でも使えば stale env を踏まない**。

### (B) Whitesnake port-keyed

`whitesnake.rs:415 file_backed_for_port(port)` → `discs/{port}/`。呼び出し: `server.rs:41`（SP 本体、Msgbox persistence）/ `server.rs:794`（`world_whitesnake`）。doc comment は「Process は port で一意なので」と言うが、**本当の不変条件は「1 project あたり同時 1 SP」**であって port ではない。port は reshuffle で動く不安定 ID。port を継いだ別 project の SP が `restore_pending`（`msgbox.rs:873`）で前 project の `msgbox` namespace（`msg/{id}` 群）を読む → dogfood 症状 (2)（`b774c741` の再浮上、しかも `normalize_from` で `from=agent@creo-ui` に化けた）。

### (C) port allocation — 位置ベース、しかも spawn 主体は port を「後追い discover」

実 SP 起動 port は `commands/sp.rs:258 resolve_port()` → `find_project_index(normalized)` → `resolve::port_for_configured(idx, config)`（`resolve.rs:207`）→ `PORT_RANGE_START + idx`（占有時は `find_available_port()` で flat range スキャン fallback）。`find_project_index` は `projects` Vec の position なので config の並び順変更で全 index シフト。

そして spawn の流れ: `vp app start` / vp-app accordion / `/api/world/start_process` HTTP は全部 `ProcessManagerCapability::start_process`（`process_manager_capability.rs:650`、= TheWorld 内）に集約される（VP-155 audit 参照）。だが `start_process` は `vp sp start -C <path>` を **`-p` 無しで** spawn し、`wait_for_process_port`（`:1056`、`PORT_RANGE` を range scan）で「子が何 port を選んだか」を後追い discover している。つまり **spawn の sink は TheWorld に集約済みだが、port 決定は不安定な子に丸投げ**。

一方、`port_layout.rs`（`PortLayout`）/ `ProjectConfig.slot: Option<u16>`（`config.rs:159`）/ `ProjectConfig.port: Option<u16>`（明示 override）/ `Config::ensure_slot()` / `next_free_slot()` / `resolve_slot_by_name()`（`config.rs:277-340`）という **deterministic slot 割当インフラは既に存在する**。用途は `vp port slot assign/unassign` という手動 CLI（`commands/port.rs`）専用で SP 起動経路に未配線。`port_layout` 自体は `project_slot_size=100`（SP=33000/33100/…、territory ごと 100 port 確保し lane/role サブ port を持つ）の階層型だが、これは `vp ps` / discovery の flat-range（`33000..=33024`）前提と非互換 → 階層型のフル adoption は別 epic。本設計は **`slot` を「flat な安定 index」として使う**（`port = PORT_RANGE_START + slot`）。

### (D) restore_pending の guard 不在

`restore_pending()`（`msgbox.rs:873`）は `ws.list_by_prefix(MAILBOX_NAMESPACE, MAILBOX_KEY_PREFIX)` の全 DISC を `is_expired()` チェック後に問答無用で `router_tx.send(msg)`。`msg.to`/`msg.from` が「自 project と無関係」でも `routing_loop` に流す → (B) で別 project の DISC を掴んだ場合、ここで汚染が拡散する。

## Target Model

### 決定A: worker MCP の SP port を「parent project → discovery 再解決」に統一、`VP_PROCESS_PORT` は格下げ

`resolve_process_port` の優先度を変更:

```
1. 明示的なポート引数（変更なし）
2. cwd 判定:
   - cwd が ~/.local/share/ccws/<parent>-<name> → worker。<parent> を discovery::find_by_project() で引く（live port）
   - cwd が登録 project の path（配下）→ conductor。その project を discovery::find_by_project() で引く（live port）
3. VP_PROCESS_PORT env（ヒント）— ただし「その port の SP の /api/health の project_dir が解決した parent と一致するか」を検証（is_sp_for_project_responding と同型）。不一致なら無視
4. find_for_cwd()（従来 fallback）
5. フォールバック 33000
```

- 肝: **2 を 3 より優先**。worker は「自分が誰の worker か」を cwd から確実に知れる → env snapshot に頼らず discovery（reconciliation の真実源 = TheWorld）で live port を引く。
- env を完全に消さない理由: discovery 一時障害時の fast path。ただし **必ず project_dir 照合してから使う**。
- QUIC channel reset 時（`mcp.rs:1019` 付近に既存の再解決ロジックあり）も同じ parent→discovery 経路に統一。
- `msg_send` の `from`: worker context なら bare `"agent"` ではなく `"agent@<parent>/<worker-name>"`（doc 16 決定E と同じ。そちらの PR で実装されるなら本設計は参照のみ）。conductor context は従来通り bare → remote forward 時 `normalize_from` で `agent@<parent>`。

### 決定B: Whitesnake を project-keyed に（全 namespace）

`file_backed_for_port(port)` → `file_backed_for_project(slug)` = `discs/p_{slug}/`（`p_` prefix で旧 numeric dir と区別）。key は project の slug（`ProjectConfig.name` を `[a-zA-Z0-9_-]` 以外を `_` に置換、空/重複時は path hash fallback）。全 namespace（`msgbox` / sessions / 他）を `discs/p_{slug}/` 配下に。`world_whitesnake`（`server.rs:794`）は `discs/world/` 固定（port 32000 由来をやめる）。

- 「1 project 同時 1 SP」前提: reshuffle/auto-reassign で transient に同 project の SP が 2 つ走る病的状態でも両者が `discs/p_{slug}/` を共有（port-keyed だと別 dir で逆に状態が分裂する）。同 project 2 SP 防止は既存 guard（`is_sp_for_project_responding`）に委ねる。
- Migration: 旧 `discs/{port}/` dir は孤児化。data はほぼ msgbox msg（TTL 48h）+ session 状態なので、移行初回に session 履歴が一瞬空に見える程度で実害軽微。best-effort で「起動時に旧 `discs/{自分の今の port}/` が存在し新 dir が空なら mv」する一回きり migration は任意（PR で判断）。

### 決定C: TheWorld が port allocation の唯一の authority になる

#### `Config::sp_port_for_project(&mut self, name) -> Result<u16>` を 1 本

`projects[].port` 明示 override があればそれ。無ければ `ensure_slot(name, None)` → `PORT_RANGE_START + slot` を返し、**新規割当なら `p.slot = Some(s)` を書いて config を save**（= 既存 `ensure_slot`/`next_free_slot`/`resolve_slot_by_name` を*そのまま再利用*、足すのは `port` override チェックと `+ PORT_RANGE_START` の薄いラッパだけ）。`max_projects` はこの flat scheme では `PORT_RANGE_END - PORT_RANGE_START + 1 = 25`（`port_layout.rs` の default 20 ではなく flat range 由来。`ports.max_projects` override も尊重）。

#### TheWorld が呼ぶ — `vp sp start` は TheWorld に聞く

- `ProcessManagerCapability::start_process(name)`: 既存の「name→path 解決」「dedup check（path 一致の既存 SP 発見なら spawn skip + re-register）」の直後に `port = config.sp_port_for_project(name)?` → `vp sp start -C <path> -p <port>` で spawn → `wait_for_health(port, path)`（= `/api/health` を 800ms→500ms backoff/10s で poll、`project_dir` 一致を確認。`wait_for_process_port` の range scan を**置換**）。
- `commands/sp.rs::resolve_port`（人間が手動で `vp sp start -C <path>` を `-p` 無しで叩いた時）: `ensure_daemon_running()` で TheWorld 起動 → `GET /api/world/port_for?project=<resolved>` で TheWorld に聞く（TheWorld 側 handler が `sp_port_for_project` を呼んで割当・永続・返答）。TheWorld 到達不可ならエラー（`vp sp start` 単独起動は TheWorld 無しでは無意味 = SoT）。→ `find_project_index → port_for_configured` の位置ベース経路を**削除**。
- `spawn_sp_detached(project_dir, port)`: 全 caller（`tui/app.rs` / `restart.rs` / `restart_all.rs` / `ensure_sp_running`）で `port = None` に統一し、port 解決は `vp sp start` 側（= TheWorld に聞く）に委ねる → `tui/app.rs` の `port_for_configured` 呼び出しも**削除**。
- `/api/world/start_process` HTTP（vp-app 等）は変更不要（中で `start_process` を呼ぶだけ。port は内部解決）。

#### 外部プロセスが assigned port を握ってる場合（`start_process` 内）

`wait_for_health(port, path)` の結果で分岐:
1. health の `project_dir` == path → 正常（自分の SP が立った）。
2. health の `project_dir` == *別 project* / または timeout だが `is_server_responding(port)` が true（非 VP プロセス占有）→ **衝突**。`projects[].port` 明示 override が未設定なら → `ensure_slot(name, <occupied slot を skip した次の空き>)` で**次の空き slot に再割当 + config 永続化** → 新 port で再 spawn。`tracing::warn!("project {name}: slot {old} の port {old_port} が外部に占有 → slot {new} (port {new_port}) に再割当して config 永続化")`。
3. timeout かつ `is_server_responding(port)` も false（SP が単に crash）→ エラー/retry。

→ これは「reshuffle」ではない: config 編集による *cascading shift* ではなく、外部衝突という *実イベント* に対する *その 1 project だけ 1 回きり* の bounded な移動。移動後は固定。サイレント別 port フォールバック（現状 = `find_available_port`）は SP が想定外 port に行く＝汚染・reshuffle の二次原因なので**やらない**。25 slot 全滅（まず起きない）→ エラー。

#### 階層型 `PortLayout` は触らない

`port_layout.rs` の `project_slot_size=100` / `lane_base`/`port(slot,lane,role)` 系は本設計のスコープ外（`vp ps`/`discovery::scan_instances`/`is_port_available`/`find_available_port` の flat-range 前提を全書き換えする必要があり VP-165 を超える。lane ごとの dev_server/canvas port は別 goal）。`slot` フィールドは flat scheme と階層型で共用できるので、将来階層型に移行する際も slot 永続はそのまま生きる（`mem_1CaKCbNE24KTQDuf9x4Eim`）。

### 決定D: restore_pending に project 境界 guard

`restore_pending()`（`msgbox.rs:873`）に「自 project と関係ある msg だけ復元」を追加。Router は `local_project`（`RemoteRoutingClient` 経由 or Router 構築時に渡す）を知っているとする:

```rust
for disc in discs {
    let msg = disc.extract::<Message>()?;
    if msg.is_expired() { /* 削除 */ continue; }
    // VP-165 (D): 異 project の msg は復元しない（port 継承時の汚染遮断）
    if !is_relevant_to_local_project(&msg, local_project) {
        tracing::warn!("Msgbox: restore skip — 異 project の msg id={} to={} from={}", msg.id, msg.to, msg.from);
        // 任意: ws.remove(...) で物理削除（孤児 DISC を残さない）。まずは skip + warn から
        continue;
    }
    self.router_tx.send(msg).await?;
    restored += 1;
}
```

`is_relevant_to_local_project`: `parse_address(&msg.to)` の project が `local_project`、または `parse_address(&msg.from)` の project が `local_project`、または bare（project 部なし、= ローカル配送の正常ケース）。

> **Note**: (D) 単独でも漏れの実害はほぼ止まる —— port 33005 を継いだ creo-ui の SP が `discs/33005/`（creo-memories の旧 msg）を読んでも、(D) が `to`/`from` ≠ `creo-ui` を全部 skip する。なので **(B) は「load-bearing な fix」ではなく「正しい構造への整理」**（GC が他 project の msg を触らない、bare msg の corner case を塞ぐ、各 project の discs が混ざらない）。(B) を削って (D) + 「永続 msg は非 bare `to`/`from` 必須」ルールだけにする選択肢もあるが、(B) は 1 関数差し替え ~10 行で*正しいモデル*になるので残す。VP-164（forward 済み再送）にも (D) guard が部分的に効く（`to` が異 project なら skip）が、VP-164 の本丸（ack-back 機構）は別 issue。

## Implementation（段階）— PR 分割

「汚染遮断（config いじらず効く）」→「reshuffle 停止」→「掃除」の順。

| PR | 内容 | 状態 |
| -- | -- | -- |
| **PR-1** ([#341](https://github.com/chronista-club/vantage-point/pull/341)) | 設計 doc（本ファイル）のみ。VP-165 の「設計の受け皿」 | ✅ landed `fff270e` |
| **PR-1b** ([#342](https://github.com/chronista-club/vantage-point/pull/342)) | 死コード削除: `commands/start.rs` の legacy `vp start` 経路（`execute` / `StartOptions` / `ResolvedProject` / `resolve_project` / `resolve_from_dir` / `resolve_from_target` / `resolve_port` / `run_headless`/`run_browser`/`run_gui`/`run_tui_mode`/`run_tui` / `ensure_sp_running` 等）を削除（caller ゼロを build で検証。`spawn_sp_detached`/`create_tmux_session`/`try_create_tmux_claude`/`collect_mise_env`/`wait_for_ready`/`is_server_responding`/`is_sp_for_project_responding` は残す） | ✅ landed `97eaf76` (-630 line) |
| **PR-2** ([#343](https://github.com/chronista-club/vantage-point/pull/343)) | **(B) 配線**: `Whitesnake::file_backed_for_project(slug)` + `discs/world/`、`server.rs:41`/`:794` の caller 切替、`file_backed_for_port` 削除。`resolve::project_slug` 追加 + テスト 3 件 | ✅ landed `2a0ded7` |
| **PR-3** ([#344](https://github.com/chronista-club/vantage-point/pull/344)) | **(D) guard**: `restore_pending` に project 境界 guard（skip + warn）。`msg_is_foreign_to_local` helper + テスト 2 件（判定マトリクス + skip 動作） | ✅ landed `efb7855` |
| **PR-4** ([#345](https://github.com/chronista-club/vantage-point/pull/345)) | **(A) 配線**: `resolve_process_port` を parent→discovery 優先に。`worker_parent_path` helper + テスト 3 件。`VP_PROCESS_PORT` env は fallback に格下げ | ✅ landed `531165a` |
| **PR-5** ([#346](https://github.com/chronista-club/vantage-point/pull/346)) | **(C) 配線 第一段**: `Config::resolve_sp_port` 追加（`port` override → `ensure_slot` → `PORT_RANGE_START + slot`）。`resolve::sp_port_for_project` が load→resolve→save。`port_for_configured` を slot 経由に rewrite + 旧 index ベース実装削除 + テスト | ✅ landed `e2eaa1f` |
| **PR-5b/PR-6** ([#347](https://github.com/chronista-club/vantage-point/pull/347)) | **(C) 配線 完成形**: `start_process` (TheWorld) が `sp_port_for_project` で port 事前解決 → `vp sp start -p <port>` で明示渡し → `wait_for_health(port, &path)` で `/api/health` の `project_dir` 一致確認（旧 `wait_for_process_port` range scan を replace）。`HealthCheckResult` enum で外部衝突を区別 → `auto_reassign_slot` で 1 回きり別 slot に退避 + config 永続 → retry。`/api/world/port_for?project=<name>` endpoint 新設 | ✅ landed `3168c70` |

→ PR-2/3/4（汚染遮断、config いじらず効く）が先、PR-5（(C)、影響広い）が後。PR-5 で reshuffle が止まり (A)(B)(D) は安全弁になるが、外部衝突 auto-reassign の存在を考えると 4 つ全部入れる価値あり。(ε) LAN `AddressBook` の port キャッシュ無効化は別 issue（VP-154/161 系）。

### 残り（任意 follow-up — VP-165 close blocker ではない）

PR-5b/PR-6 (#347) で doc 17 §決定 (A)(B)(C)(D) は全 land。残るのは「TheWorld single authority」モデルの周辺整備:

- **`commands/sp.rs::resolve_port` を `/api/world/port_for` 経由に切替**: 現状は in-process で `crate::resolve::sp_port_for_project` を直接呼ぶ。TheWorld 起動 → endpoint 問い合わせの方が「single authority」原則の clean な物理表現（`vp sp start` を `-p` 無しで CLI から叩いた時の path）
- **QUIC reset 時の port 再解決**: `mcp.rs` `auto_start` 経路の `find_for_cwd` も決定A 同様に worker 対応（cwd → parent project → discovery）に
- **`vp ps` / tray の `scan_instances`**: `discovery::list`（TheWorld query）に置換 → range scan ループ削除。 ただし「TheWorld 未起動時は何も見えなくなる」 trade-off がある（現状は scan で SP が見える）
- **`PORT_RANGE_*` の役割明文化** (`cli.rs` doc): 「slot 上限定数 + Pull-scan 範囲」とコメント
- **(ε) LAN `AddressBook` の port キャッシュ無効化**: VP-154/161 federation epic の射程。`peer:32000` 経由（β、§将来拡張 step 2）に進めば自然に消滅する

## 将来拡張 — TheWorld を machine の単一 front door にする arc（本設計はその step 1）

(C) で「TheWorld が port allocation の唯一の authority」になるのは、より大きい arc の **step 1**:

- **step 2（別 arc / 別 doc）**: クロスマシン通信を **`peer:32000` 経由**にする。今は mDNS が各 SP を advertise（`sp-X-host`）し、remote が target SP の port を解決して直結（= (ε) のクロスマシン版 + LAN に N port 露出 + mDNS が N record/machine → A record collision 苦闘の源、VP-154 PR-3.6）。代わりに mDNS は `_vp-world._tcp` を **1 machine 1 record** → remote は `host:32000` に投げる → 相手の TheWorld が `running_processes` map で target SP を引いて localhost forward。**HTTP all the way**（既存の msgbox `http_forward` chain にリンクが 1 本＝相手の TheWorld が増えるだけ。QUIC を WAN に出さない / QUIC relay 不要）。効果: firewall 1 port/machine / mDNS record 1/machine（collision 激減）/ auth 境界 1 箇所 / **remote が SP port を一切 cache しない → (ε) が消滅** / VP-154 federation epic と `lan.rs` の `AddressBook` を*痩せさせる*（`host → project → sp_port` の 3 段 map が `host → 到達可能か` だけに）。
- **step 3（さらに長期 / VP-102 Thin View 領域）**: TheWorld を *全 traffic* の reverse proxy に（`32000/p/<name>/...` で SP に proxy、SP は ephemeral / unix socket）。port 問題が*消滅*し bookmark/Canvas URL も完全安定。だが TheWorld が全 traffic SPOF / QUIC・WS proxy のコスト。本設計の (C)（TheWorld が port table を持つ）はこの未来の**前提条件**でもある（reverse proxy にするなら「TheWorld が path→port を管理」が要る。ephemeral port にするのも「`-p :0` で spawn → SP が QUIC self-register で port 報告 → table 更新」に切り替え、slot 永続をやめれば table が vestigial になり削除も簡単）。

→ step 2 の設計（β: `peer:32000` 経由、`/api/world/msgbox/forward` の API、auth/policy、VP-154 との合流、`AddressBook` の縮退）は creo の design-spark memory に切る（後で VP-154 epic 配下の issue に昇格できる種）。**本 doc には詰め込まない**（VP-165 がブロートしないように）。

## 設計判断ログ（議論で確定したもの）

| 判断 | 結論 | 理由 |
| -- | -- | -- |
| port allocation の authority | **TheWorld 1 箇所**（`start_process` が `sp_port_for_project` で決め `-p` で渡す。`vp sp start` 単独は TheWorld に聞く） | 不安定の根は「N 個の CLI プロセスが config index から各自 port を再計算」。authority を 1 つにすれば「port = authority の table の lookup」になる。spawn の sink は既に TheWorld（VP-155 audit）なので、port 決定もそこに寄せるのが最小。VP-155 Stage B の `SpSupervisor` はこの `start_process` の進化形なので前方互換 |
| port スキーム | **flat stable slot**（`port = PORT_RANGE_START + slot`、slot は config 永続）。階層型 `PortLayout` は触らない | `vp ps`/discovery/`is_port_available` の flat-range 前提を壊さず最小で reshuffle を止められる。既存 `ensure_slot`/`next_free_slot` をそのまま再利用（新規コード最小）。階層型（lane/role サブ port）は別 goal・別 epic |
| slot は「絶対」か「希望」か | **希望**: config 編集では動かないが、外部プロセスが port を握ってたら*その 1 project だけ 1 回きり*別 slot に退避 + 永続化 | 「予測可能性」と「現実の port 衝突への耐性」の両立。今の問題は cascading shift であって「たまに 1 回動く」ではない。territory 内自己衝突は構造上起きない（slot が一意）ので、起きるのは「入口 port を外部が握る」時だけ |
| 外部衝突時にサイレント別 port フォールバックするか | **しない**。auto-reassign（次の空き slot、永続）か、それも無理ならエラー | 無関係 port への fallback が「SP が想定外 port に行く → bookmark/Canvas URL/`from`/discovery cache 全部ズレる」元凶の一つ |
| Whitesnake の key | **project slug**（`discs/p_{slug}/`、全 namespace）。`world_whitesnake` は `discs/world/` | port は reshuffle で動く不安定 ID。本当の不変条件は「1 project 1 SP」。slug は人間が読めて debug しやすい（path hash より良い、空/重複時のみ hash fallback）。msgbox だけ分けるより一貫。ただし (D) があれば漏れの実害は止まるので (B) は構造整理の位置付け |
| restore_pending の guard | **project 境界 guard**（自 project 宛/発 or bare のみ復元、それ以外 skip + warn） | migration 残骸/手動編集/(B) の漏れの安全弁。VP-164 の一部にも効く |
| worker MCP の SP port 解決 | **parent project → discovery 再解決を `VP_PROCESS_PORT` env より優先**。env は project_dir 照合付き fast path | worker は cwd から「誰の worker か」を確実に知れる。discovery が reconciliation の真実源。env snapshot は stale-prone |
| クロスマシン通信 | **将来は `peer:32000`（相手の TheWorld）経由**（β）。本 doc では決定 C の step 1 のみ実装、step 2 は design-spark に切る | firewall 1 port / mDNS 1 record / auth 1 箇所 / (ε) 消滅 / HTTP all the way（新サブシステムじゃなく forward chain にリンク 1 本）/ local の「TheWorld が machine を持つ」決定の自然な延長 |

### 廃案（議論の過程で出たが採らなかったもの）

- **`Config::sp_port_for_project` を作らず `ensure_slot` を各 caller に直接インライン**: caller は `start_process` / `commands/sp.rs::resolve_port`(handler) / 他で複数。`port` override チェック + `+ PORT_RANGE_START` の重複を避けるため薄いラッパ 1 本に集約。
- **`port_layout` 階層型を VP-165 でフル adoption**: `vp ps`/`scan_instances`/`is_port_available`/`find_available_port` 全書き換えで VP-165 を超える mini-epic 化。→ flat stable slot、階層型は別 epic（`mem_1CaKCbNE24KTQDuf9x4Eim`）。
- **port = `33000 + hash(path) mod 25`（永続無し、純関数）**: hash collision が 10-15 project でほぼ確実（birthday）。linear probing で解決すると「probe 結果を永続」が要る → 結局 slot table。純 hash 単独は無理（ただし*初期割当の heuristic*としては「まず `hash mod 25` を試す」もアリ。構造の簡略化ではないが）。
- **SP は ephemeral port + Push-only discovery**: (C) が完全消滅するが Pull fallback（range scan の自律復帰）が死ぬ → resilience の belt-and-suspenders を失う。slot-table ~30 行を消す代わりに redirect endpoint + 弱った Pull で net deletion か微妙。step 3（reverse proxy）の一部としてなら検討余地。
- **TheWorld を全 traffic の reverse proxy（即座に）**: port 問題は消えるが TheWorld 全 traffic SPOF / QUIC・WS proxy コスト / VP-102 Thin View 領域の別 epic。本設計の (C) はその前提条件として先に入れる。
- **外部衝突時にサイレント別 port フォールバック（現状維持）**: SP が想定外 port に → 汚染・reshuffle の二次原因。→ auto-reassign（永続）かエラー。
- **msgbox namespace だけ project-keyed（他は port-keyed）**: 中途半端（session 状態等も分裂）。→ 全面。
- **`VP_PROCESS_PORT` 完全廃止**: discovery 一時障害時の fast path を失う。→ project_dir 照合付きヒントに格下げ。
- **(D) で異 project DISC 即物理削除**: 攻めすぎ（migration 中の誤判定で消す risk）。→ まず skip + warn。

## 実装時に詰める細部（小）

- slug 化関数: `ProjectConfig.name` → `[a-zA-Z0-9_-]` 以外を `_` に。空/重複時の fallback（path hash）。`config.rs` に `fn project_slug(name) -> String`。
- `Config::sp_port_for_project` が config を mutate（slot 永続）→ load→ensure→save の責務をどこに置くか（TheWorld の `start_process` 側 + `/port_for` handler 側で load→ensure→save。並行 SP 起動の config 同時 write race → flock or atomic write）。
- `wait_for_health(port, path)`: 既存 `wait_for_process_port` の poll パラメータ（800ms→500ms backoff/10s）を流用、scan の代わりに `http://[::1]:{port}/api/health` を直 GET して `project_dir` 照合。
- `start_process` の dedup check（`find_running_sp_at_path` の range scan）: stable port なら「assigned port を `is_sp_for_project_responding(port, path)` で先にチェック」が速い。range scan は fallback として残すか（range は 25 で cheap）— impl 時判断。
- `find_project_index`: 残る非 port 用途があるか確認（`resolve.rs:65` / `tui/app.rs` の用途は全部 port → name-based helper に移れば `find_project_index` 自体も削れる可能性）。
- `/api/world/port_for`: query は `?project=<name>` or `?path=<dir>`。後者なら `routes/world.rs` 側で path→name 解決。
- 外部衝突検出: `wait_for_health` timeout 後に `is_server_responding(port)` で「非 VP プロセスが居る」を判定。`is_sp_for_project_responding` は別 thread current_thread runtime の既存ヘルパー流用。
- auto-reassign 後、TheWorld registry / `vp ps` は次の reconciliation で新 port を拾う（既存 Push+Pull）。worker は (A) で discovery を引き直すので追従。

## Testing

- **ユニット**: `project_slug` の各種 name → 期待 slug + 重複/空 fallback / `Config::sp_port_for_project`（未割当 → 次の空き slot 割当 + save / 割当済み → そのまま / `port` override 優先 / project 追加で既存 slot 不変）/ `Whitesnake::file_backed_for_project`（slug → 期待 dir）/ `restore_pending` の project guard（異 project to/from は restore されない / bare は restore / 自 project 宛・発は restore、in-memory backend で）/ `resolve_process_port` の cwd 判定（ccws worker dir → parent 解決 / 登録 project path → conductor）。
- **統合**: assigned port を別プロセスが握ってる状況をテスト用 listener でシミュレート → `start_process` が auto-reassign + config 永続化 / SP restart → `discs/p_{slug}/` から msg restore + 旧 `discs/{port}/` は読まれない / `vp sp start -C <path>`（`-p` 無し）→ TheWorld 起動 + `/port_for` で port 取得。
- **dogfood**: project を 1 つ config に追加 → 既存 project の port / msg が壊れない（= VP-165 発見シナリオの逆再生）/ reshuffle が起きない状態で worker → conductor `msg_send` → `from` が `agent@<parent>/<worker>`（or `agent@<parent>`）/ 外部プログラムで `33000+slot` を塞いだ状態で SP 起動 → 次の空き slot に退避 + config 反映。

## 影響 / Migration

- **Whitesnake のディレクトリレイアウト変更**（`discs/{port}/` → `discs/p_{slug}/` + `discs/world/`）。旧 dir 孤児化。移行初回に session 履歴/未配信 msg が一瞬空に見える可能性（best-effort migration で緩和可）。`feedback`/docs 更新。
- **config に `slot` が書き込まれる**（既存 `slot` field を初回 SP 起動時に populate。`vp port slot assign` で手動割当済みなら尊重。`projects[].port` 明示 override が最優先）。
- **SP の port が「初回起動時の slot」で固定**: 既存環境では本変更後の初回起動で `next_free_slot`（≒ それまでの index 順）が割り当たり以降固定（「1 回だけ今の並び順で確定 → その後動かない」）。
- **`vp sp start -C <path>` の挙動が変わる**（`-p` 無しなら TheWorld を起動して `/port_for` で port を取る。TheWorld 到達不可ならエラー）。
- **`VP_PROCESS_PORT` env の意味が変わる**（mandatory な真実 → project_dir 照合付き fast path ヒント）。tmux への注入は残す。
- **`vp ps` の実装が変わる**（PR-6: port range scan → TheWorld query。TheWorld 未起動なら空を返す — 既に discovery モジュールがその挙動）。
- **外部プログラムが VP の port range を握ってた場合の挙動が変わる**（サイレント別 port → auto-reassign + 永続 or エラー）。これは改善。
- 関連 issue: VP-164（restart 重複再配信 — (D) guard が部分的に効くが ack-back は別）/ VP-166 doc 16 決定E（worker の `from` — (A) と重なる、どちらの PR が担うか impl 時調整）/ VP-147（per-lane msgbox、`from` 汚染の一因として言及）/ VP-156 epic / VP-155（SP auto-spawn trigger 集約 — (C) の port allocation はその `start_process`/`SpSupervisor` の領域、直交かつ前方互換）/ VP-154/161（federation — (ε) と §将来拡張 step 2）。

## 関連

- Linear: [VP-165](https://linear.app/chronista/issue/VP-165)（本設計の対象）/ [VP-156](https://linear.app/chronista/issue/VP-156) epic / [VP-164](https://linear.app/chronista/issue/VP-164) / [VP-163](https://linear.app/chronista/issue/VP-163)（発見元）/ [VP-147](https://linear.app/chronista/issue/VP-147) / [VP-155](https://linear.app/chronista/issue/VP-155)（SP spawn 経路）
- 設計: [14-wire-address-v3.md](14-wire-address-v3.md)（`normalize_from` / address）/ [16-worker-lane-msgbox-recv.md](16-worker-lane-msgbox-recv.md)（決定E の `from`）/ [15-auto-spawn-triggers.md](15-auto-spawn-triggers.md)（SP spawn）/ [01-architecture.md](01-architecture.md)（Reconciliation / port scheme）
- creo-memories: `mem_1Caw21f5K1Ha19DFDr5Evp`（本 SDG の記録、redesign 版で supersede）/ `mem_1CaKCbNE24KTQDuf9x4Eim`（VP Port Management Phase 1 — 階層型 PortLayout、本設計スコープ外だが slot インフラの出自）/ VP-163 milestone `mem_1Cavm9QRE3uNSDnP5XWYBL`（(A)(B) の発見記録）/ §将来拡張 step 2 の design-spark（クロスマシン front-door、本 doc 改訂と同時に作成）
- code: `crates/vantage-point/src/capability/whitesnake.rs`（`file_backed_for_port` → `file_backed_for_project`）/ `crates/vantage-point/src/capability/msgbox.rs`（`restore_pending`:873 / `routing_loop`）/ `crates/vantage-point/src/capability/msgbox_remote.rs`（`normalize_from`:521 / `RemoteRoutingClient.local_project` / `is_local`）/ `crates/vantage-point/src/process/server.rs`（`:41` `:74` `:794` Whitesnake/RemoteRoutingClient 注入）/ `crates/vantage-point/src/capability/process_manager_capability.rs`（`start_process`:650 / `wait_for_process_port`:1056 / `find_running_sp_at_path`）/ `crates/vantage-point/src/commands/sp.rs`（`resolve_port`:258）/ `crates/vantage-point/src/resolve.rs`（`port_for_configured`:207 / `find_available_port`）/ `crates/vantage-point/src/config.rs`（`ProjectConfig.slot`:159 / `ProjectConfig.port` / `ensure_slot` / `next_free_slot` / `find_project_index`:236）/ `crates/vantage-point/src/port_layout.rs`（階層型 — 触らない、参照のみ）/ `crates/vantage-point/src/mcp.rs`（`resolve_process_port`:2924 / `resolve_parent_project` / `self_register_if_worker`）/ `crates/vantage-point/src/discovery.rs`（`find_by_project` / `list`）/ `crates/vantage-point/src/process/routes/world.rs`（`/api/world/start_process` / 新 `/api/world/port_for`）/ `crates/vantage-point/src/cli.rs`（`scan_instances`:171 / `PORT_RANGE_*`）/ `crates/vantage-point/src/commands/start.rs`（legacy `execute` 経路削除 / `ensure_sp_running`/`spawn_sp_detached` は残す）/ `crates/vantage-point/src/commands/lan.rs`（`AddressBook.record_sp_port` — (ε) §将来拡張）
