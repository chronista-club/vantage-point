# 52. board 再設計 — 貼る台と描く層のゼロベース

- 起点: 2026-07-24、mako × Claude。doc 51 §6（AI が視せる面）の続きとして、A4（PP の lane pane 化）の実装前議論から派生
- 発端: mako「そもそも、canvas と board という概念って被ってるよね。しっかり board を仕様から議論したい」
- 規範: doc 51 と同じゼロベース流儀（必要の側から再審査）+ Echoes rebuild 規律（今の設計と合わない旧実装はオミット。旧経路は即撤去、中間状態を残さない）
- 事実の土台: 現状実測の地図（Artifact「canvas / board 現状地図」2026-07-24。語彙 5 義 / データ構造 / lane ヒエラルキー / 配信経路 / 発見 5 件）
- 進め方の記録: この議論の途中、実演として現状地図を board に貼った。そこで採取された摩擦（§8）が本 doc の要求の多くを供給している — mock の代わりを実物の dogfood が果たした

---

## 1. 決定: canvas と board は別の道具

### どうしたいか（mako）

> canvas は本質的な役割として Drawing、描くための新しい Heaven's Door として再度復活させたいから、他の部分との重複は避けたい

- **canvas という語は「描く」に予約する** — 新生 Heaven's Door 📖（Canvas Author、doc 31）の道具
- **board は「貼る台」** — PP のデータモデル。board 系は canvas 語彙から全面退去する

### どうすべきだったのか（回顧）

- 2026-07-15 の board モデル化で意味論は「貼る」に移住済みだった（ShowParams の scope 説明は既に "Board to **pin** this content to"）。名前の世界だけが「描く」時代に取り残された
- 語の解決は「namespace 予約」で行う: 未来の道具（HD Drawing）に語を先に割り当てることで、現住 5 者の調停が「全員退去」で一意に解けた

### 何が必要か

- §6 の語彙清算（channel 名 / Stand id / tool・型名の退去）
- HD 復活自体は本 doc のスコープ外（doc 31 系の別議論）。ここでは**席を空ける**ことだけを請け負う

---

## 2. board の役割は 4 つ

| 役割 | 動詞 | 向き | 現状 |
|---|---|---|---|
| **掲示板** | 貼る（show / append） | AI→人 | ✅ 動いている（#771 server-authoritative） |
| **計器盤** | 更新する（identity + update） | AI→人 | ❌ 無い（doc 51 §6 で予約済み、§5） |
| **中継台** | 取り出して次へ流す（read → creo 等） | AI→人→外 | ❌ 死んでいる（意図は read_pane に化石、§4） |
| **対話面** | 上に描いて返す（注釈） | **人→AI** | ❌ 新設（§3） |

- 掲示板〜中継台は AI→人 の一方向だったが、対話面で**面が往復になる**
- 4 役割すべてが item の identity（安定 id + 参照可能性）を前提にする → identity が共通土台（§5）

---

## 3. 対話面（新設）— 貼ってあるものの上から描く

### どうしたいか（mako、2026-07-24 の着想そのまま）

> 「貼ってあるものの上から描く」というのもあるといいかもしれない。貼ってもらったものに対して、その上位透明レイヤーをおいて、そこに何か書ける。それがあると、**言葉より少ない操作で、より意図をあなたと共有できそう**。

> 対話面、まだみたことないし、欲しい人いっぱいいそう。

- 実在ワークフローの置き換え: 現状は Shottr で screenshot → line / arrow / text で注釈 → chat に貼り直す、という「面から一度降りる」迂回をしている

### 原理接続

- **表象の共有**（doc 51 §0.1）の最終形: 同じ面の上で双方向にやりとりする
- **連続値は指・構造は言葉**（横断原理）: 空間的な指示（この表のここ / この 2 つの関係）は言葉だと長く曖昧、丸 1 つ・線 1 本なら一瞬で正確。描画は「場所と関係を指す指」— MIDI フェーダー（量の指）と対になる
- canvas / board を別道具に分離（§1）したからこそ「重ねる」という合成が定義できた

### 仕様骨格（v1）

| 項目 | 決定 | 根拠 |
|---|---|---|
| 構造 | board item の上の透明レイヤー | mako 着想 |
| パレット | **line / arrow / text / freehand の 4 つ** | Shottr 実使用の実測（mako は 3〜4 tool しか使わない） |
| 読み方 | **semantic anchor 解決** — webview が座標を content 位置に hit-test し、構造データで AI に渡す | engine 中立（vision 非依存で全 engine が読める）。下地が pixel でなく自前 render の文書だから可能 = Shottr より質的に上 |
| freehand の解決 | 内包（囲み → 要素列挙）/ 交差（下線・なぞり → 対象解決）。軌跡の形そのものの意味は解かない | 注釈用途に十分。形を描いて伝えるスケッチは **HD の領分**（住み分け） |
| 届け方 | **明示送信**（描いてから送る。v1 で感触探り） | mako「明示送信でまず感触探りたいね」。wire の command 意味論と同族（誤配なし） |
| 画像合成 | 将来の補助（vision 対応 engine 向けの追加情報）に格下げ | 中立の主経路は構造データ |

```
AI が貼る（board item）
  → 人が見る
  → 人が上に描く（line / arrow / text / freehand）
  → 明示送信
  → AI が読む（item + 注釈の構造データ:
      { type: "arrow", target: "§5 > finding[0]", ... }
      { type: "text", anchor: "§2 table row 3", content: "ここ古い" }）
  → AI が動く（修正 / creo へ / 次の一手）
```

- 道具を絞ると能力が上がる構図: 自由スケッチ許容 → 読み手 vision 必須 → 中立性崩壊。4 primitive 限定 → 注釈が構造データ → 全 engine + テストで扱える

---

## 4. 中継台 — read 口の復活

### どうしたいか（mako）

> board に貼ったものの更新や取得、それをそのまま creo に投げてメモリ追加する。みたいなのができなかったなーと。この辺はやりたい。

### 考古学的事実

やりたいことは**元々仕様にあった**。死んでいる read_pane の tool description に意図が化石として残っている:

> "Read the full source content of a Paisley Park Canvas pane … **so it can be saved to creo-memories (mcp__creo-memories__remember)** or otherwise processed."

board モデル化（2026-07-15）で書き込みが board 経路に intercept され、読み手（retained Show を読む list_canvas / read_pane）が無音で死んだ — reader-without-writer（memory `writer-without-reader` の鏡像）。

### 何が必要か

- board を読む口の新設（DB / retained BoardUpdated を読む。旧 retained-Show 経路は復活させない）
- item の**全文**取得（preview では creo に投げられない）
- **指し方の既定 =「今表示してるもの」**（mako 決定 2026-07-24）: read 口の無引数呼び出しは「mako がいま GUI で表示している item」を返す。旧 read_pane の無引数既定（pane が 1 つならそれ）の後継が「注視中のそれ」になる形
  - 前提: **注視（activeLane + cursor）の AI 可視化が必須**（摩擦 ③ が要求に昇格）。cursor は現在 view local — 共有状態への昇格が要る（実装形は §9: webview → SP への注視 sync か、on-demand 往復か）
  - 役割分担: 言葉の「今の」= item 全体を指す / 対話面の注釈（§3）= item 内の場所を指す。粒度の違う 2 つの指しが補完する

### しまい方の原則（mako 決定 2026-07-24）

> creo はいかに次のことを考えて、格納するかで、メモリの使い勝手が変わってくるから、データはそのままに、しまい方は、あなたを経由したい

- **中継台は配管ではなく AI の手**: board と外部（creo 等）を直結する server 側統合・GUI の export ボタンは作らない。read 口が AI に全文を渡し、**しまい方（要約・タグ・リンク・行き先 = 将来の検索可能性の設計）は AI が担う**
- **データは無加工**: item の content はそのまま渡り、そのまま保存対象になる。AI が付与するのはフレーミングだけ
- 帰結 — 出口は増やさない: 読める口が 1 つあれば、出口（creo / doc 化 / wire / PR 本文…）は AI の判断の数だけある。N 個の export 統合を作る道を封じる

---

## 5. 計器盤 — item identity は全役割の共通土台

- doc 51 §6 から継承: id 指定 update（「この表を貼っておいて、進むたび更新して」）+ pin / stream の区別
- 本議論での格上げ: **identity は計器盤だけの要求ではない**。中継台（どれを取り出すか）も対話面（どの item の上の注釈か）も安定 id を前提にする → データモデル改修の最初の一手
- 実演でも実証: 議論用に貼った現状地図を、議論の進行に合わせて更新できなかった（摩擦 ①）

---

## 6. 語彙の清算 — canvas 5 義の解体

| # | 現 canvas 用法 | 行き先 | 負債の性質 / 時期 |
|---|---|---|---|
| ① | GUI 全体の広義（「TUI で操る、Canvas で視る」） | 廃語。koan の「Canvas に描く」は HD の意味に純化 | 文書のみ / doc 更新時 |
| ② | PP の表示面（#pp-content） | board の面と呼ぶ | encapsulated / 実装時 |
| ③ | channel 名 `"canvas"`（実態: vp-app への配信路の総称） | 要 rename | **viral**（client/server/topic path に散在）/ 実装第一波と同時 |
| ④ | Stand id `canvas`（address `canvas@project/lane`） | 要 rename | **viral**（wire address）/ 実装第一波と同時 |
| ⑤ | tool・型名（`list_canvas` / `CanvasPane` / `CanvasItem` / `canvas-handler.ts`） | board 語彙へ（`BoardItem` は既に居る） | encapsulated〜中間 / 設計確定と同時 |

### 命名の決定（mako 2026-07-24）

| 対象 | 旧 | 新 |
|---|---|---|
| QUIC channel | `"canvas"` | **`"gui"`**（実態 = World → vp-app の配信路） |
| PP の Stand id | `canvas`（`canvas@project/lane`） | **`board`**（`board@project/lane` — Echoes の `agent` と同じく機能を名乗る層） |
| functional name | Navigator / Router 割れ | **Information Navigator に統一**（stands.rs 現行に寄せ、GUI の "Information Router" 文字列を修正） |
| `capture_canvas` | — | **`capture_window`**（撮っているもの = VP の window。CLI `vp shot` と意味が揃う） |

- **Navigator / Router の割れの根治**: 表示名を stands.rs から引く配線にする（文字列複製はコンパイラもテストも黙る — memory `type-flatten-string-leftovers` と同型。今回の割れがまさに実例）

---

## 7. オミット（Echoes rebuild 規律の適用）

| 対象 | 判定 | 理由 |
|---|---|---|
| `list_canvas` / `read_pane`（現実装） | **撤去**（新 read 口で置換） | retained Show を読む設計ごと死んでいる。復活でなく §4 の新設計で置換 |
| ShowParams の `pane_id` | **撤去** | doc 19 時代の dead field（board は `paisley-park` 固定）。v2 削除候補と明記済み |
| content_type `url` の口 | 未決（§9） | 現状は受理して board が捨てる silent 消失。口を閉じるか board が url を持てるようにするか、掲示板の容量の議論として決める |
| 旧 retained Show の topic 経路 | **撤去** | 書き手ゼロ。読み手（上記 2 tool）撤去と同時に完全に死ぬ |

---

## 8. 摩擦台帳（面 B、2026-07-24 の実使用で採取）

現状地図を board に貼る実演で採れたもの。§3〜§5 の要求の一次ソース。

1. 貼った item を後から**更新できない**（identity 無し）→ §5
2. 貼った後に自分で**読み返す口が無い** → §4
3. 書き手から **mako の activeLane / cursor が見えない**（届いたか・何を見ているか不明）→ §4 で要求に昇格（実装形のみ §9）
4. **長文 item の表示が弱い**（実機で本文が分断/クリップ表示）。document サイズの content は表示設計の想定外 → §9

---

## 9. 未決（次の議論で）

- read 口の tool 粒度: list + get の形（指し方の既定は §4 で決定済み —「今表示してるもの」）
- 注視可視化の実装形（可視化すること自体は §4 で決定済み）: webview → SP への注視 sync（cursor を共有状態に昇格 — server-authoritative の家風と整合）か、AI 要求時の on-demand 往復か
- update 口の形: id 指定 replace か patch か。pin / stream の区別の実装
- url / image content の扱い（§7）
- 長文 item の表示（摩擦 ④）: board の表示設計 or 中継台で doc 化に流すのが正か
- ~~A4（PP の lane pane 化）との実装順序~~ → **決定（2026-07-24）: A4 は「board pane」の名で先に単独出荷**（mako「A4は『board pane』の名で先に単独出荷でいこう」）。表示の器を lane に移してから中身（identity / read 口 / 対話面）を工事する。新規コードは最初から board 語彙（canvas kind とは呼ばない — §6 ⑤ の先取り）
- 対話面の実装詳細: レイヤーの永続（item に付随して残るか、送信で消えるか）、複数回の往復

## 10. 実装順序（2026-07-24 確定）

### ✅ wave 0 完了（2026-07-24、branch `mako/a4-board-pane`、実機 dogfood 済み）

board pane 移設 + canvas 語彙退去を一波で出荷。8 commit:
1. doc 52 起草
2. **board pane 移設** — app 層 PP（#pane-paisley-park）退役 → lane tiling の #lane-board。presence 駆動（board 非空で自動）、mode 直交、旧 pp scene（side-review/pp-overlay/pp-focus）退役
3. **死んだ読み手撤去** — list_canvas / read_pane / CanvasPane / fetch_canvas_panes / pane_id。capture_canvas → capture_window
4. **Stand id board / channel gui** — PAISLEY_PARK.id `canvas`→`board`（address `board@`）、QUIC channel `canvas`→`gui` + `canvas-ingest`→`gui-ingest`、Navigator 統一
5. **webview 語彙退去** — canvas-handler.ts→board-handler.ts、CanvasItem→BoardItem、window.vpCanvas→vpBoard
6. **seam fix**（dogfood 発見）— lane key 空間ミスマッチ（board-handler の flat key vs lane-panes の address）を `boardLaneKeyOf` で写して根治
7. **boot 窓 board:demand**（dogfood 発見）— retained BoardUpdated の bundle-ロード前 drop を Rust buffer + consumer-driven demand（bastet:devices_fetch 同型）で埋める

品質ゲート: fmt / clippy -D warnings / cargo test（workspace 1243 + vp-app 115）/ vitest 263 全緑。
実機確認: live show で board pane 生成 + cold boot（reopen）で retained から board pane 復元を screenshot 確認。

**dogfood で見つかった 2 バグ（seam / boot 窓）は webview の pure function テストでは出ず、module 間 seam と Rust↔webview boundary という runtime でしか踏めない箇所だった** — mock でなく実物 dogfood が効いた実例。

### 残り

0. **A4 = board pane を先に単独出荷**（表示の器の引っ越し。doc 51 Epic の A4 — lane roster に board pane を足し、app 層 PP を退役。新規コードは board 語彙）✅
   - 生え方（mako 決定 2026-07-24）: **board 非空で自動** — roster を「board に item がある lane に board pane」と機械導出。畳む/復元は既存 layout 文法（share 0 / live 新着で復元 = 現行 auto-open の lane 文法版）。新しい状態は足さない
   - 旧 PP（同決定）: **即退役で一本化** — scene 群（side-review / pp-overlay / pp-focus）から pp を外し、sidebar の PP クリックは現 lane の board pane focus に読み替え
   - **canvas 語彙の退去も同じ波でできるだけやる**（mako 2026-07-24「このタイミングでできるだけ Canvas をリネームして、新 HD の時にすぐ実装に入れるようにしたい」）: webview の型・ファイル名（CanvasItem → BoardItem 等）+ viral 分（channel 名 / Stand id）を含む。§6 ③④⑤ の前倒し。残置は DB pane_id `"paisley-park"`（encapsulated な const 1 点 + 既存データ保全）のみ許容
1. **identity**（§5、全役割の土台）
2. **read 口**（§4 — 最小で「今表示してるもの」+ creo 連携の輪が回る）
3. **語彙清算の viral 分**（§6 ③④ channel 名 / Stand id）を 1〜2 と同じ波で
4. **対話面 v1**（§3、明示送信）
5. **計器盤 update**（pin / stream 含む）
