> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# 16. worker-lane msgbox: recv 経路 + box 構造（per-stand）+ whoami

> **対象 Issue**: [VP-166](https://linear.app/chronista/issue/VP-166) — worker-lane msgbox の recv 経路を実装
> **親 Epic**: [VP-156](https://linear.app/chronista/issue/VP-156) — Msgbox routing 統一 + 永続化 first-class
> **関連設計**: [14-wire-address-v3.md](14-wire-address-v3.md)（v3.1 address syntax）/ [13-paisley-park-revival.md](13-paisley-park-revival.md)（`canvas#<lane>` box の consumer 側）/ [12-stand-architecture.md](12-stand-architecture.md) / [03-msgbox-vs-ccwire.md](../archive/03-msgbox-vs-ccwire.md) / [07-lane-as-process.md](07-lane-as-process.md)
> **改訂 (2026-05-21)**: 本 doc を superseded した doc 19 (Whitesnake-primary msgbox) 自体も、 その後の **wiremsg 再設計 (R1〜R6、 PR #406〜#420) で全廃**された。 現行の worker-lane 配送は wire accumulation の per-agent cursor (`wire_recv`) で実現される。 `msg_recv` / msgbox box / `agent#<lane>` 表記はいずれも historical。 本 doc は historical reference として残置。

> **Status**: **Superseded by [doc 19](19-msgbox-whitesnake-primary.md) (VP-169)** — `Router::boxes` / `register_lane` / `unregister_lane` / per-stand mpsc box は doc 19 epic (Phase 5 完了, commit `445190c`) で全廃済。 本 doc は historical reference として docs/design/ に残置する。
>
> **Superseded note**: 本 doc は `Router::boxes` + `register_lane` + per-stand mpsc box (= lane × stand の 2 軸 box key) を前提とする設計だった。 doc 19 (VP-169) で **mpsc substrate を完全廃止**し、 Whitesnake (SurrealDB embedded) を primary store に揃えた。 具体的には: ① per-lane 軸は HashMap key ではなく `msgs` table の DB row field (`to_actor` / `to_lane`) になり (doc 19 §4.5)、 ② `register_lane` / `unregister_lane` API は不要になって廃止 (doc 19 §4.5 副次効果)、 ③ worker-lane の recv 経路は consumer が自分の `WHERE to_lane=$mine` で LIVE SELECT を打つ形に置き換わった。 本 doc が解決しようとした「worker-lane msgbox が送れない・配送されない・受け取れない」 問題は、 box concept そのものの廃止によって root から解消された。 doc 19 epic は Phase 1 spike (SurrealDB LIVE Query feasibility) が PASSED したため全 Phase 完走した (= 本 doc 末尾の「spike NG なら本 doc 再活性化」 という分岐は発生しなかった)。 本 doc の決定 A〜E / PR-1b〜PR-6 は doc 19 §4 に吸収されている。

## Abstract

worker lane の Echoes（= ccws worker で動く Claude session）が **自分宛 msg を `msg_recv` で取れる**ようにする。あわせて、現状ほぼ機能していない per-lane msgbox の **アドレス syntax ⇄ box key ⇄ Handle lifecycle ⇄ recv 経路** の鎖を一本に通し、box を **lane × stand の 2 軸**で正式化する（`agent#<lane>` = その lane の Claude session 宛、`canvas#<lane>` = その lane の Canvas / PP 宛）。agent が自分の通信文脈（自分の正規アドレス）を introspect できるようにもする。

VP-147（per-lane msgbox）で「送る側 Done」と記録されていたが、実態は `routing_loop` に `msgbox_key(actor, lane)` の分岐があるだけで、**send→deliver→recv の鎖がどこも繋がっていない**（後述「現状」参照）。本設計はその鎖を端から端まで通すもの。

### 命名: `stands.rs` の `id` 体系を使う（`agent` / `canvas`、JoJo 名ではない）

msgbox actor 名 / box key は `stands.rs` の **`StandAlias.id`**（= 「コード内部の安定した機能名、愛称を変えても stands.rs だけ修正で済む」）を使う:
- coding-assistant msgbox: `agent`（= `ECHOES.id`。VP-157 で `register("agent")` 済み、本設計で worker にも延ばす）
- Canvas / PP msgbox: `canvas`（= `PAISLEY_PARK.id`。QUIC channel 名 `"canvas"` とも一致）

「Echoes」「Paisley Park」は `stand_name`（UI/CLI/ログ表示専用の JoJo 愛称）であって wire/code 層には出さない。これで「`agent` と `echoes` の 2 名で 1 箱」という混乱は **最初から起きない**（wire/code は `agent` 一本、`Echoes` は表示専用）。`agent`→`echoes` rename は**しない**、`stands.rs` も触らない、VP-157 とも整合、QUIC channel `process` / `canvas` も mDNS `world` も無傷。

（GE = `gold_experience@<project>` / HP = `hermit_purple@world` は現状 JoJo 名で advertise/register されてて `id`（`runner` / `external`）と乖離してるが、これは project/world scope の別件で本設計のスコープ外。)

## 現状 — worker-lane msgbox は 4+1 層で壊れている

dogfood（2026-05-11、VP-163 後）+ コード読みで判明。

### ① `list_lanes` が advertise するアドレスが `parse_address` で弾かれる

`list_lanes`（`mcp.rs:1308`）は `format!("{}.{}@{}", stand, lane_label, project)` で `echoes.chore@vantage-point` を生成。一方 `parse_address`（`msgbox_registry.rs`）は:

- `@` で割って `actor = "echoes.chore"`, `location = "vantage-point"`
- `validate_actor("echoes.chore")` → 許可文字は `[a-zA-Z0-9_-]` + `*` のみ → `.` で `InvalidActorChar` → **`Err`**

→ `msg_send to=echoes.chore@vantage-point` は `handle_msg_send` が受理（parse せず router_tx に積むだけ）→ `routing_loop` が `parse_address` → `Err` → `tracing::warn!("Router: address parse error ...")` → `continue` で **msg 破棄**。`msg_send` の `"Message sent"` 表示は嘘（parse は QUIC 応答後の非同期）。さらに JoJo 名（`echoes`/`paisley_park`）を使ってて `id` 体系（`agent`/`canvas`）とも乖離。本設計の決定B で `list_lanes` を正しい形（`agent@<project>[/<name>]` / `canvas@<project>[/<name>]`）に直す。

### ② worker box の actor 名が lead と不統一

lane lifecycle hook（`server.rs:1473` 付近、`spawn_msgbox_lane_lifecycle_hook`）は `Diff::Add` で:

```rust
msgbox_router.register_lane(&payload.stand, &path).await;  // payload.stand = "echoes"（JoJo 名！）, path = ["worker","chore"]
```

→ box key `echoes#worker/chore`（actor = **JoJo 名**、lane path に `worker/` prefix）。一方 lead は `agent#lead`（actor = `id`、VP-157）。「lead = `id`、worker = JoJo 名」でバラバラ。`agent@vantage-point/worker/chore`（box key `agent#worker/chore`）を送っても register されていない → `routing_loop` の「宛先 が見つからない」で破棄。

### ③ worker box の Handle（rx）が捨てられている

`register_lane_inner`（`msgbox.rs`）は `(tx, rx) = mpsc::channel(256)` を作り、`tx` を `boxes[key]` に insert、`rx` を返り `Handle` の中に入れる。lifecycle hook（`server.rs:1473`）は **戻り値 `Handle` を bind していない** → `Handle` 即 drop → `rx` drop → channel 送信側 closed → `routing_loop` / `deliver_local` の `tx.try_send()` が `Err` → **worker box 宛 msg は「配信失敗」で破棄**。

（lead box は `state.agent_msgbox_lead = Some(handle)` で AppState が Handle を保持しているので生きている。worker だけ捨てている。）

### ④ recv 経路が `lane != "lead"` で塞がれている

`handle_msg_recv`（`unison_server.rs:897-902`）も `msgbox_recv_handler`（`routes/health.rs:217-221`）も:

```rust
if lane != "lead" {
    return Err(format!("lane='{}' is not yet supported in this PR (= worker lane msgbox は VP-159 で対応予定)", lane));
}
```

→ worker の Echoes が `msg_recv`（lane 省略）を叩くと default `"lead"` → 親 SP の `agent#lead` を recv = **lead の msgbox を盗む**。`lane=worker/chore` を渡すとこの guard で弾かれる。コメントの「VP-159 で対応予定」は **stale**（VP-159 = Stand/Service framework trait 化 で別物、worker-recv 配線は含まれていなかった）。

### ⑤（おまけ）`sp-bootstrap` は actor ではないが register が残っている

`sp-bootstrap`（`server.rs:378`）は SP 起動時の seed コード（既存 ccws workers を `lane-spawn` actor に投入）が、`lane-spawn` の Handle がすでに `LaneSpawnActor` に move 済みのため、別 address で自分用 send Handle を取るために `register("sp-bootstrap")` しているもの。**send 専用の「from アイデンティティ」**で box の rx は使われない（bootstrap_handle が block を抜けて drop されたら dead-rx の幽霊 box）。VP-159 PR-3 で概念上は「actor じゃない」扱いに格下げ済みだが、register 呼び出しは残っているので `msg_peers`（= 全 box key を列挙）に出てくる。

### まとめ

| 層 | 状態 | 結果 |
| -- | -- | -- |
| ① address syntax | `list_lanes` が `.` 区切り + `worker/` 欠落 + JoJo 名 の advertise | `msg_send` できない（parse error で破棄） |
| ② box key actor 名 | lead=`id`（`agent`）、worker=JoJo 名（`echoes`） で不統一 + `worker/` prefix | 正しい形でも box が見つからない |
| ③ Handle lifecycle | worker box の rx が即 drop | 配送されない（try_send 失敗で破棄） |
| ④ recv guard | `lane != "lead"` を弾く | 受け取れない（lead box を盗むだけ） |
| ⑤ sp-bootstrap | register が残った幽霊 box | `msg_peers` ノイズ |

→ 「worker-lane msgbox」は **送れない・配送されない・受け取れない** の全部欠け。本設計でこの鎖を端から端まで通す。

## Target Model

### 決定A: box は **lane × stand の 2 軸**（per-stand box）。actor 名 = `stands.rs` の `id`

box key = `<stand-id>#<lane>`（lane は **flat 名**、`worker/` prefix なし）:

| lane | stand | box key | wire address | consumer |
| -- | -- | -- | -- | -- |
| lead | agent (= Echoes) | `agent#lead` | `agent@<project>` / `agent@<project>/lead` | lead の Claude session（既存、VP-157） |
| lead | canvas (= Paisley Park) | `canvas#lead` | `canvas@<project>` / `canvas@<project>/lead` | lead の PaisleyParkState（経由のブリッジ actor、PR-5） |
| worker `<name>` | agent | `agent#<name>` | `agent@<project>/<name>` | その worker の Claude session（PR-2/3） |
| worker `<name>` | canvas | `canvas#<name>` | `canvas@<project>/<name>` | その worker の PaisleyParkState（経由のブリッジ actor、PR-5） |

- **意味**: `agent#<lane>` = その lane の Claude session 宛（= coding-assistant msgbox、`msg_recv` で読む）。`canvas#<lane>` = その lane の Canvas / PP 宛（= `mcp__show` 系で表示される素材を msg で送る経路を msgbox に乗せる、doc 13 origin goal B の延長）。lane あたり stand ごとに **別 consumer**（Claude session ⇄ `agent#<lane>`、その lane の PaisleyParkState ⇄ `canvas#<lane>`）→ mpsc の「1 box = 1 consumer」が破れない。
- **address syntax = `<stand-id>@<project>/<lane>`**（slash 区切り、flat lane）。`/` = path 構造（`[<host>/]<project>/<lane>`）、`@` = project 境界、`.` = ホスト名内部のみ（`vp-mako.local` 等）の三役分担。`<lane>` 省略時は `lead`。**`parse_address` は無改修**（既存 v3.1 文法の location 部 `[<world>/]<project>[/<lane>...]` にそのまま乗る。`agent@vantage-point/chore` → actor=`agent`, project=`vantage-point`, lane=`["chore"]` → `msgbox_key("agent", ["chore"])` = `agent#chore`）。
- **`worker/` prefix 廃止**: lane path は flat（`["lead"]` / `["<worker-name>"]`、or `[]` で暗黙 lead）。box key も `echoes#worker/chore` → `agent#chore`。代わりに **worker 名に `lead` を禁止**（`agent#lead` と衝突するため。`ccws/config.rs` の worker 名 validator に追加）。将来 lane の「種別」が増えたら（lead / worker 以外）その時に prefix 規約を足す（YAGNI）。
- **rename ゼロ**: lead の coding-assistant box は既に `agent#lead`（VP-157）。worker も `agent#<name>` で揃える＝既存の `agent` を延ばすだけ。`agent`→`echoes` rename も `stands.rs` 変更も**しない**。MCP の `msg_send` / `msg_directory` / `msg_recv` の description（`'agent'` / `'agent@<project>'` を例示）も**そのまま正しい**ので変更不要。`canvas#<lane>` は新規（PP を msgbox-addressable に）だが `PAISLEY_PARK.id = "canvas"` 由来なので命名は既存と整合。
- **PP box の consumer = その lane の PaisleyParkState（経由のブリッジ actor）**: VP-120（PR-β-2）で PaisleyParkState は `LaneCapabilities` に物理移管済み（cardinality 1 → N、lane あたり独立 instance）。`canvas#<lane>` box を recv して、その lane の PaisleyParkState の Canvas push 経路（doc 13 の Smart Canvas: `pane-paisley-park` 内 `<div id="pp-content">`）に流す **per-lane の小ブリッジ actor** を spawn する。詳細は doc 13（paisley-park-revival）と擦り合わせ。
- **per-lane box は agent + canvas の 2 種固定**（最低限）。Gold Experience（`runner`、project scope）は `gold_experience@<project>`（現状）/ Hermit Purple（`external`、world scope）は `hermit_purple@world`（現状）のまま — per-lane box は作らない。worker の `LaneInfo.stand` フィールド（現状 JoJo 名 `echoes` 単数）に関係なく、各 lane に `agent` box と `canvas` box を持たせる。

### 決定B: wire address を `parse_address` 準拠 + `id` 体系に揃え、`list_lanes` の advertise を直す

- 廃止: `<JoJo名>.<lane>@<project>` 形式（`echoes.chore@vantage-point`、`.` 区切り + JoJo 名 で `parse_address` で弾かれる）
- 正: `<stand-id>@<project>/<lane>`（lead は `<lane>` 省略可）。`<stand-id>` ∈ {`agent`, `canvas`}。federated は既存通り `<stand-id>@<host>/<project>/<lane>`（3+ segment + 先頭に `.` → world 判定。lead でも `/lead` 省略不可）。
- `list_lanes`（`mcp.rs:1308` 周辺）の `msgbox_addresses` を entry（lane）ごとに `{ "agent": "agent@<project>[/<name>]", "canvas": "canvas@<project>[/<name>]" }` に変更（`stand` フィールドの JoJo 名 `echoes` ではなく `id` の `agent` / `canvas` をハードコード — per-lane box は常にこの 2 種）。description も `echoes.<lane>@<project>` → `agent@<project>[/<name>]` に修正。`project_addresses`（`gold_experience@<project>`）/ `world_addresses`（`hermit_purple@world`）は現状維持（別件）。
- broadcast（`*@<project>` 等、既存）はそのまま。`*` は actor 軸ワイルドカード。lane 軸ワイルドカード（`agent@<project>/*`）は本設計のスコープ外（→ 将来）。

### 決定C: per-(lane, stand) の Handle を AppState で保持

PR-1（受け皿、`#336` で merged 済み）で `AppState.lane_stand_boxes: Arc<RwLock<HashMap<(String, String), Handle>>>` を空で追加済み（key = (lane name, stand 名)、PR-2 以降で populate）。

- worker lane の spawn（lifecycle hook `Diff::Add`）で、その lane の **`agent` box と `canvas` box を両方** register し、戻り Handle を保持:
  ```rust
  let lane_name = /* payload から flat 名を得る (= "<worker-name>") */;
  for stand_id in ["agent", "canvas"] {
      let h = msgbox_router.register_lane(stand_id, &[lane_name.clone()]).await;  // lane path = ["<name>"] (flat)
      state.lane_stand_boxes.write().await.insert((lane_name.clone(), stand_id.to_string()), h);
  }
  // canvas Handle → そのlane の PaisleyParkState への Canvas push ブリッジ actor を spawn (PR-5)
  ```
  - lifecycle hook の path 規約も `["worker","<name>"]` → `["<name>"]` に変更（`SystemEvent::Lane(Diff::Add)` の payload）。`payload.stand`（JoJo 名 `echoes`）はもう register に使わない（`agent`/`canvas` ハードコード）。
- lifecycle hook `Diff::Remove`: `lane_stand_boxes` から `(lane_name, *)` を全削除 + `msgbox_router.unregister_all_at_lane(&[lane_name])` + PP ブリッジ actor を abort。
- lead lane の `agent` box: PR-1 で merged 済みの既存 `state.agent_msgbox_lead`（box key `agent#lead`、VP-157）をそのまま使う。`canvas#lead` box は PR-5 で新規 register（`lane_stand_boxes[("lead","canvas")]`）。
- 実装注意: `lane_stand_boxes` は `RwLock<HashMap<_, Handle>>` なので、read guard を持ったまま `handle.recv().await`（長時間 await）するとロックが詰まる。`Handle` は既に `#[derive(Debug, Clone)]`（中身は全部 Arc/Mutex/Option）なので、map から clone して guard を即 drop、その clone で recv する。

### 決定D: recv 経路を (lane, stand)-aware に

`handle_msg_recv`（`unison_server.rs`）/ `msgbox_recv_handler`（`routes/health.rs`）/ MCP `msg_recv` ツール:

- パラメータ: `lane`（`"lead"` default / `"<worker-name>"` の flat 名）+ `stand`（default `"agent"` — coding-assistant msgbox）。`stand="canvas"` で PP box を recv（PP ブリッジ actor 用、通常 agent は使わない）。
- 解決: `key = (lane, stand)` → `handle = lane_stand_boxes[key].clone()`（lead agent は当面 `agent_msgbox_lead` 直参照でも可）。
- 以降は既存 lead path と同型: timeout 付き Selective Receive（cancel-safe）、from filter。
- 存在しない lane / stand → 明確なエラー（`"lane '<x>' / stand '<y>' not found (worker exists?)"`）。

### 決定E: worker context の MCP は self lane を default に + `from` を正す

`vp mcp` は自分の cwd を知っている:
- cwd が `~/.local/share/ccws/<parent>-<name>` → worker `<name>` of `<parent>`
- それ以外（repo path） → `<parent>` の lead

→
- `msg_recv`（`lane` 省略）: worker context なら default を `"<name>"` に（lead context なら従来通り `"lead"`）。`stand` 省略は常に `"agent"`（自分の coding-assistant msgbox）。これで worker の Echoes が `msg_recv` を叩くと自分の `agent#<name>` box を読む（lead box を盗まない）。
- `msg_send` の `from`: 現状常に `from="agent"`（bare）。worker context なら `from="agent@<parent>/<name>"`（= 自分の lane の coding-assistant address。VP-165 の `from` 汚染の一因の解消にも効く）。lead context なら `from="agent"`（→ remote forward 時に `normalize_from` で `agent@<parent>`、既存挙動互換）。worker→他 worker 送信時も `from` は自分の `agent@<parent>/<self>`。
- 実装: `vp mcp` 起動時に cwd から `(kind, parent, lane_name)` を決め、`VantageMcp` に保持。`msg_send` / `msg_recv` / `list_lanes`（`is_self` 付与）で参照。

### whoami: `list_lanes` の entry に `is_self` を付ける（新ツールは作らない）

- 別途 `whoami` ツールを作るより、`list_lanes` の各 entry（lead / worker）に `is_self: bool` を付ける方が安い & 既存の discovery 経路に乗る。
- 実装は **MCP 側 post-processing**: SP は「誰が呼んでいるか」を知らない（QUIC リクエストに caller 情報がない）。MCP（`vp mcp`）は決定E で `(kind, parent, lane_name)` を持っているので、SP から返ってきた lane 一覧の中で「自分の entry」に `is_self: true` を付与してから返す。
- これで agent は `list_lanes` → `is_self` の entry → その `msgbox_addresses`（= 決定B で `agent@<project>[/<name>]` + `canvas@<project>[/<name>]`）を読めば「自分の正規アドレス」が分かる。「自分が send できる相手」は同じ `list_lanes` の他 entry + `msg_directory`。「自分が recv できる box」は自分の `agent#<lane>`（coding-assistant として）。

### ⑤ sp-bootstrap の掃除（PR-6、任意 / 後回し可）

`Router` に「box を register せず send だけする helper Handle」を返す API（例 `Router::sender_for(addr) -> Handle`、`router_tx` clone だけ持つ軽量 variant）を足し、bootstrap コードは `register("sp-bootstrap")` の代わりにそれを使う → 幽霊 box `sp-bootstrap` が消える。独立性が高いので別 PR でも可。

## Implementation（段階）

VP-156 epic の sub-PR スタイル（受け皿 → 配線 → cleanup）に倣う。

| PR | 内容 | 状態 |
| -- | -- | -- |
| **PR-1** | 受け皿 — `AppState.lane_stand_boxes` 追加（空）+ 設計 doc | ✅ merged（#336, main `d4d17f7`） |
| **PR-1b** | `list_lanes` の `msgbox_addresses` を `agent@<project>[/<name>]` / `canvas@<project>[/<name>]` に修正（`.`→`/` + JoJo 名→`id`、+ description）+ worker 名 `lead` 禁止（`ccws/config.rs`）+ 本設計 doc を `id` 体系維持版に改訂 | このPR |
| **PR-2** | worker lane の `agent` box 配線 — lifecycle hook `Diff::Add` で `register_lane("agent", &[lane_name])` の戻り Handle を `lane_stand_boxes[(lane_name,"agent")]` に保持、`Diff::Remove` で除去 + `unregister_all_at_lane`。lifecycle hook の lane path 規約を `["worker","<n>"]`→`["<n>"]` に。回帰テスト: lead box 無傷 / cross-lane で混線しない | |
| **PR-3** | recv 経路（agent）— `handle_msg_recv` / `msgbox_recv_handler` の `lane != "lead"` guard 撤去 → 決定D の (lane, stand)-aware recv。MCP `msg_recv` に `stand` パラメータ追加（default `agent`）。`lane` param は flat 名。stale comment 修正。`vp msgbox watch --lane <name>` がこの経路で通る。dogfood: lead → `msg_send to=agent@vantage-point/chore` → chore worker session で `msg_recv lane=chore` で受信 | |
| **PR-4** | worker MCP self-detection + `is_self` — `vp mcp` 起動時に cwd から `(kind, parent, lane_name)` 決定 → `msg_recv` の default lane / `msg_send` の `from` に反映。`list_lanes` post-processing で `is_self` 付与 | |
| **PR-5** | `canvas` box 配線 — lifecycle hook で `register_lane("canvas", &[lane_name])` も（lead lane も含めて）→ Handle を `lane_stand_boxes[(lane_name,"canvas")]` に保持。その lane の PaisleyParkState への Canvas push ブリッジ actor を per-lane で spawn（`canvas#<lane>` box を recv → doc 13 の Smart Canvas content kind に流す）。`Diff::Remove` で abort。doc 13 と擦り合わせ。dogfood: `msg_send to=canvas@vantage-point[/<name>]` → 該当 lane の Canvas に表示 | |
| **PR-6（任意）** | sp-bootstrap 掃除 — `Router::sender_for` + bootstrap 移行 → `register("sp-bootstrap")` 削除。module-level doc / `stand_service.rs` roadmap 更新。VP-166 close | |

→ PR-2 と PR-3 は近接 land 推奨（間に worker 宛 agent msg が溜まる）。PR-5（`canvas` box）は doc 13 の進捗次第で後ろに回しても良い（VP-166 の core は PR-1b〜PR-4）。

## 設計判断ログ（議論で確定したもの）

| 判断 | 結論 | 理由 |
| -- | -- | -- |
| box は per-stand か 1 本か | **per-stand**（`agent#<lane>` / `canvas#<lane>`） | PP を msgbox-addressable にしたい（doc 13 origin goal B の延長）。stand ごとに別 consumer なら mpsc「1 box 1 consumer」を破らない |
| address syntax | **`<stand-id>@<project>/<lane>`**（slash、flat lane、`worker/` なし） | `parse_address` 無改修で既存 v3.1 文法に乗る（`agent@vantage-point/chore` → actor=`agent`, project=`vantage-point`, lane=`["chore"]` → box `agent#chore`）。`.` はホスト名で使用済みなので lane 区切りに使うと曖昧。`/`=path構造 / `@`=project境界 / `.`=ホスト名内部 の三役分担 |
| msgbox actor 名は `id` 体系か JoJo 名か | **`id` 体系**（`agent` / `canvas`） | `stands.rs` の根本思想 = 「コード内部は安定した機能名（id）を使い、UI/CLI/ログでは愛称（stand_name）を表示。愛称を変えても stands.rs だけ修正で済む」。`id` を JoJo 名にすると次の rename（HD→Echoes みたいな）で wire protocol / port role / API パス が巻き添え → `stands.rs` の存在意義と矛盾。`echoes`/`paisley_park` は `stand_name`（表示専用）。VP-157 が `agent` を選んだのもこの理由。→ `agent`→`echoes` rename も `stands.rs` 変更も **しない** |
| whoami | 新ツール作らず `list_lanes` の `is_self`（MCP 側 post-processing） | SP は caller を知らない / 既存 discovery 経路に乗る方が安い |
| worker lane の stand 群 | **agent + canvas の 2 種固定** | GE は project scope / HP は world scope なので per-lane box は不要 |
| `canvas#<lane>` box の consumer | per-lane の Canvas push ブリッジ actor（doc 13 と擦り合わせ） | PaisleyParkState は VP-120 で per-lane instance、その Canvas push 経路に繋ぐ |
| worker 名 `lead` | **禁止** | `agent#lead` と衝突するため |
| sp-bootstrap 掃除 | PR-6（任意 / 後回し可、`Router::sender_for` で幽霊 box 解消） | 独立性が高い |

### 廃案（議論の過程で出たが採らなかったもの）

- **`agent` を `echoes` の alias にする / `echoes` を canonical にする**: `stands.rs` の `id` 体系を JoJo 名に寄せることになり、デカップリングが崩れる（次の rename で wire/protocol が巻き添え）。VP-157 を巻き戻すコストもある。→ `id` 体系維持（`agent`）。
- **`stands.rs` の `id` を全部 JoJo 名に統一**（`agent`→`echoes`, `canvas`→`paisley_park`, ...）: epic 級。`process` は QUIC channel 名（MCP↔SP 主チャネル）、`canvas` も QUIC channel 名（PP push）、`world` は mDNS TXT record kind — rename すると version skew / cross-machine 互換を壊す。`stands.rs` の設計思想（id = 安定技術名）とも矛盾。→ 不採用、`id` 体系維持。
- **`<stand>.<lane>@<project>`（`.` 区切り、メール的）**: `parse_address` に local-part の `.` 許可拡張が要る + ホスト名の `.` と曖昧。→ `<stand-id>@<project>/<lane>`（`/` 区切り）。

## 実装時に詰める細部（小）

- `lane` パラメータの形式は flat 名（`"chore"` / `"lead"`）で確定。`SystemEvent::Lane(Diff::Add)` の payload の lane path 規約も flat に揃える。
- federated worker（別マシンの worker 宛 `<stand-id>@<host>/<project>/<name>`）は本設計の射程外だが、syntax 上は素直に通る（3+ segment）。実配送（cross-machine forward の VP-154 / VP-161 経路）は別途。
- lane 軸ワイルドカード（`agent@<project>/*` = 全 worker の agent box にブロードキャスト）は将来。`*` の actor 軸ワイルドカードはそのまま。
- `list_lanes` の `stand` フィールド（現状 JoJo 名 `echoes`）はそのまま（lane の起動 stand の表示用）。`msgbox_addresses` だけ `id`（`agent`/`canvas`）を使う。
- GE = `gold_experience@<project>` / HP = `hermit_purple@world` の JoJo 名 advertise/register（`id` の `runner` / `external` と乖離）は project/world scope の別件。本設計では触らない（将来 cleanup）。

## Testing

- `msgbox.rs` ユニット: `register_lane("agent", ["chore"])` → 戻り Handle で recv できる / lead box（`agent#lead`）に worker 宛 msg が来ない / worker box A 宛が worker box B に来ない（cross-lane isolation の recv 版）/ `agent#chore` と `canvas#chore` が独立（cross-stand isolation）/ `unregister_all_at_lane(["chore"])` 後に box が消える。
- `parse_address` ユニット: `agent@vantage-point/chore` → `Address::Project { actor:"agent", world:None, project:"vantage-point", lane:["chore"] }` / `agent@vantage-point` → lane `[]`（→ default lead）/ `agent@vp-mako.local/creo-memories/chore` → world=`vp-mako.local`, project=`creo-memories`, lane=`["chore"]`（既存テストにケース追加）。worker 名 validator が `lead` を弾く（`ccws/config.rs` のテスト）。
- 統合 / dogfood: lead → worker（agent / canvas 両方）、worker → lead、worker → 別 worker、`vp msgbox watch --lane <name>`、CC 再起動後の `msg_recv` default lane（worker context）、`canvas@<project>/<name>` → 該当 Canvas 表示。

## 影響 / Migration

- `list_lanes` の `msgbox_addresses` の形式が変わる（`echoes.<lane>@<project>` → `agent@<project>[/<name>]` + `canvas@<project>[/<name>]`）。この caller は今のところ「`msg_send` の宛先選びに使う」だけ（しかも今は parse できないので実害ゼロ）→ breaking だが影響軽微。`feedback` memory / docs 更新。
- box key の lane path から `worker/` prefix が消える（将来 `agent#worker/chore` を register する代わりに `agent#chore`）。lifecycle hook / `SystemEvent::Lane` payload の lane path 規約変更を伴う（PR-2）。worker 名 `lead` が禁止になる（既存に該当 worker があれば rename か削除が必要 — 通常ない）。
- worker box が「実際に配送される」ようになる（PR-2 以降）ので、これまで silent drop されていた worker 宛 msg が溜まる → recv 経路（PR-3）が landed するまでの間に送られた worker 宛 agent msg は box に滞留（256 buffer、TTL 48h、SP restart で restore — VP-164 の wart の射程）。PR-2 と PR-3 は近接 land 推奨。
- `agent`→`echoes` rename はしないので `stands.rs` / QUIC channel `process`・`canvas` / mDNS `world` / MCP の description / VP-157 の `agent@<project>` には一切影響なし。
- 関連 issue: VP-164（restart 重複再配信）/ VP-165（port 不安定 × port-keyed Whitesnake / from 汚染 — 決定E の `from` 修正は VP-165 と射程が重なる）/ VP-147（per-lane msgbox、送る側 — 本設計で受け取る側完成 → 実質クローズ）/ doc 13（paisley-park-revival — PR-5 の `canvas` box は doc 13 の Smart Canvas と接続）。

## 関連

- Linear: [VP-166](https://linear.app/chronista/issue/VP-166)（本設計の対象）/ [VP-156](https://linear.app/chronista/issue/VP-156) epic / [VP-147](https://linear.app/chronista/issue/VP-147)（per-lane msgbox、送る側）/ [VP-163](https://linear.app/chronista/issue/VP-163)（発見元）/ VP-164 / VP-165
- 設計: [14-wire-address-v3.md](14-wire-address-v3.md)（address syntax）/ [13-paisley-park-revival.md](13-paisley-park-revival.md)（`canvas#<lane>` box の consumer 側）/ [12-stand-architecture.md](12-stand-architecture.md) / [03-msgbox-vs-ccwire.md](../archive/03-msgbox-vs-ccwire.md) / [07-lane-as-process.md](07-lane-as-process.md)
- creo-memories: `vp_msgbox_monitor_agent_msgbox.md`（worker が自分の msgbox を Monitor で watch する vision）/ `vp_lane_init_script.md`（Lane scripted entrypoint）/ VP-163 milestone `mem_1Cavm9QRE3uNSDnP5XWYBL`
- code: `crates/vantage-point/src/process/unison_server.rs`（`handle_msg_recv`）/ `crates/vantage-point/src/process/routes/health.rs`（`msgbox_recv_handler`）/ `crates/vantage-point/src/capability/msgbox.rs`（`register_lane` / `Handle` / `routing_loop` / `deliver_local`）/ `crates/vantage-point/src/capability/msgbox_registry.rs`（`parse_address` / `validate_actor`）/ `crates/vantage-point/src/process/server.rs`（lane lifecycle hook ~1473 / `register("agent")` ~194 / `sp-bootstrap` ~378）/ `crates/vantage-point/src/process/state.rs`（`AppState.lane_stand_boxes`）/ `crates/vantage-point/src/mcp.rs`（`list_lanes` ~1220 / `msg_recv` / `msg_send`）/ `crates/vantage-point/src/ccws/config.rs`（worker 名 validator）/ `crates/vantage-point/src/stands.rs`（`id` 体系 — 触らない、参照のみ）
