/**
 * Directive Registry (= Layer C "Directive Registry" in docs/design/18-shortcut-convention.md)
 *
 * **SSOT** for 個別の directive (動詞) 定義。 main view / sidebar 双方の keydown
 * dispatcher がこの table を読んで「`Cmd hold + key` が registered directive か」 判定する。
 *
 * Layer A (操作体系 / invariant) と Layer B (modifier binding / mutable) は doc 参照。
 * 本 file は **Layer C (個別 directive 登録、 mutable per PR)** に対応する。
 *
 * 文法は **単発キー** (= chord 2 段ではない、 規約 v0.3 で確定)。 user の「Cmd hold f → 操作
 * → Cmd hold p」 flow は OS 上で **2 つの独立 `Cmd+letter` keydown** として届く。
 */

export type DirectiveSemantic =
  | 'focus-preserving'
  | 'focus-transferring'
  | 'panel-local'
  | 'layout'
  | 'system'

export interface DirectiveEntry {
  /** 説明 (cheatsheet / hint bar 表示用) */
  description: string
  /** 主たる挙動軸 (実際は context dependent polymorphic、 ここは default の semantic) */
  semantic: DirectiveSemantic
}

/**
 * 確定 directive registry (v0.8)。 新しい directive 追加時はここに 1 行足す + 規約 doc Layer C も更新。
 *
 * key は **小文字 1 文字**。 user が打つ `Cmd hold + <letter>` keydown event の `e.key.toLowerCase()` で照合する。
 */
export const DIRECTIVE_TABLE: Record<string, DirectiveEntry> = {
  // v0.3 (PR #441) で実装
  f: {
    description: 'File Explorer overlay (sidebar) を open + focus 移動',
    semantic: 'focus-transferring',
  },
  p: {
    description:
      'send current/selected to board (file picker visible 中なら選択 file を Canvas に投擲、 picker は dismiss しない)',
    semantic: 'panel-local',
  },
  // v0.5 (PR 445) で実装 — lane 操作系
  r: {
    description:
      'restart context polymorphic: active_lane あり → lane:restart、 active_stand あり → process:restart',
    semantic: 'focus-preserving',
  },
  n: {
    description: 'active repo の AddPerformer form を keyboard で open',
    semantic: 'focus-transferring',
  },
  s: {
    description: 'Lane / repo switcher picker overlay (fuzzy + list)',
    semantic: 'focus-transferring',
  },
  d: {
    description:
      'delete focused entity with 2-click confirm (1 秒以内に 2 回目で execute、 timeout で abort)',
    semantic: 'focus-preserving',
  },
  // v0.6 (PR 447) — lane number switcher mode (= v0.4 §C.3 で予約していた `l` (lane panel) を再定義)
  l: {
    description:
      'lane number switcher mode: ⌘ hold l で mode 突入、 5 秒以内に 1-9 で expanded repo 内 lane を上から N 番目で lane:select',
    semantic: 'focus-transferring',
  },
  // v0.8 (curation) で撤去:
  //   e/g/h (Stand focus) — Scene hotkey `Ctrl+Shift+1..4` と役割重複のため Scene 側に一本化。
  //   w (daemon status)  — 将来の Unison WebView 直結 UI に status を委ねるため撤去。
  //   i (cheatsheet)       — directive を最小動詞に絞る方針で不要。SSOT は docs/design/18。
}
