# 87. 額縁を creo-ui の 3x3 に載せる — Shell / Layout / Frame と 2 つの Scene

> **Status**: Slice 1〜4 実装済（実機確認待ち、§8.4）。Conception 確定（2026-11-05、mako × lead の一問一答）。slice 1 を実装（2026-11-25、§8.1）。左 edge は consumer 側の仮実装で始め、要望が creo-ui 0.31.0 に入ったので同じ PR の中で本物の `CURail` に差し替えた。slice 2 を実装（2026-11-25、§8.2）— 増やせる部品 3 か所を registry（pure data + 導出の純関数）に揃え、`APP_PANE_IDS` と kind の表 2 枚を撤去。slice 3 を実装（2026-11-25、§8.3）— Editor の field に region を持たせ、左右の幅を Editor / MCP から動かせるようにし、registry の行が field を同梱できるようにした。左 edge の補助色を Light Grid の token に畳んだ
> **Related**: doc 49（LayoutEngine の creo-ui 基盤化、層 L0〜L5）/ doc 46・50（pane の額縁の縦軸）/ doc 51（表象の共有）/ doc 56（edge rail = 動詞の級 = 住所）/ doc 58（名簿）/ doc 59（設定ページ）/ doc 82（R sidebar）/ doc 86（Editor Link hub）/ creo-ui `docs/design/principal-layout.md`（PL-1〜PL-8）/ `layout-engine.md`（LE-1〜LE-21）/ `frame-system.md`（P-6 = VP 統合）
> **Task**: creo todo `mem_1CgjWv9eG4T1bkF8z2xrGo`（Epic）/ Conception の帳簿 `mem_1CgieMq1PCG68haZ3zACmZ`（handoff）
> **対象**: `crates/vp-app/webview/`（`src/sidebar/Shell.tsx` / `shell-layout.ts` / `right-sidebar.ts` / `EdgeRail.tsx` / `app-panes.ts` / `lane-panes.ts` / `LaneHeader.tsx`、slice 2 で `app-pane-registry.ts` / `edge-residents.ts`、slice 3 で `editor/` と `src/sidebar/shell-rails.ts`）、`crates/vp-app/src/webview/main_area.rs`（静的 DOM）

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

doc 50 が app の Edge Ring の縦軸を pane に再適用したので、VP の画面は既に「同じ形の入れ子」になっている。primitive を共通にすると 1 種類の部品で書ける。slice 4 で app（`#app-shell`）と lane（`#pane-lane`）の両方に同じ area 名の grid と `data-region` を適用した（§8.4）。

```
CUEdgeShell（app）
  upEdge    = 世界の 1 行（要求中 N / 更新あり / daemon・hub・MIDI lease）※住人の移設は別 PR
  leftEdge  = CURail（起点）: Lanes / Actions、下起点に Settings。peek = 選択 rail の中身
  center    = creo-ui-layout（app scope: lane / runner / devices / preview / …）
  rightEdge = Rail（ツール）: PRs / Log。peek = 選択 tool の面
  downEdge  = 空（描かない）
    └ lane pane = CUEdgeShell（lane）
        upEdge    = 名札（lane 名 / cwd / branch / session chip）+ now-line の 2 段 = 今の LaneHeader
        center    = creo-ui-layout（lane scope: session pane + board + code）
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

| registry | 中身（最初） | 起票時（2026-11-05）の形 | slice 2 後の形（§8.2） |
|---|---|---|---|
| Rail（左 edge の相） | Lanes / Actions / Settings | sidebar に直書き（`Shell.tsx`） | `SHELL_RAILS`（`src/sidebar/route.ts`）+ 描画の `SHELL_RAIL_REGISTRY`（`Shell.tsx`）— slice 1 で作った |
| edge resident（右 edge の lane 級動詞） | + New / しまった pane / board 取っ手 / PR。entry は上・中央・下の 3 起点 slot を持つ（doc 82 の規約） | `EdgeRail.tsx` に 4 住人が直書き | `EDGE_RESIDENT_REGISTRY`（`edge-residents.ts`） |
| pane kind（center の種類） | lane / runner / devices / preview / empty | `APP_PANE_IDS` の const が main_area.rs の静的 HTML と 1:1。1 種類足すのに Rust と TS の両方を触る | `APP_PANE_REGISTRY`（`app-pane-registry.ts`）。静的 HTML は Rust のまま（doc 49 R1）で、表との 1:1 を test が固定する |

起票時は pane kind の中身に gallery を数えていたが、gallery（`#gallery` hash の dev surface、`gallery-panes.tsx`）は
app pane ではなく、slice 2 の表にも入れていない。

slice 3 で rail の表（`SHELL_RAILS`）は `src/sidebar/shell-rails.ts` に移した（`route.ts` は re-export。§8.3）。

右 edge と lane の rightEdge は、同じ `RailDef` 型で「起点か道具か」だけ違う（あるべき姿）。slice 2 の edge resident は
まだ `RailDef` ではない（§8.2）。

## 5. Editor との統合

- 各 region が field（幅 / form / 配色）を持ち、registry の entry は自分の field（D-13 knob）を同梱できる。MCP からは doc 86 の Link hub 経由で同じ field に届く。slice 3 で幅・配色・文字と registry の同梱を実装した（form は未。§8.3）
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
- PL-2「sidebar を禁ずる」: `#sidebar-root` / `#right-sidebar` / `--sb-*` は改名対象。token は creo-ui token（約 515 参照）と VP 独自 var（`--sb-* / --lg-* / --chat-* / --vp-*`、約 414 参照）が併用で、`Shell.tsx` に Lucid Glass の色の直書きがある。slice 3 で `Shell.tsx` の直書きと左 edge の白い重ね（`#ffffffXX`）を Light Grid の token に畳んだ（§8.3）。`--sb-*` / `--lg-*` の名前は変えていない — 保存済みテーマの値は field id で持つので名前は変えられるが、改名は語彙の改名（`#sidebar-root` など）と同じ PR で行う
- Swift native 移行（doc 25 系）とは独立。webview の UI は WKWebView でもそのまま載る

## 8. 進め方

| slice | 範囲 | 待つもの |
|---|---|---|
| 1 | 額縁の載せ替え: CUEdgeShell を外殻に、左 = CURail（Lanes / Actions / Settings、slim 44px = rail 列 / full = peek）、右と center は見た目維持で region へ、upEdge は枠だけ、語彙の改名、pseudo-route | 要望①〜③（仮実装で先行可） |
| 2 | registry 化: pane kind / rail / edge resident。`APP_PANE_IDS` 撤去 | — |
| 3 | Editor: region field と registry 同梱 field。`--sb-* / --lg-*` を creo-ui token に畳む | — |
| 4 | lane スケール: LaneHeader と EdgeRail を lane の CUEdgeShell に | — |

slice 1 の mako 裁定（2026-11-05、GO）: 左 sidebar は Rail + peek の形に寄せる。slim 44px が rail 列、full が peek になるだけで操作は変わらない。

### 8.1 slice 1 の実装で決めたこと（2026-11-25、lead 承認）

- **左 edge は creo-ui の `CURail`（0.31.0〜）**。最初は同じ DOM 契約の仮実装で始め、下の要望が入った 0.31.0
  （creo-ui#188 の region / peekWidth / resizing と #204 の peekOpen / anchor / slot / keepPeeksMounted を
  #205 で release）に、同じ PR（#1291）の中で差し替えた。`src/sidebar/ShellRail.tsx` は click → rail id の変換・
  `keepPeeksMounted`・`peekWidth`（`--vp-peek-w`）・`resizing`（`body.shell-resizing`）を繋ぐ薄い wrapper。
  DOM の契約（`[data-region="leftEdge"]` / `nav[aria-label="Rail"]` / `button[data-rail][aria-current]` /
  peek `[data-expanded]` / `section[data-peek][hidden]`）は仮実装の時と同じで、`ShellRail.dom.test.ts` が固定している。
  選択の導出は creo-ui の `selectRailId` / `railsByOrder`。仮実装の時点の記録: CURail 0.30.0 を直に使えなかった理由は
  3 つ — peek 幅 240px 固定（要望②）、選択中は peek が必ず開く（slim を表せない）、列に registry の icon
  しか置けない（slim の repo badge と daemon dot、下起点の歯車の置き場が無い）。差し替えは、
  `ShellRail.dom.test.ts` の契約の assert を変えずに wrapper を書き換えるだけで済んだ（test を足したのは drag 中と
  peek 幅の 2 本、fixture は creo-ui の CSS の注入と mount 先を本番に合わせた）
- CURail に無い口として足したもの（= 追加の要望。0.31.0 で全部入った）: `peekOpen`（選択と別に peek を畳む）、rail ごとの
  `anchor`（列の上 / 下の起点）、列の `columnCenter` / `columnBottom` slot、peek の `peekFooter` slot
  （machine 帯）、**peek は全 rail 分を mount したまま選択外を `hidden`**（rail を行き来しても component の
  中の状態 — AddSub form の開閉と入力、`n` directive の登録、PAUSED の開閉、一覧の scroll 位置 — が消えない）
- **pseudo-route** は `src/sidebar/route.ts`（純関数）+ `route-store.ts`（signal 1 本）。`#app-shell` の
  `data-route` / `data-scene` に映すので DOM 検査で場面が読める。設定中に作業（lane / component）を選ぶと
  設定を抜ける（旧 SettingsPanel の dismiss と同じ意味）。Actions にいる間の lane 選択は route を動かさない
- **設定 Scene は既存の `SettingsSurface`**（#host に Portal + inert）を route で開閉する。layout engine に
  触れないので、解けば前の settle がそのまま見える（solo pane 化は pane kind の registry = slice 2 の領分）。
  Settings rail の peek は設定面の行への索引（節見出しは置かない — mako 2026-11-07「ミニマム」）
- **rail 列は 48px**（上の mako 裁定の「slim 44px」から 4px 広げた）: CURail の列幅（48px）と同じにした。
  slim は列だけなので slim の幅も 48px になる
- **directive**: `b`（ACTIONS capture）は `/actions` へ一時遷移し、取り消し（Esc / 無関係なキー / 5 秒）で
  入る前の route と形に戻す。区画を選んだ・ACTIONS の入力欄に打ち始めた時は留まる。間に user が別の場所へ
  移っていたら戻さず、間に作業の選択が変わっていたら戻り先にも上の規則を当てる（`restoreAfterTransient`）。
  `n`（New Sub）は form が Lanes の peek に住むので、先に Lanes の peek を開く
- **幅**: 保存値 `sidebar_width`（180〜640）は **peek の幅**。leftEdge = rail 列 48px + peek（`--vp-peek-w` を
  CURail の `peekWidth` に渡す）、slim = 列だけ。`#sidebar-root` は中身の幅で、幅の動きは CURail の peek だけが持つ。R の % は grid の right track に当てる（`--vp-rsb-pct`）—
  grid item の % 幅は track に対して解決され、auto の track では効かないため
- **3x3**: `#app-shell` を CUEdgeShell と同じ area 名の grid にし、`data-region` を #sidebar-root / #host /
  #right-sidebar に付けた。CUEdgeShell の component 自体は mount しない（#host を Solid の中へ移すと
  doc 49 R1 を破る）。upEdge は track だけで DOM は描かない
- **改名**: `data-frame-id` → `data-app-pane`（`data-pane-id` は active-pane.ts が lane address を書く動的
  属性で、寄せると VP-141 の衝突が再発する）。`--frame-transition-ms` は motion の token なので残す

### 8.2 slice 2 の実装で決めたこと（2026-11-25）— 3 つの registry

task `mem_1ChNtFpUyeEstk7sXoUB4x`。VP の増やせる部品 3 か所を、同じ形の registry に揃えた。

| registry | 表（pure data） | 1 行の型 | 描画の口（id で引く） | 契約 test |
|---|---|---|---|---|
| rail（左 edge の相） | `SHELL_RAILS`（`src/sidebar/route.ts`） | `ShellRailSpec`（id / route / order / label / anchor） | `Shell.tsx` の `RAIL_VIEW: Record<ShellRailId, …>` → `SHELL_RAIL_REGISTRY`（creo-ui の `RailDef[]`） | `route.test.ts`（表を反復）/ `shell-rail-integration.dom.test.ts`（本物の Shell の列と 1:1） |
| edge resident（lane の右 edge の住人） | `EDGE_RESIDENT_REGISTRY`（`edge-residents.ts`） | `EdgeResidentDef`（id / anchor / order / hostId / relocateToRightSidebar / content） | `EdgeRail.tsx` の `views: Record<SolidEdgeResidentId, …>` | `edge-residents.test.ts` |
| pane kind（center の種類） | `APP_PANE_REGISTRY`（`app-pane-registry.ts`） | `AppPaneDef`（id / domId / kinds / scene / inCycle / order） | 無し（pane の DOM は main_area.rs の静的 HTML） | `app-pane-registry.test.ts` / `active-pane.dom.test.ts` |

3 つを並べた契約は `shell-registries.dom.test.ts`: 表 ⇄ 実物（`#host` の静的 pane / `#edge-rail` の 3 起点の slot と
EdgeRail の描画・R sidebar への付け替え / CURail の列と peek）を、**表を反復して**確かめる（PL §5.2 — 行を足すと検査も
増える）。rail は表に 1 行足した版でも同じ検査を回す。

**共通の形**:

- 表は `as const satisfies readonly XxxDef[]`（rail は slice 1 の型のまま）。行の literal 型が残るので、`pane.id === "lane"` の
  比較や描画表の key が表に無い名前なら tsc が落とす
- 導出は**表を引数に取る純関数**（`appScenesOf` / `presetCycleOf` / `appPaneOfKind` / `edgeResidentsInRailOrder` /
  `relocatedResidentHostIds` など）。表に 1 行足した版を test で作り、場・巡回・kind 解決・並び・付け替えに出ることを
  確かめている（open-closed）
- 描画の component は tsx 側が id で引く表に足す（PL の pure / component 分離）。key の型は表から導くので、行を足して
  描き手を書き忘れると tsc が止める

**行の足し方**:

- **pane kind**: `APP_PANE_REGISTRY` に 1 行 + main_area.rs の `#host` 直下に `.pane`（`id` = domId、`data-kind` = kinds の
  先頭、`data-app-pane` = id）を置く。`APP_SCENES`（独占の場）・`PRESET_CYCLE`（`inCycle`）・`Ctrl+Shift+数字`（巡回の順に
  9 まで）・`setActivePane` の kind 解決（DOM の可視性と配置の両方）は表から導かれる。Rust 側が送る kind（sidebar の
  `stand:select` → `ActiveComponent.kind`）は `kinds` のどれかに合わせる
- **edge resident**: `EDGE_RESIDENT_REGISTRY` に 1 行 + main_area.rs の `#edge-rail` の `.anchor-<anchor>` に host を置く
  （`content: "solid"` なら空の `<div id=hostId>`）。solid なら `EdgeRail.tsx` の `views` に描き手を足す（足さないと tsc が
  止める）。R sidebar の Lane 節への付け替えは `relocateToRightSidebar` から導かれる
- **rail**: `SHELL_RAILS` に 1 行 + `ShellRailId` に id + `Shell.tsx` の `RAIL_VIEW` に icon / peek（`Record` なので
  書き忘れると tsc が止める）

**決めたこと**:

- **静的 HTML は表から生成しない**。pane と住人の DOM は Rust の静的 HTML に置いたまま（doc 49 R1 — xterm / iframe は
  作り直せない）、表との 1:1 を test で固定した。生成にするには Rust が TS の表を読むか、その逆が要る
- **kind の表 2 枚（`entry.tsx` の kind → 住所、`active-pane.ts` の kind → DOM id）は 1 枚の表の 2 列になった**。旧注記の
  「別の写像なので統合してはいけない」の通り、列（住所 `id` と DOM の `domId`）は分けたまま。違っていたのは kind の受け口（表の key）で、
  旧名 `terminal` を `entry.tsx` は lane の場への訪問に、`active-pane.ts` は `pane-empty` に解決していた。今は両方 lane で、address が
  あれば lane の切替になる。Rust の `push_active_view`（`lane_view.rs`）が今送るのは `lane` か component の kind か null で、
  `terminal` は送っていない。表には旧名の受け口として残した
- `entry.tsx` の分岐は純関数 `activePaneRouteOf`（`active-pane.ts`）に出した: empty / lane の切替 / pane の訪問 / 表に無い
  kind の 4 つ。address の無い `lane` は lane pane の訪問になる（旧形は `entry.tsx` が empty の場に、`active-pane.ts` が
  `pane-lane` に解決していた。Rust はこの形を送らない）
- 訪問の場の id は表の `scene.id` から引く（旧形は `${paneId}-focus` の命名規約）。表に無い pane の訪問は何も動かさない
  （旧形は訪問中なら訪問を終わらせていた）。命名規約から外れていた empty も訪問できるようになった（旧形は `empty-focus` が
  無く失敗）が、empty の kind は `activePaneRouteOf` が empty の分岐へ送るので、今は誰も empty を訪問しない
- `Ctrl+Shift+数字` の表（`keybindings.ts`）も巡回から導く。旧形は巡回と同じ 4 つを直書きしていた
- **edge resident はまだ `RailDef` にしない**。住人は route を持たない動詞で（§2 の「内側は動詞、外側は面」）、起点も 3 つ
  （creo-ui の `RailAnchor` は top / bottom の 2 つ）。lane の額縁を CUEdgeShell に載せる slice 4 で、右 edge の Rail と
  揃えるかを決める
- `content: "static"` は board 取っ手だけ: 中身は静的 HTML、開閉と badge の配線は `board-view.ts`（doc 55 の user 専有の
  表示状態）。EdgeRail は描かない

### 8.3 slice 3 の実装で決めたこと（2026-11-25）— Editor の region field・registry 同梱 field・token の畳み込み

task `mem_1ChNy9mAis1qBxN6Rzr74W`。

**field の宣言と region**（`editor/field-spec.ts` = 型と純関数、`editor/app-fields.ts` = VP の宣言と組み立て）:

- VP の app field は宣言（`AppFieldSpec`: id / label / group / region / cssVar / initial / constraints）の表になった。
  region は creo-ui の `Region`（leftEdge / center / rightEdge …、無し = 全体・境界）
- Editor の**タブは region から引く**（`editorSectionOf`）: 左右の edge = SIDEBAR、`chat.*` = CHAT、残り（全体・center・
  境界・creo-ui の token）= THEME。semantic（Editor Mode の D-2）は region の写しで、MCP / Link hub に見せる
  （leftEdge = source、rightEdge = tool、downEdge = utility、それ以外 = global）。⚠️ タブを semantic で振らない —
  creo-ui の token の自動発見（console の `creoEditor.autoDiscover()`）は semantic が既定で tool なので、VP の左右の
  edge に混ざる
- `sb.*` は leftEdge（semantic が global → source に変わった）、`chat.*` と名札は center、`shell.resizer.*` は region 無し
- **既存の field id と保存キーは 1 本も変えていない**（`app-fields.test.ts` の LEGACY_IDS）。既存の field が出るタブも変えていない（`editor-theme.test.ts` がそのまま通る）

**region の幅**（`shell.leftEdge.peek.width` 180〜640px / `shell.rightEdge.width` 15〜60%）:

- 持ち主は `shell-layout.ts` のまま。幅の field は **cssVar を持たず**、`setPeekWidth` / `setRightPct`（新設の書き口）を通る。
  `#sidebar-root` / `#app-shell` が同じ変数を inline で持つので、Editor が `:root` に書いても子に負けて届かない
- 書き口は drag と同じ箱を動かし、同じ端に丸め、同じ IPC（`shell:layout`）で保存する。保存は drag の「確定時だけ」と同じ
  規律で、続けて来る間は待ち、落ち着いてから 1 回（300ms）
- Editor の値は `onShellLayoutChange` で drag・復元に追従する。写し返しは silent（subscriber も onAnyChange も起こさない）で、
  書き口への送り返しもテーマ保存も Link への送信も起きない。MCP の `editor_values` は hub が surface に `get` して host の値を
  取り直すので、silent でも今の幅が届く。Editor から書き口へ送った後も写し返す — 書き口は丸めた値（範囲、R は小数 1 桁）が
  今と同じなら何もしないので、送った端数（33.33 など）のまま Editor に残らないように
- Editor の数の欄は、打っている途中（input）は範囲の内の値だけ当て、確定（change）で範囲に丸める（幅の field に限らない）。
  毎打鍵で丸めていた時は、min 180 の欄で「4」が 180 に化け、続けて打つと 640 に跳ね、そのたびに shell が動いていた
- **幅はテーマに入れない**（`VpEditor.tsx` の復元・保存から外す）。テーマを切り替えても幅は変わらない。Editor の登録より
  先に来た復元を、初期値（280px / 25%）で潰さない。書き出し JSON には、既定から動かした幅が入る（その時点の値）

**registry 同梱 field**（open-closed）:

- 3 つの registry の行の型（`ShellRailSpec` / `EdgeResidentDef` / `AppPaneDef`）が任意の `fields`（`BundledFieldSpec`: key /
  label / group / cssVar / initial / constraints、pure data）を持つ。collector（`collectBundledFields`）が表を走査し、id を
  `<registry>.<行 id>.<key>`（`rail.*` / `resident.*` / `pane.*`）に組み立てる。行の書き手は id を手で書かないので、rail の
  `lanes` と pane の `lane` のような似た名前の行や、将来別の表に同じ id の行が入っても衝突しない。region は表から決まる（rail = leftEdge、edge resident =
  rightEdge、pane kind = center）
- 最初の同梱は pane kind の `lane` 行の端末 3 本（`pane.lane.terminal.font.size` / `.line.height` / `.letter.spacing`、
  `--terminal-*`）。main_area.rs の `:root` にあり、term.ts が documentElement の style を見張って全 lane の端末に当て直す。
  rail と edge resident の行にはまだ同梱が無い（自然な knob が無かった）。型と collector の経路は 3 表とも通っていて、
  「どの表に 1 行足しても `appFields()` に出る」を `app-fields.test.ts` が 3 表それぞれで確かめている
- 名札の高さ（`--vp-nameplate-h`）は lane 行に同梱せず、center の基本の宣言（`shell.center.nameplate.height`）にした。
  静的な `.pane-header`（runner / devices / preview）と `#lane-header` が共有する高さで、lane だけの knob ではない
- `rail` の表は `src/sidebar/route.ts` から `src/sidebar/shell-rails.ts` に移した（`route.ts` は re-export）。Editor は main
  bundle に居て、`route.ts` を import すると creo-ui/shells の Solid の部品（module 評価時に `document` を引く）まで入るため
- ⚠️ 同梱の cssVar は **`:root` 以外で宣言しない・要素の inline に書かない** token に限る（幅が cssVar を持たないのと同じ理由）。
  `app-fields.test.ts` が main_area.rs・webview の CSS 定数・assets の css を読んで、全 field の変数についてこれを確かめ、
  数の field の `initial` が `:root` の既定値と同じことも確かめる
- テーマを切り替える時に `:root` から消す変数（旧 `MANAGED_VAR` の手書きの正規表現）は、宣言から導く（`isEditorManagedVar`。
  creo-ui の token と component の knob の正規表現は残した）。行が同梱した変数も、行を足すだけで消す対象に入る

**token の畳み込み**（`Shell.tsx` の palette、`editor/styles.ts` のプリセットの写し）:

- `--lg-veil`（地に重ねる薄い光の素、Light Grid では白）と `--lg-veil-1〜4`（`color-mix(in srgb,var(--lg-veil) N%,transparent)`）を
  足し、旧 `#ffffff04` / `06` / `08` / `0c` を置き換えた。N は旧 alpha の byte / 255（8 bit に丸めると旧値と同じ = **既定の
  見た目は変わらない**）。Light Grid 以外のプリセットでは `--lg-veil: var(--lg-hot)`（文字の色）に写す — 明るい地では暗い
  重ねになる。creo-ui の `--surface-veil-*`（components.css、`--color-text-primary` を混ぜる）と同じ考え方。Editor の左の配色に
  `sb.veil`（重ねる光）を足した — Light Grid のまま地を明るくした時に、これを暗くすると重ねが見える
- `--lg-cyan-deep`（旧 `#123039`、切れた接続の丸の地）と `--lg-cyan`（`#40d9ec`）を palette に足した。`--lg-cyan` は AtlasPicker が
  未定義のまま引いていて、fallback の `#40d9ec` が描かれていた（色は同じまま定義した）。同じく未定義だった `--lg-hover`
  （fallback `#ffffff0c`）は `--lg-veil-4` に置き換えた
- 選択中の session 名の `rgba(234,251,255,.72)` は `--lg-hot` を 72% で混ぜた値（同じ色）。context menu の影 `rgba(0,0,0,.4)` は
  palette の `--lg-shadow`（同じ値）にし、プリセットで creo-ui の `--color-shadow-base` に写す。⚠️ creo-ui の token を直に引かない —
  起動時の `data-theme` は `mint-dark`（`sidebar.tsx` が VpEditor の後に書く）で、`--color-shadow-base` は 0.3 になる
  （contrast-dark の 0.4 ではない）。最初の実装は直に引いて既定の影を薄くしていた（moody-blues の指摘で直した）
- **Light Grid（既定）のまま Editor で地を明るくした時に追従するのは、文字の色から混ぜる値だけ**（選択中の session 名）。
  `--lg-veil` は `sb.veil` で手で合わせる。`--lg-cyan-deep` / `--lg-cyan` / `--lg-shadow` は Light Grid では literal のまま
  で、Editor の field も持たない（切れた接続の丸の地などは暗いまま残る）。プリセットに切り替えれば全部 creo-ui の token に従う。
  literal のままにしたのは既定の見た目を変えないため — 文字や地の色から混ぜると、既定の値と bit 単位で一致しない
- `Shell.tsx` の CSS に残る直書きは 4 つで、どれも意図して残した: mask の `#000` ×2（mask-image の stop で、色として描かれない）と、
  危険色の塗りに載せる文字の `#fff` ×2（塗りが彩度の高い赤で、地や文字の色に依らない）
- 写しの規則: **literal の色を持つ `--lg-*` は、Light Grid 以外のプリセットで全部 creo-ui の token に写す**。
  `src/sidebar/sidebar-palette.test.ts` が、palette の外の直書き・8 桁の白・未定義の `--lg-*` 参照・写し忘れを止める
- **残り（44 か所、次の掃除）**: `WirePanel.tsx` 23（白の `rgba(255,255,255,…)` 8 + 状態色と暗い地 15）、
  `SettingsPanel.tsx` 16（白の `rgba(255,255,255,…)` 12 + 危険色系 4）、`LocalUrls.tsx` 2（状態色）、`LanePicker.tsx` /
  `CommandPalette.tsx` / `AtlasPicker.tsx` 各 1（暗幕・影）。数えたのは `src/sidebar` の `*_CSS` 定数のうち var() の fallback を除いた
  色の literal（`Shell.tsx` は palette の宣言と上の 4 つを除く）。白の rgba は alpha の段が多く（.02〜.9）、重ねと文字色の両方に
  使われているので、`--lg-veil` に寄せるか段を足すかは別に決める

### 8.4 slice 4 の実装で決めたこと（2026-11-25）— lane の額縁、自己相似の完結

Slice 1〜4 の実装が揃い、app / lane の 2 階層が Principal Layout の位置語彙を共有する。
`CUEdgeShell` component で既存 DOM を包み直すのではなく、slice 1 と同じく静的 HTML に grid と属性を載せる（R1）。

| lane の直下 | data-region | grid-area |
|---|---|---|
| `#lane-header` | upEdge | up |
| `#lane-panes` | center | center |
| `#edge-rail` | rightEdge | right |

- `#pane-lane` の area は `"up up right" "left center right" "down down right"`。
  既存の右レールが上端から下端まで占めていた形を保つ。列・行とも `auto minmax(0,1fr) auto`。
  left / down は空なので DOM を描かない。名札は `.lane-header-active` 時に共有の `--vp-nameplate-h`、不在時は高さ 0。
- 右レールは幅 36px の grid item。R sidebar 展開中 / lane 不在時に `display:none` となり、right track が畳まれる。
  `#lane-header` / `#lane-panes` の `right:36px` と R 展開時の `right:0` を撤去した。
- `#lane-panes` は `position:relative; min-width:0; min-height:0`。子の absolute rect の基準を保ち、
  `#pane-lane` / `#lane-panes` とその子を reparent しない。`#console-switching` / `#lane-empty` は
  `grid-area:center; position:absolute; inset:0` で center だけに重ね、名札とレールを覆わず track サイズにも寄与しない。
- **edge resident は引き続き `EDGE_RESIDENT_REGISTRY` と 3 起点の slot を使う**。route を選ぶ `CURail` と仕事が違い、
  右レールを grid に載せるための置換は不要。R の Lane 節への住人の付け替えは既存のまま。
- `shell-frame.dom.test.ts` は app / lane **それぞれの直下**の region を検査する。
  happy-dom は CSS Grid の実寸を計算しないため、DOM test の主張は region・area・表示・退避の撤去まで。
  Headless Chrome で 800×600 / 320×160 の各 6 状態（不在 → active → R 開 → R 閉 → 名札 42px → 再び不在）を実測し、
  track の折り畳み・右レールの全高・overlay と center の矩形一致を確認した。
  native WebView の見た目・xterm resize は実機確認待ち。

**やってはいけない**: document 全体の `[data-region="center"]` を単一要素とみなすこと。
同じ位置語彙が各スケールで繰り返されるので、対象 shell の直下で解決する。

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
- 2026-11-25: slice 1 実装（task `mem_1ChNmWC2c12iRHNmGTtgHZ`）。左 edge = CURail と同じ DOM 契約の仮実装、pseudo-route、
  設定 Scene、3x3 grid + `data-region`、`data-frame-id` → `data-app-pane`。決めたことは §8.1
- 2026-11-25: 要望（peekOpen / anchor / 列の slot / peekFooter / keepPeeksMounted / region / peekWidth / resizing）が
  creo-ui 0.31.0 に入り（creo-ui#188 + #204、release は #205）、同じ PR #1291 の中で仮実装を本物の `CURail` に差し替えた
- 2026-11-25: slice 2 実装（task `mem_1ChNtFpUyeEstk7sXoUB4x`）。pane kind / edge resident の registry を新設し、rail と並べて
  3 つの registry の契約 test を整えた。`APP_PANE_IDS` と kind の表 2 枚を撤去。決めたことは §8.2
- 2026-11-25: slice 3 実装（task `mem_1ChNy9mAis1qBxN6Rzr74W`）。Editor の field を region 付きの宣言にし、左右の幅を
  shell-layout の書き口経由で Editor / MCP から動かせるようにした（テーマの外）。3 つの registry の行が field を同梱できるようにし、
  lane 行に端末の 3 本を同梱。`Shell.tsx` の直書きと左 edge の白い重ねを Light Grid の token に畳んだ（既定の見た目は不変）。
  決めたことは §8.3

- 2026-11-25: slice 4 実装（task `mem_1ChP3kQTUGaY6sF5fNDDj7`）。lane の名札・中身・右レールを
  3x3 grid と `data-region` に載せ、手書きの退避を撤去。Slice 1〜4 の実装が揃った。実機確認待ち（§8.4）。
