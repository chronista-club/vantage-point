# VP ショートカット規約 (Shortcut Convention)

> **status: draft v0.8** — 2026-06-15 curation。 WebView 統合 (#536/#537) で main-view directive bridge を撤去した際、 `e/g/h/w/i` が no-op 化したのを機に directive 群を棚卸し。 **盲目的に復活させず、 必要最小限の動詞に絞る**方針 (VP流 = 焦らず使用感ベース) を確定。
>
> 主な変更 (v0.7 → v0.8):
> 1. §C.2 から `e` / `g` / `h` (Stand focus) を**撤去** — Scene hotkey `Ctrl+Shift+1..4` (§C.4) と役割重複のため Scene 側に一本化
> 2. §C.2 から `w` (TheWorld status) を**撤去** — 将来の Unison WebView 直結 UI に status を委ねる
> 3. §C.2 から `?` / `i` (meta cheatsheet) を**撤去** — directive を最小動詞に絞る方針で不要、 cheatsheet の SSOT は本 doc
> 4. 撤去した letter (`e/g/h/w/i`) は §C.1 「未使用 letter」 プールに戻す
> 5. 実装: main-view directive bridge (Rust 往復) は #536/#537 で撤去済、 in-process `runMainViewDirective` fallback も撤去 (= 残る directive は全て sidebar 側に実体を持つ)
>
> 確定 directive (v0.8): `f` / `p` / `r` / `n` / `s` / `d` / `l` の **7 動詞**。
>
> 主な変更 (v0.3 → v0.4):
> 1. §C.2 (確定 directive) に `e` / `g` / `h` / `w` を昇格 (v0.3 予約 → v0.4 確定)
> 2. §C.3 (予約 directive) に `s` / `n` / `r` / `d` / `t` / `m` / `a` / `o` / `?` を整理
> 3. §C.4 (既存単発) に `Ctrl+Shift+C` global fallback を追記 (undocumented gap 解消)
> 4. **新規 §C.6 — 不採用 directive list**: shortcut で操作しない動作 (= `c` clear 等 destructive action) を明記
> 5. §D に新サブセクション "**context polymorphism dispatch table**" 追加 (`r` / `d` の Scene 依存 dispatch を例示)
> 6. §A.1 panel-local 例に「picker / AddPerformer form」 を明示
> 7. §C.1 に未使用 letter (= 将来予約候補) を「reserved unused」 として明記
>
> v0.3 の core (= directive 集合 + Cmd hold + 単発キー、 3 layer 疎結合) は keep。 文法 / 実装方針 / Avoid List は variable。

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
| `panel-local` | panel 内操作 | 既に panel に focus がある状態の選択 / 確定操作 | picker 内 `p` = "現在 selected file を PP へ" / FileExplorer の `↑↓ / Enter / Esc` / AddPerformer form の `Enter / Esc` |
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

> **v0.8 で撤去**: `e` (Echoes) / `g` (Gold Experience) / `h` (Hermit Purple) の **Stand focus** は Scene hotkey `Ctrl+Shift+1..4` (§C.4) と役割が重複していたため、 Scene 側に一本化して directive からは撤去した。 `w` (TheWorld status) も撤去 (将来 Unison WebView 直結 UI で status を別表現)。 これらの letter は「未使用 letter」 プールに戻る。 `p` は Canvas (PP) への投擲動詞として唯一の Stand 系 directive。

#### Action / category 系 directive (= panel / mode trigger)

| letter | 意味 | 主挙動 |
|--------|------|--------|
| `f` | **f**ile | File Explorer overlay (sidebar) を open + focus 移動 |
| `l` | **l**ane | Lane list panel (dedicated) を open + focus 移動 (v1.0 では既存 sidebar の lane list が常時 visible なので no-op 寄り、 将来 dedicated panel) |
| `n` | **n**ew | new performer 作成 prompt (= sidebar "+ Add Performer" form を起動) |
| `s` | **s**elect / **s**witch | Lane / project 切替 picker (active lane を選ぶ overlay) |
| `r` | **r**estart | context: project focus → process:restart、 lane focus → lane:restart |
| `d` | **d**elete | focused entity 削除 (2-click confirm 内蔵) |
| `t` | **t**itle | session rename (= cc の /rename と等価) |
| `m` | **m**ailbox | mailbox 経由 messaging picker |
| `a` | **a**ctivate | stopped project の SP 起動 |
| `o` | **o**pen | generic open (URL / lane / etc 別 picker) |

#### 不採用 letter (= shortcut では操作しない、 v0.4 で明示確定)

| letter | 意味 | 理由 |
|--------|------|------|
| `c` | clear / close | destructive action (= 何かを消す) は 1 押し misfire リスクが高い。 明示的な UI button 経由のみ。 詳細は §C.6 |

#### 未使用 letter (= 将来予約候補、 まだ意味割当なし)

`b` `e` `g` `h` `i` `j` `k` `q` `u` `v` `w` `x` `y` `z` (14 letters)。 `e/g/h/w/i` は v0.8 curation で directive から外して本プールに復帰 (Stand focus は Scene hotkey に移譲、 cheatsheet は本 doc が SSOT)。 新 directive 追加時はこの中から選ぶか、 既存 letter の polymorphism 拡張を検討。

### C.2 確定 directive (v0.4 時点で実装済 / 実装予定)

| binding | directive | semantic | 意味 (context dispatch) | 実装状況 |
|---------|-----------|----------|-------------------------|---------|
| `Cmd hold f` | `f` (file) | focus-transferring | **どこから打っても** sidebar の File Explorer overlay を open + sidebar focus へ移動 | PR #441 merged |
| `Cmd hold p` | `p` (PP) | panel-local | **File Explorer picker visible 中なら**: 選択中 file を PP (Canvas) に送る + picker は pin 状態に関係なく **連続選択を許す** (= dismiss しない) | PR #441 merged |
| `Cmd hold r` | `r` (restart) | context polymorphic | active_lane → `lane:restart` IPC、 active_stand → `process:restart` IPC、 どちらもなければ no-op (詳細 §D.7) | PR 445 |
| `Cmd hold n` | `n` (new performer) | focus-transferring | active project (= active_lane / active_stand の project) の AddPerformer form を keyboard で open (= sidebar 内 ProjectAccordion の form を expand) | PR 445 |
| `Cmd hold s` | `s` (switch) | focus-transferring | Lane / project switcher picker overlay を open (LanePicker.tsx、 fuzzy 検索 + flat list)。 lane 選択で `lane:select`、 project 選択で `process:toggle` (= accordion expand) | PR 445 |
| `Cmd hold d` | `d` (delete) | context polymorphic | 2-click confirm 内蔵: 1 回目で pending state + sidebar 下端 hint bar 表示、 1 秒以内 2 回目で execute (active_lane の Performer → `lane:delete`、 active_stand → `process:delete`)、 timeout で abort | PR 445 |
| `Cmd hold l` | `l` (lane number switcher mode) | focus-transferring (mode) | **mode-based directive**: ⌘ hold l で mode 突入 (= sidebar 下端に hint bar)、 **5 秒以内に modifier なし 1-9 単発キー** で `collectVisibleLanes()` (= expanded project の中の lane を上から flat list) の N 番目を `lane:select`。 Esc / 他キー / timeout で abort。 input フォーカス時は数字入力を妨げない (= mode abort) | PR 447 |

> **v0.8 で撤去**: `e` / `g` / `h` (Stand focus、 旧 PR #444) は Scene hotkey `Ctrl+Shift+1..4` (§C.4) と重複のため撤去。 `w` (TheWorld status、 旧 PR #444) は将来の Unison WebView 直結 UI に委ねるため撤去。 `?`→`i` (meta cheatsheet、 旧 PR 447/454) は directive を最小動詞に絞る方針で撤去 (cheatsheet の SSOT は本 doc)。 いずれも #536/#537 で main-view bridge が撤去され no-op 化していたものを、 復活させず正式に削除した。

### C.3 予約 directive (v0.4 文法に基づく、 実装は別 PR)

| binding | directive | semantic | 意味 |
|---------|-----------|----------|------|
| `Cmd hold t` | `t` | focus-preserving | active lane の cc session の rename (= `/rename` 等価) |
| `Cmd hold m` | `m` | focus-transferring | mailbox 経由 messaging picker |
| `Cmd hold a` | `a` | focus-preserving | stopped project の SP を auto-spawn 起動 |
| `Cmd hold o` | `o` | focus-transferring | generic open picker (URL / lane / external doc 等) |

### C.4 既存単発 shortcut (規約 v0.4 と整合)

| key | 用途 | 状態 |
|-----|------|------|
| `Cmd+N` | New Window (muda menu) | **既存 keep** — `n` は directive 予約 (§C.3) と棲み分け: menu accelerator は `Cmd+N` 単発、 directive は `Cmd hold n` (= 結果として同 keydown event だが、 dispatch 順は menu accelerator が先) |
| `Cmd+W` / `Cmd+Q` | Close Window / Quit (predefined) | **既存 keep** (system) |
| `Cmd+C/V/X/Z/A` 等 | Edit menu predefined | **既存 keep** (system) |
| `Ctrl+Shift+1..4` | Scene 切替 (`webview/keybindings.ts`) | **既存 keep** (layout) — v0.8 以降は **Stand focus の SSOT** (旧 `e/g/h` directive を吸収。 Scene = lead-focus / side-review / pp-overlay / pp-focus) |
| `Ctrl+Shift+] / [` | Scene cyclic | **既存 keep** (layout) |
| **`Ctrl+Shift+C`** (`main_area.rs:1303-1319`) | active lane の selection copy (global fallback listener) | **既存 keep** — v0.4 で明示化 (= undocumented gap 解消)。 lane individual handler が捕り逃した場合の system-level clipboard copy fallback、 規約 system カテゴリに属する |
| `Ctrl+Insert` / `Shift+Insert` (xterm.js) | terminal context での copy / paste | **既存 keep** (system) — terminal は xterm.js native handler で上書き、 詳細は §D.6 |

### C.5 Legacy / 互換 shortcut

| key | 状態 |
|-----|------|
| `Cmd+F` (sidebar focus 中の File Explorer 起動、 PR #439 で実装) | 規約 v0.3 で **`f` directive にそのまま昇格** — 文字 binding は変わらない、 但し挙動が「sidebar focus 中のみ」 から「どこからでも」 に拡張。 旧 code path は generalize される |
| sidebar の `📁` button (LaneRow) | 恒久的 keep (discoverability + mouse 派 user 救済) |

### C.6 不採用 directive (= shortcut で操作しない動作、 v0.4 で明示確定)

shortcut として **採用しない** 動作を明示する。 これらは「将来 directive 化してはいけない」 ことを規約として宣言する section。

| letter / 動作 | 不採用理由 | 代替手段 |
|---|---|---|
| `c` (clear / close) | **destructive action は 1 押し misfire のリスクが高い**。 PP clear / Echoes session clear / lane performer 削除等を keyboard 1 動作で行うと、 隣のキーとの押し間違いで content が失われる。 user は「焦らず使用感を確かめる」 VP 方針と整合 | 明示的な UI button (PP の `data-action="clear"` button 等)、 もしくは `Cmd hold d` (delete) の 2-click confirm path |
| (TBD) | (将来追加される候補) | (将来) |

不採用宣言は **規約 v0.4 以降の invariant**。 不採用宣言を覆す (= 採用に転じる) には Layer A 級の decision が必要 (= creo-memories memory + 本 doc 大幅改訂)。

---

## Layer D — 実装方針

### D.1 単純な keydown dispatcher (state machine 不要)

`Cmd hold + key` は keydown event 上で `metaKey: true` + 文字キー 1 つの 1 event。 chord 2 段 state machine は不要、 単純な keydown listener で directive lookup + exec:

```ts
// webview/src/shortcuts/directive.ts (要旨)
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

`webview/src/shortcuts/directive-table.ts` (= **single key map**):

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

### D.6 menu との関係 + terminal (xterm.js) との関係

- directive は muda menu accelerator として表現できない (= `Cmd+letter` 単発と同じ key event だが、 menu accelerator は OS-level で WebView より先に発火するため、 menu accelerator に登録した key は WebView listener に届かない)
- **directive 用 letter は menu accelerator に bind しない**: `Cmd+N` (= menu accelerator) は keep、 `Cmd+F` / `Cmd+P` 等 directive 用 letter は **menu accelerator なし**
- menu item は **chord-less で残し** (mouse 派 user 救済)、 title に「(⌘ hold f)」 等の hint を併記

**terminal (xterm.js) 領域**:
- terminal pane の keydown は xterm.js の `CustomKeyEventHandler` で **上書きされ得る** (= `main_area.rs:1025-1045` 参照)
- 主な override: `Ctrl+Insert` / `Shift+Insert` (clipboard)、 terminal selection 中の `Ctrl+C` (= copy semantics の context-aware fallback)
- これらは規約 system カテゴリで keep。 directive listener は capture phase で取るため、 directive 用 letter (`f` / `p` / `r` / ...) は xterm.js より先に preventDefault される

### D.7 context polymorphism dispatch table

`r` / `d` 等の context dependent directive は、 「どの panel / Scene に focus があるか」 で挙動を分岐する。 dispatch 判定の SSOT を以下に明示:

| directive | context (= focused panel / Scene) | 動作 |
|---|---|---|
| `r` | active panel が **project header / SP scope** (Scene: `conductor-focus` 等) | `process:restart` IPC を送信 |
| `r` | active panel が **lane / Echoes** (Scene: `pp-overlay` 等) | `lane:restart` IPC を送信 |
| `d` | sidebar focus + lane row selected | `lane:delete` IPC (2-click confirm) |
| `d` | sidebar focus + project header selected | `process:delete` IPC (2-click confirm) |
| `d` | picker visible 中 | (TBD) selected file の delete (= 危険、 v0.4 では no-op) |
| `p` | File Explorer picker visible | selected file を Canvas へ送る (panel-local) |
| `p` | cc 入力欄 focus 中 | (TBD、 v0.4 では未実装) cc 内容を PP に送る |
| `p` | その他 | no-op + debug log |

dispatch 判定は **main view の Scene state** (`frameEngine.getCurrentSceneId()`) + **sidebar の active_lane / active_stand** + **picker visibility** の 3 軸を組み合わせる。 詳細実装は各 directive PR で確定。

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
3. **Cheatsheet は本 doc が SSOT** (v0.8): 旧 in-app cheatsheet directive (`?`→`i`) は撤去。 全 directive 一覧は §C.2 を参照。 in-app discoverability は ⌘K Command Palette (= `registry.ts` ACTIONS) が担う
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
| 2026-06-15 | v0.8 draft | §C.1 / §C.2 / §C.4 / Layer F | **directive curation** (no-op 撤去)。 #536/#537 の WebView 統合で main-view directive bridge (Rust 往復) が撤去され `e/g/h/w/i` が no-op 化したのを機に棚卸し。 復活させず正式削除: `e/g/h` (Stand focus) は Scene hotkey `Ctrl+Shift+1..4` に一本化、 `w` (TheWorld status) は将来 Unison WebView 直結 UI に委譲、 `?`→`i` (cheatsheet) は最小動詞方針で撤去 (SSOT は本 doc + ⌘K palette)。 in-process `runMainViewDirective` fallback も撤去。 確定 directive は `f/p/r/n/s/d/l` の 7 動詞。 | (本 PR) |
| 2026-05-27 | v0.7 draft | §C.2 / Layer D | meta directive を **`?` → `i` に rebind**。 v0.6 で `Cmd+Shift+/` (= `?`) に bind したが macOS の AppKit が `Cmd+?` を OS-level で「Help menu search」 に予約しており keydown が webview に届かず directive 発火しない issue を dogfood で発見。 `Cmd hold i` (info / cheatsheet) で機能復活。 chord.ts shift 例外は keep (= 将来 shift+symbol 系の余地)。 | PR 454 |
| 2026-05-27 | v0.6 draft | §C.2 / §C.3 / Layer D | (1) `?` (meta cheatsheet) を §C.2 確定昇格、 chord.ts に shift 例外 (= shift + symbol は通す、 letter のみ reject)。 (2) `l` の意味を「lane panel 予約」 から **「lane number switcher mode」** に再定義 → §C.2 確定昇格: ⌘ hold l で mode 突入、 modifier なし 1-9 で expanded project 内 lane を上から N 番目で切替 (mode-based directive、 5 秒 timeout)。 (3) cheatsheet markdown を Rust 静的生成、 `AppEvent::DirectiveInject` で PP に inject | PR 447 |
| 2026-05-27 | v0.5 draft | §C.2 / §C.3 | `r` / `n` / `s` / `d` を確定 directive に昇格 (PR 445 で実装、 sidebar 側 polymorphic dispatch + LanePicker.tsx 新規 + 2-click confirm hint bar)。 §C.3 から 4 entry を移動、 残り予約は `t/m/a/l/o/?` | PR 445 |
| 2026-05-27 | v0.4 §C.2 | `e` / `g` / `h` / `w` を「v0.4 予定」 から **実装済 (PR #444)** に reclassify | PR 445 (同梱) |
| 2026-05-26 | v0.4 draft | §C / §D / §A.1 | 既存 shortcut の完全棚卸し + Layer C 拡充 (`e/g/h/w` を確定 directive に昇格、 `s/n/r/d/t/m/a/o/?` を予約 directive に整理) + undocumented (`Ctrl+Shift+C` 等) を §C.4 に明示化 + 新規 §C.6「不採用 directive」 (= `c` clear 不採用を invariant 宣言) + §D.7 「context polymorphism dispatch table」 | PR 442 |
| 2026-05-26 | v0.3 draft | (full) | chord 2 段 state machine 設計を破棄、 「directive 集合 + Cmd hold 単発キー」 に再構築。 user の flow 視点 (`Cmd hold f → 操作 → Cmd hold p`) を素直に表現 | PR #441 |
| 2026-05-26 | v0.2 draft | (full) | 2 layer 構造 (invariant / mutable 分離) | (前 commit、 同 PR で上書き) |
| 2026-05-26 | v0.1 draft | (full) | 初版起草 (single layer、 superseded by v0.2) | (前 commit、 同 PR で上書き) |

---

## 関連

- PR #439: File Explorer overlay picker (Rust IPC + UI)
- PR #440: File Explorer follow-up (z-index / race guard)
- PR #441: 規約 v0.3 + Layer C `f` / `p` directive 実装 (merged)
- PR 442 (本 update): 規約 v0.4 doc only update (棚卸し + Layer C 拡充 + `c` 不採用宣言)
- PR 443 (予定): `e` / `g` / `h` / `w` directive 実装 (Stand focus 系)
- PR 444 (予定): lane 操作 directive `s` / `n` / `r` / `d` 実装 (context polymorphism)
- PR 445 (予定): `?` directive (cheatsheet)
- PR 446+ (余力で): `t` / `m` / `a` / `l` / `o` 逐次

将来の創発 (ideas / TBD):
- mouse / drag-drop で directive を発火する equivalent path
- multi-lane targeting (= 別 lane の PP に投げる、 prefix-key で lane number 指定 等)
- accessibility (sticky-key で directive 発火サポート確認)
- picker context-aware な `p` の挙動: pin 状態とどう integrative にするか
- 未使用 letter (`b/i/j/k/q/u/v/x/y/z`) への directive 割当 (= 必要が出てから)
