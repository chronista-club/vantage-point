# 87. 額縁を creo-ui の 3x3 に載せる — Shell / Layout / Frame と 2 つの Scene

> **Status**: Conception 確定（2026-11-05、mako × lead の一問一答）。実装未着手。slice 1 は creo-ui への要望 ①〜③（§6）の着地を待つが、consumer 側の仮実装で先行してもよい
> **Related**: doc 49（LayoutEngine の creo-ui 基盤化、層 L0〜L5）/ doc 46・50（pane の額縁の縦軸）/ doc 51（表象の共有）/ doc 56（edge rail = 動詞の級 = 住所）/ doc 58（名簿）/ doc 59（設定ページ）/ doc 82（R sidebar）/ doc 86（Editor Link hub）/ creo-ui `docs/design/principal-layout.md`（PL-1〜PL-8）/ `layout-engine.md`（LE-1〜LE-21）/ `frame-system.md`（P-6 = VP 統合）
> **Task**: creo todo `mem_1CgjWv9eG4T1bkF8z2xrGo`（Epic）/ Conception の帳簿 `mem_1CgieMq1PCG68haZ3zACmZ`（handoff）
> **対象**: `crates/vp-app/webview/`（`src/sidebar/Shell.tsx` / `shell-layout.ts` / `right-sidebar.ts` / `EdgeRail.tsx` / `app-panes.ts` / `lane-panes.ts` / `LaneHeader.tsx`）、`crates/vp-app/src/webview/main_area.rs`（静的 DOM）

## 1. 何を決めたか

mako（2026-11-05）:

> frame と layout をしっかり導入して、これらを使ったあるべき姿にリデザインしていこう。
> GFP や CreoMemories が creo-ui を使うようにするから、VP も合わせていこうかと。
> いい感じにレイアウト、frame による画面構成なんかは、共通エンジン (creo-ui) で組めれば良いなと。
> ここのパーツは、増やせるように拡張性持たせつつだね。／個々のパーツ／あとは Editor との統合
> Principal Layout の「3x3 Frame」とか creo-ui の layout でやりたいね。
> VP の設定 frame と、通常の開発の 2 つの frame？scene？ほしいなまず。
> Editor パネルは、float か、もしくは、できれば別 Window がいい

- **額縁 = Principal Layout**（creo-ui core 0.30.0 の `shells`: `CUEdgeShell` / `CURail` / `railRegistry` / `regions`）、**中身 = creo-ui-layout**（0.3.0、VP は既に app / lane の tiling で採用済）。**creo-ui-frame（3D morph）は対象外**。doc 49 §2 が「creo-ui-frame は morph engine で分割を持たない」と見立てたとおり、残っていた欠けは「額縁」で、それは Principal Layout が持っている
- **語彙は 1 語 1 仕事**: **Shell**（額縁・領域・Rail）/ **Layout**（分割・注視、Scene = 場の snapshot）/ **Frame**（遷移・奥行き、optional）。「frame による画面構成」は Shell の仕事
- **額縁は 1 つ、Scene は 2 つ**（開発 / 設定）。設定は別の額縁ではなく、左 rail の選択と center の見え方が変わる場面（§3）
- **Editor panel は float か別 Window**。右の tool rail には収めない。別 Window は doc 86 の Link hub に繋がる controller として作る（§5）
- **増やせる部品は 3 つの registry** で受ける（Rail / edge の住人 / pane kind。§4）。primitive は意味づけせず、app が registry を渡す（PL-3 と同じ向き）

### 1.1 「frame」は 3 か所で別物だった

| 語の出どころ | 何を指すか |
|---|---|
| creo-ui-frame（package、npm 未公開） | 名前付き 3D 配置（slots × perspective × transition）の morph。分割は持たない |
| Principal Layout（core の `shells`） | 「3x3 Frame」= 4 edge が center を囲む 2D 額縁 + Rail。コード上の語は **shell** |
| VP の名残 | `data-frame-id`（main_area.rs の 5 pane）と `--frame-transition-ms`。旧 frame-engine.ts の語彙 |

doc 49 §5 が 2026-07-22 に「新 engine は layout と呼び、frame は motion 層の名に残す」と裁定済。本 doc はそれを額縁側に延ばし、VP の名残（`data-frame-id`）は pane id に改名して消す。

## 2. あるべき姿 — Edge Ring の自己相似

doc 50 が app の Edge Ring の縦軸を pane に再適用したので、VP の画面は既に「同じ形の入れ子」になっている。primitive を共通にすると 1 種類の部品で書ける。

```
CUEdgeShell（app）
  upEdge    = 世界の 1 行（要求中 N / 更新あり / daemon・hub・MIDI lease）※住人の移設は別 PR
  leftEdge  = CURail（起点）: Lanes / Actions、下起点に Settings。peek = 選択 rail の中身
  center    = creo-ui-layout（app scope: lane / runner / devices / preview / …）
  rightEdge = Rail（ツール）: PRs / Log。peek = 選択 tool の面
  downEdge  = 空（描かない）
    └ lane pane = CUEdgeShell（lane）
        upEdge    = 名札（lane 名 / cwd / branch / session chip）+ now-line の 2 段 = 今の LaneHeader
        center    = creo-ui-layout（lane scope: session pane + board）
        rightEdge = lane 級動詞（上 = + New / 中央 = しまった pane / 下 = board 取っ手 + PR）= 今の EdgeRail
        left / down = 空
```

今の部位との対応（2026-11-05 実測）:

| region | app スケール（今） | app スケール（後） |
|---|---|---|
| leftEdge | `#sidebar-root`（名簿。full 180〜640px / slim 44px） | CURail の列（48px）+ peek。slim = 列だけ、full = peek 展開。名簿は Lanes rail の peek の中身 |
| center | app panes（creo-ui-layout、済） | 変えない |
| rightEdge | edge rail 36px（lane pane の中）⇄ R sidebar 25%（最小 320px） | tool Rail + peek。rail の住人は lane の額縁へ（下表） |
| upEdge | 無し | 1 行の静かな帯（枠だけ先に。住人は移設なので別 PR） |
| downEdge | 無し | 空のまま（PL: 中身が無い edge は DOM を描かない） |

左右が同じ形（rail 列 + peek）になるのが要点で、違いは「起点か道具か」という意味だけ。creo-ui への要望①はここから来る。

lane の rightEdge は「rail 列だけで peek 無し」。PR を押すと **外側の額縁**（app の rightEdge）が PRs の面を開く — 今の `vp:right-sidebar-reveal` に当たる。内側は動詞を持ち、外側は面を持つ。この向きを守ると lane 級と app 級の動詞が混ざらない（doc 56）。

## 3. 2 つの Scene — 開発と設定

| | 開発 | 設定 |
|---|---|---|
| 左 rail の選択（起点） | Lanes（peek = 名簿、doc 58） | Settings（peek = 設定の分類、doc 59 の項目群） |
| center | creo-ui-layout の tiling（最後に settle した姿） | 設定ページが **solo**（他の pane は attention 0 = 非表示） |
| 右 rail（ツール） | PRs / Log | 畳んだまま |
| upEdge | 同じ 1 行 | 同じ 1 行 |

- 額縁は羅針盤であって格子ではない（doc 29 §3.5、PL-8）。設定に行っても左は起点、右は道具、上は世界、という方位が変わらない方が、戻ってきた時に迷わない。だから設定は別の額縁にしない
- **場面の語は Scene**。creo-ui-layout の Scene（場の名前付き snapshot）に、左 rail の選択を添えた組を 1 つの場面として扱う。設定 = settings pane の solo、開発 = 最後の settle。solo を解くと前の settle に戻るのが mixer 語彙の既定なので「設定から作業へ戻る」は 1 規則で済む
- **選択の SSOT は pseudo-route**（PL-4「route が唯一の SSOT」）。VP に router は無いので文字列を 1 本持つ: `/lanes/<addr>` が開発、`/settings/<section>` が設定。左 rail の選択も center の Scene もここから導く。AI も `layout_*` と同じ address でこの場面を指せる（doc 51 表象の共有）
- 今の設定 UI は sidebar bundle の overlay（`SettingsPanel`、doc 59 P1）。新しい額縁では center の住人になり、overlay ではなくなる

## 4. 増やせる部品 — 3 つの registry

primitive は意味づけせず、app が registry を渡す（PL-3）。VP の 3 か所を同じ形に揃える。

| registry | 中身（最初） | 今の形 |
|---|---|---|
| Rail（左 edge の相） | Lanes / Actions / Settings | sidebar に直書き（`Shell.tsx`） |
| edge resident（右 edge の lane 級動詞） | + New / しまった pane / board 取っ手 / PR。entry は上・中央・下の 3 起点 slot を持つ（doc 82 の規約） | `EdgeRail.tsx` に 4 住人が直書き |
| pane kind（center の種類） | lane / runner / devices / preview / gallery | `APP_PANE_IDS` の const が main_area.rs の静的 HTML と 1:1。1 種類足すのに Rust と TS の両方を触る |

右 edge と lane の rightEdge は、同じ `RailDef` 型で「起点か道具か」だけ違う。

## 5. Editor との統合

- 各 region が field（幅 / form / 配色）を持ち、registry の entry は自分の field（D-13 knob）を同梱できる。MCP からは doc 86 の Link hub 経由で同じ field に届く
- GFP と CreoMemories も同じ region 語彙（`leftEdge` / `center` / …、Editor Mode の 4-region と同一）なので、Editor の触り方が揃う
- **panel の居場所**: 近手は今の float（`#editor-root` の overlay）のまま。あるべき姿は **別 Window = Link hub の controller**。vp-app は window ごとに面 `vp-app#<N>` として hub に繋がっている（doc 86 §2.6）。Editor Window は面ではなく controller として繋がり、active な面の fields を表示して `set` を送る。1 枚で vp-app も Chrome の creo-ui site も GFP も触れ、ROTO のノブ（doc 86 §4）と MCP と Editor Window が同じ hub の 3 つの controller になる。足りないのは Link protocol の controller 役（hub → controller に fields / values、controller → hub に set）で、creo-ui の editor-link.md 側の拡張（要望④）

## 6. creo-ui への要望

creo-memories（creo-web）も GFP（desktop / portal）も creo-ui 0.30.0 を入れているが、`shells` と `creo-ui-layout` はまだ誰も使っていない（2026-11-05 grep）。VP がここでも最初の consumer になる。

| # | 要望 | 理由 |
|---|---|---|
| ① | `CURail` を右 edge にも置けるように（`region` を受ける。今は `data-region="leftEdge"` と `border-right` が固定） | 左右同形の額縁（§2） |
| ② | peek の幅を外から与える口（今は 240px 固定） | VP は 180〜640px の resize と永続が要る（`shell-layout.ts`） |
| ③ | 選択の供給を pseudo-route で成立させる（PL-4 と両立する形の確認） | VP に router は無い（§3） |
| ④ | Editor Link の controller 役 | 別 Window の Editor（§5） |
| ⑤ | Scene を「場面」の語に使う相談 | creo-ui-layout の Scene（場の snapshot）と同じ語を、rail の選択を含む場面にも使う（§3） |

①〜③は slice 1、④は Editor Window、⑤は語彙。要望は wire で creo-ui lead へ送り、principal-layout.md に VP consumer の Phase（P-5 相当）を足してもらう。

## 7. 技術の骨

- 額縁は今 Rust の静的 HTML（`main_area.rs` の `MAIN_AREA_HTML`、1025 行の file）で、Solid root は 8 か所に散っている（sidebar / Editor / EdgeRail ×3 / R sidebar PRs / resync / gallery）。bundle は 2 本（`sidebar.bundle.js` / `editor-host.bundle.js`、同一 document）。CUEdgeShell を使うには額縁を Solid 側へ移す
- doc 49 R1（DOM を reparent しない、xterm は再生成不可）を守る: 「額縁だけ Solid に、中身の DOM は据え置き」で段階的に進める。pane host は Solid が 1 度 mount したら動かさない
- PL-2「sidebar を禁ずる」: `#sidebar-root` / `#right-sidebar` / `--sb-*` は改名対象。token は creo-ui token（約 515 参照）と VP 独自 var（`--sb-* / --lg-* / --chat-* / --vp-*`、約 414 参照）が併用で、`Shell.tsx` に Lucid Glass の色の直書きがある。slice 3 で畳む
- Swift native 移行（doc 25 系）とは独立。webview の UI は WKWebView でもそのまま載る

## 8. 進め方

| slice | 範囲 | 待つもの |
|---|---|---|
| 1 | 額縁の載せ替え: CUEdgeShell を外殻に、左 = CURail（Lanes / Actions / Settings、slim 44px = rail 列 / full = peek）、右と center は見た目維持で region へ、upEdge は枠だけ、語彙の改名、pseudo-route | 要望①〜③（仮実装で先行可） |
| 2 | registry 化: pane kind / rail / edge resident。`APP_PANE_IDS` 撤去 | — |
| 3 | Editor: region field と registry 同梱 field。`--sb-* / --lg-*` を creo-ui token に畳む | — |
| 4 | lane スケール: LaneHeader と EdgeRail を lane の CUEdgeShell に | — |

slice 1 の mako 裁定（2026-11-05、GO）: 左 sidebar は Rail + peek の形に寄せる。slim 44px が rail 列、full が peek になるだけで操作は変わらない。

## 9. 切ったこと

- creo-ui-frame の採用（3D morph）。optional の遷移実行系として後から載せられる
- Editor panel を右の tool rail に収める（mako: float か別 Window）
- 設定を別の額縁にする（羅針盤を変えない）
- upEdge の住人の移設（要求中 N・更新あり・daemon / hub / MIDI lease）を slice 1 に入れる — 枠だけ作り、住人は別 PR
- Devices を左の rail にする（doc 81 の 3D 表示は pane の広さが要るので center のまま）

## 10. 検証

- unit: pseudo-route → rail 選択 / Scene の導出（純 calculation、vitest）。registry 1 行足すと描画・選択に出る contract test（PL §5.2 の枠を借りる）
- 実機（app:swap）: slim / full の往復、R の開閉、lane 切替で tiling が保たれる（R1）、設定 ⇄ 開発で前の settle に戻る、Editor（float）が従来どおり開く
- vp shot で 3x3 の region が `data-region` で見える（AI の address、doc 51）

## Status log

- 2026-11-05: Conception 確定 + 起草。mako 裁定: Principal Layout の 3x3 + creo-ui-layout / 語彙 Shell・Layout・Frame /
  Scene 2 つ（開発・設定）/ Editor は float か別 Window / slice 1 は Rail + peek。creo-ui への要望 5 点を wire で送付。
