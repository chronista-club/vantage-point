# VP ショートカット規約 (Shortcut Convention)

> **status: draft v0.3** — 2026-05-26 起草。 dogfood feedback で **binding は変動** することを前提に、 **操作体系 (invariant) と modifier binding (mutable) を疎結合に分離** する layered 構造で記述する。
>
> v0.3 で **chord 2 段 state machine 設計を破棄**、 「**operative directive (動詞) の集合**」 + **`Cmd hold + 単発キー` で各 directive を発火** という Vim operator / Emacs prefix 系の単純な design に再構築 (= user の flow 視点「Cmd hold f → 操作 → Cmd hold p」 を素直に表現)。

---

## 1. コンセプト

VP の shortcut 体系は **3 layer に分離** して設計する:

- **Layer A — 操作体系 (Operative Directives)**: 不変。 VP の **動詞 (directive) 集合** を定義する。 動詞は context-aware polymorphic dispatch を持つ (= 「どこから打ったか / 何が selected か」 で挙動が変わるが、 動詞 identity は不変)
- **Layer B — Modifier Binding**: 可変。 各 directive を **どの modifier + key に bind するか** の table。 dogfood で頻繁に更新
- **Layer C — Directive Registry**: 可変。 個別 directive の意味 + context dispatch を列挙

加えて **Layer D — 実装方針**、 **Layer E — Avoid List**、 **Layer F — Discoverability**、 **Layer G — 更新フロー**。

### Why この分離 ?

- 個別 shortcut を hardcode で議論すると、 1 つ変えるたびに規約 doc が dirty
- 動詞 (semantic) 側を fix できれば、 binding は実験可能領域として user に解放できる
- VP 全体の design philosophy "components を分けて binding は変動可能に" の反映

### Why 「動詞 (directive)」 集合か ?

- chord 2 段 (`Cmd hold f, p` を 1 つの atom として登録) は学習負荷が高い + flexibility が低い
- **動詞単位** で覚えると、 「Cmd hold f = file mode」 + 「Cmd hold p = send to PP」 という独立 verbs が user の flow で composition される
- Vim の operator (`d` / `y` / `c`) や Emacs の prefix と同じ思想: 動詞 × 名詞 / 文脈 を user が組み合わせる

### Flow 視点

user が「Cmd を hold したまま f → ↑↓ → p」 と連続して打つ流れは、 OS keydown 上では **2 つの独立 `Cmd+letter` event** として届く。 規約は 1 つの atom を強制するのではなく、 各 directive が **独立 & 連鎖可能** な形で設計する:

```
[cc に入力中、 main view focus]
   │  ⌘ hold + f   (= 'f' directive を発火)
   ▼
[File Explorer overlay 表示、 focus が picker に移る]
   │  ↑↓ で file 選択 (picker context-local)
   ▼
[file が selected な state]
   │  ⌘ hold + p   (= 'p' directive を発火、 picker context では「選択 file を PP に投げる」)
   ▼
[file 内容が PP (Canvas) に表示]
```

---

## Layer A — 操作体系 (invariant)

VP の操作は **動詞 (directive) の集合** + **その挙動軸 (semantics)** で分類する。

### A.1 挙動軸 (= semantic) の分類

| id | 軸 | semantics | 例 |
|----|----|-----------|-----|
| `focus-preserving` | 投げる | 自分の focus を keep、 別 pane に command / content を投擲 | `p` を main view focus 中に打つ → PP に content 送信、 focus は cc に戻る |
| `focus-transferring` | 移動する | focus 自体を別 pane に移す | `f` で File Explorer overlay 表示 + sidebar focus へ移動 |
| `panel-local` | panel 内操作 | 既に panel に focus がある状態の選択 / 確定操作 | picker 内 `p` = "現在 selected file を PP へ" |
| `layout` | UI structure | Scene / pane の切替 / resize | `Ctrl+Shift+1..4` |
| `system` | OS 標準 | undo / redo / cut / copy / paste 等 | `Cmd+C` 他 |

各 directive は **複数の挙動軸を context dependent に持つ** 可能性がある (= polymorphic):
- `f`: どこから打っても `focus-transferring` (sidebar の File Explorer overlay へ)
- `p`: context によって挙動が変わる
  - File Explorer picker 内で file selected → `panel-local` (= "選択 file を PP へ")
  - cc 入力欄 focus 中 → `focus-preserving` (= cc 内容を PP に送る、 v1.0 では未実装)
  - LaneRow focus 中 → (v1.0 では no-op)

### A.2 directive と「focus を移動するか」

user 概念 (前 turn で提示):
- **Cmd 系** ≈ 「自分は動かない、 投げているだけ」 (focus-preserving 寄り)
- **Ctrl 系** ≈ 「自分が target に移動」 (focus-transferring 寄り)

ただし directive 自体は polymorphic で、 modifier との binding は Layer B の責務。 同じ `f` directive を:
- `Cmd hold + f` (現状) = `focus-transferring` (File Explorer 開いて focus 移動)
- 別 binding (将来) = `focus-preserving` (File Explorer は別 window で開く、 focus は cc のまま) のようにも binding 可能

= **directive identity と挙動軸の bind も mutable** (= Layer B で記述、 Layer A は「VP の動詞集合」 のみ規定)

### A.3 categories の追加 / 変更

- 動詞 (directive) を追加 = Layer C に行追加 + 必要なら Layer A 挙動軸を拡張
- 挙動軸 (semantic) 自体の追加は大きな decision = creo-memories memory + 本 doc 更新

---

## Layer B — Modifier Binding (mutable v1.0)

> **mutable section**: dogfood で更新される。 update の際は本 doc を patch + creo-memories に decision memory (atlasId: `vantage-point`、 `category: decision`、 `tags: shortcut, layer-b`)。

### B.1 binding 規則 (v1.0、 2026-05-26 時点)

| pattern | semantic 主体 | 例 |
|---------|---------------|-----|
| **`Cmd hold + <directive>`** | 動詞発火 (= Layer A の主要 modifier path) | `Cmd hold f`, `Cmd hold p` |
| `Ctrl+Shift + <key>` | layout / visual 系 (既存実装) | `Ctrl+Shift+1..4` (Scene), `Ctrl+Shift+] / [` (cyclic) |
| OS 標準 (muda predefined) | system | `Cmd+C/V/X/Z/A`, `Cmd+W`, `Cmd+Q` |

### B.2 directive 実装の特徴

「Cmd hold + 文字キー」 は OS keydown event 上では **`Cmd + letter` の単発 keydown** と区別がつかない (`metaKey: true` の keydown 1 つ)。 user が「指を離さず連続して打つ」 のは **OS 上では 2 つの独立 keydown event の連続発火**。

→ **規約は「Cmd hold + 単発キー」 と書き、 実装は「`Cmd+letter` 単発 keydown listener」 で行う**。 state machine / chord 2 段の timer は **不要**。

### B.3 update flow

1. dogfood で「この binding が手に合わない」 と感じた時、 本 doc §B.1 を update する PR
2. PR description で「変えた理由」 を必ず書く
3. merge 時に creo-memories へ decision memory `remember`
4. CHANGELOG (本 doc 末尾) に行追加

### B.4 過去 binding history

(該当なし、 v1.0 が初版)

---

## Layer C — Directive Registry (mutable)

> **mutable section**: 個別 directive の意味 + context dispatch。 PR ごとに増減する。

### C.1 Letter reservation

#### Stand 系 directive (= 投擲先 / focus 移動先)

| letter | Stand | 主挙動 | 補足 |
|--------|-------|--------|------|
| `p` | **P**aisley Park | "send current to PP" or "focus to PP" | Canvas 表示先 |
| `e` | **E**choes | "send to Echoes" or "focus to Echoes 入力欄" | Claude CLI |
| `g` | **G**old Experience | "send to GE" or "focus to GE output" | Code Runner |
| `h` | **H**ermit Purple | "send to HP" or "focus to HP" | MIDI / tmux |
| `w` | The**W**orld | "show TheWorld status" | Process Manager |

#### Action / category 系 directive (= panel / mode trigger)

| letter | 意味 | 主挙動 |
|--------|------|--------|
| `f` | **f**ile | File Explorer overlay (sidebar) を open + focus 移動 |
| `l` | **l**ane | Lane list panel (sidebar) を open + focus 移動 (v1.0 では既存 sidebar の lane list が常時 visible なので no-op 寄り、 将来 dedicated panel) |
| `r` | **r**ecord / **r**estart | (TBD) context dependent: lane focus 中なら lane restart 等 |
| `o` | **o**pen | (TBD) generic open |
| `c` | **c**lear / **c**lose | (TBD) clear current panel content 等 |
| `n` | **n**ew | (legacy: `Cmd+N` 単発 = New Window menu accelerator) |
| `s` | **s**ave / **s**witch | (TBD) |

### C.2 確定 directive (v1.0 で実装するもの)

| binding | directive | 意味 (context dispatch) |
|---------|-----------|-------------------------|
| `Cmd hold f` | `f` (file) | **どこから打っても** sidebar の File Explorer overlay を open + sidebar focus へ移動 |
| `Cmd hold p` | `p` (PP) | **File Explorer picker visible 中なら**: 選択中 file を PP (Canvas) に送る + picker は pin 状態に関係なく **連続選択を許す** (= dismiss しない) |

### C.3 予約 directive (v1.0 文法に基づく、 実装は別 PR)

| binding | directive | 意味 |
|---------|-----------|------|
| `Cmd hold e` | `e` | focus を Echoes 入力欄に移動 |
| `Cmd hold g` | `g` | focus を Gold Experience 表示に移動 |
| `Cmd hold h` | `h` | focus を Hermit Purple に移動 |
| `Cmd hold l` | `l` | Lane list panel (dedicated) を open |
| `Cmd hold r` | `r` | (context) lane focus 中 → lane restart |
| `Cmd hold c` | `c` | (context) PP focus 中 → PP clear、 Echoes focus 中 → cc clear |
| `Cmd hold w` | `w` | TheWorld status を Canvas (PP) に表示 |

### C.4 既存単発 shortcut (規約 v1.0 と整合)

| key | 用途 | 状態 |
|-----|------|------|
| `Cmd+N` | New Window (muda menu) | **既存 keep** — `n` は directive にも予約候補だが、 当面 menu accelerator 専用 |
| `Cmd+W` / `Cmd+Q` | Close Window / Quit (predefined) | **既存 keep** (system) |
| `Cmd+C/V/X/Z/A` 等 | Edit menu predefined | **既存 keep** (system) |
| `Ctrl+Shift+1..4` | Scene 切替 (web-bundle/keybindings.ts) | **既存 keep** (layout) |
| `Ctrl+Shift+] / [` | Scene cyclic | **既存 keep** (layout) |

### C.5 Legacy / 互換 shortcut

| key | 状態 |
|-----|------|
| `Cmd+F` (sidebar focus 中の File Explorer 起動、 PR #439 で実装) | 規約 v1.0 で **`f` directive にそのまま昇格** — 文字 binding は変わらない、 但し挙動が「sidebar focus 中のみ」 から「どこからでも」 に拡張。 旧 code path は generalize される |
| sidebar の `📁` button (LaneRow) | 恒久的 keep (discoverability + mouse 派 user 救済) |

---

## Layer D — 実装方針

### D.1 単純な keydown dispatcher (state machine 不要)

`Cmd hold + key` は keydown event 上で `metaKey: true` + 文字キー 1 つの 1 event。 chord 2 段 state machine は不要、 単純な keydown listener で directive lookup + exec:

```ts
// web-bundle/src/shortcuts/directive.ts (要旨)
import { DIRECTIVE_TABLE } from './directive-table'

export function installDirectiveHandler(ctx: DirectiveContext): () => void {
  const handler = (event: Event): void => {
    const e = event as KeyboardEvent
    const isMac = navigator.platform.toUpperCase().includes('MAC')
    const mod = isMac ? e.metaKey : e.ctrlKey
    if (!mod || e.shiftKey || e.altKey) return
    if (e.key === 'Meta' || e.key === 'Control') return
    const key = e.key.toLowerCase()
    if (key.length !== 1) return
    const entry = DIRECTIVE_TABLE[key]
    if (entry) {
      e.preventDefault()
      ctx.exec(key)
    }
  }
  window.addEventListener('keydown', handler, true) // capture phase
  return () => window.removeEventListener('keydown', handler, true)
}
```

### D.2 SSOT としての directive table

`web-bundle/src/shortcuts/directive-table.ts` (= **single key map**):

```ts
export interface DirectiveEntry {
  description: string
  semantic: 'focus-preserving' | 'focus-transferring' | 'panel-local' | 'layout' | 'system'
}

export const DIRECTIVE_TABLE: Record<string, DirectiveEntry> = {
  f: { description: 'File Explorer overlay を open', semantic: 'focus-transferring' },
  p: { description: 'send current/selected to PP', semantic: 'panel-local' },
}
```

### D.3 各 WebView での install

- **sidebar WebView**: `directive` 'f' は direct に `window.vpFilePicker.open(addr)` を呼べる。 'p' は picker visible 中なら selected file を投げる
- **main view (terminal/Canvas) WebView**: directive 発火時に sidebar の picker が無いので、 **Rust 経由で sidebar に inject** する bridge を使う (= `window.ipc.postMessage({ t: 'directive:fire', key })` → Rust → `sidebar.evaluate_script`)

### D.4 main view → sidebar の bridge

新規 IPC type `directive:fire`:

```ts
// main view
window.ipc?.postMessage(JSON.stringify({ t: 'directive:fire', key: 'f' }))
```

Rust (`terminal.rs::handle_ipc_message`):
- `directive:fire` を受信 → `AppEvent::DirectiveFire { key }` を発火

Rust (`app.rs`):
- `AppEvent::DirectiveFire` arm:
  - `key == "f"` → `sidebar.evaluate_script("window.vpFilePicker.open(<addr>)")`
  - `key == "p"` → `sidebar.evaluate_script("window.vpFilePickerSendSelected()")` (= 既存 picker の "Enter で送る" path を関数化)

### D.5 picker での `p` directive 挙動 (= panel-local)

File Explorer overlay 内で user が `Cmd hold p` を打った場合の挙動:
1. picker 内で selected な entry を取得
2. file なら `files:open` IPC を Rust に投げる (= 既存 path)
3. picker は dismiss しない (= 連続選択を許す、 pin 状態に関係なし)

これは picker 内の独立 listener として実装する (sidebar 全体 listener とは別)。 もしくは picker 内で `window.vpFilePicker.sendSelectedToPP()` 等の API を expose して、 directive dispatcher から呼ぶ。

### D.6 menu との関係

- directive は muda menu accelerator として表現できない (= `Cmd+letter` 単発と同じ key event だが、 menu accelerator は OS-level で WebView より先に発火するため、 menu accelerator に登録した key は WebView listener に届かない)
- **directive 用 letter は menu accelerator に bind しない**: `Cmd+N` (= menu accelerator) は keep、 `Cmd+F` / `Cmd+P` 等 directive 用 letter は **menu accelerator なし**
- menu item は **chord-less で残し** (mouse 派 user 救済)、 title に「(⌘ hold f)」 等の hint を併記

---

## Layer E — Avoid List (invariant)

| 衝突 | 理由 | VP の方針 |
|------|------|----------|
| `Cmd+Shift+3/4/5` | macOS screenshot global hook | 使わない |
| `Cmd+Space` | Spotlight | 使わない |
| `Cmd+Tab` | App switcher | 使わない |
| `Ctrl + letter` 単独 | readline (`Ctrl+A/E/F/B/N/P/R/W/U/L/K`) / tmux prefix と被る | terminal を生かすため `Ctrl+letter` は VP の主要 binding として使わない (Scene 等は `Ctrl+Shift+` で逃げ済) |
| `Opt/Alt + letter` | terminal で特殊文字入力 (Opt+P = π 等) | terminal に譲る |
| `Cmd+P` 単発 print | print dialog だが WKWebView default で disabled、 v1.0 では directive `p` に割当 | directive 用に使う |

---

## Layer F — Discoverability

1. **menu に出す** (single-key shortcut の SSOT): menu accelerator がある shortcut は muda menu に必ず item として登場
2. **menu item の subtitle に directive 表記**: 例 「Open File... (⌘ hold f)」
3. **Cheatsheet 画面 (将来 `meta` directive `?`)**: 全 directive を Canvas に markdown table で render
4. **LaneRow の `📁` icon button**: directive `f` の affordance (mouse 派 user 救済)

---

## Layer G — 規約の更新フロー

1. **Layer A (操作体系・挙動軸 追加 / 変更)**: 大きな decision、 PR で本 doc update + creo-memories decision memory
2. **Layer B (modifier binding 変更)**: 中規模 decision、 本 doc §B update + creo-memories decision
3. **Layer C (directive registry 追加 / 変更)**: PR 単位、 本 doc §C update + 個別 PR description に rationale 記載
4. CHANGELOG (下記) に行追加

---

## CHANGELOG

| date | version | section | change | PR |
|------|---------|---------|--------|----|
| 2026-05-26 | v0.3 draft | (full) | chord 2 段 state machine 設計を破棄、 「directive 集合 + Cmd hold 単発キー」 に再構築。 user の flow 視点 (`Cmd hold f → 操作 → Cmd hold p`) を素直に表現 | (本 commit) |
| 2026-05-26 | v0.2 draft | (full) | 2 layer 構造 (invariant / mutable 分離) | (前 commit、 同 PR で上書き) |
| 2026-05-26 | v0.1 draft | (full) | 初版起草 (single layer、 superseded by v0.2) | (前 commit、 同 PR で上書き) |

---

## 関連

- PR #439: File Explorer overlay picker (Rust IPC + UI)
- PR #440: File Explorer follow-up (z-index / race guard)
- PR #441 (予定): 本規約 v1.0 と Layer C の `f` / `p` directive 実装

将来の創発 (ideas / TBD):
- `meta` directive `?` の cheatsheet 実装
- `e` / `g` / `h` directive (Stand focus / send) 実装
- mouse / drag-drop で directive を発火する equivalent path
- multi-lane targeting (= 別 lane の PP に投げる、 prefix-key で lane number 指定 等)
- accessibility (sticky-key で directive 発火サポート確認)
- picker context-aware な `p` の挙動: pin 状態とどう integrative にするか
