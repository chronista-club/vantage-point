/** Grok の model / reasoning_effort。候補と現在値は engine、保存も Grok が担う（design 42）。 */
import { createSignal, For, onCleanup, Show } from 'solid-js'
import { ReadonlyModel, postSettings, type SettingsContext } from './engine-settings-shared'
import { nextRequestId } from './console'
import type { AcpConfigOption } from './src/generated/AcpConfigOption'
import type { AcpConfigView } from './src/generated/AcpConfigView'
import type { EngineSettings } from './src/generated/EngineSettings'

const GROK_AXES = ['model', 'reasoning_effort'] as const
type GrokAxis = (typeof GROK_AXES)[number]
// model と effort の応答待ち（各 15 秒）より長く待つ。
const PENDING_TIMEOUT_MS = 35_000

export function grokAxes(view: AcpConfigView | null | undefined): AcpConfigOption[] {
  return GROK_AXES.flatMap((id) => {
    const opt = view?.options.find((o) => o.id === id)
    if (!opt) return []
    return [!opt.current_value || opt.choices.some((c) => c.value === opt.current_value)
      ? opt
      : { ...opt, choices: [...opt.choices, { value: opt.current_value, name: `${opt.current_value}（候補外）` }] }]
  })
}

export function grokSettingsFor(axis: GrokAxis, value: string): EngineSettings {
  return { grok: { [axis]: value } }
}

export function GrokSettingsPanel(props: SettingsContext) {
  const view = () => props.rosterEntry()?.acp_config
  const [pending, setPending] = createSignal<string | null>(null)
  const [localError, setLocalError] = createSignal<string | null>(null)
  const selects = new Map<string, HTMLSelectElement>()
  const resetSelections = () => {
    for (const [axis, select] of selects) {
      const current = view()?.options.find((o) => o.id === axis)?.current_value
      if (current !== undefined) select.value = current
    }
  }
  let lastRequestId: string | null = null
  let timer: ReturnType<typeof setTimeout> | undefined
  const clearPending = () => {
    if (timer !== undefined) clearTimeout(timer)
    timer = undefined
    setPending(null)
  }
  const settled = (event: Event) => {
    const result = (event as CustomEvent).detail
    if (result.lane !== props.lane || result.session !== props.session || result.request_id !== lastRequestId) return
    clearPending()
    resetSelections()
    setLocalError(result.error ?? null)
  }
  window.addEventListener('vp:conversation-settings-result', settled)
  onCleanup(() => { clearPending(); window.removeEventListener('vp:conversation-settings-result', settled) })
  const error = () => localError() ?? view()?.error
  const busy = () => props.lc.state.streaming || pending() !== null
  const change = (opt: AcpConfigOption, select: HTMLSelectElement) => {
    const value = select.value
    if (busy() || value === opt.current_value || !GROK_AXES.includes(opt.id as GrokAxis)) return
    const requestId = nextRequestId('grok-settings')
    setLocalError(null)
    lastRequestId = requestId
    setPending(requestId)
    timer = setTimeout(() => {
      clearPending()
      // 成否を推定せず、最後に engine が知らせた値に戻す。
      select.value = view()?.options.find((o) => o.id === opt.id)?.current_value ?? opt.current_value
      setLocalError('切替結果を確認できません。現在値を確認してください')
    }, PENDING_TIMEOUT_MS)
    if (!postSettings(props, grokSettingsFor(opt.id as GrokAxis, value), requestId)) {
      clearPending()
      select.value = opt.current_value
      setLocalError('設定を送信できませんでした')
      return
    }
  }
  return (
    <Show when={grokAxes(view()).length > 0} fallback={<ReadonlyModel model={props.lc.state.header?.model ?? ''} />}>
      <For each={grokAxes(view())}>
        {(opt) => (
          <select ref={(el) => selects.set(opt.id, el)} class="conversation-model-select" aria-label={`grok ${opt.name}`} disabled={busy()}
            title={`${opt.name}（この会話に適用）`} onChange={(e) => change(opt, e.currentTarget)}>
            <For each={opt.choices}>
              {(c) => <option value={c.value} selected={c.value === opt.current_value} title={c.description ?? ''}
                disabled={!view()?.options.find((o) => o.id === opt.id)?.choices.some((choice) => choice.value === c.value)}>{c.name}</option>}
            </For>
          </select>
        )}
      </For>
      <Show when={pending()}><span class="conversation-model-readonly" role="status">切替中…</span></Show>
      <Show when={!pending() && view()?.notice}>{(notice) => (
        <span class="conversation-model-readonly" role="status">{notice().message}（<time
          dateTime={new Date(notice().occurred_at_ms).toISOString()}
          title={new Date(notice().occurred_at_ms).toLocaleString()}
        >{new Date(notice().occurred_at_ms).toLocaleTimeString()}</time>）</span>
      )}</Show>
      <Show when={!pending() && error()}>{(message) => (
        <span class="conversation-model-readonly conversation-settings-error" role="alert" title={message()}>⚠ {message()}</span>
      )}</Show>
    </Show>
  )
}
