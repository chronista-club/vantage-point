// Codex の agent-turn-complete を OSC 9 / always に限定して起動する。
// 復元画面・別 session の通知を待機中の完了として扱わない。
class CompletionOsc {
  private state = 0
  private code = ''
  reset(): void { this.state = 0; this.code = '' }
  feed(bytes: Uint8Array): boolean {
    let complete = false
    for (const byte of bytes) {
      if (this.state === 0) { if (byte === 27) this.state = 1 }
      else if (this.state === 1) {
        this.state = byte === 93 ? 2 : byte === 27 ? 1 : 0
        this.code = ''
      } else if (this.state === 2) {
        if (byte === 59) this.state = 3
        else if (byte >= 48 && byte <= 57 && this.code.length < 4) this.code += String.fromCharCode(byte)
        else this.reset()
      } else if (byte === 7 || (this.state === 4 && byte === 92)) {
        if (this.code === '9') complete = true
        this.reset()
      } else this.state = byte === 27 ? 4 : 3
    }
    return complete
  }
}

let waiting: { lane: string; session: number; parser: CompletionOsc; accept: () => void } | undefined

/** live=false は replay / transport gap。古い通知は動作のきっかけにしない。 */
export function observeConsoleOutput(lane: string, session: number, bytes: Uint8Array, live: boolean): void {
  const current = waiting
  if (!current || current.lane !== lane || current.session !== session) return
  if (!live) { current.parser.reset(); return }
  if (current.parser.feed(bytes)) current.accept()
}

/** Console は turn 状態を配信しないため、今すぐ切替と次の完了待ちを明示的に選ぶ。 */
export function confirmCodexConsoleHandoff(lane: string, session: number, accept: () => void): void {
  if (document.querySelector('[data-codex-handoff]')) return
  const previous = document.activeElement as HTMLElement | null
  const dialog = document.createElement('dialog')
  dialog.dataset.codexHandoff = ''
  dialog.setAttribute('aria-labelledby', 'codex-handoff-title')
  dialog.setAttribute('aria-describedby', 'codex-handoff-description')
  dialog.style.cssText = 'max-width:460px;padding:24px;border:1px solid var(--color-border,#555);border-radius:12px;background:var(--color-bg,#202128);color:var(--color-text,#eee);font:inherit'
  dialog.innerHTML = `
    <h3 id="codex-handoff-title" style="margin-top:0">Chat に切り替えますか？</h3>
    <p id="codex-handoff-description">Console で応答中の場合、今すぐ切り替えると処理を中断します。保存済みの会話から続け、入力は自動再送しません。</p>
    <p data-handoff-status aria-live="polite"></p>
    <div style="display:flex;flex-wrap:wrap;gap:12px;justify-content:flex-end">
      <button type="button" data-handoff-cancel autofocus>キャンセル</button>
      <button type="button" data-handoff-wait>応答完了後に切り替える</button>
      <button type="button" data-handoff-confirm>今すぐ切り替える</button>
    </div>`
  const dismiss = () => {
    waiting = undefined
    dialog.remove()
    if (previous?.isConnected) previous.focus()
  }
  const finish = () => { dismiss(); accept() }
  dialog.addEventListener('cancel', event => { event.preventDefault(); dismiss() })
  dialog.addEventListener('close', dismiss)
  dialog.querySelector('[data-handoff-cancel]')!.addEventListener('click', dismiss)
  dialog.querySelector('[data-handoff-confirm]')!.addEventListener('click', finish, { once: true })
  dialog.querySelector('[data-handoff-wait]')!.addEventListener('click', () => {
    waiting = { lane, session, parser: new CompletionOsc(), accept: finish }
    dialog.querySelector('[data-handoff-status]')!.textContent = '次の応答完了を待っています。完了通知を受け取ると Chat に切り替えます。すでに応答が終わっている場合は「今すぐ切り替える」を選んでください。'
    dialog.querySelector<HTMLButtonElement>('[data-handoff-wait]')!.disabled = true
    const cancel = dialog.querySelector<HTMLButtonElement>('[data-handoff-cancel]')!
    cancel.textContent = '待機を取り消す'
    cancel.focus()
  }, { once: true })
  document.body.append(dialog)
  dialog.showModal()
}
