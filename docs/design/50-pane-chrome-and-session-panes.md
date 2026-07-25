# doc 50 — Pane の額縁（縦軸）と session = Pane

**Status**: 額縁の縦軸は実装済（`d1b71dde` / `cd037f66`）。session = Pane は **P1（視る）+
P2（打つ）実装済**（`38f5f00e` 〜 `c6b93791`、2026-07-24）。**P4 は doc 51 §1 A1
（帯撤去パッケージ、`ec38a479`）で完了** — lane-level Act toggle 撤去、避難路は EchoesHeader
root picker の「見え方」行へ。ただし `console:set_mode` の session 単位化は **P3 送り**
（pre-P3 は term になれるのが root だけで、session 引数は行使できる意味を持たない —
読み手のない口を先に作らない）。tiling 既定 + 下端の帯（`#pane-tabs`）撤去も同 commit。
**P3（World A xterm re-key = doc 51 A6）も実装完了**（2026-07-25、設計 = §4.6 / 実装記録 =
§4.7）。xterm が `(lane, session)` へ re-key され、Act は session の属性に一本化された
（lane 単位 `console_mode` の概念は GUI から全消し）。残りは **P5（layout 永続 = A7）**。
PP Proj 撤去（`0330eb0d`）・名札ツールの hover 召喚（`d8d33efd`）も出荷済。
**Owners**: vp-app（webview World B + main_area World A）
**Related**: [46-lane-pane-model.md](./46-lane-pane-model.md)（session ↔ Pane 1:1 の確定）、
[49-gui-layout-engine.md](./49-gui-layout-engine.md)（lane scope の tiling 機構）、
creo-memories doc 29 `29-3x3-frame.md` / doc 30 `30-principal-layout.md`（縦軸の起源）

---

## 1. Why — 額縁が「何を載せるか」の原理を持っていなかった

pane の chrome（額縁）が二系統に割れていた（静的 `.pane-header` と SolidJS `EchoesHeader`）。
実画面で比べると、高さが 28px / 30px で割れ、PP の名前が 2 行に折り返し、Echoes だけ帯が 5 本
あった。だが本質は見た目の不揃いではなく、**額縁に何を載せるべきかの判定基準が無かった**こと。
基準が無いので、新しい実装を足すたびに旧実装が残り、同じ役割が二重・三重になっていた。

## 2. What — 縦軸を pane スケールへ再適用する

creo-memories doc 29 §3.2 が app スケールで確立した 2 直交軸のうち、**縦軸**を pane に降ろす:

| | 意味（doc 29 原文） | pane では |
|---|---|---|
| **上** | global — どこにいても変わらない要素 | **名札**: この pane が何であるか（素性 + layer 切替） |
| **下** | local — 現在文脈・局所 | **計器盤**: 今の文脈と、それへの操作 |

- **空なら描かない** — doc 29 §3.5「羅針盤であって格子でない」の継承。下に置くものが無い pane
  （Bastet 等）は下の帯を持たない。「全 pane に同じ帯を敷く」ことが統一ではない
- 横軸（起点 ⇄ ツール）は pane では扱わない。左 = sidebar は app スケールのまま、右 = lane 共通
  サポートエリアは**構想として棚上げ**（mako 2026-07-23）

### 2.1 読み取り / 操作の第二軸（composer）

下段をさらに 2 つに割る。入力欄を挟んで:

- **入力の上** = status bar。engine が今何をしているかの**読み取り専用**の計器（状態 / 停滞 /
  最終 event / 送信待ち / context 残量）。触れる要素を置かない
- **入力の下** = アクション。送る前に決める設定（model / permission）と実行（停止 / 送信）

入力欄は既定 **1 行**（`rows=1` + autosize）。打った分だけ伸び、`max-height` で頭打ち。

## 3. 撤去した重複（実装済）

いずれも「新実装を足して旧実装を残した」形。額縁の監査で露出した。

| 撤去したもの | 生き残った実装 | 同一性の根拠 |
|---|---|---|
| `✨ New`（上段） | Root 切替 picker の「新 ID から」 | 同一 IPC（`console:new_session`、engine/act 無し） |
| `⏹ Stop`（上段） | 入力欄の「停止」 | 同一 IPC（`echoes:interrupt`）、どちらも Act II 限定 |
| perm chip（上段） | composer の perm select | 表示専用 chip は操作器に含まれる |
| `⚠ engine` / `💤 休眠` | status 行 | `deriveStatus` が同じ event を畳んでいる |
| session chip（Act II 分） | tab strip | Act II では tab が識別と切替の両方を担う |
| `permModeLabel` | — | 参照が消え、テストだけが残っていた |

### 3.1 ⚠️ 重複に見えて別物だったもの（消すな）

**`#pane-tabs` の Console/Chat chip は Act 切替ではない。**

| | 実体 |
|---|---|
| Act toggle（`console:set_mode`） | **backend** の console_mode 切替。engine の resume handoff を伴う |
| Console/Chat chip | **frontend** の pane 可視性（doc 46 の lane 内 tiling、attention 0/1） |

mode 変更が `applyConsoleMode` → `showOnly` を呼ぶため結果が似て見えるだけ。静的に読まずに
片方を消すと Act 切替そのものが壊れる。両者にコメントを残した。

> なお §4 が完了すると **Act toggle は役目ごと消える**（Act = Pane の kind に畳まれるため）。
> 今の「下段の右端」は終着点ではなく、消えるまでの置き場。

## 4. session = Pane（未着手）

### 4.0 Echoes の定義 — 入力すると何かが返ってくるもの

> **Echoes = 文字を入力したら何かが返ってくる、その往復そのもの。**
> 違うのは **見え方**（surface）と **投げる先**（target）だけ。（mako 2026-07-23）

doc 37/38 の「engine × Act の直交 2 軸」を、**軸が何を変化させているのか**の側から言い直した
もの。2 軸は前からあったが、その 2 軸が修飾する**不変項**に名前が無かった。

| | 変化するもの | 例 |
|---|---|---|
| **見え方**（= Act / Pane の kind） | 返ってきたものをどう描くか | `term`（PTY の生バイト）/ `chat`（構造化 event） |
| **投げる先**（= engine / stand） | 誰に呼びかけるか | claude / codex / grok / **login shell** |

**効く帰結**:

1. **login shell は劣化ケースではなく、正規の投げる先**。「普通の console にもなる」は
   Echoes の例外ではなく定義そのもの。`vp lane slot-new`（Act = Tui 固定・engine 未指定）で
   建つ素の console も、一級の Echoes session
2. **session ↔ Pane 1:1（doc 46 §1.5）が自然に導かれる** — Pane = 1 本の往復路。その kind が
   見え方、その engine が投げる先。「同じ会話を 2 枚出さない」も、往復路が 1 本だから
3. **Act I と Act II は同格**（mako 2026-07-23「見え方が違うだけで同じ役割」）。どちらにも
   入力と応答がある。Act I の入力口は PTY そのもの、Act II は composer — 同じ器官の別描画
4. ⚠️ **語彙の綻び**: `sessionChipPrefix` は未知 stand に `sid` を返し、root picker は
   `sid` を「engine が未知のため切替不可」として無効化する。だが本定義では shell は
   **正規の投げる先**であって未知ではない。無効化の実質的な理由は「resume が効かない」
   （doc 39 P4）であって「未知だから」ではない — 語彙が 2 つの別の事実を 1 語に畳んでいる
   （[[one-predicate-three-properties]] と同型）。session = Pane の実装時に分離する

### 4.1 これは新しい決定ではない

doc 46 が既に確定させている:

- **§1.5「Pane は必ず新しい session id で始まる」= session ↔ Pane は 1:1**
- **§1.3「タブは Pane の状態であって別 UI ではない」** — タブ = 畳まれた Pane。タブ strip という
  独立 UI は要らない
- **§1.4「Act は lane の mode ではなく Pane の kind」** — `term` = Act I / `chat` = Act II。
  lane は Act を持たない。「Act I と II は見え方が違うだけで同じ役割」（mako 2026-07-23）と一致

backend は**もう出来ている**: P4（#848）で Act が session の属性へ移設、P5（#854）で
`pty_slots` が `HashMap<LaneAddress, HashMap<SessionKey, _>>` へ re-key 済。CLI も
`vp lane slots` / `slot-new` で「session ごとの console」を喋る。

残っていたのは P5 決定表の 1 行 — **「GUI 配線（pump）は張らない」（表示はミニマム据え置き）**。
本 doc の §4 がその配線。

### 4.2 塞いでいるのは client 側の 1 行（→ P1 で解消済）

```ts
// chatview.tsx
if (session !== focusedOf(lane)) return   // background session の event を捨てている
```

backend は**全 session の event を既に流している**。JS が focused 以外を捨てているだけ。
つまり Rust 側で終わった P5 の re-key と**同型の作業が JS 側に残っている**。

### 4.3 変更範囲 — 「lane で key されている層」は 4 つある

> ⚠️ **訂正（2026-07-23、実装着手時）**: 初稿の本節は **JS 層しか数えていなかった**。
> 実際には lane を key にした層が vp-app（Rust）にもあり、さらに **chat 動詞の宛先は
> session を引数に取らず lane の focused に解決される**。§4.2 の「塞いでいるのは 1 行」は
> **表示について**は正しいが、**打てるようにする**には足りない。

xterm は **World A**（`main_area.rs` のインライン JS）にあり、bundle（World B）からは触らない
境界規律がある（doc 33 §8）。session = Pane はこの境界の両側 **+ Rust 側**に同型の変更を要求する。

| # | 層 | 現在の key | 必要な key | 場所 |
|---|---|---|---|---|
| 1 | chat state | `laneChats: Map<lane, _>` | `Map<lane, Map<session, _>>` | B `chatview.tsx`（呼び出し 14 箇所） |
| 2 | event fold | focused 以外を**破棄** | session 別に振り分け | B `chatview.tsx:416` |
| 3 | console facade | `lanes: Map<lane, LaneConsole>` | buffer / renderer を session 別に | B `console.ts` |
| 4 | **xterm instance** | `laneInstances: Map<lane, _>` | `Map<lane, Map<session, _>>` | **A** `main_area.rs` |
| 5 | **SP 接続** | `echoes_sessions: HashMap<lane, _>` | `(lane, session)` | **Rust** `app.rs:4075` |
| 6 | **chat 動詞の宛先** | `EchoesSubmit { lane, prompt }` = **focused へ** | `session` を明示で受ける | **Rust** `terminal.rs:191` + SP |
| 7 | pane refs | `LANE_PANE_REFS` 固定 2 枚 | session list から動的に | B `lane-panes.ts` |
| 8 | DOM host | `#lane-host` / `#console-chat-host` 固定 2 | session ごとに生成 | A + B |
| 9 | tab strip | `.echoes-tabs` | 撤去（chip = 畳まれた Pane、§1.3） | B `chatview.tsx` |
| 10 | Act toggle | lane の mode 切替 | **消滅**（Act = Pane の kind） | B `entry.tsx` |

**#6 が本丸**。doc 46 §P5 の決定表が「focused は chat 動詞の宛先」と書いたとおり、submit /
set_model / set_permission_mode / interrupt はすべて lane の focused session に落ちる。
N 枚の chat Pane が**それぞれ打てる**ためには、この動詞群が session を引数に取る必要がある。

> ⚠️ **やってはいけない回避策**: 「submit の直前に focusSession を送る」。別 IPC なので順序保証が
> 無く、**他の session に送信される**レースを作る。宛先は引数で運ぶ（[[wire-command-ack-timing]]
> と同型の失敗）。

### 4.4 配置は既存の engine がそのまま担う

lane scope（`lane:<addr>`）の tiling は doc 49 LE-P4 で完成済。session を pane として登録
できれば、mako が描いた配置は **LayoutEngine の記法そのもの**で表せる:

```
cc17 | cc16/sid18     ← 左 1 枚・右に 2 段（| = 列, / = 縦積み）
cc16 | cc17 | sid18   ← 3 列並列
```

新しい配置機構は要らない。**pane の顔ぶれを動的にするだけ**。

### 4.5 Phase

「見る」と「打つ」で必要な層が違うので、そこで切る。

| Phase | scope | 層（§4.3 の #） | 規模 |
|---|---|---|---|
| **P1** | **視る** — chat session を Pane として並べる。state を `(lane, session)` へ re-key、fold の破棄をやめ、host を session ごとに生成、tab strip 撤去 | 1,2,3,7,8,9 | 中 |
| **P2** | **打つ** — chat 動詞（submit / set_model / set_permission_mode / interrupt）に session を通す。SP 接続も `(lane, session)` へ | 5,6 | 中 |
| **P3** ✅ | **World A** — xterm を `(lane, session)` へ re-key、term session も Pane 化（設計 = §4.6 / 実装記録 = §4.7、2026-07-25 完了） | 4,8 | 中〜大 |
| **P4** | Act toggle 撤去（Act = Pane の kind に畳み切る）+ `console_mode` の残滓掃除 | 10 | 小 |
| **P5** | lane scope の layout 永続 + MCP 公開（write gate / 承認 UX、doc 49 の follow-up） | — | 小 |

- **P1 で mako の画（cc#16 / cc#17 / sid#18 を並べる）は出る**。ただし composer が有効なのは
  focused Pane だけ = 「並べて視る」まで。P2 でどの Pane からも打てるようになる
- P1 と P2 の間の中間状態（並ぶが打てない Pane がある）は **1 リリースを跨がせない** —
  [[pre-mvp-development-stance]]「中間状態を作らず最短で canonical に切る」。P1→P2 を続けて出す
- P3 は World A の境界を越えるので単独で扱う（doc 46 §3 が `pty_slots` re-key で測ったのと
  同じ「1 辺が 2 仕事」の危険域）

### 4.6 P3（= doc 51 A6）設計確定 — 2026-07-25、mako × Claude 議論

> **実装完了（2026-07-25）**。実装中に確定した 2 点は §4.7 に追記した（topic の物理形 =
> Design B / replay の発火点）。以下は設計時の記述で、§4.7 が優先する。

§4.0 の定義と club-nostos の lifecycle 語彙（Bracket / Outcome）で前提を固定し、確定 4 点を
その導出として書く。

#### 前提 — 続くもの / 走るもの / 不変条件

- **続くもの = 往復路**（§4.0 の Echoes）。**走るもの = 化身**（PTY の engine TUI / headless
  host。nostos の Bracket 1 回分に相当）。engine 側の会話 id（cc_session）は resume のたび
  新 id へ rotate し得る — 往復路の安定名は **SessionKey**（lane 内の局所名）。SessionKey は
  プロセスの名ではない
- **不変条件: 1 往復路につき Active な化身は高々 1**。「1 会話 1 プロセス」の正確な言い方で、
  同 session の term / chat 同時 2 枚不可（lane-panes.ts 冒頭）の根拠
- **act 切替 = 見え方の乗り換え**（doc 51 §2 の語彙）— 化身を exit して別の形で enter するが、
  **会話は死んでいない**（transcript は engine 側が SSOT で、VP が取り替えるのは読み取り装置）。
  > ⚠️ 初稿は「act 切替 = Reborn（記憶を引き継ぐ再誕）」と書いていたが **§4.7 で撤回**した。
  > VP は会話を持っていないので「引き継ぐ」動作すら無く、Reborn は別の操作（同じ場所で新しい
  > session を始める）に割り当てた。理由と語彙の全体は §4.7「語彙の確定」。
- **→chat では必ず transcript を読み直す** — 「**窓を開けたら中を見直す**」（attach 時の replay）。
  これが無いと「プロセスは続くが視界が古い」= II→I→II で Act I の分が chat に出ない既知の症状に
  なる。窓を開けた側が読み直すので、**client の demand で撃つのが正しい**（§4.7 逸脱②）。
  →tui は engine TUI が resume で自前描画するので VP 側 replay は不要
- 往復路の**分離 id（global id）は発行留保**（[[writer-without-reader]] — 読み手が今日
  存在しない）。発行条件 = 会話が lane / 機械を跨いで動く日。その日が来ても名は機能名
  （Stand 名 `EchoesId` にはしない）

#### 確定 ① 動詞 — `session_set_act`（lane 単位 mode は概念ごと撤去）

- `session_set_act {lane, session, act}` を新設し、lane 単位 `console_set_mode` を撤去。
  最初から act 語彙で作る（mode / act の二語併存期間を作らない）
- lane 単位 mode の GUI 概念も全消し: `vpConsole.setMode(lane, mode)` / `'vp:console-mode'`
  bus / lane-panes の `laneModes` mirror → session 単位の act 通知へ置換
- **handler は handoff 完了後、同じ流れでその session の transcript replay を撃つ**（切替と
  replay は 1 動詞の中の対）。attach / demand のエッジ観測に依らない**動詞駆動** — replay 系の
  既知レース（demand edge race、pre-existing bug）の影響圏から切替動線を外す
- 変えないもの: root 特例（boot 時 PTY spawn 可否 / wire nudge 配送 = **root session の act**
  で決まる）は server 意味論として不変。act の**所有も server のまま**（session_registry.rs の
  線 — 「PTY を立てるか」は実体で、見え方に決めさせると projection が逆流する）

#### 確定 ② UI — 名札の kind badge（in-place 変身）

- Act = Pane の kind（doc 46 §1.4）= 「この pane が何であるか」の一部 → **名札（§2 上段）の
  管轄**。名札に kind badge を常設し、click = `session_set_act`
- **pane は in-place で変身する**: tiling 上の位置・名札の同一性は不変、中身だけ
  chatview ⇄ xterm。§3.1 の「下段右端は消えるまでの置き場」の終着点がこれ。root picker の
  「見え方」行（doc 51 A1 の避難路）も badge に吸収して退役
- gating は**能力表引き**（型分岐にしない）:
  - term→chat: stands の `chatCapable`（`newPaneChoices` と同じ規則）。shell の chat は
    「原理不可」ではなく「host 未実装」（§4.0 — bash の往復も Echoes。Warp 型 block UI は
    将来作れる余白）。実装された日に gating 側は無変更で badge が生える形に
  - chat→tui: engine TUI の resume 能力（claude `--resume` は既知。codex / grok は実装時に
    engine 能力表で確認）
- 切替の実利用（mako dogfood 実測）: Act I 固有 = `/mcp`・esc 二度押し巻き戻し・subagent
  観測 / Act II 固有 = HTML・画像の rich 描画。**同格の使い分け**（§4.0 帰結 3 の実証）で、
  badge は不足時の fallback ではなく**表面選択器**。日常動線なので 1 click 常設

#### 確定 ③ New の対称化（root 乗っ取り廃止）

- `console:new_session` の tui 分岐を chat 分岐と対称へ: 新 session（act=tui）を作り、新 term
  pane が tiling に入場するだけ。**root 張り替え + slot respawn を廃止**
- root の付け替えは `console:switch_root`（root picker）の明示操作に一本化
- 旧挙動は「xterm が lane に 1 枚」制約下の**正しい適応**だった — 制約撤廃と同時に「勝手に
  root を動かす副作用」へ意味が反転する。同型（制約前提の適応）を実装時に全数洗うこと

#### 実装範囲と進め方

- §4.3 の残り層: **#4** `laneInstances: Map<lane, _>` → `Map<lane, Map<session, _>>` /
  **#8** `#lane-host` 固定 1 枚 → session ごと動的生成（いずれも World A）
- World B 側の随伴: `TERM_PANE_REF`（静的 1 枚）→ session 由来の動的化、roster 導出を
  console_mode 依存から **session 一覧 × act** へ（lane-panes.ts 冒頭の pre-A6 注記を清算）
- P3 は**単独 PR**（他フェーズと混ぜない）。doc 33 §8 の境界（World B から xterm に触らない）
  を維持し、「1 辺が 2 仕事」の同型チェックを PR 前に全数（doc 46 §3 の pty_slots re-key と
  同じ危険域）

### 4.7 A6 実装記録（2026-07-25）— 設計からの逸脱 2 点と、実装で見つかった同型

実装は S1〜S6 の 6 スライス（単独 PR）。§4.6 の確定 4 点はすべて実装されたが、**実装段階の
発見で 2 点を設計から変えた**。理由ごと残す（同じ問いが再訪されたときに再検討を省くため）。

#### 逸脱 ① topic に session を埋めない（Design B）

§4.6 の「実装範囲」は topic 形を実装者に委ねていた。姉妹の Act II を確認したところ、
`ProcessMessage::EchoesEvent` のコメントが **doc 38 落とし穴①「session を lane 名に埋めない
— topic key は lane のまま、session は本 field で運ぶ」** を明文で禁じていた。terminal も
これに倣う:

| | Design A（当初案） | **Design B（採用）** |
|---|---|---|
| topic | `…/<lane~>/<session>/out` | `…/<lane~>/out`（**不変**） |
| session | topic segment | `LaneTerminalOutput.session` field |
| demand hook | `+/+/out` へ改修（共有関数を割る） | **不変**（`+/out` のまま） |

Design B は diff が小さいだけでなく、§4.6 が警告した「1 辺が 2 仕事」の危険域
（topic rename = 配送と demand 契機の両方を運ぶ辺）**そのものを消す**。
振り分けは受信側（World A の `vpTerminal.handleOutput(lane, session, b64)`）で行う。

#### 逸脱 ② replay は動詞でなく client の demand で撃つ

§4.6 ① は「handler が handoff 後に replay を撃つ」としたが、server が切替直後に撃つと
**client が新 pane の topic を購読する前に流れて落ちる**（非 retained topic）。既存
`ConsoleNewSession` と同じ「pane mount → 購読 → demand」の規律に倣い、client 側の
`echoes_demand_start` / terminal subscribe で撃つ。

§4.6 の狙い（demand **edge** race の圏外）は保たれる — これは購読 0→1 の*エッジ観測*ではなく、
「切り替えたので読み直す」という**明示 demand** だから。動詞は state 遷移に徹する。

> ⚠️ **この逸脱は最初の実装で機能していなかった**（team-b review が指摘）。`ensure_echoes_attach`
> の gate は購読ハンドル（`echoes_sessions`、**lane 単位**）の有無で判定し、そのハンドルは lane
> 削除まで残る。つまり 2 回目以降の chat 化では attach が no-op になり、**A6 が根治すると宣言した
> 当の症状（II→I→II で Act I の分が出ない）が別の理由で再現**していた。
> 修正 = chat 分岐で **gate を経由しない明示 `echoes_demand_start {lane, session}`** を撃つ。
> 購読を落として張り直す案は不採用 — 購読は lane 単位で**他の chat session の live stream も
> 運んでいる**ため巻き添えになる。session を明示するのも必須（`None` は focused に解決される
> ので、非 focused な pane を切り替えた時に別会話を読む）。
> **実機で解消を確認**（Act I の TUI で交わした発話が、chat に戻ったとき吹き出しで現れた）。

#### 逸脱 ③ pane の in-place 変身は「id の置換」で作る

§4.6 ② は「位置と同一性は不変、中身だけ入れ替わる」と書いたが、見え方が変わると **host id も
変わる**（`chat-session-N` ⇄ `lane-host` / `term-session-N`）。roster 同期に任せると
`syncPaneColumns` が「旧 id が消えた / 新 id が入場した」と解釈し、**列の位置と share を失う**
（enterShare で右端の細い列に新規入場 = 実機で「立ち上がっていない」ように見えた）。

→ `renamePane(layout, fromId, toId)` を新設し、**roster 同期より先に**列の中で id を差し替える。
順序を逆にすると syncPaneColumns が先に消してしまう。

> **設計文書に「in-place」と書いても、機構が伴わなければ言葉だけ**。focus の引き継ぎだけ実装して
> いたので部分的に「それらしく」見えており、自分のコメントを実装の証明として読んでいた。

#### 実装で見つかった同型（制約撤廃の随伴）

「xterm は lane に 1 枚」を前提にした適応が、制約撤廃で**意味が反転**した箇所:

| 箇所 | 旧（制約下では正しかった） | 新 | 発見 |
|---|---|---|---|
| `console:new_session` tui 分岐 | 新 session + **root 張り替え** + slot respawn | 新 session + slot 起立（root 不動） | 設計時 |
| `ink.ts` の送り先 | lane 単位 `getMode` + `term:write {lane}` | focused **session の act** + `{lane, session}` | 清算時 grep |
| `activate_lane` / boot catch-up | `vpConsole.setMode` で lane の mode を同期 | 退役（roster が session×act から導出） | 清算時 grep |
| **boot 経路の gate**（`LanesLoaded` / `LanesEnsureAll`） | `pid.is_none() \|\| console_mode == "chat"` で lane ごと skip | `term_sessions_of(lane).is_empty()` | **実機 dogfood** |
| **`ensure_echoes_attach` の gate** | `lane_is_chat`（= root の act） | `lane_has_chat_session`（どれか 1 つでも chat か） | 実機の同型探索 |
| **`handle_lane_slot_new`** | pump を張らない（demand hook / act 切替の 2 契機しかなく、slot 追加はどちらでもない） | 末尾で `respawn_terminal_pump` | team-b review |
| **`remove_chat_session`** | `chat_engines` だけ畳む（名前どおり chat 専用） | `drop_slot` も呼ぶ（**孤児 PTY を残さない**） | term に ✕ が付いて露出 |
| **名札の「click で focus」** | chat の focus 概念を term にも表示 | chat 限定（term は World B が focus を持たない） | 実機 dogfood |
| **kind badge の gating** | 未配線（S5 の `actSwitchBlockedReason` は**一度も呼ばれていなかった**） | server が `chat_capable` を送り、client は押せる見た目を出さない | 実機（押しても無言） |
| **`handle_echoes_demand_start` の gate** | lane 単位 `console_mode`（root cache）— root=tui だと非 root の chat に **ReplayStart すら送らない** | `resolved.act`（`ResolvedSession` に `act` を追加） | team-b 再 review（score 92） |
| **`ensure_chat_engine` の gate** | `resolved.focused && info.console_mode != Chat` — 非 root chat を focus すると **engine 起動が拒否**、逆に非 focused は素通り | `resolved.act != Chat`（focused の特例も不要に） | 上記の同型探索 |
| **handoff lock** | 単一 slot の存在チェック — **無関係な pane の badge click を無言で落とす**（解除側は (lane,session) を照合していたので入口だけ非対称） | `Map<lane#session, target>` で pane ごとに独立 | team-b 3 回目（score 85） |
| **boot の slot 復元** | root だけ立てる — World / project 再起動後に**非 root term の pane が空で無反応**（roster は registry から出るので pane は現れる） | `restore_term_slots` で act=Tui の非 root も eager 復元 | team-b 3 回目（score 78） |
| **`switch_root` / `new_root` の gate** | `console_mode != Tui` — root=chat のとき**代表を付け替えられない**（root は act と直交する概念なのに） | 撤去（残る制限は engine の有無だけ = mailbox の主は engine を持つ必要がある） | mako 判断（「最初は tui しか安定していなかったから」） |

**共通形は「lane 単位で判断している箇所」**。A6 は「session ごとに act が違いうる」世界を作った
ので、lane 単位の述語（`console_mode` / `pid` / `lane_is_chat`）はすべて**誤った要約**になる。

> ⚠️ **この清算は 4 周かかった**（設計時 → 清算 grep → 実機 → team-b 2 回）。`console_mode` を
> 読む場所を消したつもりでも、**その投影を読む場所**（`LaneInfo.console_mode` は root の act の
> 投影）が残る。最後の 2 件（`echoes_demand_start` / `ensure_chat_engine`）は「root=tui のまま
> 非 root だけ chat」という**構成の組み合わせ**でしか露出せず、root=chat の既存テストでは
> 検出できなかった。同型を探すときは grep だけでなく、**A6 が新たに到達可能にした構成**を
> 列挙してテストを書く方が確実（`echoes_demand_start_replays_non_root_chat_while_root_is_tui` は
> 旧実装に戻すと落ちることを確認済み）。

発見のされ方が 3 通りあったのが示唆的:

- **ink が最も危険だった**（静的発見）— roster を直しても壊れたままで、症状は「送信は成功するが
  root に届く」= エラーゼロの誤配送。しかもテストが「tui は session を無視する」を*正*として
  固定していた。**制約の撤廃は「正しさの定義」も変える**ので、古い正しさを守るテストは変更を
  守らず隠す。
- **boot gate は実機でしか出なかった**（動的発見）— unit も型も通り、pane も並ぶ。server 側も
  正常（PtySlot 生存・prompt も出ている）。**「pane は並ぶのに中身が来ない」**という、静的には
  見えない形だった。root=chat + 非 root=tui という**構成の組み合わせ**が要るのも、テストを
  書きにくくしていた（だから見つけた後に `session_derivation_tests` で固定した）。
- **echoes attach は実機の 1 件目から横に探して見つけた**（[[one-edge-two-jobs]]「同型は必ず横に
  探す」）。1 件直して満足せず、同じ述語を使う場所を全数見ることが効く。

#### 撤去したもの（読み手/書き手を失った残骸）

`console_set_mode`（動詞・IPC・allowlist・handler・テスト）/ `vpConsole.setMode` /
`getMode` / `'vp:console-mode'` / `laneModes` / `ConsoleSetMode` / `ConsoleModeApplied` /
`ConsoleSessionRenewed`（New 対称化で送り手が消えた）/ EchoesHeader の `requestActSwitch` と
`mode` signal（picker の「見え方」行を消した連鎖）。

#### 境界を跨ぐ contract のテスト

Rust→JS の `evaluate_script` は**引数の数が食い違ってもコンパイルも実行時も黙る**
（`undefined` が渡って silent に壊れる）。IPC allowlist の二箇所規則と同じ地形なので、
`embedded_terminal_api_is_session_keyed` が HTML 文字列に対して signature を assert する。

#### term pane の名札 — 「後回し」が片道ドアを作った

S5 では「term 名札は Pane 共通 chrome（§2）の話だから後続でよい。今は chat pane の badge で
往復できる」と切ったが、**この理由付けが誤り**だった。全 session が tui になると chat pane が
0 枚になり、**badge ごと消えて Act II へ戻る入口が無くなる**（実機で mako が踏んだ）。
§4.6 ② が「各 pane の名札に badge」と書いていたのは、まさにこの対称性のためだった。

> **スコープを切るときに問うべきは「どちらが大きいか」ではなく「切った先が到達不能状態を
> 作らないか」**。作業量で切ると、依存が見えないまま片道ドアが残る。

実装（`SessionPlate`）は term / chat 共通。載せるものと載せないもの:

| 要素 | term | 理由 |
|---|---|---|
| ラベル / root chip / 会話 id / kind badge / ✕ | ✅ | 素性 = 全 pane が同じ顔で名乗る |
| **灯**（活動の脈動） | ❌ | `EchoesEvent` stream から導出 = chat 固有（§2「空なら描かない」） |
| 「click で focus」 | ❌ | focus は chat の概念（World B は term の focus を持たない） |

2 pane が同じ実装を使うことで **「root chip が無い」が情報になる**（片方だけ名札があると、
無いのは「名札が無いから」か「root でないから」か区別できない）。これが §2「額縁の統一」の
実利で、見た目が揃うだけの話ではない。

#### 語彙の確定（mako × Claude、2026-07-25）

A6 の実装中に「act 切替を Reborn と呼ぶ」と書いたが、**議論で撤回した**。

**act 切替（shell ⟷ tui ⟷ chat）は Reborn ではない。** 理由:

- **VP は会話を持っていない** — transcript は engine 側が SSOT。VP が取り替えるのは
  *読み取り装置*（PTY / headless）で、主体は死んでいない。**レンズの交換**であって死と再生ではない
- nostos の Reborn は「**記憶を引き継ぐ**再誕」だが、act 切替には引き継ぐ動作すら無い
  （記憶は最初から VP の外にある = 引き継ぐ主体がいない）
- `shell` を並べると決定的: shell は act ではなく **投げる先**（engine）なので、この 3 つは
  §4.0 の 2 軸（見え方 × 投げる先）を**横断**する。死と再生の語彙では捉えられない粒度

呼び名は doc 51 §2 の既存語彙 **「見え方の乗り換え」**を使う。badge は `[Console]` / `[Chat]` の
**表示そのものが操作を語る**ので、動作名を持たなくてよい。

3 操作の語彙（重複なし）:

| 操作 | 場所 | pane の数 | 何が起きるか |
|---|---|---|---|
| **Add** | lane の名札 | **増える** | Echoes を足す（root 不動） |
| **Reborn** | 各 Pane の名札 | **不変** | いまの session を終え、新しい session を同じ場所で始める |
| **root picker** | lane の名札 | 不変 | **既存の** Echoes から代表を選ぶ |

- `New` → **Add**: 「New」は *何ができるか*（新しい session）を言うが、Reborn も新しい session を
  作るので**区別にならない**。Add は *集まりがどう変わるか* を言うので対比が立つ。
  **名前の良し悪しは単体では決まらず、隣に何が並ぶかで決まる**（語彙は集合として設計する）
- ~~`Restart`~~ は不適 — 「同じものが戻る」含意（`restart_lane` は `--resume` で同じ会話が戻る）で、
  Reborn は逆（別のものが同じ場所に入る）。語も衝突する
- picker の「✨ 新 ID から」は **Add + Reborn の合成**なので撤去した（同じことをする口を 2 つ作らない）

> **Reborn の実装は次 PR**（A6 は re-key に集中）。server 側の種は
> `echoes_session_new_root`（現在 client からの呼び手なし、その旨を doc コメントに明記）。

#### 残タスク

- **Reborn の実装**（各 Pane の名札に動線）+ `+ New` → `Add` のラベル変更。次の小さい PR
- **board が daemon 再起動を跨いで空になる**（A6 とは独立 — `git diff` で board / db に触れて
  いないことを確認済み）。doc 52 の領域なので別途

## 5. やってはいけない

- `#pane-tabs` の Console/Chat chip を「Act 切替の重複」と見て消す（§3.1 — 別レイヤー）
- 額縁の統一を「全 pane に同じ帯を敷く」と解する（§2 — 空なら描かない）
- 名札に「今の文脈」（状態 / 設定 / engine 異常）を載せる（§2 — 下段が home）
- status bar に操作を置く / アクション行に読み取り専用の計器を置く（§2.1 の第二軸）
- session を「タブで切り替えるもの」として新 UI を足す（doc 46 §1.3 — タブは Pane の状態）
- World B から World A の xterm を直接触る（doc 33 §8 の境界規律）

## 6. 名札 token

高さが 28px（`.pane-header`）/ 30px（`#echoes-header`）で割れ、隣り合うと段差が見えていた。
`--vp-nameplate-{h,pad-x,font-size,bg,border}` を SSOT にして実装 2 本で共有する
（実装を 1 本に畳むのは見送り — 静的 pane と文脈連動 pane で要件が違う）。
あわせて名札を `nowrap` 化（PP の "Paisley Park" が 2 行に折り返していた）。

glyph は Phosphor（`ph:`）に統一。sidebar は既に `CreoIcon` へ移行済で、額縁だけ絵文字が
残っていた — 対応表は `webview/icons/stand.ts` が既に持っている（Echoes = `ph:chat-circle`）。
imperative DOM 側は同じ実体である `<iconify-icon>` を直接書く。
