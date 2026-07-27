> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 49 — GUI LayoutEngine と creo-ui 基盤化

> **起点（mako × lead、2026-07-22）**: doc 47 §1 の決定（Epic の最後に GUI LayoutEngine を
> 作り、FrameEngine と PaneLayout の両方を置き換える）を受けて、**どこで・誰の所有物として
> 設計するか**を確定する doc。結論 = **creo-ui を protocol owner として設計し、
> creo-ui を VP の GUI 基盤ライブラリにする**（mako 確定 2026-07-22）。
>
> 前提 doc: doc 47 §1（置き換え決定）/ doc 48（Editor Mode ループ = 本 Epic の道具）/
> doc 44 §タブ=注視 / doc 46（lane pane model）。

## 1. 決定 — LayoutEngine は creo-ui の protocol として設計する

所有モデルは **Editor Mode（creo-ui editor-mode.md D-11）の反復**:

- **creo-ui** = schema / protocol owner + reference 実装（SolidJS）
- **VP** = 最初で最難の consumer。要件を駆動し、検証台を提供する

この向きを裏付ける事実（2026-07-22 調査）:

- creo-ui frame README（2026-05-05）は「**P-6 (VP 統合) のみ remaining**」— creo-ui 側は
  最初から VP に載せる計画で止まっていた
- 位置語彙は**既に creo-ui が所有**している: `packages/web/src/shells/regions.ts`
  （Principal Layout PL-2）の 2 軸意味論「横=起点⇄ツール / 縦=global⇄local」は
  VP doc 29 §3.2 由来で、Editor Mode の 4-region（D-2/D-3）と同一。
  canonical vocabulary の統一は済んでいて、engine 統合は翻訳でなく実装作業になる

## 2. 事実の地図 — 空間配置系は 3 兄弟、本物の「分割」は誰も持っていない

doc 47 §1 は「VP 内で同じものを 2 回作っていた」と指摘したが、repo を跨ぐと 3 つ:

| | VP `frame-engine.ts`（VP-140） | VP Pane shell（doc 46 P1） | creo-ui `creo-ui-frame` |
|---|---|---|---|
| 概念 | Scene = transform snapshot | flex row tiling | Frame = slots × perspective × transition |
| サイズ（w/h） | ✅ 0..1 比率 | ✅ flex | ❌ **無い**（x/y/z/rotate/scale/opacity のみ） |
| 状態（min/max/hidden） | ✅ `PaneState` | 一部 | ❌ |
| motion | ❌ | ❌ | ✅ FLIP / spring / morph / reduced-motion（自作所有、Motion One archive 化のため） |
| 検証 | VP 実戦 | 凍結中の最小構成 | creo-ui site Playground dogfood 済 |

3 つは重複ではなく**欠けたピースが互い違い**: 分割を持つ 2 つ（VP 側）に motion が無く、
motion を持つ 1 つ（creo-ui）に分割が無い。creo-ui-frame は layout engine ではなく
**spatial morph engine**（配置間の遷移装置）である点が重要 — 「creo-ui frame を VP に
持ってくる」は解にならず、**LayoutEngine = 3 つを 1 つに畳む新設の器**が正しい定義。

## 3. 層アーキテクチャ（creo-ui = VP の GUI 基盤）

```
L5  Editor Mode（調整ループ = doc 48）──────── 全層の knob      ［creo-ui 所有・済］
L4  motion（FLIP / spring / morph）─────────── 配置遷移の実行系  ［creo-ui 所有・済］
L3  LayoutEngine ★新設 ─ 空間分割 + Scene/注視 ［creo-ui が protocol owner に］
L2  位置語彙（Principal Layout regions）─────── ［creo-ui 所有・済、VP doc 29 由来］
L1  components（button/badge/… + D-13 knob）── ［creo-ui 所有・済、VP は未採用 → UI フェーズで採用］
L0  tokens / theme（DTCG SSOT・8 theme）────── ［creo-ui 所有・済、vp-tokens.css は段階統合］
```

VP 側で L3 が置き換えるもの: `frame-engine.ts` の Scene 層と doc 46 `PaneLayout` の両方
（doc 47 §1 の決定どおり「1 つで両方」）。

## 4. VP が持ち込む要件（engine 設計の入力）

| # | 要件 | 根拠 |
|---|---|---|
| R1 | **DOM 安定性**: 配置変更は transform / grid 座標のみ、DOM ノードを reparent しない | xterm.js console は再生成不可。VP frame-engine と creo-ui FLIP の共通 DNA |
| R2 | **projection 境界**: server = 何が存在するか（lane/session/pane content）、engine = どう並べるか（純 client）。**layout state の scope を protocol に明示**（per-lane 等） | doc 47 §0（「所有者を決めることと scope を決めることは別の決定」の実バグ教訓） |
| R3 | **注視モデル**: Scene は注視（attention）の表現。タブ=注視のみ | doc 44 |
| R4 | **pane kind 語彙**: Engine×Act を pane の kind として扱える | doc 46 P2 |
| R5 | **2D core + 3D optional**: 分割 + focus が core。Gaze / perspective は optional 層 | creo-ui PL-1 の既存規律（2D consumer に 3D 依存を持ち込まない） |
| R6 | **構造規律**: 純 data（Layout/Scene）/ 純 calculation（遷移解決）/ 純 action（DOM 反映）の分離を protocol に保存 | `frame-engine.ts` ヘッダで実証済の分離。editor-host の Target × Control 分離とも同型 |

## 5. リスクと手当

| リスク | 手当 |
|---|---|
| 抽象の早熟（consumer 1 つで汎用 protocol を切る） | 初版は VP の要件（§4）**だけ**で切る。受け手は最初から 2 つ（VP gallery mode = doc 48 Phase 3 + creo-ui site Playground） |
| 2 repo 開発の摩擦 | doc 48 Phase 1（`bun link` した creo-ui が watch → reload に乗る）が**前提条件**。順序は doc 48 → 本 doc の実装 |
| toy case で磨いて実戦で崩れる（site 先行 dogfood の罠） | **primary dogfood を最初から VP gallery mode にする**（§6）。Phase 1 の秒ループがこれを可能にする。site Playground は secondary |
| 名前の衝突（"frame" が VP と creo-ui で別物） | 新 engine は本質機能名（仮: `creo-ui-layout`）。"frame" は morph/motion 層の名に残す。コード識別子は不変・本質名の規律 |
| doc 47 の凍結規律（現行 2 系統に追加投資しない / UI は最後に一気に）との整合 | **設計 doc は内部フェーズ中に起こせる**（doc 47 自身が「解き方は着手時に別 doc」）。実装着手は UI フェーズ頭。現行 2 系統は凍結のまま |

## 6. 進め方（Epic の弧）

1. **doc 48 Phase 1-3** — Editor Mode ループ整備（HMR / MCP bridge / gallery mode）。全ての前提道具
2. **LayoutEngine 設計 doc** — creo-ui 側（`docs/design/frame-system.md` の後継として protocol 設計）+ 本 doc（VP 側要件、§4）。設計は内部フェーズ中に可
3. **creo-ui 実装、primary dogfood = VP gallery mode**（doc 48 Phase 3。`bun link` × Phase 1 の
   秒ループで cross-repo でも WKWebView 実機で回る）。site Playground は secondary
4. **VP 統合** — `frame-engine.ts` / `PaneLayout` を置換（= creo-ui frame P-6 の完了形 =
   doc 47 の「シンプル表示を当てはめる」）。UI フェーズ本体。gallery はこの時点で
   LayoutEngine の最初のコンテンツとして pane 化する

## 7. やらないこと

- creo-ui frame（morph engine）の VP への直接移植 — 分割概念が無く、解にならない（§2）
- 現行 2 系統（frame-engine.ts / PaneLayout）への追加投資 — doc 47 の凍結規律を維持
- VP 以外の consumer 要件の先取り — Editor Mode と同じく、2nd consumer が現れた時に protocol を広げる
- L0/L1 の一括置換 — tokens 統合と component 採用は UI フェーズで画面ごとに段階採用
