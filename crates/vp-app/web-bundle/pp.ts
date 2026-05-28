/**
 * Paisley Park (PP) body の markdown render API。
 *
 * VP-141 / PR-ε-2 で marked.parse 直挿しで開始、 pp-content-persist follow-up (2026-05-28)
 * で **creoui-md-view (SolidJS + creo-views/md WASM mdast)** + **mermaid (npm) 自前 hook**
 * に置換。 PP pane の `<div class="pp-content" id="pp-content">` を render target として、
 * `window.vpPP.renderPP(content, contentType)` で markdown / text / html を流し込む。
 *
 * 設計の核:
 * - 純 action layer (DOM 操作 + Solid mount のみ)、 state は持たない
 * - markdown は `<CreoMarkdown>` を mount。 内部で WASM mdast parse → SolidJS JSX render
 * - mermaid block は creoui-md-view が `.creo-md-mermaid-placeholder` で placeholder 化、
 *   render 完了後に `runMermaidPostProcess()` が `mermaid.render` で SVG に置換 (= path B)
 * - 上流 (creoui-md-view 0.2 + creo-views/mermaid Phase 2) が完成したら post-process hook を
 *   削除して移行できる構造に
 * - markdown / text は caller (= mcp__show 経由、 開発者自身の Claude session) 信頼前提。
 *   html (content_type=html) は `<iframe srcdoc sandbox="allow-scripts">` に隔離
 *
 * 公開 API (entry.tsx で window.vpPP に attach):
 * - `renderPP(content, contentType?)`: PP body を上書き render
 * - `clearPP()`: PP body を空にする
 * - `appendPP(content, contentType?)`: PP body に末尾追加 (text/html のみ純 DOM、 markdown は
 *   置換動作にフォールバック — accumulator が要れば S3+ で導入)
 */

import { CreoMarkdown } from 'creoui-md-view'
import mermaid from 'mermaid'
import { render } from 'solid-js/web'

/** PP body の DOM target selector. main_area.rs HTML 側で `id="pp-content"` を保証. */
const TARGET_SELECTOR = '#pp-content'

export type ContentType = 'markdown' | 'text' | 'html'

function getTarget(): HTMLElement | null {
  return document.querySelector<HTMLElement>(TARGET_SELECTOR)
}

// ----------------------------------------------------------------------------
// mermaid post-process (path B: creoui-md-view 0.2 / creo-views/mermaid Phase 2 完成までの
// 自前 hook。 上流来たら `runMermaidPostProcess` ごと削除すれば自動移行)
// ----------------------------------------------------------------------------

let mermaidInitialized = false

function ensureMermaidInitialized(): void {
  if (mermaidInitialized) return
  mermaidInitialized = true
  // securityLevel=loose で raw text を直接食わせる (= WebView 内 closed環境、 user 自身の content)。
  // creoui の theme に追従させたい場合は将来 'dark' / 'forest' 等を CSS variable から resolve。
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'loose',
    theme: 'dark',
  })
}

/** 連番 id 用 counter。 `mermaid.render` の DOM unique id 生成に使う。 */
let mermaidIdSeq = 0

/**
 * `.creo-md-mermaid-placeholder` (= creoui-md-view が Phase 0.1 で吐く placeholder) を見つけて、
 * 中の code text を `mermaid.render` で SVG 化して placeholder の outer を置換する。
 *
 * - creoui-md-view 0.2 が出たら `node.lang === 'mermaid'` を直接 SVG に describe するので、
 *   その時は本関数を削除 (= 1 行 removal で移行)。
 * - render エラー時は placeholder を残し、 error message を `<pre>` で見せる (= silent fail しない)。
 */
async function runMermaidPostProcess(container: HTMLElement): Promise<void> {
  const placeholders = container.querySelectorAll<HTMLElement>('.creo-md-mermaid-placeholder')
  if (placeholders.length === 0) return
  ensureMermaidInitialized()
  for (const ph of Array.from(placeholders)) {
    // placeholder 中の `<pre><code>` から元 text を取り出す (= creoui-md-view が体裁を持って保持)。
    // 念のため両方を試して、 textContent が取れたら採用。
    const code = ph.querySelector<HTMLElement>('code')?.textContent ?? ph.textContent ?? ''
    if (!code.trim()) continue
    const id = `pp-mermaid-${mermaidIdSeq++}`
    try {
      const { svg } = await mermaid.render(id, code)
      // SVG element で placeholder を outer 置換。 wrapper class を残して theming に使えるように。
      const wrap = document.createElement('div')
      wrap.className = 'creo-md-mermaid'
      wrap.innerHTML = svg
      ph.replaceWith(wrap)
    } catch (e) {
      // 失敗時は placeholder を残しつつ error 表示。 console にも出して dev で気付けるように。
      console.warn('[vpPP] mermaid.render failed', e)
      const errEl = document.createElement('pre')
      errEl.className = 'creo-md-mermaid-error'
      errEl.textContent = `mermaid render error: ${String(e)}\n\n${code}`
      ph.replaceWith(errEl)
    }
  }
}

// ----------------------------------------------------------------------------
// markdown / html / text の dispatch
// ----------------------------------------------------------------------------

/** PP body 上の現 SolidJS root を破棄するための teardown ハンドル。 */
let currentMarkdownDispose: (() => void) | null = null

function disposeMarkdown(): void {
  if (currentMarkdownDispose) {
    try {
      currentMarkdownDispose()
    } catch (e) {
      console.warn('[vpPP] markdown dispose failed', e)
    }
    currentMarkdownDispose = null
  }
}

function renderMarkdown(target: HTMLElement, content: string): void {
  // Solid の mount 先を一度空に + 過去の root を dispose してから再 mount。
  // (= 同じ DOM node に 2 度 mount すると Solid が複数 root を管理する形になり安全性が下がる)
  disposeMarkdown()
  target.innerHTML = ''
  currentMarkdownDispose = render(() => CreoMarkdown({ text: content }), target)
  // SolidJS の reactive sync 直後に mermaid を置換。 createResource (async) なので
  // `queueMicrotask` だけだと placeholder が未マウントの場合あり → MutationObserver で待つ。
  // 簡易策: 50ms 後に走らせて、 不在なら no-op で抜ける。 大規模 markdown でも追従性 OK。
  // (= 厳密にやるなら CreoMarkdown の onAst で AST から mermaid 数を予測して polling、
  //  v0.2 で削除する hook なので over-engineer しない)
  window.setTimeout(() => {
    void runMermaidPostProcess(target)
  }, 50)
}

function renderHtml(target: HTMLElement, content: string): void {
  disposeMarkdown()
  // raw HTML は sandbox iframe (srcdoc) に隔離して render する。
  // srcdoc 属性値に埋めるので & と " をエスケープ — & を先に処理する
  // (逆順だと " 由来の &quot; の & が二重エスケープされる)。
  const escaped = content.replace(/&/g, '&amp;').replace(/"/g, '&quot;')
  target.innerHTML = `<iframe class="pp-html-frame" sandbox="allow-scripts" srcdoc="${escaped}"></iframe>`
}

function renderText(target: HTMLElement, content: string): void {
  disposeMarkdown()
  const span = document.createElement('span')
  span.textContent = content
  target.innerHTML = ''
  target.appendChild(span)
}

/** PP body を完全置換 render。 placeholder も含めて innerHTML が書き換わる. */
export function renderPP(content: string, contentType: ContentType = 'markdown'): void {
  const target = getTarget()
  if (!target) {
    console.warn('[vpPP] renderPP: target not found:', TARGET_SELECTOR)
    return
  }
  if (contentType === 'markdown') {
    renderMarkdown(target, content)
  } else if (contentType === 'html') {
    renderHtml(target, content)
  } else {
    renderText(target, content)
  }
  // html は iframe を PP pane いっぱいに広げるため container を full-bleed に切り替える。
  // markdown / text は通常の padding 付き flow に戻す。
  target.classList.toggle('pp-content-html', contentType === 'html')
}

/** PP body を空にする (Clear button 等から呼ばれる). */
export function clearPP(): void {
  const target = getTarget()
  if (!target) return
  disposeMarkdown()
  target.innerHTML = ''
  // html render 時に付けた full-bleed class を戻す。
  target.classList.remove('pp-content-html')
}

/**
 * PP body の末尾に append。
 *
 * markdown は SolidJS root を持つため逐次 append が難しい (= 子要素累積は SolidJS の
 * reactivity と相性悪い)。 pp-content-persist の Canvas Stack Model 移行後は item 単位の
 * push が strip 側で扱われるため、 本 path は html/text のみ append、 markdown は **置換に
 * フォールバック** で簡素化 (= 旧 timeline 累積 UX は doc 19 で stack model に統合済)。
 */
export function appendPP(content: string, contentType: ContentType = 'markdown'): void {
  const target = getTarget()
  if (!target) return
  if (contentType === 'markdown') {
    renderMarkdown(target, content)
    return
  }
  // html / text は innerHTML += が DOM listener を破壊するので insertAdjacentHTML を使う。
  if (contentType === 'html') {
    const escaped = content.replace(/&/g, '&amp;').replace(/"/g, '&quot;')
    target.insertAdjacentHTML(
      'beforeend',
      `<iframe class="pp-html-frame" sandbox="allow-scripts" srcdoc="${escaped}"></iframe>`,
    )
    return
  }
  const span = document.createElement('span')
  span.textContent = content
  target.appendChild(span)
}
