# doc 50 — Pane の額縁（縦軸）と session = Pane

**Status**: 額縁の縦軸は実装済（`d1b71dde` / `cd037f66`）。session = Pane は **P1（視る）+
P2（打つ）実装済**（`38f5f00e` 〜 `c6b93791`、2026-07-24）。**P4 は doc 51 §1 A1
（帯撤去パッケージ、`ec38a479`）で完了** — lane-level Act toggle 撤去、避難路は EchoesHeader
root picker の「見え方」行へ。ただし `console:set_mode` の session 単位化は **P3 送り**
（pre-P3 は term になれるのが root だけで、session 引数は行使できる意味を持たない —
読み手のない口を先に作らない）。tiling 既定 + 下端の帯（`#pane-tabs`）撤去も同 commit。
残りは P3（World A xterm re-key、**設計確定 = §4.6**）/ P5（layout 永続）。PP Proj 撤去
（`0330eb0d`）・名札ツールの hover 召喚（`d8d33efd`）も出荷済。
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
| **P3** | **World A** — xterm を `(lane, session)` へ re-key、term session も Pane 化（設計確定 = §4.6） | 4,8 | 中〜大 |
| **P4** | Act toggle 撤去（Act = Pane の kind に畳み切る）+ `console_mode` の残滓掃除 | 10 | 小 |
| **P5** | lane scope の layout 永続 + MCP 公開（write gate / 承認 UX、doc 49 の follow-up） | — | 小 |

- **P1 で mako の画（cc#16 / cc#17 / sid#18 を並べる）は出る**。ただし composer が有効なのは
  focused Pane だけ = 「並べて視る」まで。P2 でどの Pane からも打てるようになる
- P1 と P2 の間の中間状態（並ぶが打てない Pane がある）は **1 リリースを跨がせない** —
  [[pre-mvp-development-stance]]「中間状態を作らず最短で canonical に切る」。P1→P2 を続けて出す
- P3 は World A の境界を越えるので単独で扱う（doc 46 §3 が `pty_slots` re-key で測ったのと
  同じ「1 辺が 2 仕事」の危険域）

### 4.6 P3（= doc 51 A6）設計確定 — 2026-07-25、mako × Claude 議論

§4.0 の定義と club-nostos の lifecycle 語彙（Bracket / Outcome）で前提を固定し、確定 4 点を
その導出として書く。

#### 前提 — 続くもの / 走るもの / 不変条件

- **続くもの = 往復路**（§4.0 の Echoes）。**走るもの = 化身**（PTY の engine TUI / headless
  host。nostos の Bracket 1 回分に相当）。engine 側の会話 id（cc_session）は resume のたび
  新 id へ rotate し得る — 往復路の安定名は **SessionKey**（lane 内の局所名）。SessionKey は
  プロセスの名ではない
- **不変条件: 1 往復路につき Active な化身は高々 1**。「1 会話 1 プロセス」の正確な言い方で、
  同 session の term / chat 同時 2 枚不可（lane-panes.ts 冒頭）の根拠
- **act 切替 = Reborn** — 今の化身を exit し、別の形で enter、記憶（transcript）を引き継ぐ。
  **→chat の Reborn は必ず transcript replay を伴う**（replay の無い切替は「プロセスは続くが
  視界が古い」— II→I→II で Act I の分が chat に出ない既知の症状はこの欠落。→tui は engine
  TUI が resume で自前描画するので VP 側 replay は不要）
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
