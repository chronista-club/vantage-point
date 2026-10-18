import { For, Show, createSignal } from 'solid-js'
import { DEFAULT_DIM_GAIN, DEFAULT_PIN_COLORS, PIN_COUNT, isPinColor, withDimGain, withPinColor } from '../midi-pins'
import type { MidiSettings } from './midi-settings'

function PinColor(props: { slot: number; midi: MidiSettings }) {
  const label = () => `ピン #${props.slot + 1}`
  const value = () => props.midi.settings().colors[props.slot]
  const initial = DEFAULT_PIN_COLORS[props.slot]
  const [invalid, setInvalid] = createSignal(false)
  const set = (raw: string) => {
    // LED へ送るので #rrggbb だけを受ける（Editor の THEME 色のような OKLCH は不可）
    setInvalid(!isPinColor(raw))
    if (isPinColor(raw)) props.midi.update(withPinColor(props.midi.settings(), props.slot, raw))
  }
  return <div class="vp-editor-field" data-midi-field={`pin.${props.slot + 1}.color`}>
    <div class="vp-editor-field-head"><label for={`vp-midi-pin-${props.slot}`}>{label()}<small> · pad {props.slot + 1}</small></label>
      <button type="button" class="vp-editor-reset" aria-label={`${label()}の色を既定に戻す`} title="既定に戻す"
        disabled={!props.midi.ready() || value().toLowerCase() === initial} onClick={() => { setInvalid(false); set(initial) }}>↺</button>
    </div>
    <div class="vp-editor-color">
      <input type="color" aria-label={`${label()}のカラーピッカー`} disabled={!props.midi.ready()} value={value()} onInput={event => set(event.currentTarget.value)} />
      <input id={`vp-midi-pin-${props.slot}`} type="text" disabled={!props.midi.ready()} value={value()} spellcheck={false} aria-invalid={invalid()}
        onInput={event => set(event.currentTarget.value)} />
    </div>
    <Show when={invalid()}><small role="alert">#rrggbb の形で入力してください。</small></Show>
  </div>
}

function DimGain(props: { midi: MidiSettings }) {
  const percent = () => Math.round(props.midi.settings().dimGain * 100)
  const set = (raw: string) => {
    if (!raw.trim()) return
    const n = Number(raw)
    if (!Number.isFinite(n)) return
    props.midi.update(withDimGain(props.midi.settings(), Math.min(100, Math.max(0, n)) / 100))
  }
  return <div class="vp-editor-field" data-midi-field="dim">
    <div class="vp-editor-field-head"><label for="vp-midi-dim">未選択の Lane の明るさ</label>
      <button type="button" class="vp-editor-reset" aria-label="未選択の Lane の明るさを既定に戻す" title="既定に戻す"
        disabled={!props.midi.ready() || props.midi.settings().dimGain === DEFAULT_DIM_GAIN}
        onClick={() => props.midi.update(withDimGain(props.midi.settings(), DEFAULT_DIM_GAIN))}>↺</button>
    </div>
    <div class="vp-editor-number">
      <input type="range" aria-label="未選択の Lane の明るさのスライダー" min={0} max={100} step={1} disabled={!props.midi.ready()}
        value={percent()} onInput={event => set(event.currentTarget.value)} />
      <input id="vp-midi-dim" type="number" min={0} max={100} step={1} disabled={!props.midi.ready()}
        value={percent()} onInput={event => set(event.currentTarget.value)} />
      <span>%</span>
    </div>
  </div>
}

/** Editor の MIDI タブ。保存先は THEME の localStorage ではなく window の session（doc 78 / 81）。 */
export function MidiSection(props: { midi: MidiSettings }) {
  return <>
    <div class="vp-editor-intro"><h3>LPD8 のピン</h3>
      <p>選択は不要です。pad 1〜8 に固定した Lane の色と、選んでいない Lane の LED の明るさを調整します。この Window に保存し、サイドバーのピンと LPD8 にすぐ反映します。Lane の固定はサイドバーのピン枠で行います。</p>
      <Show when={!props.midi.ready()}><p role="status">この Window の設定を読み込み中です。</p></Show>
    </div>
    <details class="vp-editor-group" open>
      <summary>ピンの色<span>{PIN_COUNT}</span></summary>
      <For each={Array.from({ length: PIN_COUNT }, (_, slot) => slot)}>{slot => <PinColor slot={slot} midi={props.midi} />}</For>
    </details>
    <details class="vp-editor-group" open>
      <summary>LED<span>1</span></summary>
      <DimGain midi={props.midi} />
    </details>
  </>
}
