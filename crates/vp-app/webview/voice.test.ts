// 音声入力（push-to-talk）: chat 入力欄の🎙。creo mem_1CfTjYYiCsiCUgazoGGugP（[音声入力 1]）。
// 裁定: 入力欄が空のときだけ録音できる / 録音中・認識中は入力欄を編集できない /
// 認識結果は入力欄に入れるだけ（送信はユーザーが Enter）。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement, type HTMLElement, type HTMLTextAreaElement } from 'happy-dom'

type Harness = {
  sent: Array<Record<string, any>>
  voiceText: (lane: string, session: number, text: string) => void
  voiceError: (lane: string, session: number, message: string) => void
  unmount: () => void
  emit: (event: unknown, session: number) => void
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
    window.unmount=api.mountSession(mount,'voice-test/main',1)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife', conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
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

it('text that came back into the composer while transcribing is kept — the result is appended, not overwritten', async () => {
  // 録音中・認識中は draft が空なので「送信を戻す」等のボタンが押せる。戻した文章を
  // 認識結果で上書きして消さない（moody-blues #1、2026-09-30）。
  const { window, h, input, press, release } = await mountChat()
  try {
    // Claude の turn 中に「次に実行する」で溜め、認識中に「送信待ち」を押して入力欄に戻す
    h.emit({ kind: 'message_chunk', text: 'working' }, 1)
    input.value = '戻した文章'
    input.dispatchEvent(new window.Event('input', { bubbles: true }))
    const queue = [...window.document.querySelectorAll<HTMLButtonElement>('button')].find(b => b.textContent?.includes('次に実行する'))!
    queue.click()
    expect(input.value).toBe('')
    press(); release()
    window.document.querySelector<HTMLElement>('.conversation-msg.user.pending')!.click()
    expect(input.value).toBe('戻した文章')
    h.voiceText('voice-test/main', 1, '続き')
    expect(input.value).toBe('戻した文章 続き')
  } finally {
    await window.happyDOM.close()
  }
}, 20000)

it('unmounting while recording sends voice:stop so the mic is not left open', async () => {
  const { window, h, press, voiceSent } = await mountChat()
  try {
    press()
    expect(voiceSent().at(-1)?.t).toBe('voice:start')
    // session が閉じると mountSession の戻り値（dispose）で unmount される
    h.unmount()
    expect(voiceSent().at(-1)).toEqual({ t: 'voice:stop', lane: 'voice-test/main', session: 1 })
  } finally {
    await window.happyDOM.close()
  }
}, 20000)

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

// mem_1CgAPJPpcCwy2s1QohP3Zj: ROTO no longer owns a voice input.
it('retired ROTO events cannot start or stop keyboard voice; unload stops its owner once', async () => {
  const {window,h,voiceSent} = await mountChat()
  const ptt = (pressed:boolean) => window.dispatchEvent(new window.CustomEvent('vp:roto-ptt',{detail:{pressed}}))
  try {
    ptt(true);ptt(false)
    expect(voiceSent()).toEqual([])
    window.dispatchEvent(new window.KeyboardEvent('keydown',{code:'AltRight'}))
    window.dispatchEvent(new window.KeyboardEvent('keyup',{code:'AltRight'}))
    ptt(false)
    window.eval("window.dispatchEvent(new CustomEvent('vp:midi-target',{detail:false}))")
    window.dispatchEvent(new window.CustomEvent('vp:midi-unavailable',{detail:{port_name:'Roto-Control'}}))
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start'])
    window.dispatchEvent(new window.Event('beforeunload'))
    h.unmount()
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start','voice:stop'])
  } finally { await window.happyDOM.close() }
},20000)

it('keyboard voice stops its original session on focus change and recognition does not steal focus', async () => {
  const {window,h,input,voiceSent} = await mountChat()
  try {
    window.dispatchEvent(new window.KeyboardEvent('keydown',{code:'AltRight'}))
    window.dispatchEvent(new window.KeyboardEvent('keyup',{code:'AltRight'}))
    window.document.dispatchEvent(new window.CustomEvent('vp:conversation-sessions',{detail:{lane:'voice-test/main',focused:2,sessions:[{key:1},{key:2}]}}))
    expect(voiceSent()).toEqual([{t:'voice:start',lane:'voice-test/main',session:1},{t:'voice:stop',lane:'voice-test/main',session:1}])
    const next=window.document.createElement('textarea');window.document.body.append(next);next.focus()
    h.voiceText('voice-test/main',1,'元の宛先')
    await Promise.resolve()
    expect(input.value).toBe('元の宛先')
    expect(window.document.activeElement).toBe(next)
  } finally { await window.happyDOM.close() }
},20000)

it('unmount stops the active keyboard recording exactly once', async () => {
  const {window,h,voiceSent} = await mountChat()
  try {
    window.dispatchEvent(new window.KeyboardEvent('keydown',{code:'AltRight'}))
    window.dispatchEvent(new window.KeyboardEvent('keyup',{code:'AltRight'}))
    h.unmount()
    window.dispatchEvent(new window.Event('beforeunload'))
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start','voice:stop'])
  } finally { await window.happyDOM.close() }
},20000)

// mem_1CgANNcfXDjfnXyBQtSCsZ: right Option toggles the shared voice owner.
it('right Option taps toggle start/stop and wait for recognition without sending the draft', async () => {
  const {window,h,input,voiceSent} = await mountChat()
  const tap = () => {
    window.dispatchEvent(new window.KeyboardEvent('keydown',{code:'AltRight',altKey:true}))
    window.dispatchEvent(new window.KeyboardEvent('keyup',{code:'AltRight'}))
  }
  try {
    tap(); tap(); tap()
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start','voice:stop'])
    h.voiceText('voice-test/main',1,'')
    tap();tap()
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start','voice:stop','voice:start','voice:stop'])
    h.voiceText('voice-test/main',1,'入力した声')
    expect(input.value).toBe('入力した声')
    expect(h.sent.some(m=>m.t==='conversation:submit')).toBe(false)
  } finally { await window.happyDOM.close() }
},20000)

it('right Option respects draft, console and hidden composer gates, then stops on blur', async () => {
  const {window,input,voiceSent} = await mountChat()
  const tap = () => {
    window.dispatchEvent(new window.KeyboardEvent('keydown',{code:'AltRight',altKey:true}))
    window.dispatchEvent(new window.KeyboardEvent('keyup',{code:'AltRight'}))
  }
  try {
    input.value='書きかけ';input.dispatchEvent(new window.Event('input',{bubbles:true}));tap()
    expect(voiceSent()).toEqual([])
    expect(window.document.querySelector('.conversation-voice-error')?.textContent).toContain('入力欄を空に')
    input.value='';input.dispatchEvent(new window.Event('input',{bubbles:true}))
    const term=window.document.createElement('div');term.className='term-session-host pane-focused';window.document.body.append(term)
    tap();expect(voiceSent()).toEqual([]);term.remove()
    input.parentElement!.setAttribute('hidden','');tap();expect(voiceSent()).toEqual([]);input.parentElement!.removeAttribute('hidden')
    tap();window.dispatchEvent(new window.Event('blur'))
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start','voice:stop'])
  } finally { await window.happyDOM.close() }
},20000)

it('right Option ignores chords, left Option, repeats, IME and interrupted gestures', async () => {
  const {window,voiceSent} = await mountChat()
  const down = (code:string,extra:Record<string,boolean>={}) => window.dispatchEvent(new window.KeyboardEvent('keydown',{code,...extra}))
  const up = (code:string,extra:Record<string,boolean>={}) => window.dispatchEvent(new window.KeyboardEvent('keyup',{code,...extra}))
  try {
    down('AltLeft');up('AltLeft')
    down('AltRight');down('ArrowLeft');up('ArrowLeft');up('AltRight')
    down('KeyA');down('AltRight');up('KeyA');up('AltRight')
    down('AltRight',{shiftKey:true});up('AltRight')
    down('AltRight',{isComposing:true});up('AltRight')
    down('AltRight');up('AltRight',{isComposing:true})
    down('AltRight');window.dispatchEvent(new window.Event('compositionstart'));window.dispatchEvent(new window.Event('compositionend'));up('AltRight')
    down('AltRight');window.dispatchEvent(new window.Event('blur'));up('AltRight')
    expect(voiceSent()).toEqual([])
    down('AltRight');down('AltRight',{repeat:true});up('AltRight');up('AltRight')
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start'])
  } finally { await window.happyDOM.close() }
},20000)

it('a recording error releases the shared owner so the next tap starts immediately', async () => {
  const {window,h,voiceSent} = await mountChat()
  const tap = () => {
    window.dispatchEvent(new window.KeyboardEvent('keydown',{code:'AltRight'}))
    window.dispatchEvent(new window.KeyboardEvent('keyup',{code:'AltRight'}))
  }
  try {
    tap();h.voiceError('voice-test/main',1,'マイクの許可がありません');tap()
    expect(voiceSent().map(m=>m.t)).toEqual(['voice:start','voice:start'])
  } finally { await window.happyDOM.close() }
},20000)
