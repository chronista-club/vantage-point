/**
 * mermaid post-process の共有実装 — board（board-render.ts）と chat（chatview.tsx）の両方が使う。
 *
 * marked は ```mermaid fence を `<pre><code class="language-mermaid">` で吐く。ここは
 * render 済み container を走査して SVG に置換する（WASM 不要、npm mermaid package のみ）。
 *
 * ## board 版（旧 runMermaidPostProcess）からの一般化 2 点
 *
 * - **showErrors**: chat の streaming 中は fence 未閉の不完全 source が毎 chunk 流れてくる —
 *   その parse 失敗は「まだ書きかけ」であって error ではないので code のまま残す（keep）。
 *   確定後（sealed / board）は書き損じを silent fail させず error pre で見せる（従来挙動）。
 * - **svg cache**: chat は chunk 毎に innerHTML が全置換され post-process 済み SVG も消える。
 *   同じ source の再 render を cache（source → svg）で同期置換にして、streaming 中の
 *   「SVG → code → SVG」のちらつきを最小化する。cache は成功分のみ・**webview プロセス寿命**
 *   （全会話共通に蓄積するが 1 entry は数 KB の svg 文字列 — reload で消える規模）。
 */
import mermaid from 'mermaid'

let initialized = false

function ensureInitialized(): void {
  if (initialized) return
  initialized = true
  // securityLevel=loose で raw text を直接食わせる（WebView 内 closed 環境、user 自身の content）。
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'loose',
    theme: 'dark',
  })
}

/** 連番 id 用 counter。`mermaid.render` の DOM unique id 生成に使う。 */
let idSeq = 0

/** 成功した render の cache（source → svg）。挙動は上記 doc 参照。 */
const svgCache = new Map<string, string>()

/** cache 済み svg で `<pre>` を置換する（同期）。 */
function replaceWithSvg(target: Element, svg: string): void {
  const wrap = document.createElement('div')
  wrap.className = 'creo-md-mermaid'
  wrap.innerHTML = svg
  target.replaceWith(wrap)
}

/**
 * container 内の `code.language-mermaid` を SVG に置換する。
 *
 * @param showErrors render 失敗を error pre で見せるか。false = code のまま残す
 *   （streaming 中の書きかけ fence 用 — 「まだ完成していない」は error ではない）。
 */
export async function renderMermaidBlocks(
  container: HTMLElement,
  opts: { showErrors: boolean },
): Promise<void> {
  const blocks = container.querySelectorAll<HTMLElement>('code.language-mermaid')
  if (blocks.length === 0) return
  ensureInitialized()
  for (const code of Array.from(blocks)) {
    const src = code.textContent ?? ''
    if (!src.trim()) continue
    // 置換対象は <pre><code> の <pre>。無ければ code 自身。
    const replaceTarget = code.closest('pre') ?? code
    const cached = svgCache.get(src)
    if (cached !== undefined) {
      replaceWithSvg(replaceTarget, cached)
      continue
    }
    const id = `pp-mermaid-${idSeq++}`
    try {
      const { svg } = await mermaid.render(id, src)
      svgCache.set(src, svg)
      // ⚠️ await 中に container が再構築されていたら（chat の chunk 更新）置換先は
      // もう document に居ない — その場合は何もしない（次回の post-process が cache で拾う）。
      if (replaceTarget.isConnected) replaceWithSvg(replaceTarget, svg)
    } catch (e) {
      // ⚠️ mermaid.render は失敗時、自分が document.body に挿した作業用要素を残すことが
      // ある（既知の挙動）。掃除してから error 表示へ。
      document.getElementById(`d${id}`)?.remove()
      if (!opts.showErrors) continue
      console.warn('[mermaid-post] render failed', e)
      if (!replaceTarget.isConnected) continue
      const errEl = document.createElement('pre')
      errEl.className = 'creo-md-mermaid-error'
      errEl.textContent = `mermaid render error: ${String(e)}\n\n${src}`
      replaceTarget.replaceWith(errEl)
    }
  }
}
