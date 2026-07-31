# doc 56 — edge rail: lane 級動詞の家（動詞の級 = 住所）

**Status**: **設計確定 + prototype 出荷（2026-07-30、mako × Claude の一問一答）。**
発端 = doc 55 P1 の board 取っ手（右 edge の最初の住人）を触った mako「ちょっと話変わるけど、
Lane ヘッダの "New" を、ここにおきたかったんだよね」。doc 29/30 の Edge Ring 以来
棚上げされていた「右 edge 構想」（doc 50 §2）が、実物を得て再起動した。
**Owners**: vp-app（webview + main_area.rs）
**Related**: [55-board-projection.md](./55-board-projection.md)（board 取っ手 = rail の最初の
住人。視認性の教訓もここから）/ [50-pane-chrome-and-session-panes.md](./50-pane-chrome-and-session-panes.md)
（§2 = pane ツールの hover 召喚は「暫定形。恒久の home は右 edge 構想 = 棚上げ中」）/
[51-zero-base-review.md](./51-zero-base-review.md)（§1 A1 = 下端の帯の退役。+ New が LaneHeader に
移った経緯）

> 各決定に mako の決め言葉を引用してある（facts-over-narrative）。

---

## 1. 原理: 動詞の級 = 住所

VP の動詞は「何に効くか」で級が分かれ、**級がそのまま UI 上の住所を決める**。

| 級 | 効く範囲 | 住所 |
|---|---|---|
| **lane 級** | lane 全体（対象を選ばない） | **edge rail（右端の帯）** |
| **pane 級** | 特定の pane（「どの pane に」が要る） | 各 pane の名札（hover 召喚 — doc 50 §2 の現状維持） |
| **app 級** | app 全体 | サイドバー（⚙ 設定 = 下部、daemon status の上 — §7） |

> mako「lane ヘッダと pane ヘッダの話だよね」「二つ動線は混乱する。選択肢はまず 1
> （= lane 級専用の rail）」

- pane 級を rail に住まわせない理由: 「Clear を押したらどの pane が消えるのか」という
  **対象解決の曖昧さ**が生まれる。これは Edge Ring が当時棚上げされた難所そのもので、
  lane 級専用と決めれば構造的に消える
- この原理は今日の議論で 3 連続で効いた: ① rail は lane 級専用 / ② 設定は app 級だから
  rail でなくサイドバー（mako の直感が分類と一致）/ ③ pane 級は名札に残留。
  以後の新機能も「この動詞は何に効くか」を問えば住所が決まる

---

## 2. 決定: B 形（帯）が既定、A 形（浮遊）は設定トグル

> mako「B で、lane に対して何ができるか、初めて使う人でもわかるようにしたいね。
> A は、将来的に設定画面（けっこうすぐほしいけど w）をおいて、トグルボタンで
> 切り替えることができる。というふうなのは、どう？」

| 形 | 中身 | 位置づけ |
|---|---|---|
| **B: 帯** | 不透明の縦帯（幅 36px）。右端に常設 | **既定** — rail を見れば「この lane に何が
できるか」が一覧できる = 初見への self-documentation |
| **A: 浮遊** | ボタンだけが浮かぶ（doc 55 P1 の取っ手の形） | 設定画面のトグルで切替（将来） |

- 占有は**静的**: `#lane-panes` / `#lane-header` が right:36px に退避。rail は常設なので
  **reflow イベントは発生しない**（doc 55 の reflow 規律と矛盾しない）
- 発見性の三段構え: 帯の常設 + **hover で label が左に滑り出る** + 新着 badge glow。
  doc 55 P1 の教訓「暗色 on 暗色の取っ手は存在を知らないと気づけない」への応答
- form が user preference になるのは doc 55 の board form（float/docked）と同じ手筋 —
  「同じ実体の見え方を属性で切り替える」語彙の再利用

> **追記（2026-08-01、sidebar view modes）**: rail に第 3 の形が加わった —
> **R sidebar（rail のフル幅形、420px）= debug log viewer**。`Cmd+]` で
> rail ⇄ R sidebar を行き来する（展開中は帯を隠す排他表示）。左 sidebar も対で
> `Cmd+[` によりフル（280px）⇄ スリム帯（44px）を持つ。binding の経緯は
> doc 18 §B.1 v1.1、実装は right-sidebar.ts / sidebar bundle の form.ts。
> A 形（浮遊）の設定トグルはこの追記後も未実装のまま（§2 の位置づけ不変）。

---

## 3. 住人（2026-07-30 時点、上から）

| 住人 | 由来 | 挙動 |
|---|---|---|
| **＋ New** | LaneHeader から machinery ごと移設 | click → `conversation:agents_fetch`（相関 id）→ engine × mode menu が**帯の左横**に開く → `console:new_session` |
| **🧭 board 取っ手** | doc 55 P1（右端タブ → rail の住人へ住み替え） | 開閉 toggle + 新着 badge（+ glow）。**配線 = board-view.ts は 1 行も変わっていない**（id 依存だけなので住所は CSS と親要素の変更で済んだ — view 所有を module に閉じた設計の配当） |
| （空き） | — | 将来の lane 級動詞 |

- 生成系（New）が最上段: 旧位置（右上）に近く筋肉記憶が繋がる
- lane 不在時は **帯ごと消える**（rail は lane 級動詞の家 — lane が無ければ意味を持たない）

---

## 4. LaneHeader の純化 — 「読むのは上、押すのは右」

> mako「二つ動線は混乱する」

- LaneHeader の + New は**撤去**（両方に残さない = 動線一本）。上端の帯は lane の素性
  （名前・cwd・branch・session chips）を**読む**場所へ一歩純化した
- 残る操作 = root 切替 picker。これは root chip（読み）と一体の操作なので上に残す
- 分業の完成形: **上 = 読む（素性）/ 右 = 押す（lane 級動詞）/ 名札 = pane 級 / サイドバー = app 級**

---

## 5. 実装（prototype、`mako/edge-rail-p1`）

- **`EdgeRail.tsx` 新設**: + New の machinery（agents_fetch 相関 id / menu / new_session）を
  LaneHeader から移設。mount API = `setLane(addr | null)`（entry.tsx の applyActivePane が
  lane 追従・不在時は帯ごと隠す）
- **LaneHeader**: + New の button / menu / signals / listener / CSS を撤去
- **main_area.rs**: `#edge-rail` DOM（`#edge-rail-new-host` + `#board-handle`）+ 帯 CSS +
  `.rail-btn` 共通スタイル + hover label（`data-label` → `::after`）+ 36px 静的退避
- float の clamp 境界（`#lane-panes`）は退避に自動追従 — board float は帯の左で止まる

---

## 6. 検証（実機 dogfood、2026-07-30）

- 帯の表示 / New menu が左横に開く / hover label / 取っ手の全機能（B・N・badge glow）/
  LaneHeader からの New 消失 — mako「いい感じ」
- 既知の別件: + New menu の中身が「claude - Console」のみに痩せる bug は **rail 移設とは
  独立**（`conversation:agents_fetch` 経路は本 PR の diff が触れていない）。
  creo `mem_1CdXsfmNfQ4egTNATPCpx6` で別 PR 対応

---

## 7. 今後

- **設定画面**（app 級・サイドバー下部・daemon status の上）= 別糸
  （creo `mem_1CdXtkPWmtmikXwygbed5B`）。最初の設定項目 = rail の形態トグル（B ⇄ A）。
  A 形の実装は設定画面と同時で良い（トグルの読み手が居ない間は B 一本）
- **pane 級の恒久の家**（doc 50 §2 の宿題）は本 doc のスコープ外のまま — 名札 hover 召喚を
  継続。rail に持ち込む場合は対象解決（focused pane への暗黙適用）の設計が先
- 住人が増えたらゾーン分け（生成系 / 面系 / 通知系）を §3 に追記していく
