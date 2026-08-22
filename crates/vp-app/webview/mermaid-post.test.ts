// @vitest-environment happy-dom
/**
 * mermaid-post.ts — render 置換・cache・エラー枝の回帰保護（moody-blues 提案 2026-08-22）。
 *
 * ⚠️ このファイルだけ happy-dom（他は node env + 自前 DOM stub）。本 module の責務が
 * 「実 DOM の置換」そのものなので、stub では isConnected / replaceWith の実挙動を試せない。
 *
 * mermaid 本体は mock する（node env に SVG renderer は無い）。検証するのは本 module の
 * 責務だけ: 置換の対象選び / showErrors の出し分け / cache の同期復元 / 失敗時の掃除。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'

const renderMock = vi.fn()
vi.mock('mermaid', () => ({
  default: { initialize: vi.fn(), render: (...a: unknown[]) => renderMock(...a) },
}))

import { renderMermaidBlocks } from './mermaid-post'

/** marked が吐く形（<pre><code class="language-mermaid">）の container を作る。 */
function containerWith(src: string): HTMLElement {
  const div = document.createElement('div')
  const pre = document.createElement('pre')
  const code = document.createElement('code')
  code.className = 'language-mermaid'
  code.textContent = src
  pre.appendChild(code)
  div.appendChild(pre)
  document.body.appendChild(div) // isConnected を真にする（実環境と同じ前提）
  return div
}

beforeEach(() => {
  renderMock.mockReset()
  document.body.innerHTML = ''
})

describe('renderMermaidBlocks', () => {
  it('成功 → <pre> が .creo-md-mermaid の SVG に置換される', async () => {
    renderMock.mockResolvedValue({ svg: '<svg data-x="1"></svg>' })
    const c = containerWith('flowchart LR\n A-->B')
    await renderMermaidBlocks(c, { showErrors: true })
    expect(c.querySelector('pre')).toBeNull()
    expect(c.querySelector('.creo-md-mermaid svg')).not.toBeNull()
  })

  it('同じ source の 2 回目は cache から復元（mermaid.render は 1 回だけ）', async () => {
    renderMock.mockResolvedValue({ svg: '<svg></svg>' })
    const src = 'flowchart LR\n C-->D'
    await renderMermaidBlocks(containerWith(src), { showErrors: true })
    const c2 = containerWith(src)
    await renderMermaidBlocks(c2, { showErrors: true })
    expect(renderMock).toHaveBeenCalledTimes(1)
    expect(c2.querySelector('.creo-md-mermaid')).not.toBeNull()
  })

  it('失敗 + showErrors: 書きかけ（false）は code のまま / 確定（true）は error pre', async () => {
    renderMock.mockRejectedValue(new Error('parse error'))
    const streaming = containerWith('flowchart LR\n E-->')
    await renderMermaidBlocks(streaming, { showErrors: false })
    expect(streaming.querySelector('code.language-mermaid')).not.toBeNull() // 温存
    expect(streaming.querySelector('.creo-md-mermaid-error')).toBeNull()

    const sealed = containerWith('flowchart LR\n F-->')
    await renderMermaidBlocks(sealed, { showErrors: true })
    expect(sealed.querySelector('.creo-md-mermaid-error')?.textContent).toContain('parse error')
  })

  it('await 中に detach された container には置換しない（isConnected guard）', async () => {
    let resolve!: (v: { svg: string }) => void
    renderMock.mockReturnValue(new Promise((r) => (resolve = r)))
    const c = containerWith('flowchart LR\n G-->H')
    const done = renderMermaidBlocks(c, { showErrors: true })
    c.remove() // MsgBody の innerHTML 全置換に相当
    resolve({ svg: '<svg></svg>' })
    await done
    // 置換されず code のまま（detached tree に SVG を差し込まない）
    expect(c.querySelector('.creo-md-mermaid')).toBeNull()
    expect(c.querySelector('code.language-mermaid')).not.toBeNull()
  })

  it('失敗時に mermaid が body に残す作業用要素 d{id} を掃除する', async () => {
    renderMock.mockImplementation((id: string) => {
      const junk = document.createElement('div')
      junk.id = `d${id}`
      document.body.appendChild(junk) // mermaid 実挙動の再現（suppressErrorRendering=false）
      return Promise.reject(new Error('boom'))
    })
    const c = containerWith('flowchart LR\n I-->')
    await renderMermaidBlocks(c, { showErrors: true })
    expect(document.querySelector('[id^="dpp-mermaid-"]')).toBeNull()
  })
})
