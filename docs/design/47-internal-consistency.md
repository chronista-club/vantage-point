# doc 47 — 内部実装の整合性 棚卸し（UI 改修の前に）

> **方針（mako、2026-07-21）**: 焦らず、**内部実装をしっかり整えてから、UI は最後に一気に**直す。
>
> doc 46（Pane model）の P1/P2 を実機 dogfood した結果、**表に出た不具合の多くが
> 内部の不整合の投影**だった。UI を先に整えても、下が割れていれば同じ形で再発する。
>
> 本 doc は「何が割れているか」の**目録**。個々の解き方は着手時に別 doc / section で決める。

## 1. レイアウト系が 2 つ並存している（最大）

| | 何 | 対象 |
|---|---|---|
| **Frame Engine** | 3D Scene / transform で配置 | `#pane-terminal` `#pane-paisley-park` `#pane-gold-experience` `#pane-hermit-purple` `#pane-preview` `#pane-empty` |
| **Pane shell**（doc 46 P1） | flex row の tiling | `#pane-terminal` の**内側**（`#lane-host` / `#console-chat-host`） |

`#pane-terminal` は **外から Frame Engine に配置され、内では自前 tiling をする**という
二重構造になっている。doc 46 P3（canvas を Pane に）が止まったのはこれが理由（#839 / doc 46 §6）。

**決めること**: Scene と tiling の関係。3 択ある。
1. Scene が上位（Pane 構成は Scene の中に閉じる）
2. tiling が上位（Scene を tiling に置き換える = Frame Engine 退役）
3. 併存を続ける（境界を明文化して、どちらが何を決めるか固定する）

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
