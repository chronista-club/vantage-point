/** Pure gesture detector: right Option alone, emitted on release. */
type KeyEdge = Pick<KeyboardEvent, 'code'> & Partial<Pick<KeyboardEvent,
  'repeat' | 'isComposing' | 'shiftKey' | 'ctrlKey' | 'metaKey'>>

export function createRightOptionTap() {
  const down = new Set<string>()
  let candidate = false
  const modified = (event: KeyEdge) => event.isComposing || event.shiftKey || event.ctrlKey || event.metaKey
  return {
    keydown(event: KeyEdge): void {
      if (event.code === 'AltRight' && !event.repeat && !down.has(event.code)) {
        candidate = down.size === 0 && !modified(event)
      } else if (event.code !== 'AltRight' || modified(event)) {
        candidate = false
      }
      down.add(event.code)
    },
    keyup(event: KeyEdge): boolean {
      down.delete(event.code)
      if (event.code !== 'AltRight') return false
      const tapped = candidate && !modified(event)
      candidate = false
      return tapped
    },
    reset(): void {
      candidate = false
      down.clear()
    },
  }
}

/** Keyboard events stay local to the foreground WebView; never consume Option chords. */
export function installVoiceHotkey(toggle: () => void, stop: () => void): void {
  const tap = createRightOptionTap()
  let composing = false
  window.addEventListener('keydown', event => {
    if (!composing) tap.keydown(event)
  }, true)
  window.addEventListener('keyup', event => {
    if (!composing && tap.keyup(event)) toggle()
  }, true)
  window.addEventListener('compositionstart', () => { composing = true; tap.reset() }, true)
  window.addEventListener('compositionend', () => { composing = false; tap.reset() }, true)
  window.addEventListener('blur', () => { composing = false; tap.reset(); stop() })
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) { composing = false; tap.reset(); stop() }
  })
}
