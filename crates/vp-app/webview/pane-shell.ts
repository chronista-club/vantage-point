/**
 * Pane shell — lane の表示領域を tiling にする（doc 46 P1）。
 *
 * lane の中身を「Act I **か** Act II」の排他から、**N 枚の Pane を並べる**形に変える。
 * 要件（mako 2026-07-21）:
 *   1. 既定で左右に並べる
 *   2. 1 クリックでタブエリアに縮小 / Pane に復元
 *   3. フォーカスが視認でき、移動できる
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: [`PaneLayout`] — 並び / 縮小 / focus だけを持つ。DOM を知らない
 * - **calculations**: `dock` / `minimize` / `focus` / `moveFocus` — 全て純関数的な状態遷移。
 *   document 非依存なので vitest（node 環境）で全分岐を固定できる
 * - **actions**: [`PaneShell`] — layout を DOM に写すだけ。class の付け外しと chip の再構築
 *
 * Pane の中身（xterm・ChatView）は既存の host 要素がそのまま担い、本 module は
 * **要素に class を付け外しするだけ**。中身のロジックに触れないことで、Act 切替まわりの
 * 既存資産（doc 33 / doc 38）を巻き込まずに tiling を被せられる。
 *
 * layout の真実源は **in-memory**（doc 46 §4.2）。並べ方は「今この瞬間の作業の形」で
 * lane より寿命が短く、永続すべきかは dogfood してから決める。
 */

/** Pane 1 枚分の identity（中身は本 module の関心外）。 */
export type PaneRef = {
  /** 安定 id（= host 要素の DOM id）。 */
  id: string
  /** タブ chip / tooltip に出す短い名前。 */
  label: string
}

/**
 * Pane の並び・縮小・focus を持つ純粋な状態（data + calculations）。
 *
 * `panes` の**並び順が表示順**。minimize しても配列からは抜かない — 復元時に元の位置へ
 * 戻すため（抜くと必ず末尾に戻り「畳んだら場所が変わった」になる）。
 */
export class PaneLayout {
  private panes: PaneRef[] = []
  private minimized = new Set<string>()
  private focusedId: string | null = null

  /** Pane を登録する（既存 id なら差し替え）。最初の 1 枚が focus を持つ。 */
  dock(pane: PaneRef): void {
    const i = this.panes.findIndex((p) => p.id === pane.id)
    if (i >= 0) this.panes[i] = pane
    else this.panes.push(pane)
    if (this.focusedId == null) this.focusedId = pane.id
  }

  /** 登録順（= 表示順）の全 Pane。 */
  all(): readonly PaneRef[] {
    return this.panes
  }

  /** 今 docked（= 並んでいる）Pane の id 列。表示順。 */
  dockedIds(): string[] {
    return this.panes.filter((p) => !this.minimized.has(p.id)).map((p) => p.id)
  }

  /** 今 minimized な Pane。タブ chip の並び順でもある。 */
  minimizedPanes(): PaneRef[] {
    return this.panes.filter((p) => this.minimized.has(p.id))
  }

  isMinimized(id: string): boolean {
    return this.minimized.has(id)
  }

  focused(): string | null {
    return this.focusedId
  }

  /**
   * Pane を最小化する（要件 2）。実際に畳んだら true。
   *
   * **最後の 1 枚は畳ませない** — 全部畳むと lane の表示が空になり、戻る取っ掛かりが
   * タブ chip だけになる。空の画面から戻れなくなるより、常に 1 枚残す方が安全。
   */
  minimize(id: string): boolean {
    if (this.minimized.has(id)) return false
    if (!this.panes.some((p) => p.id === id)) return false
    if (this.dockedIds().length <= 1) return false
    this.minimized.add(id)
    // 畳んだ Pane が focus を持っていたら残った先頭へ移す（focus を失わせない）。
    if (this.focusedId === id) this.focusedId = this.dockedIds()[0] ?? null
    return true
  }

  /** 元の位置へ復元し、focus も移す（畳んだものを開く = 見たいはずなので）。 */
  restore(id: string): void {
    if (!this.minimized.delete(id)) return
    this.focusedId = id
  }

  /** 1 クリックで往復（要件 2）。 */
  toggle(id: string): void {
    if (this.minimized.has(id)) this.restore(id)
    else this.minimize(id)
  }

  /** focus を当てる（要件 3）。minimized を指したら復元も行う。 */
  focus(id: string): void {
    if (!this.panes.some((p) => p.id === id)) return
    if (this.minimized.has(id)) {
      this.restore(id)
      return
    }
    this.focusedId = id
  }

  /**
   * focus を隣へ移す（要件 3、`delta` は -1 / +1）。
   *
   * docked な Pane の**表示順**で回る（minimized は飛ばす）。端で止めず巻き戻すのは、
   * 2 枚構成で「右へ」を繰り返した時に無反応にならないため。
   */
  moveFocus(delta: number): void {
    const ids = this.dockedIds()
    if (ids.length === 0) return
    const cur = this.focusedId == null ? -1 : ids.indexOf(this.focusedId)
    const next = cur < 0 ? 0 : (cur + delta + ids.length) % ids.length
    this.focusedId = ids[next] ?? null
  }
}

/** 新 Pane の選択肢 1 つ（doc 46 P2 要件 4: Engine × Act）。 */
export type NewPaneChoice = {
  /** stand 名（`echoes` / `codex` / `grok` …）。 */
  engine: string
  /** 表示名（engine の人間可読名）。 */
  engineLabel: string
  act: 'tui' | 'chat'
}

/**
 * Engine × Act の総当たりを作る（doc 46 P2 要件 4、純関数）。
 *
 * `chatCapable` が false の engine は **Act II（chat）を出さない** — chat host を持たない
 * engine で chat Pane を作ると「作れるが submit がエラーになるだけ」の行き止まりになる
 * （doc 38 Phase 3 が tab の「+」で同じ判断をしている）。Act I（tui）は login shell に
 * 流し込むだけなのでどの engine でも成立する。
 */
export function newPaneChoices(
  stands: readonly { name: string; label?: string; chat_capable?: boolean }[],
): NewPaneChoice[] {
  const out: NewPaneChoice[] = []
  for (const s of stands) {
    if (!s.name) continue
    const engineLabel = s.label && s.label.length > 0 ? s.label : s.name
    out.push({ engine: s.name, engineLabel, act: 'tui' })
    if (s.chat_capable) out.push({ engine: s.name, engineLabel, act: 'chat' })
  }
  return out
}

export const CLASS_MINIMIZED = 'pane-minimized'
export const CLASS_FOCUSED = 'pane-focused'
/** タブエリアの開閉（chip がある時だけ開く = 空なら従来の見た目）。 */
export const CLASS_TABS_ACTIVE = 'pane-tabs-active'

/**
 * [`PaneLayout`] を DOM に写す薄い binder（actions）。
 *
 * `render` は**べき等** — 状態から DOM を作り直すだけで、差分を追わない。
 * Pane 数が数枚である前提で、差分計算のバグより作り直しの単純さを取る。
 */
export class PaneShell {
  readonly layout = new PaneLayout()

  constructor(
    /** Pane host 要素の解決（id → 要素）。テストから差し替え可能にするため関数で受ける。 */
    private readonly hostOf: (id: string) => HTMLElement | null,
    private readonly tabs: HTMLElement,
    /** タブエリアの開閉 class を載せる要素。 */
    private readonly frame: HTMLElement,
  ) {}

  dock(pane: PaneRef): void {
    this.layout.dock(pane)
    this.render()
  }

  toggle(id: string): void {
    this.layout.toggle(id)
    this.render()
  }

  focus(id: string): void {
    this.layout.focus(id)
    this.render()
  }

  moveFocus(delta: number): void {
    this.layout.moveFocus(delta)
    this.render()
  }

  render(): void {
    for (const p of this.layout.all()) {
      const el = this.hostOf(p.id)
      if (!el) continue
      el.classList.toggle(CLASS_MINIMIZED, this.layout.isMinimized(p.id))
      el.classList.toggle(CLASS_FOCUSED, this.layout.focused() === p.id)
    }
    // タブエリアは **全 Pane のスイッチャー**。畳んだものだけ並べると
    // 「並んでいる Pane を畳む」入口が UI に無くなる（要件 2 の片道しか通らない）。
    // docked は `.docked` を付けて「今出ている」ことを示し、click で往復する。
    const min = this.layout.minimizedPanes()
    this.tabs.replaceChildren()
    for (const p of this.layout.all()) {
      const isMin = this.layout.isMinimized(p.id)
      const chip = document.createElement('button')
      chip.type = 'button'
      chip.className = isMin ? 'pane-tab' : 'pane-tab docked'
      chip.dataset.paneId = p.id
      chip.textContent = p.label
      chip.title = isMin ? `${p.label} を開く` : `${p.label} を畳む`
      chip.addEventListener('click', () => this.toggle(p.id))
      this.tabs.appendChild(chip)
    }
    this.frame.classList.toggle(CLASS_TABS_ACTIVE, min.length > 0)
  }
}
