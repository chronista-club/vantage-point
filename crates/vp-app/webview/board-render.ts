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
// ----------------------------------------------------------------------------
// html item の土台（doc: board の HTML を「素の semantic HTML」で書けるようにする）
// ----------------------------------------------------------------------------
//
// ## 何のためか
//
// html item は sandbox iframe に隔離されるので、親の creo token も `.board-content` の
// CSS も**一切継承しない**。そのため AI は毎回 `<!DOCTYPE html>` + 全部入りの `<style>` を
// 書く羽目になっていた。土台を srcdoc 側に注ぐと、**AI は素の semantic HTML だけ**を
// 書けばよくなる:
//
// | 目的 | 効き方 |
// |---|---|
// | **検索しやすさ** | class を使わず要素セレクタで飾るので、`<h2>` / `<table>` が構造として残る |
// | **生成コスト** | `<style>` を書かない分、生成も creo に保存される content も軽い |
// | **見た目の制御** | VP 側の 1 箇所。token を変えれば**過去の item も追従**する |
//
// ## ⚠️ token は実行時に親から読む（ハードコピーを作らない）
//
// 値をここに書き写すと creo-tokens.css と二重管理になり、片方だけ変わった日に無音で
// ずれる。`getComputedStyle` で live な値を引いて `:root` に流し込むので、**theme を
// 変えた瞬間に既存 item まで追従**する（rebuild も再 show も要らない）。

/** 土台が使う token。`.board-content`（markdown 側）が参照しているものと同じ集合に揃える。 */
const BASE_TOKENS = [
  '--color-text-primary',
  '--color-text-secondary',
  '--color-text-tertiary',
  '--color-surface-bg-base',
  '--color-surface-surface',
  '--color-surface-border-subtle',
  '--color-brand-primary',
  '--color-brand-primary-subtle',
  '--typography-family-sans',
  '--typography-family-mono',
  '--vp-font-sans',
] as const

/** 親 document から token の live 値を読んで `:root{...}` を組む。 */
function tokenBlock(): string {
  // 単体テスト等 DOM 不在環境では空（土台なしでも HTML 自体は出る）。
  if (typeof document === 'undefined' || typeof getComputedStyle !== 'function') return ''
  const cs = getComputedStyle(document.documentElement)
  const decls = BASE_TOKENS.map((t) => `${t}:${cs.getPropertyValue(t).trim()}`)
    .filter((d) => !d.endsWith(':'))
    .join(';')
  return decls ? `:root{${decls}}` : ''
}

/**
 * 素の semantic HTML を board の見た目にする土台。
 *
 * ⚠️ セレクタは**要素だけ**（class を作らない）。class を配ると AI がそれを使い始め、
 * 「検索しやすい素の HTML」という目的と逆に働く。
 *
 * ⚠️ 作者の `<style>` は**この後ろ**に来るので、全部入りで書かれた既存 item は
 * 今までどおり自分の見た目で出る（土台は壊さない）。
 */
const BASE_STYLE = `
html,body{margin:0;padding:16px 20px;background:var(--color-surface-bg-base);
  color:var(--color-text-primary);font-size:13px;line-height:1.6;
  font-family:var(--vp-font-sans),var(--typography-family-sans);font-weight:300;}
h1{font-size:1.6rem;font-weight:500;margin:0 0 .5rem;color:var(--color-text-primary);}
h2{font-size:1.3rem;font-weight:500;margin:1.2rem 0 .5rem;}
h3{font-size:1.1rem;font-weight:500;margin:1rem 0 .4rem;}
p{margin:.5rem 0;color:var(--color-text-secondary);}
code{background:var(--color-surface-surface);padding:1px 5px;border-radius:3px;
  font-family:var(--typography-family-mono);font-size:.9em;}
pre{background:var(--color-surface-surface);padding:12px;border-radius:6px;overflow-x:auto;}
pre code{background:transparent;padding:0;}
a{color:var(--color-brand-primary);}
ul,ol{padding-left:1.5em;margin:.5rem 0;}
blockquote{border-left:3px solid var(--color-brand-primary-subtle);margin:.5rem 0;padding:0 1em;
  color:var(--color-text-tertiary);}
table{border-collapse:collapse;margin:.5rem 0;}
th,td{border:1px solid var(--color-surface-border-subtle);padding:4px 8px;}
th{font-weight:500;color:var(--color-text-primary);}
hr{border:0;border-top:1px solid var(--color-surface-border-subtle);margin:1rem 0;}
img{max-width:100%;}
`

/** srcdoc に注ぐ土台（token + 要素の既定）。純関数 = test 対象。 */
export function boardHtmlPrelude(): string {
  return `<style>${tokenBlock()}${BASE_STYLE}</style>`
}

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
    // 土台を先に、作者の HTML を後に。順序が逆だと作者の `<style>` を土台が上書きして、
    // 全部入りで書かれた既存 item の見た目を壊す。
    const escaped = (boardHtmlPrelude() + content)
      .replace(/&/g, '&amp;')
      .replace(/"/g, '&quot;')
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
