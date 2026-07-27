> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 47 — 内部実装の整合性 棚卸し（UI 改修の前に）

> **方針（mako、2026-07-21）**: 焦らず、**内部実装をしっかり整えてから、UI は最後に一気に**直す。
>
> doc 46（Pane model）の P1/P2 を実機 dogfood した結果、**表に出た不具合の多くが
> 内部の不整合の投影**だった。UI を先に整えても、下が割れていれば同じ形で再発する。
>
> 本 doc は「何が割れているか」の**目録**。個々の解き方は着手時に別 doc / section で決める。
>
> **Epic としての進め方（mako、2026-07-21 確定）**: lane 等の**内部実装をきっちり仕上げる**まで、
> 表示は**ミニマム・シンプルのまま据え置く**（`minimizeOthers` で 1 枚ずつ、`04364fe0`）。
> UI は Epic の**最後に一気に**整える。順序と条件は §7。

## 0. 組織原理 — projection の境界を 1 本引く（mako 2026-07-21）

> 「内部モデルと view は疎結合なんだよね？ daemon での Lane の状態の projection というか。」

**半分そうで、半分そうなっていない。** 以下の 6 件は、突き詰めると**この 1 本の線が
引かれていない**ことの各所での現れ。

### 投影になっている（server 所有）

lane の**実体**は daemon-canonical な一方向の投影:

```
daemon (真実源) ──LanesSnapshot──▶ vp-app SidebarState ──▶ webview
   lane descriptor / origin / ord / lifecycle
```

doc 44 §10.3 / §12.4 で origin と並び順の**楽観更新を避けた**のはこれを守るため
（「帳簿が真実源、view は投影」）。doc 24 §10 Phase 2 の「lane descriptor は
daemon-canonical durable truth」も同じ線。

### 投影になっていない（client 所有）

**「見え方」は webview のローカル state**:

| state | 持ち主 |
|---|---|
| `PaneLayout`（並び / 縮小 / focus） | webview in-memory（doc 46 §4.2 で意図的に） |
| Frame Engine の Scene / per-lane Scene 記憶 | webview の `Map` |
| `consoleActiveMode` | webview |

### 境界を跨いでいる（両方にある）

**`console_mode`（Act）だけが daemon 永続 + webview 複製**。しかも doc 46 §1.4 で
Act は「Pane の kind」= 見え方に移る予定なので、**今まさに移動中**の状態にある。

### だから何を決めるか

1. **server 所有 / client 所有の線をどこに引くか**（実体 = server、見え方 = client、で良いか）
2. client 所有のものを**何単位で持つか**（app / project / lane / session）
3. client 所有のものを**永続するか**（するなら誰が持つか — session.json か DB か）

> **所有者を決めることと、scope を決めることは別の決定**。`PaneLayout` を client 所有に
> したのは正しかったが、**scope を lane に紐付けなかった**（app 1 つのまま）ため、
> 実機で「どの lane に移動しても常に 2 Pane」として出た（§3）。
> 一緒に決めた気になっていた。

## 1. レイアウト系が 2 つ並存している（最大）

| | 何 | 対象 |
|---|---|---|
| **Frame Engine** | 3D Scene / transform で配置 | `#pane-terminal` `#pane-paisley-park` `#pane-gold-experience` `#pane-hermit-purple` `#pane-preview` `#pane-empty` |
| **Pane shell**（doc 46 P1） | flex row の tiling | `#pane-terminal` の**内側**（`#lane-host` / `#console-chat-host`） |

`#pane-terminal` は **外から Frame Engine に配置され、内では自前 tiling をする**という
二重構造になっている。doc 46 P3（canvas を Pane に）が止まったのはこれが理由（#839 / doc 46 §6）。

### 決定: **Epic の最後に GUI LayoutEngine を作り、シンプル表示をそこに当てはめる**

> 改訂（mako 2026-07-21 夜）: 当初は「doc 46 の Pane を FrameEngine に**畳む**」としたが、
> **どちらかというと、ちゃんとした GUI の LayoutEngine を最後にしっかり作って、
> シンプルな表示をそこに当てはめていきたい**。

向きは「A を B に移植する」ではなく、**設計し直した LayoutEngine 1 つで両方を置き換える**。
FrameEngine は**母体であり前例**（語彙は既に広い、下記）だが、それを温存するために
doc 46 側を歪めることはしない。

**帰結（内部フェーズ中の規律）**:

- 現行の layout 2 系統（FrameEngine / `PaneLayout`）に**追加投資しない** — 凍結して
  「1 枚ずつ + タブ chip」のまま運ぶ。直すのは**壊れた時だけ**、最小で
- 表示がミニマムであること自体が、**LayoutEngine の最初の受け手が小さい**という
  設計上の利点になる（当てはめる対象が 1 構成なので、engine の初回検証が軽い）

#### 判断材料 — 既に同じものを 2 つ作っていた

FrameEngine は **VP 自前**（`frame-engine.ts`、VP-140、creo-ui 由来ではなく外部依存ゼロ）。
中を読むと doc 46 で作ったものと**同じ概念・同じ層構造**を既に持っている:

```ts
export type PaneState = 'normal' | 'minimized' | 'maximized' | 'hidden'
export interface PaneTransform { x, y, w, h, z, opacity, state }
// 冒頭: 「純 data / 純 calculation / 純 action、DOM 反映は外部 renderer が担う」
```

- **minimized 状態**を既に持つ（doc 46 の `PaneLayout` と同義）
- **比率 transform** で配置（flex tiling の上位互換 — 左右分割は特殊形）
- 層の切り方まで同型（`PaneLayout`/`PaneShell` と `FrameEngine`/`renderer`）
- しかも `maximized` がある = **既存の方が語彙が広い**

→ doc 46 の `PaneLayout` は**知らずに作った 2 個目**。新 LayoutEngine の語彙は
FrameEngine 側（`normal` / `minimized` / `maximized` / `hidden` + 比率 transform）を
**下限**として始める — せっかく広い方を捨てて狭い方に揃えない。

#### LayoutEngine の設計で決めること

FrameEngine は現状 **lane を知らない**。pane は boot 時に固定 6 個
（`echoes` `pp` `ge` `hp` `preview` `empty`）を register するだけで、
doc 46 が要求する「lane ごとの構成」「同種の Pane を N 枚」を表せない。

1. **lane スコープをどう入れるか**
   - a) FrameEngine が lane を知る（Scene を lane ごとに持つ）
   - b) lane ごとに FrameEngine インスタンスを持つ（engine 自体は lane を知らないまま）
   - c) Scene id に lane を混ぜる（`lane:<addr>/focus-echoes` 等）
2. **Pane の動的増減** — 現状 boot 時 register の固定集合（`FRAME_PANE_IDS`）。
   session を作るたび Pane が増える doc 46 の要求と合わない。
   ⚠️ **P5 で現行実装が先に壊れる** — §3 で着地した `LaneLayouts.dock()` は
   「顔ぶれは lane 共通 = 全 lane に効く」前提なので、session ごとに Pane（chip）が
   増えると **lane A の chip が lane B にも生える**。
   → **本設計は LayoutEngine に持ち越し**、P5 では現行実装に「chip 集合も per-lane」
   の**最小修正**だけ入れて凌ぐ（凍結の例外 = 壊れた時だけ直す）
3. **タブエリア（minimized の置き場）を誰が描くか** — `PaneState.minimized` は既にあるので、
   renderer が「minimized な pane を dock に出す」を担えば doc 46 の `pane-tab` は不要になる
4. **`pp` / `ge` / `hp` / `preview` の帰属** — これらは lane 横断の Stand pane。
   lane スコープを入れた時、どこに属するか（app 直下 / 各 lane に写像 / project スコープ）
5. **§2（語彙）の統一** — LayoutEngine が立った時点で「Pane」は 1 語に揃う。
   PP の `pane_contents` は別語（`board` 等）へ寄せるか、そのまま残すか
6. **最初に当てはめる表示は「1 枚ずつ + タブ chip」** — 内部フェーズ中の
   ミニマム表示がそのまま LayoutEngine の**最初の Scene**になる。
   tiling（左右並列）はその次の Scene で足す

> **どちらも移植しない理由**: doc 46 の `PaneLayout` は「左右に並べる」という
> **特殊形に最適化した API**（`dockedIds` / `moveFocus` が列挙順に依存）で、
> 比率 transform に載せれば自然に消える。一方 FrameEngine は lane を知らず、
> pane 集合が boot 時固定。**どちらを土台にしても片方の歪みが残る**ので、
> 語彙は FrameEngine を下限に取りつつ、engine 自体は設計し直す。

## 2. 「Pane」が 3 つの意味を持っている

| 語 | 実体 | 由来 |
|---|---|---|
| Frame Engine の pane | `PaneId = "echoes" \| "pp" \| …`、Scene が配置する単位 | VP-140 |
| doc 46 の Pane | lane 内で並ぶ tiling の単位 | 2026-07-21 |
| PP の pane | `pane_contents` table / `vp pane` CLI / `.vp-pane` | doc 19 |

doc 46 §1.1 で衝突は注記したが、**見積もりには反映できていなかった**（§6 の訂正）。
語が 3 つの層で同名なのは、読み手も書き手も間違える。**語彙の整理は 1 の判断とセット**。

## 3. 寿命（scope）が混ざっている

- `#lane-host` は **app singleton**、その内側の `.lane-pane.active` が **per-lane**
- `#console-chat-host` は **app singleton**（per-lane の入れ子を持たない）
- doc 46 の `PaneLayout` は **app に 1 つ**

→ 実機で「どの lane に移動しても常に 2 Pane 開いている」として観測された。
doc 46 は「**lane の中を** tiling にする」設計なので、Pane 構成は per-lane であるべき。

**決めたこと**: どの層を per-lane にするか。
- **A**: `PaneLayout` を `Map<lane, PaneLayout>` に（DOM は共有のまま）— 小〜中
- **B**: Pane 自体を lane ごとに生成（chat も `.lane-pane` と同じ形に）— 大、P5 と一緒

### ✅ 着地: **A**（`44c119af`、2026-07-21）

`pane-shell.ts` の `LaneLayouts` = 「**顔ぶれ（template）は lane 共通 / 構成（並び・縮小・
focus）は per-lane**」という分割。DOM host は app 共有のままで、lane 切替時に新 lane の
構成を DOM へ写し直す（`PaneShell.setLane`）。

さらに `04364fe0` で**既定の見せ方**を「1 枚ずつ」に戻したので、症状は二重に解消済み。

> ⚠️ **A の前提は P5 で崩れる**。`LaneLayouts.dock()` は「全 lane に効く」= 顔ぶれが
> lane によらないことに依存している。doc 46 P5 で session ごとに Pane が増えると、
> session は lane ごとに違うのに全 lane へ生えてしまう。
> **B は捨てたのではなく、LayoutEngine の設計に持ち越した**（§1 / §7 の条件 ③）。
> P5 では現行実装に最小修正（chip 集合も per-lane）だけ入れる。

> 既存 host を「中身に触らず包む」方針は安全だったが、**その host が持っていた寿命
> （app 全体）も一緒に引き継いだ**。包む時は「何を触らないか」だけでなく
> 「**何を引き継ぐか**」を数える。

## 4. Act が lane の属性のまま

`console_mode`（per-lane 永続 / `console:set_mode` / `#console-switching` overlay /
`vp:console-mode` bus）が残っている。doc 46 §1.4 では **Act = Pane の kind** になるはず。

### 棚卸しで判明: `console_mode` は 3 仕事を兼ねていた

当初「並列表示の今、lane の Act は意味が薄い（両方見えている）」と書いたが、
**それは ① にしか当てはまらなかった**。

| # | 仕事 | 実体 |
|---|---|---|
| ① | 表示の排他選択 | doc 46 が Pane kind に移す想定の分。ここだけ意味が薄い |
| ② | **boot 時の PTY spawn 可否** | chat lane は engine-less（`pid=None` + `Running`）で登録。PTY を立てると `echoes_submit` が 2 本目の engine を呼び **1 会話 2 エンジン**になる |
| ③ | **wire nudge の配送分岐** | `lane_nudge`（PtySlot 直書き）vs `echoes_nudge`（engine 注入）。さらに Tui の時だけ readiness / channel D を適用 |

②③ は現役だった。素直に撤去すると「全 lane で PTY が立って 1 会話 2 エンジン」または
「wire が届かない」という**無音の壊れ方**をする。

> 「1 辺が 2 仕事をしている罠」の実例（しかも 3 仕事）。§1 の 2 系統並存と同じで、
> **消す前に、その辺が運んでいる仕事を数える**。

### ✅ 決定と着地: Act は **session の属性**へ（lane → session）

doc 46 §1.5「session ↔ Pane は 1:1」に従い、`SessionEntry.act` に移設した。
②③ は **root session（器に化身する session、doc 39）の act** で決まる —
slot は lane に 1 枚、mailbox `agent@<lane>` を名乗るのも root だから、既存の
`root` 概念にそのまま乗る。

- **client 所有（Pane の kind）にはしない**。「PTY を立てるか」は**実体**の話で、
  見え方に決めさせると doc 47 §0 の projection が逆流する。
  `LaneInfo.console_mode` は wire 互換のまま残るが、意味は **root act の投影**に変わった
- 旧 `console_modes/` state file は退役。boot で root act へ畳む one-shot migration 付き
  （Tui は既定なので書かずに捨てる = migration が state を増やさない）
- lane state GC の破棄対象が 7 種 → 6 種に減った（leak ではなく state が 1 つ畳まれた）

### 残りは UI フェーズ（訂正 2026-07-21）

当初「client 側の退役はこの後の小 PR」と書いたが、**§4 の内部部分は上の移設で完了している**。
残りは全部 §7 の UI フェーズに属する:

| 残件 | なぜ UI フェーズか |
|---|---|
| `console:set_mode` / `#console-switching` overlay / `vp:console-mode` bus | Act 切替 **UI** の話。タブ chip が既に同じ切替を提供しており、どちらに寄せるかは LayoutEngine の設計と一緒に決まる |
| `LaneInfo.console_mode` の退役 | client が Pane kind を持って初めて落とせる |

`+ New` の Act 指定は「client の RPC 分岐に溶けている」が、**移設後は結果が正しい** —
`echoes_session_create` は必ず `SessionAct::Chat`、`echoes_session_new_root` は必ず `Tui` を
記録するので、どちらの RPC を選んだかと記録される act は一致する。明示化は cleanup であって
バグ修正ではないため、UI フェーズで Pane kind を通す時にまとめる。

> **作らなくていい仕事を作らない**。「溶けている」と「間違っている」は別。
> 溶けたままでも結果が正しいなら、直す価値は語彙の統一（§2）と同時にしか生まれない。

## 5. 「New」が 3 箇所ある

| 場所 | 何をする |
|---|---|
| header の `✨ New` | 今いる Act に新 session（doc 39 §4） |
| タブエリアの `+ New` | Engine × Act を選んで新 session（doc 46 P2） |
| chat 内の `+` | engine を選んで新 chat session（doc 38 Phase 2、コメント上「仮置き UI」） |

doc 46 §1.3「タブは Pane の状態であって別 UI ではない」に沿えば、chat 内の `+` は
タブエリアに吸収できる。header の `✨ New` との関係も決める。

## 6. 共有 bus に要求元タグが無い

`vp:echoes-stands` は要求元を持たない broadcast で、**誰の要求に対する応答か**が判らない。
doc 46 P2 で「+ New」が要求した応答に chat 側の menu も反応する混線が起き、
window 経由の一時フラグで凌いだ（#838）。他の bus（`vp:echoes-sessions` 等）も同型。

**決めること**: request/response の相関 id を持つか、bus を分けるか。

### ✅ 決定と着地: **相関 id**（`vp:echoes-stands`、2026-07-21）

要求時に採番した id を **webview → Rust IPC → `stands_list` → `handleStands` → bus** と
往復させ、購読側は **自分が出した要求の id と一致した時だけ**反応する。

**bus 分離を採らなかった理由** — どちらの案でも「要求元の札を Rust 経由で往復させる」
必要は同じで、差は札を *data* に持つか *event 名* に持つかだけ。名前に持たせると
**発火元（`console.ts` = 投影側）が購読側 UI の顔ぶれを列挙する**ことになり、
§0 の「実体 → 見え方」の向きが逆流する。id なら発火元は要求元を知らないままでいられ、
購読側が増えても発火元は不変。副次として **stale 応答**（連打で先行要求が遅れて着く）も
同じ仕掛けで落ちる — bus 分離では落ちない。

| 層 | 実体 |
|---|---|
| 採番 / 照合 | `console.ts` の `nextRequestId(scope)` / `isMyResponse(pending, req)`（SSOT） |
| 要求 | IPC `echoes:stands_fetch` に `req` field を追加（省略可） |
| 往復 | `AppEvent::EchoesStandsFetch` → `EchoesStands` が `Option<String>` で持ち回る（Rust は中身を解釈しない不透明な札） |
| 応答 | `vp:echoes-stands` の detail に `req`（要求外の発火は `null`） |
| 購読 | `entry.tsx`（`pane-new`）/ `chatview.tsx`（`chat-add`）が自分の id と照合 |

- **`window.vpPaneNewPending` の凌ぎは撤去**（`entry.tsx` で立て `chatview.tsx` が 1 回だけ
  読み捨てていた暗黙の握手）。要求元が 3 つ目に増えても成立しなかった形を、
  「要求した側だけが応答を拾う」規約に置き換えた
- ⚠️ `isMyResponse` を素の `===` にしない — 要求していない購読側（`pending = null`）に
  `req` 無しの応答（`null`）が来ると一致してしまう。**要求していない側は常に false** が規約
- テストは `console.test.ts`（「**別の要求元の応答では発火しない**」を両向き + 両方要求中 +
  stale + `req` 無しの 5 ケースで固定）

**スコープ**: 今回は `vp:echoes-stands` のみ。**`vp:echoes-sessions` も同型**
（要求元タグ無しの broadcast）だが、現状 購読側が chatview の tab strip 1 つで
混線が顕在化していないため据え置き。2 つ目の購読側が生えた時点で同じ仕掛けを当てる
（`nextRequestId` / `isMyResponse` は bus 非依存に作ってあるので、往復させる field を
足すだけで済む）。

## 6.5 lane の中の景色が変わった（mako 2026-07-21）

> 「プロダクトの成長と dogfood から、昔と今とは見えてる景色違ってきていて、今は、各 Lane に
> いろいろな機能を持った機能が、同居して連携して、その中でも conductor 等々 役割が
> 割り当てられるものがあるって感じ。」
> 「1 Lane の中に、N 体の Echoes、PP が同居してる感じ？」
> 「Lane の root Echoes から始まり、それがどんどん必要に応じて拡張していく」

### モデル

- **lane = 場**。中身は 1 本の会話ではなく、**複数の住人が同居して連携する**
- **住人 = session**（engine を 0 or 1 持つ。0 = Draft）
- **役割は住人の属性**。lane に付くのではない
- lane は **root 1 体から始まり**、必要に応じて住人が増える

### 実装との対応（半分は既にそうなっていた）

| 景色 | 現状 | 差 |
|---|---|---|
| 1 体から始まり増える | `SessionRegistry::single()` → `create` | 一致 |
| 代表役がいる | `root`（doc 39「器に化身する session」） | **役割が位置の名前で暗黙表現** |
| PP も同居 | `LaneCapabilitiesPool` が lane ごとに PP を持つ（VP-120） | **session とは別 pool に住んでいる** |
| N 体の Echoes | session は N 体 | ~~**端末を持てるのは root 1 体だけ**~~ → ✅ P5 (`#854`) で解消 |

> 3 行目が §1 の実体版: 同じ「lane の住人」なのに `chat_engines`（session ごと）/
> ~~`pty_slots`（lane ごと）~~ / `LaneCapabilitiesPool`（lane ごと）の **3 つの別々の入れ物**に
> 分かれている。
> → P5 で `pty_slots` は session ごとになり、`chat_engines` と**同じ形**に揃った。
> 残る不揃いは `LaneCapabilitiesPool`（PP が lane ごと = 住人になっていない）1 つ。

### ✅ 決定: `conductor` → `root` に改名（全面）

「conductor（指揮者）」は**振る舞い**の名前なので階層ごとに意味がズレる
（project の起点 lane / lane の中の代表）。「root（根）」は**位置**の名前なので、
どの階層でも同じ関係を指せる — lane の root session も、project の root lane も
「その階層の代表・起点」で一貫する。

- 実装: #851（露払い = 定義を `vp-paths` に 1 本化 + 直書きを定数経由に）→ 本 PR（値の変更）
- **旧「1 lane = 高々 1 エンジン（`pty_slots` xor `chat_engines`）= 1 cc_session」は
  doc 33 時代の記述で、doc 38 の時点で既に事実と違っていた**（`chat_engines` は
  session ごとの map）。現行の法は「**1 session = 高々 1 エンジン**」

## 7. 着手順 — Epic 全体（2026-07-21 確定）

**内部を仕上げるまで表示はミニマム据え置き、UI は最後に一気に**（冒頭の方針）。
doc 44 / 46 / 47 を 1 本の順序に並べたもの。

### 現在地

| | 状態 |
|---|---|
| doc 44 P1（World fold-in） | ✅ `#823` |
| doc 44 P2（`LaneAddress` フラット化） | ✅ `949ac6f4` / `#830` |
| doc 46 P1 / P2（Pane shell / Engine × Act） | ✅ `#837` / `#838` |
| §3（Pane 構成を per-lane に） | ✅ `44c119af` |
| 表示のミニマム化（`minimizeOthers`） | ✅ `04364fe0` |
| §6（共有 bus の相関 id / `vp:echoes-stands`） | ✅ 下記 §6 |

### 内部フェーズ（表示はミニマム固定のまま）

1. ✅ **doc 46 P4 — Act を lane から session へ**（#848）= 本 doc §4。
   撤去ではなく移設だった（3 仕事）。残件は UI フェーズへ送った
2. ✅ **§6 — 共有 bus の相関 id**（#850。`#838` の window フラグ凌ぎを根治）
3. ✅ **doc 46 P5 — `pty_slots` を `(lane, session)` へ re-key**（内部の本丸、`#854`）
   - 実測: 参照 27 箇所中 **25 が `lanes_state.rs` に閉じている**（private field + method 越し）。
     doc 46 §3 の「lane key を前提にした全経路」より**カプセル化されていた**
     → 着地も同じで、触った経路は 12（内 8 / 外 4）。外に出たのは引数追加だけ
   - 重いのは経路数ではなく**不変条件の意味論**: 「1 lane = 高々 1 エンジン
     （`pty_slots` xor `chat_engines`）」が「**1 session = 高々 1 エンジン**」に変わる。
     これは型ではなく規律で守られている
     → 着地: 法は `pty_slots[addr][key]` xor `chat_engines[addr][key]` の
     **同じ入れ子の高さでの検査**になった（旧実装は lane 全体を focused の時だけ見る近似）
   - 条件②（読み手）は `vp lane capture --session` / `vp lane slots` / `vp lane nudge --session` /
     `lanes_list` の `slots` で満たした。条件③（`LaneLayouts.dock()`）は**発火しなかった** —
     P5 が増やすのは slot であって session ではないので chip 集合は不変（webview 無改修）
   - 判明: **非 root slot の producer は別レイヤ待ちだった**。wire identity が lane 単位
     （`VP_LANE`）で、hook の `record_root_conversation` が root entry に書くため、同じ lane に
     2 本目の claude を立てると root の会話 id を上書きする
     → ✅ **会話 id の記録は session 粒度になった**（2026-07-22。`VP_SESSION_KEY` で hook が
     自分の session を名乗り、SP は報告された session に書く。設計 = doc 40 §4-1、
     着地メモ = doc 46 §3）
     → ✅ **producer も着地**（2026-07-22。`lane_slot_new` / `vp lane slot-new` = 新 session を
     採番して console を 1 枚立てる。法の番人が両向き揃った: chat 側 `ensure_chat_engine` /
     slot 側 `open_slot_for_session`）。**UI（Pane として並べる）は UI フェーズのまま**、
     wire mailbox は lane 粒度のまま = どちらも意図的な据え置き
4. ✅ **doc 44 P3 / doc 45** — Project Host の帳簿、transport の Unison 統一
   - ✅ **稼働中 lane の保護**（#855）— `judge_farewell` の guard は最初から正しかったが、
     CLI が `survey_project(.., &[], ..)` と**常に空配列**を渡していて**一度も発火していなかった**。
     判定ではなく**供給の穴**。daemon 不達は `Liveness::Unknown` で型に固定し、
     「不明」と「稼働 0 件」を同一視しない（`--force` でも通さない）
   - ✅ **見送りの帳簿**（#859）— `host_farewell` table（key = `lane_id` + 記録時点の名前
     スナップショット）。**「計算で復元できない事実」だけ**記録し、同じ判定の連続は
     `streak` + `first_seen_at` に畳む（観測ごとに行を足すと、放置された lane ほど帳簿を
     太らせる = 滞留を追う表が滞留で壊れる）
     - ⚠️ doc 44 §8.5 は消費者を board UI としていたため**そのままでは UI フェーズまで
       読み手ゼロ**だった。**最初の読者を `vp lane cleanup` の滞留表示に変更**
       （`— 3 回連続、初回 2026-07-15`）。`vp lane history` も追加
   - ✅ **doc 45 段 1+2**（#858）— `world-control` に RPC 8 本を新設し CLI を Unison へ。
     `/api/world/{port_for,refresh}` は**精査したら 0 本**（fold-in で撤去済、コメントだけ残存）
   - ✅ **doc 45 段 3**（#861）— vp-app の REST client **12 method → 1**（`/api/health` のみ）。
     server 側の RPC 追加はゼロ（#858 で足りた）
   - ✅ **presence 2 値縮約**（#862、doc 44 §5.5 PR3）— `Connecting` / `Disconnected` は
     fold-in で生産者が消えて**テストの中でしか作られていなかった**。しかも
     「全 variant を網羅する」テストが**死んだ variant を生かす役**をしていた
   - **doc 45 段 4（HTTP route 撤去）/ 段 5（`apple/` port scan）**が残り

> **今日（2026-07-21〜22）の内部フェーズで繰り返し出た形**（次に同型を探す時の索引）:
>
> | 形 | 実例 |
> |---|---|
> | 読み手のない書き込み | `LaneId` 2 年 / `protocol/acp.rs` の購読者ゼロ層 / P5 の slot（読み手を同 PR で作って回避） |
> | **書き手のいない読み手** | presence の死んだ variant（テストが唯一の書き手） |
> | **供給の穴で guard が never-fire** | 見送りの `running`（判定は正しいのに空配列） |
> | 型を変えてもコンパイラが黙る | ROTO の `kind` 直読み / 予約名の直書き 10 箇所 |
> | 経路は消したが残骸が残る | `db/sp_*` 1.2 GB / `console_modes/` |
> | doc の見立てが実装とズレる | 「Act は意味が薄い」（実は 3 仕事）/ 「滞留は復元可能」（復元できるのは現在値だけ） |

### UI フェーズ（Epic の最後、一気に）

5. **§1 + §2 — GUI LayoutEngine を設計して作る + 語彙統一**
   → できたら**まず今のミニマム表示（1 枚ずつ + chip）をそこに当てはめる**。
   それが通ってから tiling を Scene として足し、`minimizeOthers` を外す
6. **doc 46 P3**（canvas を Pane に）/ **§5**（New の統合）/
   **doc 44 P4**（タブ header 昇格・sidebar 起点 UI）/ **doc 46 P6**（layout 永続）

### この順序が成立する条件

1. **P4（`console_mode` 撤去）は UI 決着を待たなくてよい** — 撤去がやるのは
   「lane は Act を持たない」という **server 側の事実を作ること**だけで、受け皿
   （kind = `term` / `chat`）は既に client 側にある。UI をミニマムに保ったまま進む
2. ⚠️ **P5 は「読み手のない書き込み」になりやすい** — UI をミニマムに保ったまま
   複数 slot を作ると消費側がゼロになる（`LaneId` が 2 年間 生成・永続されながら
   誰にも読まれなかったのと同型）。**UI 以外の読み手を同じ PR に入れる**こと —
   `vp lane capture` の session 指定 / タブ chip の枚数 / `vp ps` の slot 数。
   > 「UI は最後」という方針は、放置すると構造的にこの罠を再生産する
3. ⚠️ **P5 で現行 layout が先に壊れる** — `LaneLayouts.dock()` が全 lane に効くため、
   session ごとに chip が増えると lane を跨いで生える。**本設計は LayoutEngine に
   持ち越し**、P5 では「chip 集合も per-lane」の最小修正だけ入れる（§1 の設計項目 2）
4. **layout 2 系統は凍結** — 内部フェーズ中、FrameEngine にも `PaneLayout` にも
   機能を足さない。足すと LayoutEngine で捨てる分が増えるうえ、
   「どちらの規約で書いたか」が後から判らなくなる

> UI の一気改修は **§1 が決着してから**。レイアウト系が 2 つある状態で見た目を
> 揃えても、どちらの規約に揃えたのかが後から判らなくなる。

## 8. dogfood 手順（内部フェーズ → UI フェーズの境目で 1 回）

内部フェーズは「表示ミニマムのまま実機を触らない」方針で進めたため、**変更が実機 state に
効くのは次の daemon 起動時**。しかも **3 つの migration が同時に走る**ので、
1 回の再起動で全部確認できる。

### 起動ログで見る 3 行

```
旧 project DB を回収: N dir                                ← #853（実機 23 dir / 約 1.2 GB）
予約 lane 名 migration: state file N 件を root へ改名       ← #852（実機 6 dir / 107 file）
console_mode → session act migration: N lane を畳んだ       ← #848（実機 15 file）
```

いずれも**冪等**で、衝突時（新名が既存）は**上書きしない**。2 回目以降は 0 件になる。

> ⚠️ 順序が効く: 予約名 migration は **lane spawn より前**に走る（先に boot すると新名で
> 空 state を作り、旧名の会話 id が「衝突時は上書きしない」規則で永久に取り残される）。

### 挙動の確認（migration の後）

| 確認 | コマンド / 見るところ |
|---|---|
| 予約名が `root` になった | `vp lane list` に `root` が出る。`~/.local/state/vp/*/` に `__conductor` が残っていない |
| Act が session に移った | chat lane が再起動後も chat のまま復活する（PTY が立たない） |
| **端末の複数枚化** | `vp lane slot-new <lane>` → `vp lane slots <lane>` に **2 枚**出る |
| session 指定 | `vp lane capture <lane> --session 2` が 2 枚目を読む |
| 見送りの滞留 | `vp lane cleanup` の要判断行に `— N 回連続、初回 …` が付く |
| 稼働 lane の保護 | daemon を止めて `vp lane cleanup` → **保留**して 1 件も消さない |
| transport | `vp config` / `vp daemon status` / `vp ps` が Unison 経由で従来どおり出る |

### 注意

- **daemon 再起動は lane を全部落とす**（fold-in 後は project = World 内 `Arc<AppState>`）。
  会話は `cc_session` の `--resume` で復帰するが、**VP の中から再起動すると自分が死ぬ** —
  実機検証は VP の外（kitty 等）で行う
- `db/sp_*` の回収は**戻せない**（doc 44 §5.2 で破棄と検証済。旧 PP board は引き継がれない）
