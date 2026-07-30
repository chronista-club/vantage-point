# doc 55 — board の投影と表示所有権（float ⇄ dock）

**Status**: **議論確定（2026-07-30、mako × Claude の一問一答）。実装未着手。**
発端 = mako「Board の表示/非表示の動線って今どの経路が存在してるかな？」→ 棚卸しの結果、
表示経路が「presence（非空）一本」に集約されており、「中身は残したいが今は見たくない」の
置き場が無いことが判明。「ここはちょっとゆっくりで良いから、議論をして、仕様を固めたいな」
（mako）で本 doc の議論に入った。
**Owners**: vp-app（webview + Rust）+ vantage-point（MCP tool 公開 / 撤去のみ）
**Related**: [52-board-redesign.md](./52-board-redesign.md)（board の 4 役割と item identity —
本 doc は §10 wave 0 の表示モデルを更新する）/
[50-pane-chrome-and-session-panes.md](./50-pane-chrome-and-session-panes.md)（A6「mode は
session の属性」— 本 doc の form は同型の手筋。P5 = A7 layout 永続が乗せ替え先）/
[49-gui-layout-engine.md](./49-gui-layout-engine.md)（LayoutEngine — share / admit / 永続の土台）/
[51-zero-base-review.md](./51-zero-base-review.md)（原理: 表象の共有）

> 各決定に mako の決め言葉を引用してある（facts-over-narrative — 決定の出典を残す）。

---

## 1. 一枚絵

```
data 層    board = lane ごとの永続 item リスト
           AI・user とも item 単位で同じ操作（対称 IF）。全消し（clear）だけが消える
             │
signal 層  fresh（新着）は表示を起こす力を失い、通知に純化
           閉時 = 取っ手 badge / 開時 = cursor follow のときだけ focus 寄せ（現行規則継承）
             │
view 層    BoardPane の投影 = form: float | docked ＋ open: bool
           user 専有（AI は書けない）。lane ごとに記憶（暫定 in-memory → A7 で永続）
```

- **board は roster 上の pane（`lane-board`）のまま**。変わるのは投影だけ — 新概念を建てない
- 表示の所有権が「内容の事実（presence）」から「user の明示状態」へ移る
- reflow（他 pane が動く）は dock⇄undock の user 操作のときだけ

---

## 2. 現状の棚卸し（2026-07-30 実測）

### 2.1 生きている経路 — presence 自動一本

```
MCP show → DB append → BoardUpdated(retained) broadcast
  → board-handler.ts notifyBoardPresence(lane, items.length > 0, fresh)
  → 'vp:board-presence' → lane-panes.ts roster 末尾に BOARD_PANE_REF
```

- roster 式 = `boardPresent ? [...sessionPanes, BOARD_PANE_REF] : sessionPanes`（lane-panes.ts）
- 非表示にする正規手段は **clear（中身を捨てる）だけ**。「見たくない」と「消す」が分離できない
- fresh（active view + cursor follow）で focusPane → 畳まれていた pane も RESTORE_SHARE で復元

### 2.2 死んでいる経路 — 読み手ゼロの命令型三兄弟

MCP `toggle_pane` / `close_pane` / `split_pane`（+ `vp pane` CLI）は
`handle_process_message` が hub broadcast するだけで、**vp-app 側に受け手が 0 件**
（旧 localhost browser canvas 時代の遺物）。呼べるが何も起きない。→ §8 で撤去。

---

## 3. 決定: 畳む欲求は layout の関心（board モデルには持ち込まない）

> mako「１とか３なんだよね。３に近い」
> （1 = 畳み状態をモデルに追加 / 3 = workspace の形の問題として layout 側で解決）

- board モデル（data 層）に hide flag を持ち込まない。「見たくない」は **view 層だけの操作**
- presence（内容の事実）→ roster（何が存在するか）→ layout（どう見えるか）の 3 段写像で、
  層を分ければ AI の show と user の hide の綱引き自体が起きない（doc 47「projection の境界を
  1 本引く」の再演）

---

## 4. 決定: float の復権 — 同じ pane の 2 投影

> mako「board は、以前の話だけど、chat、console に float ができて、対話 Pane の横幅が
> 変わらなくて、見やすいかつ、情報密度が少ない右側に置かれて、いい感じだった」

### 4.1 tiling と float の構造比較

| | tiling pane（現行） | float（旧 pp-overlay の美点） |
|---|---|---|
| board の出入り | **全 pane の share 再配分** = chat の行折返しが動き、xterm は PTY resize | **自分だけの事象**。他 pane は 1px も動かない |
| presence 自動表示との相性 | AI が show を打つたび workspace が変形 = 侵襲的 | 浮いて現れるだけ = 非侵襲 |
| 占有 | 場所を正式に取る（隠さないが狭くする） | 右側の低情報密度領域に重なる（実害小） |

session pane は **user が作る**（出入り = user の意図）が、board は **AI が書くと現れる**
（出入り = 他者の事象）。出入りの主体が違うものを同じ tiling に載せたことが「勝手に
workspace が変形する」違和感の根 — 形態は「誰が出入りを起こすか」で選ぶ。

### 4.2 ただし overlay への逆戻りではない

> mako「ただ Workbench のモデルとの整合や、board のリストは、永続化させて残したい」

- **board は roster 上の pane であり続ける**。focusPane / 名札 / layout 語彙がそのまま通用する
- 変わるのは投影（form）だけ: `float`（右側余白に浮く層）⇄ `docked`（現行 tiling 配置）
- doc 50 A6「mode（見え方）は session の属性」と同型の「**form（投影）は board pane の属性**」。
  workbench には既に「同じ実体の見え方を属性で切り替える」前例があり、その語彙の再利用

### 4.3 view 状態

```
lane ごと: { open: bool, form: "float" | "docked", floatRect: 位置・サイズ }
```

- **AI はこの層に書けない**（「奪わない」の形態レベル保証。MCP に board 表示動詞は作らない）
- `floatRect` は user の**移動・リサイズ**（§7.1）で更新され、lane ごとに記憶される。
  既定値は初回表示にしか効かない（一度動かせば以後は記憶が正）
- 記憶は暫定 in-memory → A7（doc 50 P5 = lane scope layout 永続 + MCP 公開）到着時に乗せ替え

---

## 5. 決定: item 永続と表示所有権の移動

> mako「board のリストは、永続化させて残したい。各内容はユーザが消すという形にしたい」

- item は user の削除まで残る → board は時間とともに**ほぼ常に非空**になる
- したがって「presence（非空）= 表示」の現行等式は成立しなくなり、表示状態は必然的に
  **user の明示状態**になる（§3 の畳み議論は「認めるか」ではなく「認めるしかない」に変わった）
- presence 駆動は「clear で空になる」ことへの暗黙依存だった — データの寿命を延ばすと、
  それに寄生していた UI の意味論が連鎖して変わる

### 5.1 roster 式の変更

```
旧: boardPresent ? [...sessionPanes, BOARD_PANE_REF] : sessionPanes
新: open && form == "docked" のときだけ tiling roster に入る
    （float 時は tiling の外だが pane 語彙の内 — focusPane は float に効く）
```

### 5.2 fresh の純化

- 閉時（open=false）: 取っ手 badge で「新着あり」を知らせるだけ。**開かない**
- 開時: 現行規則を継承 — cursor follow した新着のときだけ focus 寄せ（doc 52 §5「奪わない」）
- float なら開いても他 pane が動かないため、新着で現れる痛み自体が構造的に小さい

---

## 6. 決定: data 層の動詞再編 — 対称 IF

> mako「cli はいいとして、mcp ではユーザとあなたが同様のことができる IF は必要と思ってます」

**反転記録**: 議論の途中では「削除は user 専有」だったが、mako の上記裁定で
「**削除は item 単位。user と AI が同じ IF を持つ**」に反転した。消えるのは全消し（clear）だけ。

| 主体 | 動詞 |
|---|---|
| AI（MCP） | show / board_update / read_board / **delete_item（新規公開）** |
| user（GUI） | 同上の対称操作（✕ = 既存 `board:delete` IPC）+ 開閉・form 切替・focus |
| 退役 | **clear**（MCP tool + server 処理。訂正は board_update、削除は delete_item で足りる） |

- `delete_item` は新配管ではない: GUI ✕ → `board:delete` IPC → repo が DB 更新 →
  BoardUpdated broadcast の**既存経路を MCP tool として公開するだけ**
- 削除 UI は現状維持（HistoryStrip の thumbnail ✕ → `deleteItem(id)`）。mako「削除 UI は現状のまま」
- 線引き: **data 層は完全対称 / view 層（開閉・form）だけが user 専有**

---

## 7. 決定: 操作 — 2 動詞 × 各 1 操作

> mako「float と、現在の画面分割での配置は、ワンクリック・ワンショートカットで切り替えたい」
> mako「shortcut は ctrl+shift+（横並びのキー二つ）これでやりたい」

| 動詞 | shortcut | click |
|---|---|---|
| **開閉** toggle | `Ctrl+Shift+B` | 取っ手（閉時に残る badge 付きハンドル） |
| **form** toggle（float ⇄ dock） | `Ctrl+Shift+N` | board 名札上の 1 ボタン（dock/float アイコン） |

- B・N は QWERTY 横並び（B = Board の mnemonic）。具体ペアは Claude 提案の採用 — 差し替え容易
- 閉→開は**前回の form のまま**開く。閉時に form キーを押したら「その form で開く」に化ける
- 1 キーで 3 状態巡回（閉→float→dock→閉）は**不採用** — 行き過ぎ事故（1 回多く押して
  逆側に落ちる）を避け、各キーが確実に目的の状態へ着地する
- 既存 scene hotkey（`Ctrl+Shift+1..4` / `]/[`、keybindings.ts）と同じ棚に並べる。衝突なし

### 7.1 float の移動とリサイズ（P1 スコープ内）

> mako「float 時のリサイズと移動は、ここで組み込みたい」（2026-07-30 — 未決から昇格）

| 操作 | 取っ手 | 挙動 |
|---|---|---|
| **移動** | float の名札（header 帯）を drag | pointer capture で追従。workbench 矩形内に clamp（画面外に迷子にしない） |
| **リサイズ** | 縁 / 角の resize handle を drag | 最小サイズあり（名札 + 数行が読める程度）。上限は workbench 矩形 |

- どちらも `floatRect` を更新して lane ごとに記憶（§4.3）。dock 中の share とは独立 —
  form を往復しても float の置き場所・大きさは保たれる
- 移動・リサイズは **view 層の user 操作**なので他 pane は動かない（§9 の reflow 規律に抵触しない）
- window resize 時は clamp を再適用（記憶 rect が新しい workbench 矩形からはみ出す場合は
  収まるまで縮退・平行移動。記憶自体は壊さない）

---

## 8. 掃除（本 doc のスコープ内で撤去）

- MCP `toggle_pane` / `close_pane` / `split_pane` + `vp pane` CLI（§2.2 の死に経路）。
  新仕様でも view 層は user 専有なので復活の目なし
- MCP / server の `clear`（§6）
- `'vp:board-presence'` の意味変更: roster 駆動 → badge / signal 駆動へ（撤去ではなく転生）

---

## 9. 実装順序 — 先行 + 乗せ替え

> mako「先行 + 乗せ替えで OK」

A7（layout 永続）を待たず、**board form を in-memory で先行**する。float の寸法・置き場所の
当たりは dogfood で先に確かめ、永続はその答え合わせの後（A7 到着時に `{open, form, floatRect}`
を A7 の永続層に乗せ替える）。

| 段 | 内容 | 層 |
|---|---|---|
| P1 | float 投影の実装（右側 layer / floatRect / 取っ手 + badge / **移動・リサイズ** §7.1）+ 開閉・form の 2 動詞（shortcut + 名札ボタン + 取っ手）。view 状態は in-memory | vp-app webview |
| P2 | roster 式の変更（§5.1）+ fresh 純化（§5.2）+ presence event の転生（§8） | vp-app webview |
| P3 | MCP `delete_item` 公開 + `clear` 退役 | vantage-point |
| P4 | 死に経路撤去（toggle_pane / close_pane / split_pane + `vp pane` CLI） | vantage-point |
| P5 | A7 実装時: view 状態を lane layout 永続へ乗せ替え | 両方（A7 側の仕事） |

- P1–P2 が本丸（webview のみ、server 0 行）。P3–P4 は独立に出せる掃除
- Lane 層は**無変更**: board は lane-scoped のまま（flat key 体系 `conductor`/performer 名も不変）。
  ink（doc 52 §3）は board pane の上に描く設計のままで、form がどちらでも動く

### 実装留意

- float の非表示は `visibility: hidden`（`opacity: 0` は透明 iframe が wheel を吸う — #899 の
  再発防止。board は sandbox iframe を抱えるのでまさに該当）
- **drag 中は iframe に pointer を食わせない**: board の sandbox iframe 上を pointer が通ると
  pointermove が iframe に吸われて drag が切れる。移動・リサイズ中は透明シールドを被せる
  （または iframe を `pointer-events: none` に落とす）+ 取っ手側で `setPointerCapture`。
  ink の overlay 配線（pointer capture 方式）と同じ流儀
- dock⇄undock は xterm の PTY resize を伴う（tiling 再配分）— user 起点なので許容。
  **AI 起点で reflow が起きる経路を作らない**ことが本 doc の規律
- float の**初回既定値**（初回表示にしか効かない — 一度動かせば記憶が正 §4.3。mako 裁定 2026-07-30）:
  - 縦 = **表示中 lane の縦の 92%**。右側に寄せて浮かべる
  - 横 = **縦 × 1/√2 ≈ 0.71（A4 縦置き比率）** — 「縦幅に合わせて縦長書類（A4 とか）くらいの
    比率で」。縦から導出するので独立変数は縦 92% の 1 つだけ。workbench 矩形 clamp（§7.1）は
    ここにも適用（横に収まらない狭い window では比率より clamp が勝つ）
