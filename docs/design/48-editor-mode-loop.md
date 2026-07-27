> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 48 — Editor Mode ループ完結（UI フェーズの作業台）

> **起点（mako × lead、2026-07-22）**: doc 47 の内部整合 Epic を経て UI フェーズに入るにあたり、
> 「GUI は文字指示が難しい」問題を診断した結果、**creo-ui Editor Mode は 9 割できているのに
> ループが切れていて habit にならない**ことが判明。本 doc はその切断点を閉じ、
> Editor Mode を UI フェーズの常用作業台にするための設計。
>
> **要件（mako）**: dev profile（VP_PROFILE=dev、二重起動）を**要求しない**こと。
> 日常の brew 版 VP 一台でループ全体が回ることが条件。
>
> **改訂（2026-07-22 Simplicity review）**: 初版の Phase A（書き戻し受け手）を**削除**。
> 書き戻しは Phase 2（MCP bridge）+ CC の Edit が包含する（§2 D-B）。これにより
> 新設 env（旧 `VP_WEBVIEW_SRC` 案）も消え、dev gate は #494 の `VP_WEBVIEW_DEV` 復活だけになった。

## 1. 現状の棚卸し（事実、2026-07-22 時点）

### できていること

| 部品 | 場所 | 状態 |
|---|---|---|
| Editor Mode runtime | creo-ui `packages/editor-host/`（SolidJS、19 tests） | shipped、VP は npm `^0.6.0` で消費 |
| VP 統合 | `crates/vp-app/webview/entry.tsx`（`EditorHostProvider` + Ctrl+Shift+E） | shipped |
| bind 済み knob | 同 `SidebarTokenBinds()`: `sb.text.*` 4 / `sb.conn.*` 6 / 色 2 | 2026-07-11 Light Grid 探索で実戦済 |
| Export | editor-host `export.ts`: json / yaml / css / **css-patch**（`--var: value;` を吐く） | shipped |
| console API | `window.creoEditor`（slider/picker を REPL で動的追加、`host.fields()` / `host.values()`） | shipped |
| AI agent bridge（参照実装） | creo-ui `apps/site/vite-plugins/creo-agent-bridge.ts`（`POST /_creo/agent/cmd` / `/_creo/agent/set`） | **site 限定**（vite dev middleware） |
| 書き戻し client | editor-host `console.ts` `commitToTokens()` → `POST /_creo/tokens/commit` | client のみ存在（受け手はどの repo にも無い） |

### 切れているところ（診断）

1. **調整結果が source に還る道が細い**。export → 手動転記（CC に貼る）だけで、
   成果が diff にならないループは habit にならない — 「使えてない」の主因。
2. **VP（wry）に AI の口が無い**。agent bridge は site の vite middleware 限定。wry には
   claude-in-chrome も繋がらないため、CC と mako が同じ調整画面を共有できない。
   editor-mode.md D-10（AI agent access）は VP では未実装。
3. **knob 追加・component 変更に rebuild 税**。bundle は `include_str!` inline で、
   tsx 1 行の変更も bundle 再生成 + cargo build + app:swap を要求する。
   旧 `VP_WEBVIEW_DEV`（disk-read HMR）は #494（`3fa2389a`）で導入されたが、
   bundle inline 化で dead branch となり #815（`a0373ef2`）で撤去済
   （復活手順は `web_assets.rs` の撤去コメント + 両 commit が SSOT）。

### 鍵になる既存事実（設計を軽くする）

- `EditorField` は **`cssVar` と `constraints.unit` を既に保持**する（editor-host `types.ts`）。
  MCP で fields + values を読めば、書き戻しに必要な情報は全部外へ出せる。
- vp-app（GUI）は daemon から見て**使い捨て可能な client**。`vp app stop && vp app start` は
  lane / daemon に無風。GUI 側の実験は日常 VP で低リスク。
- GUI だけの deploy 経路（`mise run app:swap`、daemon 無風）が既にある。

## 2. 設計決定

### D-A: brew の日常 VP 一台で完結（dev profile 非要求、新 env なし）

- dev gate が要るのは Phase 1（HMR）だけで、それは **#494 の `VP_WEBVIEW_DEV` の復活そのもの**
  （新設 env なし）。未設定 = 一般ユーザ / launchd 経由では完全に無効。
- Phase 2（MCP bridge）と Phase 3（gallery mode）は **product 機能**（D-10 / 開発道具としての
  表示 mode）であり、gate 不要。
- `VP_PROFILE` とは直交。state 分離が要る時だけ併用、必須にしない。二重起動は不要。

### D-B: 書き戻しは CC 経由 — VP に受け手（rewriter）を作らない

- 経路: mako が slider で探索 → **CC が MCP `editor_fields` + `editor_values` で読む →
  CC が自分の Edit で source（`Shell.tsx` の `:root` / tokens 等）に落とす**。
  「export → CC に貼る」で実証済みの経路から、コピペだけを消した直線化。
- 決定的 rewriter を VP に作らない理由: VP の CSS var 定義は tsx 内に散在し heterogeneous で、
  機械書き換えはどうせ「曖昧なら skip」分岐だらけになる。その判断は CC の方が強い。
- `commitToTokens` の受け手は **creo-ui site（`tokens/*.json`、構造化 DTCG で決定的に
  書ける）の領分**とし、VP スコープから外す。実装するなら creo-ui 側 vite plugin
  （agent-bridge の隣）— 本 doc の管轄外。

### D-C: AI の口は VP MCP tool（D-10 の実装先、product 機能）

- `editor_fields` / `editor_values` / `editor_set` の最小 3 本。書き戻し専用 tool は
  作らない（D-B: commit = CC の Edit そのもの）。
- webview への配送路は実装時決定。候補: (1) 既存 pipe（MCP → daemon → vp-app）の先で
  `evaluate_script` により `window.creoEditor` を叩く、(2) 既存の webview 購読路に乗せる。
  いずれも新規 listener は作らない（daemon が唯一の listener の原則を維持）。

## 3. Phase 計画

会話上の呼称との対応: 旧 P0（受け手）= **削除**（D-B）、旧 P2 前半 = Phase 1、旧 P1 = Phase 2、
旧 P2 後半 = Phase 3。GUI 側（1/3）は app:swap で daemon 無風に dogfood でき、
server を触るのは 2 だけ（deploy 時に daemon 再起動 = lane 巻き込み、`--resume` で復帰）。

| Phase | 内容 | 触る場所 | 受け入れ基準 |
|---|---|---|---|
| **1** | `VP_WEBVIEW_DEV` 復活（bundle `<script src>` 外部化 + disk read + reload） | vp-app | tsx 変更 → `bun run dev`(watch) → reload で cargo build なしに GUI 反映（秒）。**`bun link` した creo-ui の変更も同じ watch → reload に乗ること**（UI フェーズの creo-ui component 採用を初日から秒ループにする） |
| **2** | MCP bridge（editor_fields / values / set） | vantage-point（server）+ vp-app | CC が `editor_set` → 画面が変わる。mako の探索値を CC が `editor_values` で読み、**CC の Edit だけで source に落ちて `git diff` に出る**（= 書き戻しの受け入れ基準もここ） |
| **3** | Gallery **mode**（query param で root を story 一覧に切替） | webview（純 client） | sidebar / chatview / HistoryStrip の主要状態（empty / error / 長文等）が mock で並び、`vp shot` で CC が読める。**凍結中の layout 2 系統（Frame Engine / PaneLayout）には一切触れない** |

順序は **1 → 2 → 3**（1 が以降全部の反復速度を決める。2 は server deploy を 1 回に束ねる。
3 は 1 の上に乗ると安く、doc 49 LayoutEngine の primary dogfood 台を兼ねる）。

> gallery を **pane にしない**のは doc 47 §1 の凍結規律（現行 layout 2 系統に追加投資しない）の
> 帰結。LayoutEngine（doc 49）が着地したら、gallery はその**最初のコンテンツ**として pane 化する。

## 4. やらないこと

- 書き戻し rewriter（`/_creo/tokens/commit` 受け手）を VP に作る — CC の Edit が包含（D-B）。
  creo-ui site 側で tokens/*.json に対して作る分には決定的に働くが、本 doc の管轄外
- 汎用 product 機能化（他プロジェクトの UI を映す等）— pre-MVP、VP 自身の dev tool に閉じる
- Editor Mode の Swift / Rust runtime（creo-ui Phase 2c の領分）
- creo-ui protocol の先行拡張 — 必要が固まったら後追いで upstream
- gallery の pane 化・story の網羅主義 — pane 化は LayoutEngine 後（doc 49）、story は
  「調整したい component から順に」で足す
