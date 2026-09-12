import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement, type HTMLInputElement } from 'happy-dom'

it('生成した質問カードは選択・自由入力を保持し、送信操作だけで回答する', async () => {
  const result = await build({
    stdin: { contents: `
      import { render } from 'solid-js/web'
      import { createComponent } from 'solid-js'
      import { CodexInteractionCard } from './codex-interactions'
      window.answers = []
      render(() => createComponent(CodexInteractionCard, { sending: false, request: {
        request_id: 'q', kind: 'async_question', title: '質問', details: '', can_accept: true, blocking: false,
        questions: [{ id: '0', header: '', question: 'どちら？', is_secret: false,
          options: [{label:'A',description:''},{label:'B',description:''}] }]
      }, respond: (id, behavior, answers) => window.answers.push({id, behavior, answers}) }), document.body)
    `, resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', conditions: ['browser'], plugins: [solidPlugin()],
  })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const doc = window.document
    const confirm = doc.querySelector<HTMLButtonElement>('.conversation-prompt-confirm')!
    expect(confirm.disabled).toBe(true)
    doc.querySelector<HTMLButtonElement>('.conversation-prompt-opt')!.click()
    expect((window as unknown as { answers: unknown[] }).answers).toEqual([])
    expect(doc.querySelector<HTMLInputElement>('input')!.value).toBe('A')
    confirm.click()
    expect((window as unknown as { answers: unknown[] }).answers).toEqual([{ id: 'q', behavior: 'allow', answers: { '0': 'A' } }])
    const input = doc.querySelector<HTMLInputElement>('input')!
    input.value = '自由な回答'
    input.dispatchEvent(new window.Event('input', { bubbles: true }))
    confirm.click()
    expect((window as unknown as { answers: unknown[] }).answers[1]).toEqual({ id: 'q', behavior: 'allow', answers: { '0': '自由な回答' } })
  } finally {
    await window.happyDOM.close()
  }
}, 15000)
