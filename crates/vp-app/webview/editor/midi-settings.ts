import { createSignal, onCleanup, type Accessor } from 'solid-js'
import { defaultPins, restorePins, type PinSettings } from '../midi-pins'

/**
 * Editor（main bundle）から見た LPD8 ピン設定。
 *
 * 正本は Rust の `midi_pins`（app 全体で 1 組、`midi-pins.json`）。sidebar bundle の
 * `midi-pin-store` は別 bundle なので import しない（doc 81）。読むのは Rust が両 bundle に配る
 * `vp:midi-settings`、書くのは sidebar と同じ IPC `midi:navigation` — Rust が保存して全 window に配り直すので、
 * sidebar のピン帯と LPD8 の LED はその 1 本で追随する。
 */
export interface MidiSettings {
  settings: Accessor<PinSettings>
  /** 保存値が届くまでは書かない。既定値で送ると保存済みのピンを上書きしてしまう。 */
  ready: Accessor<boolean>
  update(next: PinSettings): void
}

type MidiWindow = Window & { __vpMidiSettings?: unknown; ipc?: { postMessage(message: string): void } }

export function createMidiSettings(win: MidiWindow = window as MidiWindow): MidiSettings {
  const [settings, setSettings] = createSignal<PinSettings>(defaultPins())
  const [ready, setReady] = createSignal(false)
  // 送ったが配り直しがまだ戻っていない値（正規化した JSON）。スライダーを動かし続けている間に
  // 古い値の echo が後から届いても、手元の新しい値へ巻き戻さない。
  let inflight: string[] = []
  const receive = (value: unknown) => {
    const next = restorePins(value)
    const key = JSON.stringify(next)
    const at = inflight.indexOf(key)
    if (at >= 0) {
      inflight = inflight.slice(at + 1)
      if (inflight.length) return
    }
    setSettings(next)
    setReady(true)
  }
  const onSettings = (event: Event) => receive((event as CustomEvent).detail)
  win.addEventListener('vp:midi-settings', onSettings)
  onCleanup(() => win.removeEventListener('vp:midi-settings', onSettings))
  if (win.__vpMidiSettings !== undefined) receive(win.__vpMidiSettings)

  return {
    settings,
    ready,
    update(next) {
      if (!ready()) return
      const key = JSON.stringify(next)
      if (key === JSON.stringify(settings())) return
      setSettings(next)
      inflight.push(key)
      win.ipc?.postMessage(JSON.stringify({ t: 'midi:navigation', settings: key }))
    },
  }
}
