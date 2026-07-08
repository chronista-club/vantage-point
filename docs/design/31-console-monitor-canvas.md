# 31. Console / Monitor / Canvas — VP 語彙システムのリファクタ

- **日付**: 2026-07-09（hearing 収束 4 点: 面は非stand / author は Echoes 隣の capability / PP は Information Router (& Display) として再定義 / author stand = **Heaven's Door 📖 復活**）
- **status**: 設計確定

> ⚠️ **用語再定義（2 度目の前例）**: Heaven's Door はかつて CC オーケストレーター（現 Echoes）の旧名だった（VP-118 で改名）。本 doc 以降、**Heaven's Door = Canvas Author（描く能力）**を正とする。岸辺露伴 = JoJo 随一の「描く」人であり、読み書き能力が canvas authoring と一致するための復活採用。旧文脈（`vp hd` 等）は既に退役済みで実害はない。
- **関連 doc**: [05 Pane/Content モデル](05-pane-content-lane-smart-canvas.md) / [13 PP revival](13-paisley-park-revival.md) / [19 Canvas stack](19-canvas-stack-model.md) / [30 Echoes Act II](30-echoes-act2-gui.md)

## 0. TL;DR — 三層の語彙

旧「PP の Canvas」は**面（映す場所）・媒体（描かれる物）・能力（描く/配る力）を 1 語に混載**していた。三層に割る:

> **Console で語り、Monitor で視て、Canvas に描く。**

| 層 | 語 | 定義 |
|---|---|---|
| **面**（非stand） | Console / Monitor | pane の役割。Console = 対話面、Monitor = 表示面 |
| **媒体**（Content） | Canvas | 描かれる成果物。永続・スタック（doc 19） |
| **能力**（stand） | Echoes 💬 / Heaven's Door 📖 / PP 🧭 | 会話する力 / 描く力 / 配る力 |

原則: **面は stand ではない。能力（stand）が面を運用する。**

## 1. Why

Echoes Act II（doc 30）で会話面が構造化 GUI になると、対になる「描画・成果物・配信」の語彙の混濁が露出する。現状 `PAISLEY_PARK { id: "canvas" }` は (a) 表示面 pane、(b) show/clear の書き込み能動、(c) 情報ルーティングの 3 役を 1 stand に混載。GUI 時代の pane 命名（PR2 が実装で直面）の前に分離する。

## 2. 情報の流れ（refactor 後の全体像）

```
User ⇄ Console（対話面: Act I xterm / Act II EchoesChatPane）
          │  Echoes 💬 が会話を駆動（doc 30）
          ▼
       Heaven's Door 📖 が成果を Canvas に描く（Draft to Canvas）
          ▼
       Canvas（媒体: markdown / html / …）
          │  Paisley Park 🧭 = Information Router が「何をどの面に」を裁く
          ▼
       Monitor（表示面）→ User が視る
```

対称性: **Echoes は Console を、PP は Monitor 群を、Heaven's Door は Canvas を運用する。**

## 3. pane 役割の形式化

**pane 役割 = f(Content kind)**。doc 05 の Pane/Content 分離はそのまま、pane に役割語彙が付く:

| Content kind | 載る pane の役割 |
|---|---|
| `echoes`（chat/tui）/ `bare-shell` / `ruby-repl` | **Console**（対話的 — 人が打ち、プロセスが応える） |
| `canvas` / log / url / db-viewer | **Monitor**（観察的 — 映すだけ） |

## 4. Stand 台帳（refactor 後の全景）

| stand | id | functional_name | 変更 |
|---|---|---|---|
| TheWorld 👑 | `world` | Process Manager | — |
| Star Platinum ⭐ | `process` | Project Core | — |
| Echoes 💬 | `agent` | Coding Assistant | —（Act I/II は doc 30） |
| **Paisley Park 🧭** | `canvas`→**`router`** | Information Navigator→**Information Router** | **再定義**: 「何をどの面に映すか」のルーティング + 表示資産（Monitor 群・layout・pin・mirror）の運用（= & Display）。show の「置き場所を決める」半分は PP の裁き |
| **Heaven's Door 📖**（復活） | **`canvas`**（PP から継承） | **Canvas Author** | **新設**: Draft to Canvas — canvas を書き起こす・版重ねで更新する能動。show の「内容を作る」半分の後継。short: "HD" |
| Gold Experience 🌿 | `runner` | Code Runner | — |
| The Hand ✋ | `shell` | Shell Terminal | —（その pane は Console） |
| Bastet 🧲 / Justice 🌫️ | `bastet` / `justice` | Device Registry / Device I/O | — |

- ⚠️ `id` 移行（PP: canvas→router、HD: canvas 継承）は wire/kind に漏れている可能性 — **実装時に gitnexus impact 必須**。`router`/`runner` の一字違いはログ可読性の懸念として認知しておく
- Console / Monitor は stands.rs に**載せない**
- Heaven's Door 採用の経緯と用語再定義は冒頭の注記を参照（Bohemian Rhapsody / Paper Moon King は次点で落選）

## 5. 文言の対応表（旧 → 新）

| 旧 | 新 |
|---|---|
| Canvas（pane の意味） | **Monitor** |
| smart-canvas（content kind 名, doc 05） | **canvas**（媒体） |
| Information Navigator | **Information Router** |
| CLAUDE.md「Canvas + TUI: TUI で操る、Canvas で視る」 | 「**Console で語り、Monitor で視て、Canvas に描く**」 |
| CLAUDE.md アーキテクチャ樹形図の PP 行 | `Paisley Park 🧭 (Information Router / 配信・表示)` + `Heaven's Door 📖 (Canvas Author / Draft to Canvas)` 行を追加 |

## 6. 互換方針（v1 の範囲）

- **MCP tool 名は据え置き**: `show` / `clear` / `capture_canvas` / `read_pane` / `list_canvas` は CC セッション横断の public API。改名しない（author の新能力が生えた時に `draft` 系を追加検討）
- **wire 文字列は据え置き**: Unison channel `"canvas"` / `"canvas-ingest"`、topic セグメント等の内部識別子は変更しない（不可視・churn に価値なし）。後日移行可
- **今回動くもの**: stands.rs / vp-app の UI ラベル（pane ヘッダ等）/ docs / CLAUDE.md コアコンセプト行

## 7. Draft to Canvas の将来能力（seed、over-scope しない）

- AI の反復的著述: 会話（Echoes）から独立に canvas を版重ねで更新（draft → revise → publish）
- Gold Experience（runner）の実行結果を canvas へ描画
- canvas の版管理・並置比較（Act II の事後 diff レビューと同型の UX を成果物側に）

## 8. 出荷順序

Epic（Echoes Act II, mem_1Ccpxns93xeiwMyzVhVzrn）に **PR1.5** として挿入する小 PR:
stands.rs 再編（PP 再定義 + author 新設 + id 移行）+ UI ラベル + docs/CLAUDE.md 語彙。
PR2 以降（EchoesChatPane = Console の実装）が新語彙の上に建つよう、**PR2 の前**に置く。

## 9. 未決事項

- `id` 移行の blast radius — 実装時に gitnexus impact で確定
