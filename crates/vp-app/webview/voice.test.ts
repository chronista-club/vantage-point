// 音声入力（push-to-talk）: chat 入力欄の🎙。creo mem_1CfTjYYiCsiCUgazoGGugP（[音声入力 1]）。
// 裁定: 入力欄が空のときだけ録音できる / 録音中・認識中は入力欄を編集できない /
// 認識結果は入力欄に入れるだけ（送信はユーザーが Enter）。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement, type HTMLTextAreaElement } from 'happy-dom'

type Harness = {
  sent: Array<Record<string, any>>
  voiceText: (lane: string, session: number, text: string) => void
  voiceError: (lane: string, session: number, message: string) => void
}

async function mountChat() {
  const result = await build({ stdin: { contents: `
    import {installChatView} from './chatview'
    import {deliverVoiceText, deliverVoiceError} from './voice'
    window.sent=[]
    window.ipc={postMessage: m=>window.sent.push(JSON.parse(m))}
    window.voiceText=deliverVoiceText
    window.voiceError=deliverVoiceError
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('voice-test/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'voice-test/main',focused:1,sessions:[{key:1,agent:'claude',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    const mount=document.createElement('div');document.body.append(mount)
    api.mountSession(mount,'voice-test/main',1)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife', conditions: ['browser'], plugins: [solidPlugin()] })
  const window = new Window()
  window.eval(result.outputFiles[0].text)
  const h = window as unknown as Harness
  const input = window.document.querySelector<HTMLTextAreaElement>('textarea')!
  const mic = () => window.document.querySelector<HTMLButtonElement>('button.conversation-mic')!
  const press = () => mic().dispatchEvent(new window.PointerEvent('pointerdown', { bubbles: true }))
  const release = () => mic().dispatchEvent(new window.PointerEvent('pointerup', { bubbles: true }))
  const voiceSent = () => h.sent.filter(m => String(m.t).startsWith('voice:'))
  return { window, h, input, mic, press, release, voiceSent }
}

it('press → voice:start, release → voice:stop, and the recognized text lands in the composer without sending', async () => {
  const { window, h, input, mic, press, release, voiceSent } = await mountChat()
  try {
    expect(mic()).not.toBeNull()
    expect(mic().disabled).toBe(false)

    press()
    expect(voiceSent()).toEqual([{ t: 'voice:start', lane: 'voice-test/main', session: 1 }])
    expect(input.disabled).toBe(true) // 録音中は編集できない

    release()
    expect(voiceSent().at(-1)).toEqual({ t: 'voice:stop', lane: 'voice-test/main', session: 1 })
    expect(input.disabled).toBe(true) // 認識中も編集できない
    expect(mic().disabled).toBe(true) // 認識が返るまで次を録らない

    // 別の session 宛ての結果は、この入力欄に入らない
    h.voiceText('voice-test/main', 2, '別の欄')
    expect(input.value).toBe('')

    h.voiceText('voice-test/main', 1, 'board に今の差分を貼って')
    expect(input.value).toBe('board に今の差分を貼って')
    expect(input.disabled).toBe(false)
    // 入れるだけ — 送信はしない
    expect(h.sent.some(m => m.t === 'conversation:submit')).toBe(false)
  } finally {
    await window.happyDOM.close()
  }
}, 20000) // esbuild bundle 込み（codex-queue.test.ts と同じ）

it('the mic is only available while the composer is empty', async () => {
  const { window, input, mic, press, voiceSent } = await mountChat()
  try {
    input.value = '書きかけ'
    input.dispatchEvent(new window.Event('input', { bubbles: true }))
    expect(mic().disabled).toBe(true)
    expect(mic().title).toContain('空')
    press()
    expect(voiceSent()).toEqual([])
    expect(input.value).toBe('書きかけ') // 書きかけは消さない
  } finally {
    await window.happyDOM.close()
  }
}, 20000) // esbuild bundle 込み（codex-queue.test.ts と同じ）

it('an error is shown and the composer is released; empty recognition is reported too', async () => {
  const { window, h, input, mic, press, release } = await mountChat()
  const error = () => window.document.querySelector('.conversation-voice-error')?.textContent ?? ''
  try {
    press(); release()
    h.voiceError('voice-test/main', 1, '認識モデルがありません')
    expect(error()).toContain('認識モデルがありません')
    expect(input.disabled).toBe(false)
    expect(mic().disabled).toBe(false)

    // 次の録音を始めたらエラー表示は消える
    press()
    expect(error()).toBe('')
    release()
    // 無音などで何も認識できなかった
    h.voiceText('voice-test/main', 1, '')
    expect(input.value).toBe('')
    expect(error()).toContain('聞き取れませんでした')
  } finally {
    await window.happyDOM.close()
  }
}, 20000) // esbuild bundle 込み（codex-queue.test.ts と同じ）
