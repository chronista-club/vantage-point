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
 * 確定 directive registry (v0.3)。 新しい directive 追加時はここに 1 行足す + 規約 doc Layer C も更新。
 *
 * key は **小文字 1 文字**。 user が打つ `Cmd hold + <letter>` keydown event の `e.key.toLowerCase()` で照合する。
 */
export const DIRECTIVE_TABLE: Record<string, DirectiveEntry> = {
  f: {
    description: 'File Explorer overlay (sidebar) を open + focus 移動',
    semantic: 'focus-transferring',
  },
  p: {
    description:
      'send current/selected to PP (file picker visible 中なら選択 file を Canvas に投擲、 picker は dismiss しない)',
    semantic: 'panel-local',
  },
}
