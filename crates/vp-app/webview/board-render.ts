/**
 * Board (board) body の markdown render API。
 *
 * VP-141 / PR-ε-2 で marked.parse で開始。 pp-content-persist follow-up (2026-05-28) で
 * creoui-md-view (= creo-views/md WASM mdast) に置換したが、 wry WebView で WASM module の
 * base URL 解決が落ちて `creo_md_wasm_bg.wasm cannot be parsed as a URL` で全 markdown
 * render が失敗 (dogfood NG)。 上流 creoui-md-view が phase-1-stub で wry 未対応のため
 * **marked-based に revert** (2026-05-28)。 mermaid は creo-md とは独立の npm package
 * なので維持 (= WASM 不要、 marked render 後に自前 hook で SVG 化)。
 *
 * creo-md migration は creoui-md-view 0.2 + creo-views/mermaid Phase 2 完成 + wry WASM URL
 * 解決ができてから別 PR で再挑戦する。
 *
 * 設計の核:
 * - 純 action layer (DOM 操作のみ)、 state は持たない
 * - `marked` を sync mode (default) で使用、 戻り値 string を `as string` で narrow
 * - markdown / text は caller (= mcp__show 経由、 開発者自身の Claude session) 信頼前提で
 *   innerHTML 直挿し。 html (content_type=html) は `<iframe srcdoc sandbox="allow-scripts">`
 *   に隔離 — script は実行できるが opaque origin で親 document / cookie / storage に触れない
 * - mermaid block は marked 後に `runMermaidPostProcess()` が ```mermaid code block を見つけて
 *   `mermaid.render` で SVG に置換 (= WASM 不要、 npm mermaid package のみ)
 *
 * 公開 API (entry.tsx で window.vpPP に attach):
 * - `renderBoard(content, contentType?)`: board body を上書き render
 * - `clearBoard()`: board body を空にする
 * - `appendBoard(content, contentType?)`: board body に末尾追加 (timeline-style 累積表示用)
 */

import { marked } from 'marked'
import mermaid from 'mermaid'

/** board body の DOM target selector. main_area.rs HTML 側で `id="board-content"` を保証. */
const TARGET_SELECTOR = '#board-content'

export type ContentType = 'markdown' | 'text' | 'html'

function getTarget(): HTMLElement | null {
  return document.querySelector<HTMLElement>(TARGET_SELECTOR)
}

// ----------------------------------------------------------------------------
// mermaid post-process (= marked が ```mermaid を <pre><code class="language-mermaid">
// で吐くので、 render 後に走査して SVG 置換。 WASM 非依存、 npm mermaid package のみ)
// ----------------------------------------------------------------------------

let mermaidInitialized = false

function ensureMermaidInitialized(): void {
  if (mermaidInitialized) return
  mermaidInitialized = true
  // securityLevel=loose で raw text を直接食わせる (= WebView 内 closed 環境、 user 自身の content)。
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'loose',
    theme: 'dark',
  })
}

/** 連番 id 用 counter。 `mermaid.render` の DOM unique id 生成に使う。 */
let mermaidIdSeq = 0

/**
 * marked が ```mermaid を吐く `<pre><code class="language-mermaid">` を見つけて、
 * 中の code text を `mermaid.render` で SVG 化して `<pre>` の outer を置換する。
 *
 * render エラー時は元の code block を残しつつ error message を `<pre>` で見せる (= silent fail しない)。
 */
async function runMermaidPostProcess(container: HTMLElement): Promise<void> {
  const blocks = container.querySelectorAll<HTMLElement>('code.language-mermaid')
  if (blocks.length === 0) return
  ensureMermaidInitialized()
  for (const code of Array.from(blocks)) {
    const src = code.textContent ?? ''
    if (!src.trim()) continue
    // 置換対象は <pre><code> の <pre>。 無ければ code 自身。
    const replaceTarget = code.closest('pre') ?? code
    const id = `pp-mermaid-${mermaidIdSeq++}`
    try {
      const { svg } = await mermaid.render(id, src)
      const wrap = document.createElement('div')
      wrap.className = 'creo-md-mermaid'
      wrap.innerHTML = svg
      replaceTarget.replaceWith(wrap)
    } catch (e) {
      console.warn('[vpPP] mermaid.render failed', e)
      const errEl = document.createElement('pre')
      errEl.className = 'creo-md-mermaid-error'
      errEl.textContent = `mermaid render error: ${String(e)}\n\n${src}`
      replaceTarget.replaceWith(errEl)
    }
  }
}

// ----------------------------------------------------------------------------
// markdown / html / text の dispatch
// ----------------------------------------------------------------------------

function toHtml(content: string, contentType: ContentType): string {
  if (contentType === 'markdown') {
    // marked.parse は default sync mode で string を返す。 async option を入れた時のみ Promise。
    return marked.parse(content) as string
  }
  if (contentType === 'html') {
    // raw HTML は sandbox iframe (srcdoc) に隔離。 srcdoc 属性値に埋めるので & と " を
    // エスケープ — & を先に処理する (逆順だと " 由来の &quot; の & が二重エスケープ)。
    const escaped = content.replace(/&/g, '&amp;').replace(/"/g, '&quot;')
    return `<iframe class="board-html-frame" sandbox="allow-scripts" srcdoc="${escaped}"></iframe>`
  }
  // text: HTML escape して span で出す
  const span = document.createElement('span')
  span.textContent = content
  return span.outerHTML
}

/** board body を完全置換 render。 placeholder も含めて innerHTML が書き換わる. */
export function renderBoard(content: string, contentType: ContentType = 'markdown'): void {
  const target = getTarget()
  if (!target) {
    console.warn('[vpPP] renderBoard: target not found:', TARGET_SELECTOR)
    return
  }
  target.innerHTML = toHtml(content, contentType)
  // markdown は render 後に mermaid block を SVG 化 (= 非同期、 best-effort)。
  if (contentType === 'markdown') {
    void runMermaidPostProcess(target)
  }
  // html は iframe を board pane いっぱいに広げるため container を full-bleed に切り替える。
  // markdown / text は通常の padding 付き flow に戻す。
  target.classList.toggle('board-content-html', contentType === 'html')
}

/** board body を空にする (Clear button 等から呼ばれる). */
export function clearBoard(): void {
  const target = getTarget()
  if (!target) return
  target.innerHTML = ''
  // html render 時に付けた full-bleed class を戻す。
  target.classList.remove('board-content-html')
}

/**
 * board body の末尾に append。
 *
 * `innerHTML += ...` は既存 DOM の event listener を破棄するため使わない。
 * `insertAdjacentHTML('beforeend', ...)` で既存 DOM を保ったまま挿入する。
 * markdown の場合は append 後に mermaid post-process も走らせる。
 */
export function appendBoard(content: string, contentType: ContentType = 'markdown'): void {
  const target = getTarget()
  if (!target) return
  target.insertAdjacentHTML('beforeend', toHtml(content, contentType))
  if (contentType === 'markdown') {
    void runMermaidPostProcess(target)
  }
}
