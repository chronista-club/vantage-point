// mem_1ChV1bq3MjjazR9J8LH1qG — agy / grok の turn 中の送信 2 択（今伝える = Enter 既定 / 次に実行する）。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLButtonElement } from 'happy-dom'
import { beginSubmission, emptyChatState, foldInto } from './chat-model'

type Sent = { sent: Array<Record<string, any>>; emit: (event: unknown, session: number) => void }

async function mountWith(agent: string) {
  const result = await build({ stdin: { contents: `
    import {installChatView} from './chatview'
    window.sent=[]
    window.ipc={postMessage: m=>window.sent.push(JSON.parse(m))}
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('steer-test/lead')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'steer-test/lead',focused:1,sessions:[{key:1,agent:'${agent}',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    const mount=document.createElement('div');document.body.append(mount)
    api.mountSession(mount,'steer-test/lead',1)
    window.emit({kind:'message_chunk',text:'working'},1)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife', conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const window = new Window()
  evalDom(window, result.outputFiles[0].text)
  const input = window.document.querySelector('textarea')!
  const type = (text: string) => {
    input.value = text
    dispatchDom(input, new window.Event('input', { bubbles: true }))
  }
  const enter = () => dispatchDom(input, new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  const button = (label: string) =>
    [...window.document.querySelectorAll<HTMLButtonElement>('button')].find(b => b.textContent?.includes(label))
  return { window, sent: window as unknown as Sent, type, enter, button }
}

for (const agent of ['agy', 'grok']) {
  it(`${agent}: turn 中の Enter は今伝える（即 submit）、「次に実行する」は turn の終わりに送る`, async () => {
    const { window, sent, type, enter, button } = await mountWith(agent)
    try {
      expect(button('今伝える')).toBeDefined()
      expect(button('次に実行する')).toBeDefined()
      type('今の補足')
      enter()
      const submits = () => sent.sent.filter(m => m.t === 'conversation:submit')
      expect(submits()).toHaveLength(1)
      expect(submits()[0]).toMatchObject({ lane: 'steer-test/lead', session: 1, prompt: '今の補足' })
      callDom(window, () => sent.emit({ kind: 'submit_result', request_id: submits()[0].request_id, error: null }, 1))
      // 次に実行する: 送らずに pending へ貯める。
      type('次の仕事')
      clickDom(button('次に実行する')!)
      expect(submits()).toHaveLength(1)
      expect(window.document.querySelector('.conversation-msg.user.pending')?.textContent).toContain('次の仕事')
      // turn の終わりで送る。
      callDom(window, () => sent.emit({ kind: 'turn_completed', session_id: 's' }, 1))
      expect(submits()).toHaveLength(2)
      expect(submits()[1]).toMatchObject({ prompt: '次の仕事' })
    } finally { await window.happyDOM.close() }
  }, 20000)
}

it('steer を持たない engine（opencode）は、turn 中の Enter を従来どおり turn の終わりまで貯める', async () => {
  const { window, sent, type, enter, button } = await mountWith('opencode')
  try {
    expect(button('今伝える')).toBeUndefined()
    type('あとで')
    enter()
    expect(sent.sent.filter(m => m.t === 'conversation:submit')).toHaveLength(0)
    callDom(window, () => sent.emit({ kind: 'turn_completed', session_id: 's' }, 1))
    expect(sent.sent.filter(m => m.t === 'conversation:submit').at(-1)).toMatchObject({ prompt: 'あとで' })
  } finally { await window.happyDOM.close() }
}, 20000)

// 今伝えた後の吹き出しの並び。送った時点で user の吹き出しが末尾に入るので、その後に届く
// chunk は新しい assistant の吹き出しになる。grok のように 1 本目の答えを言い切ってから 2 本目に
// 答える engine では、1 本目の残りと 2 本目の答えが同じ吹き出しに続けて入る（turn の途中に
// 区切りの event が無いため。turn 自体は 1 つで、閉じるのは最後の 1 回）。
it('今伝えた後の chunk は、送った発話の後ろの吹き出しに入る', () => {
  const s = emptyChatState()
  foldInto(s, { kind: 'message_chunk', text: 'DONE' })
  beginSubmission(s, 'id-2', 'second', [])
  foldInto(s, { kind: 'submit_result', request_id: 'id-2', error: null } as never)
  foldInto(s, { kind: 'message_chunk', text: '1' })
  foldInto(s, { kind: 'message_chunk', text: ' STEERED' })
  foldInto(s, { kind: 'turn_completed', session_id: 's' } as never)
  expect(s.items.map(i => [i.kind, 'text' in i ? i.text : ''])).toEqual([
    ['assistant', 'DONE'],
    ['user', 'second'],
    ['assistant', '1 STEERED'],
  ])
  expect(s.streaming).toBe(false)
})
