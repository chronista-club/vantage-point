# doc 47 — 内部実装の整合性 棚卸し（UI 改修の前に）

> **方針（mako、2026-07-21）**: 焦らず、**内部実装をしっかり整えてから、UI は最後に一気に**直す。
>
> doc 46（Pane model）の P1/P2 を実機 dogfood した結果、**表に出た不具合の多くが
> 内部の不整合の投影**だった。UI を先に整えても、下が割れていれば同じ形で再発する。
>
> 本 doc は「何が割れているか」の**目録**。個々の解き方は着手時に別 doc / section で決める。

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

### 決定: **doc 46 の Pane を FrameEngine に畳む**（mako 2026-07-21）

ただし **畳む前に再設計する**（そのまま移植しない）。

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

→ doc 46 の `PaneLayout` は**知らずに作った 2 個目**。畳む向きは「doc 46 → FrameEngine」。

#### 再設計で決めること

FrameEngine は現状 **lane を知らない**。pane は boot 時に固定 6 個
（`echoes` `pp` `ge` `hp` `preview` `empty`）を register するだけで、
doc 46 が要求する「lane ごとの構成」「同種の Pane を N 枚」を表せない。

1. **lane スコープをどう入れるか**
   - a) FrameEngine が lane を知る（Scene を lane ごとに持つ）
   - b) lane ごとに FrameEngine インスタンスを持つ（engine 自体は lane を知らないまま）
   - c) Scene id に lane を混ぜる（`lane:<addr>/focus-echoes` 等）
2. **Pane の動的増減** — 現状 boot 時 register の固定集合。session を作るたび Pane が増える
   doc 46 の要求と合わない
3. **タブエリア（minimized の置き場）を誰が描くか** — `PaneState.minimized` は既にあるので、
   renderer が「minimized な pane を dock に出す」を担えば doc 46 の `pane-tab` は不要になる
4. **`pp` / `ge` / `hp` / `preview` の帰属** — これらは lane 横断の Stand pane。
   lane スコープを入れた時、どこに属するか（app 直下 / 各 lane に写像 / project スコープ）
5. **§2（語彙）の統一** — 畳んだ時点で「Pane」は FrameEngine の pane 1 つに揃う。
   PP の `pane_contents` は別語（`board` 等）へ寄せるか、そのまま残すか

> **そのまま移植しない理由**: doc 46 の `PaneLayout` は「左右に並べる」という
> **特殊形に最適化した API**（`dockedIds` / `moveFocus` が列挙順に依存）。
> FrameEngine の比率 transform に載せると、この API は自然に消える。
> 移植すると 2 つの語彙が混ざったまま残る。

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

**決めること**: どの層を per-lane にするか。
- **A**: `PaneLayout` を `Map<lane, PaneLayout>` に（DOM は共有のまま）— 小〜中
- **B**: Pane 自体を lane ごとに生成（chat も `.lane-pane` と同じ形に）— 大、P5 と一緒

> 既存 host を「中身に触らず包む」方針は安全だったが、**その host が持っていた寿命
> （app 全体）も一緒に引き継いだ**。包む時は「何を触らないか」だけでなく
> 「**何を引き継ぐか**」を数える。

## 4. Act が lane の属性のまま

`console_mode`（per-lane 永続 / `console:set_mode` / `#console-switching` overlay /
`vp:console-mode` bus）が残っている。doc 46 §1.4 では **Act = Pane の kind** になるはず。

並列表示の今、「lane の Act」は既に意味が薄い（両方見えている）。撤去は doc 46 P4。

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

## 7. 着手順の案

1. **§3（寿命）** — 実機で見えている不具合の直因。A なら小さく、すぐ効く
2. **§4（Act 撤去）** — §1 の判断材料が増える（Pane の kind が確定する）
3. **§1 + §2（レイアウトと語彙）** — 最大。ここが決まると P3（canvas）が動く
4. **§6（bus）** — 独立、いつでも
5. **§5（New の統合）** — UI 一気改修の一部として最後に

> UI の一気改修は **§1 が決着してから**。レイアウト系が 2 つある状態で見た目を
> 揃えても、どちらの規約に揃えたのかが後から判らなくなる。
