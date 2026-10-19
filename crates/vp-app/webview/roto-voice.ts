/** Voice inputs own the composer selected at start time, never the one at stop. */
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

/** Start the visible focused composer through its existing voice gate. */
function startFocusedVoice(target: () => VoiceTarget | null): void {
  if (recording) return
  const destination = target()
  const control = destination && controls.get(key(destination))
  if (!control?.visible()) return
  if (consoleFocused()) {
    control.reject('Console にフォーカスがあるため録音を開始できません')
    return
  }
  if (control.start()) recording = control
}

export function toggleFocusedVoice(target: () => VoiceTarget | null): void {
  if (recording) stopRotoVoice()
  else startFocusedVoice(target)
}
