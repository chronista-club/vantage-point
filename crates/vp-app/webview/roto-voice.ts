/** ROTO PTT owns the composer selected at press time, never the one at release. */
export interface VoiceTarget { lane: string; session: number }
export interface RotoVoiceControl {
  start(): boolean
  stop(): void
  visible(): boolean
  reject(message: string): void
}
const key = (target: VoiceTarget) => `${target.lane}#${target.session}`
const controls = new Map<string, RotoVoiceControl>()
let recording: RotoVoiceControl | null = null

export function stopRotoVoice(control?: RotoVoiceControl): void {
  if (!recording || (control && recording !== control)) return
  const owner = recording
  recording = null
  owner.stop()
}

export function registerRotoVoiceControl(target: VoiceTarget, control: RotoVoiceControl): () => void {
  controls.set(key(target), control)
  return () => {
    stopRotoVoice(control)
    if (controls.get(key(target)) === control) controls.delete(key(target))
  }
}

export function visibleVoiceElement(element: HTMLElement | null | undefined): boolean {
  if (!element?.isConnected) return false
  for (let node: HTMLElement | null = element; node; node = node.parentElement) {
    if (node.hidden || node.style.display === 'none') return false
  }
  return true
}

function consoleFocused(): boolean {
  const active = document.activeElement?.closest<HTMLElement>('.xterm, .term-session-host, #lane-host')
  if (visibleVoiceElement(active)) return true
  return [...document.querySelectorAll<HTMLElement>('.term-session-host.pane-focused, #lane-host.pane-focused')]
    .some(visibleVoiceElement)
}

/** Install once in the main bundle; sidebar sends only target-window button edges. */
export function installRotoVoiceInput(target: () => VoiceTarget | null): void {
  let held = false
  let enabled = document.documentElement.dataset.midiTarget !== 'false'
  window.addEventListener('vp:roto-ptt', event => {
    const pressed = (event as CustomEvent<{pressed: boolean}>).detail?.pressed
    if (pressed === false) {
      held = false
      stopRotoVoice()
      return
    }
    if (pressed !== true || held) return
    held = true
    if (!enabled) return
    const destination = target()
    const control = destination && controls.get(key(destination))
    if (!control?.visible()) return
    if (consoleFocused()) {
      control.reject('Console にフォーカスがあるため録音を開始できません')
      return
    }
    if (control.start()) recording = control
  })
  window.addEventListener('vp:midi-target', event => {
    enabled = (event as CustomEvent<boolean>).detail === true
    if (!enabled) {
      stopRotoVoice()
      held = false // The release will be routed to the new target window.
    }
  })
  window.addEventListener('vp:midi-unavailable', event => {
    const port = (event as CustomEvent<{port_name?: string}>).detail?.port_name
    if (port?.toLowerCase().includes('roto')) {
      stopRotoVoice()
      held = false
    }
  })
  window.addEventListener('beforeunload', () => stopRotoVoice())
}
