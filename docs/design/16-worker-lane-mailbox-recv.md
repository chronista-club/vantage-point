# 16. worker-lane mailbox: recv 経路 + box 構造（per-stand）+ whoami

> **対象 Issue**: [VP-166](https://linear.app/chronista/issue/VP-166) — worker-lane mailbox の recv 経路を実装
> **親 Epic**: [VP-156](https://linear.app/chronista/issue/VP-156) — Mailbox routing 統一 + 永続化 first-class
> **関連設計**: [14-mailbox-address-v3.md](14-mailbox-address-v3.md)（v3.1 address syntax）/ [13-paisley-park-revival.md](13-paisley-park-revival.md)（PP box の consumer 側）/ [03-mailbox-vs-ccwire.md](03-mailbox-vs-ccwire.md) / [07-lane-as-process.md](07-lane-as-process.md) / [12-stand-architecture.md](12-stand-architecture.md)
> **Status**: Draft（2026-05-11、address syntax = `<stand>@<project>/<lane>` / box = per-stand / `agent` alias 廃止＝`echoes` 一本 確定）
>
> **2026-05-11 改訂**: `agent` を `echoes` の alias として残す案は廃止。`echoes` を唯一の canonical 名にする（lead = `echoes@<project>` / box `echoes#lead`、worker = `echoes@<project>/<name>` / box `echoes#<name>`）。VP-157 で導入された `agent@<project>` は本設計で `echoes@<project>` に置き換え（contained breaking change — `agent@<project>` を使ってるのは MCP tool 自身 + dogfood だけ）。以下の本文中で「`agent` alias」に言及している箇所はこの改訂で無効。設計判断ログ参照。

## Abstract

worker lane の Echoes（= ccws worker で動く Claude session）が **自分宛 msg を `msg_recv` で取れる**ようにする。あわせて、現状ほぼ機能していない per-lane mailbox の **アドレス syntax ⇄ box key ⇄ Handle lifecycle ⇄ recv 経路** の鎖を一本に通し、box を **lane × stand の 2 軸**で正式化する（`echoes#<lane>` = その lane の Claude session 宛、`paisley_park#<lane>` = その lane の Canvas / PP 宛）。`agent#<lane>` は `echoes#<lane>` の alias として残し、VP-157 の canonical lead address `agent@<project>` を壊さない。agent が自分の通信文脈（自分の正規アドレス）を introspect できるようにもする。

VP-147（per-lane mailbox）で「送る側 Done」と記録されていたが、実態は `routing_loop` に `inbox_key(actor, lane)` の分岐があるだけで、**send→deliver→recv の鎖がどこも繋がっていない**（後述「現状」参照）。本設計はその鎖を端から端まで通すもの。

## 現状 — worker-lane mailbox は 4+1 層で壊れている

dogfood（2026-05-11、VP-163 後）+ コード読みで判明。

### ① `list_lanes` が advertise するアドレスが `parse_address` で弾かれる

`list_lanes`（`mcp.rs:1308`）は `format!("{}.{}@{}", stand, lane_label, project)` で `echoes.chore@vantage-point` を生成。一方 `parse_address`（`msgbox_registry.rs`）は:

- `@` で割って `actor = "echoes.chore"`, `location = "vantage-point"`
- `validate_actor("echoes.chore")` → 許可文字は `[a-zA-Z0-9_-]` + `*` のみ → `.` で `InvalidActorChar` → **`Err`**

→ `msg_send to=echoes.chore@vantage-point` は `handle_msg_send` が受理（parse せず router_tx に積むだけ）→ `routing_loop` が `parse_address` → `Err` → `tracing::warn!("Router: address parse error ...")` → `continue` で **msg 破棄**。`msg_send` の `"Message sent"` 表示は嘘（parse は QUIC 応答後の非同期）。本設計の決定B で `list_lanes` を正しい形（`<stand>@<project>/<lane>`）に直す。

### ② worker box の actor 名が lead と不統一

lane lifecycle hook（`server.rs:1473` 付近、`spawn_msgbox_lane_lifecycle_hook`）は `Diff::Add` で:

```rust
msgbox_router.register_lane(&payload.stand, &path).await;  // payload.stand = "echoes", path = ["worker","chore"]
```

→ box key `echoes#worker/chore`（actor = **stand 名**、lane path に `worker/` prefix）。一方 lead は `agent#lead`（actor = `agent`、VP-157 で canonical 化）。よって「lead = `agent`、worker = stand 名」でバラバラ。`agent@vantage-point/worker/chore`（box key `agent#worker/chore`）を送っても register されていない → `routing_loop` の「宛先 が見つからない」で破棄。

### ③ worker box の Handle（rx）が捨てられている

`register_lane_inner`（`msgbox.rs`）は `(tx, rx) = mpsc::channel(256)` を作り、`tx` を `boxes[key]` に insert、`rx` を返り `Handle` の中に入れる。lifecycle hook（`server.rs:1473`）は **戻り値 `Handle` を bind していない** → `Handle` 即 drop → `rx` drop → channel 送信側 closed → `routing_loop` / `deliver_local` の `tx.try_send()` が `Err` → **worker box 宛 msg は「配信失敗」で破棄**。

（lead box は `state.agent_msgbox_lead = Some(handle)` で AppState が Handle を保持しているので生きている。worker だけ捨てている。）

### ④ recv 経路が `lane != "lead"` で塞がれている

`handle_msg_recv`（`unison_server.rs:897-902`）も `msgbox_recv_handler`（`routes/health.rs:217-221`）も:

```rust
if lane != "lead" {
    return Err(format!("lane='{}' is not yet supported in this PR (= worker lane mailbox は VP-159 で対応予定)", lane));
}
```

→ worker の Echoes が `msg_recv`（lane 省略）を叩くと default `"lead"` → 親 SP の `agent#lead` を recv = **lead の inbox を盗む**。`lane=worker/chore` を渡すとこの guard で弾かれる。コメントの「VP-159 で対応予定」は **stale**（VP-159 = Stand/Service framework trait 化 で別物、worker-recv 配線は含まれていなかった）。

### ⑤（おまけ）`sp-bootstrap` は actor ではないが register が残っている

`sp-bootstrap`（`server.rs:378`）は SP 起動時の seed コード（既存 ccws workers を `lane-spawn` actor に投入）が、`lane-spawn` の Handle がすでに `LaneSpawnActor` に move 済みのため、別 address で自分用 send Handle を取るために `register("sp-bootstrap")` しているもの。**send 専用の「from アイデンティティ」**で box の rx は使われない（bootstrap_handle が block を抜けて drop されたら dead-rx の幽霊 box）。VP-159 PR-3 で概念上は「actor じゃない」扱いに格下げ済みだが、register 呼び出しは残っているので `msg_peers`（= 全 box key を列挙）に出てくる。

### まとめ

| 層 | 状態 | 結果 |
| -- | -- | -- |
| ① address syntax | `list_lanes` が `.` 区切り + `worker/` 欠落の advertise | `msg_send` できない（parse error で破棄） |
| ② box key actor 名 | lead=`agent`、worker=stand 名 で不統一 + `worker/` prefix | 正しい形でも box が見つからない |
| ③ Handle lifecycle | worker box の rx が即 drop | 配送されない（try_send 失敗で破棄） |
| ④ recv guard | `lane != "lead"` を弾く | 受け取れない（lead box を盗むだけ） |
| ⑤ sp-bootstrap | register が残った幽霊 box | `msg_peers` ノイズ |

→ 「worker-lane mailbox」は **送れない・配送されない・受け取れない** の全部欠け。本設計でこの鎖を端から端まで通す。

## Target Model

### 決定A: box は **lane × stand の 2 軸**（per-stand box）、`agent` は `echoes` の alias

box key = `<stand>#<lane>`（lane は **flat 名**、`worker/` prefix なし）:

| lane | stand | box key | wire address |
| -- | -- | -- | -- |
| lead | echoes | `echoes#lead` | `echoes@<project>` / `echoes@<project>/lead` |
| lead | paisley_park | `paisley_park#lead` | `paisley_park@<project>` / `paisley_park@<project>/lead` |
| lead | （legacy alias） | `agent#lead` → `echoes#lead` | `agent@<project>` / `agent@<project>/lead`（= `echoes@<project>` の alias、VP-157 互換） |
| worker `<name>` | echoes | `echoes#<name>` | `echoes@<project>/<name>` |
| worker `<name>` | paisley_park | `paisley_park#<name>` | `paisley_park@<project>/<name>` |
| worker `<name>` | （legacy alias） | `agent#<name>` → `echoes#<name>` | `agent@<project>/<name>`（= `echoes@<project>/<name>` の alias） |

- **意味**: `echoes#<lane>` = その lane の Claude session 宛（= coding-assistant inbox、`msg_recv` で読む）。`paisley_park#<lane>` = その lane の Canvas / PP 宛（= `mcp__show` 系で表示される素材を msg で送る経路を mailbox に乗せる、doc 13 origin goal B の延長）。lane あたり stand ごとに **別 consumer**（Echoes session ⇄ `echoes#<lane>`、その lane の PaisleyParkState ⇄ `paisley_park#<lane>`）→ mpsc の「1 box = 1 consumer」が破れない。
- **address syntax = `<stand>@<project>/<lane>`**（slash 区切り、flat lane）。`/` = path 構造（`[<host>/]<project>/<lane>`）、`@` = project 境界、`.` = ホスト名内部のみ（`vp-mako.local` 等）の三役分担。`<lane>` 省略時は `lead`。**`parse_address` は無改修**（既存 v3.1 文法の location 部 `[<world>/]<project>[/<lane>...]` にそのまま乗る。`echoes@vantage-point/chore` → actor=`echoes`, project=`vantage-point`, lane=`["chore"]` → `inbox_key("echoes", ["chore"])` = `echoes#chore`）。
- **`worker/` prefix 廃止**: lane path は flat（`["lead"]` / `["<worker-name>"]`、or `[]` で暗黙 lead）。box key も `echoes#worker/chore` → `echoes#chore`。代わりに **worker 名に `lead` を禁止**（`echoes#lead` と衝突するため。worker 名の validator に追加）。将来 lane の「種別」が増えたら（lead / worker 以外）その時に prefix 規約を足す（YAGNI）。
- **`agent` alias**: `agent` は「その lane の coding-assistant」の stand-agnostic な別名 = 常に `echoes` に解決。実装は **routing / recv lookup 時に actor を normalize**（`if actor == "agent" { "echoes" }`、`Address::Local { actor: "agent" }` も同様）。register 側は `echoes#<lane>` だけ register、`agent#<lane>` という実 box は作らない（alias のみ）。VP-157 の `agent@<project>` / MCP `msg_recv`（default）/ `msg_send` の `from="agent"` は全部この alias 経由で `echoes#<lane>` に解決され、既存挙動を壊さない。**新コードは `echoes` を使う**（`agent` は legacy 互換 alias）。
- **PP box の consumer = その lane の PaisleyParkState（経由のブリッジ actor）**: VP-120（PR-β-2）で PaisleyParkState は `LaneCapabilities` に物理移管済み（cardinality 1 → N、lane あたり独立 instance）。`paisley_park#<lane>` box を recv して、その lane の PaisleyParkState の Canvas push 経路（doc 13 の Smart Canvas: `pane-paisley-park` 内 `<div id="pp-content">`）に流す **per-lane の小ブリッジ actor** を spawn する。詳細は doc 13（paisley-park-revival）と擦り合わせ。
- **per-lane box は echoes + paisley_park の 2 種固定**（最低限）。Gold Experience は project scope、Hermit Purple は world scope なので per-lane box は作らない（`gold_experience@<project>` / `hermit_purple@world` のまま）。worker の `LaneInfo.stand` フィールド（現状 `echoes` 単数）に関係なく、各 lane に echoes box と paisley_park box を持たせる。

### 決定B: wire address を `parse_address` 準拠に揃え、`list_lanes` の advertise を直す

- 廃止: `<stand>.<lane>@<project>` 形式（`.` 区切り、`parse_address` で弾かれる）
- 正: `<stand>@<project>/<lane>`（lead は `<lane>` 省略可）。`<stand>` ∈ {`echoes`, `paisley_park`}。`agent@...` は `echoes@...` の alias。federated は既存通り `<stand>@<host>/<project>/<lane>`（3+ segment + 先頭に `.` → world 判定）。
- `list_lanes`（`mcp.rs:1308` 周辺）の `mailbox_addresses` を entry（lane）ごとに `{ "echoes": "echoes@<project>[/<name>]", "paisley_park": "paisley_park@<project>[/<name>]" }`（lead entry には加えて `"agent": "agent@<project>"` で canonical short form を明示）に変更。`project_addresses`（`gold_experience@<project>`）/ `world_addresses`（`hermit_purple@world`）は維持。description / `feedback` memory も更新。
- broadcast（`*@<project>` 等、既存）はそのまま。`*` は actor 軸ワイルドカード。lane 軸ワイルドカード（`echoes@<project>/*`）は本設計のスコープ外（→ 将来）。

### 決定C: per-(lane, stand) の Handle を AppState で保持

```rust
// AppState
// key = (lane name, stand 名)。 lane name は "lead" or "<worker-name>" の flat 名。
pub lane_stand_boxes: Arc<RwLock<HashMap<(String, String), Handle>>>,
```

- worker lane の spawn（lifecycle hook `Diff::Add`）で、その lane の **echoes box と paisley_park box を両方** register し、戻り Handle を保持:
  ```rust
  let lane_name = /* payload から flat 名を得る (= "<worker-name>") */;
  for stand in ["echoes", "paisley_park"] {
      let h = msgbox_router.register_lane(stand, &[lane_name.clone()]).await;  // lane path = ["<name>"] (flat)
      state.lane_stand_boxes.write().await.insert((lane_name.clone(), stand.to_string()), h);
  }
  // paisley_park Handle → そのlane の PaisleyParkState への Canvas push ブリッジ actor を spawn
  ```
  - lifecycle hook の path 規約も `["worker","<name>"]` → `["<name>"]` に変更（`SystemEvent::Lane(Diff::Add)` の payload）。
- lifecycle hook `Diff::Remove`: `lane_stand_boxes` から `(lane_name, *)` を全削除 + `msgbox_router.unregister_all_at_lane(&[lane_name])` + PP ブリッジ actor を abort。
- lead lane の echoes/paisley_park box: PR-1 で既存 `state.agent_msgbox_lead`（= 内部的に `echoes#lead` に rename）はそのまま使い、PP の lead box は新規 register（`lane_stand_boxes[("lead","paisley_park")]`）。echoes の lead box も将来 `lane_stand_boxes[("lead","echoes")]` に一本化する余地は残すが段階的に。
- 実装注意: `lane_stand_boxes` は `RwLock<HashMap<_, Handle>>` なので、read guard を持ったまま `handle.recv().await`（長時間 await）するとロックが詰まる。`Handle` を `Clone` 可能にして（`router_tx` / `rx: Arc<Mutex<_>>` / `stash: Arc<Mutex<_>>` / `whitesnake` / `history` は全部 cheap clone or Arc）map から clone して guard を即 drop、その clone で recv する。`Handle` を `#[derive(Clone)]` にできるはず（中身は全部 Arc/Mutex/Option）。

### 決定D: recv 経路を (lane, stand)-aware に

`handle_msg_recv`（`unison_server.rs`）/ `msgbox_recv_handler`（`routes/health.rs`）/ MCP `msg_recv` ツール:

- パラメータ: `lane`（`"lead"` default / `"<worker-name>"` の flat 名）+ `stand`（default `"echoes"` — coding-assistant inbox。`agent` を渡したら `echoes` に normalize）。
- 解決: `key = (lane, normalize_stand(stand))` → `handle = lane_stand_boxes[key].clone()`（lead echoes は当面 `agent_msgbox_lead` 直参照でも可）。
- 以降は既存 lead path と同型: timeout 付き Selective Receive（cancel-safe）、from filter。
- 存在しない lane / stand → 明確なエラー（`"lane '<x>' / stand '<y>' not found (worker exists?)"`）。

### 決定E: worker context の MCP は self lane を default に + `from` を正す

`vp mcp` は自分の cwd を知っている:
- cwd が `~/.local/share/ccws/<parent>-<name>` → worker `<name>` of `<parent>`
- それ以外（repo path） → `<parent>` の lead

→
- `msg_recv`（`lane` 省略）: worker context なら default を `"<name>"` に（lead context なら従来通り `"lead"`）。`stand` 省略は常に `"echoes"`（自分の coding-assistant inbox）。これで worker の Echoes が `msg_recv` を叩くと自分の `echoes#<name>` box を読む（lead box を盗まない）。
- `msg_send` の `from`: 現状常に `from="agent"`。worker context なら `from="echoes@<parent>/<name>"`（= 自分の lane の coding-assistant address。VP-165 の `from` 汚染の一因の解消にも効く）。lead context なら `from="agent"`（→ `echoes` alias → remote forward 時に `normalize_from` で `echoes@<parent>` or `agent@<parent>`、既存挙動互換）。worker→他 worker 送信時も `from` は自分の `echoes@<parent>/<self>`。
- 実装: `vp mcp` 起動時に cwd から `(kind, parent, lane_name)` を決め、`VantageMcp` に保持。`msg_send` / `msg_recv` / `list_lanes`（`is_self` 付与）で参照。

### whoami: `list_lanes` の entry に `is_self` を付ける（新ツールは作らない）

- 別途 `whoami` ツールを作るより、`list_lanes` の各 entry（lead / worker）に `is_self: bool` を付ける方が安い & 既存の discovery 経路に乗る。
- 実装は **MCP 側 post-processing**: SP は「誰が呼んでいるか」を知らない（QUIC リクエストに caller 情報がない）。MCP（`vp mcp`）は決定E で `(kind, parent, lane_name)` を持っているので、SP から返ってきた lane 一覧の中で「自分の entry」に `is_self: true` を付与してから返す。
- これで agent は `list_lanes` → `is_self` の entry → その `mailbox_addresses`（= 決定B で `echoes@<project>[/<name>]` + `paisley_park@...` + lead なら `agent@<project>`）を読めば「自分の正規アドレス」が分かる。「自分が send できる相手」は同じ `list_lanes` の他 entry + `msg_directory`。「自分が recv できる box」は自分の `echoes#<lane>`（coding-assistant として）。

### ⑤ sp-bootstrap の掃除（PR-6、任意 / 後回し可）

`Router` に「box を register せず send だけする helper Handle」を返す API（例 `Router::sender_for(addr) -> Handle`、`rx` を空 closed channel にするか、`router_tx` clone だけ持つ軽量 variant）を足し、bootstrap コードは `register("sp-bootstrap")` の代わりにそれを使う → 幽霊 box `sp-bootstrap` が消える。独立性が高いので別 PR でも可。

## Implementation（段階）

VP-156 epic の sub-PR スタイル（受け皿 → 配線 → cleanup）に倣う。core は PR-1〜PR-4、PR-5（PP box）は doc 13 の進捗次第で後ろに回しても良い。

1. **PR-1: アドレス syntax 修正 + 受け皿 + lead box rename + agent normalize**
   - `list_lanes`（`mcp.rs`）の `mailbox_addresses` を `echoes@<project>[/<name>]` / `paisley_park@...`（+ lead に `agent@<project>`）に。`echoes.<lane>@<project>` の `.` 形式廃止。description / `feedback` memory 更新。
   - routing / recv lookup に `agent → echoes` actor normalize（`parse_address` 直後 or routing/recv 時。`Address::Local { actor: "agent" }` も `"echoes"` に）。既存 `agent#lead` box を `echoes#lead` に rename（`server.rs:194` `register("agent")` → `register("echoes")`。`state.agent_msgbox_lead` の field 名は据え置きで normalize 吸収でも可、rename しても可）。worker 名 validator に `lead` 禁止を追加。
   - `AppState` に `lane_stand_boxes: Arc<RwLock<HashMap<(String, String), Handle>>>` 追加（空で初期化）。`Handle` に `#[derive(Clone)]`。
   - 既存挙動 0 影響（`agent@<project>` は normalize 経由で `echoes#lead` に届く = 従来と同じ箱）。回帰: `msg_send to=agent@vantage-point` → `msg_recv` で受信、を確認。
2. **PR-2: worker lane の echoes box 配線**
   - lifecycle hook `Diff::Add` で `register_lane("echoes", &[lane_name])` の戻り Handle を `lane_stand_boxes[(lane_name, "echoes")]` に保持。`Diff::Remove` で除去 + `unregister_all_at_lane`。lifecycle hook の lane path 規約を `["worker","<name>"]` → `["<name>"]` に変更。
   - これで `echoes#<name>` box の rx が生きる → 配送されるようになる（recv 経路は次 PR）。
   - 回帰テスト: lead box 無傷 / cross-lane で混線しない（VP-147 の `test_lane_isolation_cross_lane_msg_not_delivered` の recv 版を `msgbox.rs` に追加）。
3. **PR-3: recv 経路（echoes）**
   - `handle_msg_recv` / `msgbox_recv_handler` の `lane != "lead"` guard 撤去 → 決定D の (lane, stand)-aware recv（stand は当面 `echoes` 固定でも可）。stale comment 修正。MCP `msg_recv` に `stand` パラメータ追加（default `echoes`）。`lane` param は flat 名（`"chore"` / `"lead"`）。
   - `vp mailbox watch --lane <name>` がこの経路で通る。
   - dogfood: lead → `msg_send to=echoes@vantage-point/chore` → chore worker session で `msg_recv lane=chore` で受信、を実機確認。
4. **PR-4: worker MCP self-detection + `is_self`**
   - `vp mcp` 起動時に cwd から `(kind, parent, lane_name)` 決定 → `msg_recv` の default lane / `msg_send` の `from` に反映。`list_lanes` post-processing で `is_self` 付与。
   - dogfood: worker session で `msg_recv`（lane 省略）が自分の box を読む / `list_lanes` で自分の entry に `is_self`。
5. **PR-5: paisley_park box 配線（PP を mailbox-addressable に）**
   - lifecycle hook で `register_lane("paisley_park", &[lane_name])` も（lead lane も含めて）→ Handle を `lane_stand_boxes[(lane_name, "paisley_park")]` に保持。
   - その lane の PaisleyParkState への Canvas push ブリッジ actor を per-lane で spawn（PP box を recv → doc 13 の Smart Canvas content kind に流す）。`Diff::Remove` で abort。doc 13 と擦り合わせ。
   - dogfood: `msg_send to=paisley_park@vantage-point[/<name>]` → 該当 lane の Canvas に表示。
6. **PR-6（任意）: sp-bootstrap 掃除 + cleanup**
   - `Router::sender_for` + bootstrap 移行 → `register("sp-bootstrap")` 削除。module-level doc / `stand_service.rs` roadmap 更新。VP-166 close。

→ PR-2 と PR-3 は近接 land 推奨（間に worker 宛 echoes msg が溜まる）。

## 設計判断ログ（議論で確定したもの）

| 判断 | 結論 | 理由 |
| -- | -- | -- |
| box は per-stand か `agent` 一本か | **per-stand**（`echoes#<lane>` / `paisley_park#<lane>`） | PP を mailbox-addressable にしたい（doc 13 の延長）。stand ごとに別 consumer なら mpsc「1 box 1 consumer」を破らない |
| address syntax | **`<stand>@<project>/<lane>`**（slash、flat lane、`worker/` なし） | `parse_address` 無改修で既存 v3.1 文法に乗る。`.` はホスト名で使用済みなので lane 区切りに使うと曖昧（`echoes@vp-mako.local.chore`）。`/`=path構造 / `@`=project境界 / `.`=ホスト名内部 の三役分担 |
| `agent` の扱い | **廃止**（`echoes` が唯一の canonical 名。alias も置かない） | 「2 名で 1 箱」は混乱の元。`echoes` は Stand 名であり mailbox actor 名でもある（1 名）。VP-157 の `agent@<project>` は `echoes@<project>` に置き換え（contained breaking change、使用者は MCP tool 自身 + dogfood のみ）。波及: `register("agent")`→`register("echoes")` / `state.agent_msgbox_lead`→`echoes_msgbox_lead` / MCP `from="agent"`→`from="echoes"` / self-send error 文 / `msg_directory`・`msg_peers` の出力と description |
| whoami | 新ツール作らず `list_lanes` の `is_self`（MCP 側 post-processing） | SP は caller を知らない / 既存 discovery 経路に乗る方が安い |
| worker lane の stand 群 | **echoes + paisley_park の 2 種固定** | GE は project scope / HP は world scope なので per-lane box は不要 |
| PP box の consumer | per-lane の Canvas push ブリッジ actor（doc 13 と擦り合わせ） | PaisleyParkState は VP-120 で per-lane instance、その Canvas push 経路に繋ぐ |
| worker 名 `lead` | **禁止** | `echoes#lead` と衝突するため |
| sp-bootstrap 掃除 | PR-6（任意 / 後回し可、`Router::sender_for` で幽霊 box 解消） | 独立性が高い |

## 実装時に詰める細部（小）

- `lane` パラメータの形式は flat 名（`"chore"` / `"lead"`）で確定。`SystemEvent::Lane(Diff::Add)` の payload の lane path 規約も flat に揃える。
- federated worker（別マシンの worker 宛 `<stand>@<host>/<project>/<name>`）は本設計の射程外だが、syntax 上は素直に通る（3+ segment）。実配送（cross-machine forward の VP-154 / VP-161 経路）は別途。
- lane 軸ワイルドカード（`echoes@<project>/*` = 全 worker の echoes box にブロードキャスト）は将来。`*` の actor 軸ワイルドカードはそのまま。
- `Handle` を `Clone` にすると history tracker / whitesnake が共有される（意図通り — 同 box の複数 Handle は同じ履歴・永続化を見るべき）。register 同 address の「tx 上書き」仕様との兼ね合いは現状維持（最後に register したものが boxes[] の tx を持つ）。

## Testing

- `msgbox.rs` ユニット: `register_lane("echoes", ["chore"])` → 戻り Handle で recv できる / `agent` normalize（`agent@p` → `echoes#lead` に届く、`agent@p/chore` → `echoes#chore`）/ lead box（`echoes#lead`）に worker 宛 msg が来ない / worker box A 宛が worker box B に来ない（cross-lane isolation の recv 版）/ `echoes#chore` と `paisley_park#chore` が独立（cross-stand isolation）/ `unregister_all_at_lane(["chore"])` 後に box が消える。
- `parse_address` ユニット: `echoes@vantage-point/chore` → `Address::Project { actor:"echoes", world:None, project:"vantage-point", lane:["chore"] }` / `echoes@vantage-point` → lane `[]`（→ default lead）/ `echoes@vp-mako.local/creo-memories/chore` → world=`vp-mako.local`, project=`creo-memories`, lane=`["chore"]` / `agent@vantage-point` → normalize 後 `echoes` 扱い（既存テストにケース追加）。worker 名 validator が `lead` を弾く。
- 統合 / dogfood: lead → worker（echoes / paisley_park 両方）、worker → lead、worker → 別 worker、`vp mailbox watch --lane <name>`、CC 再起動後の `msg_recv` default lane（worker context）、`paisley_park@<project>/<name>` → 該当 Canvas 表示。

## 影響 / Migration

- `list_lanes` の `mailbox_addresses` の形式が変わる（`echoes.<lane>@<project>` → `echoes@<project>[/<name>]` + `paisley_park@...` + lead に `agent@<project>`）。この caller は今のところ「`msg_send` の宛先選びに使う」だけ（しかも今は parse できないので実害ゼロ）→ breaking だが影響軽微。`feedback` memory / docs 更新。
- `agent#lead` box が `echoes#lead` に rename される（`agent` は alias normalize で吸収 → `agent@<project>` 送信は従来通り届く）。`msg_peers` の出力が `agent` → `echoes` に変わる（+ `paisley_park` が増える）。
- box key の lane path から `worker/` prefix が消える（`echoes#worker/chore` → `echoes#chore`）。lifecycle hook / `SystemEvent::Lane` payload の lane path 規約変更を伴う。worker 名 `lead` が禁止になる（既存に該当 worker があれば rename か削除が必要 — 通常ない）。
- worker box が「実際に配送される」ようになるので、これまで silent drop されていた worker 宛 msg が溜まる → recv 経路（PR-3）が landed するまでの間に送られた worker 宛 echoes msg は box に滞留（256 buffer、TTL 48h、SP restart で restore — VP-164 の wart の射程）。PR-2 と PR-3 は近接 land 推奨。
- 関連 issue: VP-164（restart 重複再配信）/ VP-165（port 不安定 × port-keyed Whitesnake / from 汚染 — 決定E の `from` 修正は VP-165 と射程が重なる）/ VP-147（per-lane mailbox、送る側 — 本設計で受け取る側完成 → 実質クローズ）/ doc 13（paisley-park-revival — PR-5 の PP box は doc 13 の Smart Canvas と接続）。

## 関連

- Linear: [VP-166](https://linear.app/chronista/issue/VP-166)（本設計の対象）/ [VP-156](https://linear.app/chronista/issue/VP-156) epic / [VP-147](https://linear.app/chronista/issue/VP-147)（per-lane mailbox、送る側）/ [VP-163](https://linear.app/chronista/issue/VP-163)（発見元）/ VP-164 / VP-165
- 設計: [14-mailbox-address-v3.md](14-mailbox-address-v3.md)（address syntax）/ [13-paisley-park-revival.md](13-paisley-park-revival.md)（PP box の consumer 側）/ [03-mailbox-vs-ccwire.md](03-mailbox-vs-ccwire.md) / [07-lane-as-process.md](07-lane-as-process.md) / [12-stand-architecture.md](12-stand-architecture.md)
- creo-memories: `vp_mailbox_monitor_agent_inbox.md`（worker が自分の inbox を Monitor で watch する vision）/ `vp_lane_init_script.md`（Lane scripted entrypoint）/ VP-163 milestone `mem_1Cavm9QRE3uNSDnP5XWYBL`
- code: `crates/vantage-point/src/process/unison_server.rs`（`handle_msg_recv`）/ `crates/vantage-point/src/process/routes/health.rs`（`msgbox_recv_handler`）/ `crates/vantage-point/src/capability/msgbox.rs`（`register_lane` / `Handle` / `routing_loop` / `deliver_local`）/ `crates/vantage-point/src/capability/msgbox_registry.rs`（`parse_address` / `validate_actor` / lane validator）/ `crates/vantage-point/src/process/server.rs`（lane lifecycle hook ~1473 / `register("agent")` ~194 / `sp-bootstrap` ~378）/ `crates/vantage-point/src/process/state.rs`（`AppState`）/ `crates/vantage-point/src/mcp.rs`（`list_lanes` ~1220 / `msg_recv` / `msg_send`）/ PaisleyParkState 関連（`LaneCapabilities`）
