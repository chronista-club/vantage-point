/**
 * Paisley Park (PP) body の markdown render API。
 *
 * VP-141 / PR-ε-2。 PP pane の `<div class="pp-content" id="pp-content">` を render target
 * として、 `window.vpPP.renderPP(content, contentType)` で markdown / text / html を流し込む
 * ための minimal API。 PR-ε-3 で `/ws/show` 経由 mcp__show が来た時の inject point として使う。
 *
 * 設計の核:
 * - 純 action layer (DOM 操作のみ)、 state は持たない (PP の RetainedStore 連動は S3+)
 * - `marked` を sync mode (default) で使用、 戻り値は string なので `as string` で narrow
 * - markdown / text は caller (= mcp__show 経由、 開発者自身の Claude session) 信頼前提で
 *   innerHTML 直挿し。 html (content_type=html) は `<iframe srcdoc sandbox="allow-scripts">`
 *   に隔離する — script は実行できるが opaque origin で親 document / cookie / storage に
 *   触れない。 外部 untrusted な markdown/text を扱う段階で sanitize 層 (DOMPurify 等) を検討
 *
 * 公開 API (entry.tsx で window.vpPP に attach):
 * - `renderPP(content, contentType?)`: PP body を上書き render
 * - `clearPP()`: PP body を空にする
 * - `appendPP(content, contentType?)`: PP body に末尾追加 (timeline-style 累積表示用、 S3+ で活きる)
 */

import { marked } from 'marked'

/** PP body の DOM target selector. main_area.rs HTML 側で `id="pp-content"` を保証. */
const TARGET_SELECTOR = '#pp-content'

export type ContentType = 'markdown' | 'text' | 'html'

function getTarget(): HTMLElement | null {
  return document.querySelector<HTMLElement>(TARGET_SELECTOR)
}

function toHtml(content: string, contentType: ContentType): string {
  if (contentType === 'markdown') {
    // marked.parse は default sync mode で string を返す。 async option を入れた時のみ Promise。
    // 今回 sync で十分なので as string で narrow。
    return marked.parse(content) as string
  }
  if (contentType === 'html') {
    // raw HTML は sandbox iframe (srcdoc) に隔離して render する。
    // innerHTML 直挿しだと <script> が実行されず <style> も PP 外へ漏れるため。
    // srcdoc 属性値に埋めるので & と " をエスケープ — & を先に処理する
    // (逆順だと " 由来の &quot; の & が二重エスケープされる)。
    const escaped = content.replace(/&/g, '&amp;').replace(/"/g, '&quot;')
    return `<iframe class="pp-html-frame" sandbox="allow-scripts" srcdoc="${escaped}"></iframe>`
  }
  // text: HTML escape して <pre> 風に出す
  const span = document.createElement('span')
  span.textContent = content
  return span.outerHTML
}

/** PP body を完全置換 render。 placeholder も含めて innerHTML が書き換わる. */
export function renderPP(content: string, contentType: ContentType = 'markdown'): void {
  const target = getTarget()
  if (!target) {
    console.warn('[vpPP] renderPP: target not found:', TARGET_SELECTOR)
    return
  }
  target.innerHTML = toHtml(content, contentType)
  // html は iframe を PP pane いっぱいに広げるため container を full-bleed に切り替える。
  // markdown / text は通常の padding 付き flow に戻す。
  target.classList.toggle('pp-content-html', contentType === 'html')
}

/** PP body を空にする (Clear button 等から呼ばれる). */
export function clearPP(): void {
  const target = getTarget()
  if (!target) return
  target.innerHTML = ''
  // html render 時に付けた full-bleed class を戻す。
  target.classList.remove('pp-content-html')
}

/**
 * PP body の末尾に append。
 *
 * `innerHTML += ...` は既存 DOM の event listener を破棄するため使わない。
 * `insertAdjacentHTML('beforeend', ...)` で既存 DOM を保ったまま挿入する。
 * S3 で creo memory feed の timeline 累積表示に使う想定。
 */
export function appendPP(content: string, contentType: ContentType = 'markdown'): void {
  const target = getTarget()
  if (!target) return
  target.insertAdjacentHTML('beforeend', toHtml(content, contentType))
}
