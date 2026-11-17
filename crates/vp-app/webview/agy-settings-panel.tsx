/**
 * agy（Antigravity CLI）の settings panel — Model / Thinking Level / Mode（doc 88 §6）。
 *
 * 候補と現在値は **engine が出す configOptions が正**（roster の `acp_config`、server が host から
 * enrich）。thought_level の候補は model で変わるので VP は固定の表を持たない。
 *
 * 切替は 1 軸ずつ `{agy: {<軸>: 値}}` を送る。server は engine の応答を待ってから registry に意図を
 * 残し、roster を push し直す（成否どちらでも）— 現在値は engine の実測なので、Claude のような
 * 「intent 優先」は要らない。失敗は roster の `acp_config.error` に載り、ここで ⚠ として出す。
 */
import { createEffect, createMemo, createSignal, For, on, onCleanup, Show } from 'solid-js'
import { ReadonlyModel, postSettings, type SettingsContext } from './engine-settings-shared'
import type { AcpConfigOption } from './src/generated/AcpConfigOption'
import type { AcpConfigView } from './src/generated/AcpConfigView'
import type { AgySettings } from './src/generated/AgySettings'
import type { EngineSettings } from './src/generated/EngineSettings'

/** panel が描く軸（= registry に意図として残せる軸、Rust `AgySettings` の field と同じ並び）。 */
const AGY_AXES = ['model', 'thought_level', 'mode'] as const satisfies ReadonlyArray<keyof AgySettings>
type AgyAxis = (typeof AGY_AXES)[number]

/** roster の応答が来ないときに「送信中」を解く上限（server の応答待ちは 1 軸 15 秒）。 */
const PENDING_TIMEOUT_MS = 20_000

/**
 * 描く軸を Model → Thinking Level → Mode の順に並べる（純関数）。
 * VP が意図として残せない軸は出さない。候補に無い現在値は候補に足す（engine の真実を隠さない）。
 */
export function agyAxes(view: AcpConfigView | null | undefined): AcpConfigOption[] {
  if (!view) return []
  return AGY_AXES.flatMap((axis) => {
    const opt = view.options.find((o) => o.id === axis)
    if (!opt) return []
    const known = opt.choices.some((c) => c.value === opt.current_value)
    return [
      known || !opt.current_value
        ? opt
        : { ...opt, choices: [...opt.choices, { value: opt.current_value, name: opt.current_value }] },
    ]
  })
}

/** 切り替えた 1 軸だけの settings（他の軸は server が registry の意図を保つ）。 */
export function agySettingsFor(axis: AgyAxis, value: string): EngineSettings {
  return { agy: { [axis]: value } }
}

const isAgyAxis = (id: string): id is AgyAxis => (AGY_AXES as ReadonlyArray<string>).includes(id)

export function AgySettingsPanel(props: SettingsContext) {
  const view = () => props.rosterEntry()?.acp_config ?? null
  const axes = () => agyAxes(view())
  /** 送信中の軸（応答の roster が届くまで select を止める — 連打で順序が崩れないように）。 */
  const [pending, setPending] = createSignal<string | null>(null)
  let timer: ReturnType<typeof setTimeout> | undefined
  const clearPending = () => {
    if (timer) clearTimeout(timer)
    timer = undefined
    setPending(null)
  }
  // server は成否どちらでも roster を push し直す → **この session の設定面の中身**が変わったら解く。
  // view は全 lane 共有の roster signal から引くので、別 lane の更新でも再評価される —
  // 中身（JSON）で比べないと、応答の前に select が開いて切替が並ぶ（registry 側は
  // update_settings で軸を失わないが、engine に順不同で届く）。同じ失敗の繰り返し等で中身が
  // 変わらない時は timeout で解く。
  const viewContent = createMemo(() => JSON.stringify(view()))
  createEffect(on(viewContent, clearPending, { defer: true }))
  onCleanup(clearPending)
  const busy = () => props.lc.state.streaming || pending() !== null

  const change = (opt: AcpConfigOption, select: HTMLSelectElement) => {
    const value = select.value
    if (!isAgyAxis(opt.id) || value === opt.current_value) return
    if (!postSettings(props, agySettingsFor(opt.id, value))) {
      select.value = opt.current_value
      return
    }
    setPending(opt.id)
    timer = setTimeout(() => {
      // 同じ失敗の繰り返し等で roster が変わらず push されなかった — 表示を engine の値へ戻す
      select.value = opt.current_value
      setPending(null)
    }, PENDING_TIMEOUT_MS)
  }

  return (
    <Show when={axes().length > 0} fallback={<ReadonlyModel model={props.lc.state.header?.model ?? ''} />}>
      <For each={axes()}>
        {(opt) => (
          <select
            class="conversation-model-select"
            aria-label={`agy ${opt.name}`}
            disabled={busy()}
            title={`${opt.name}（この session に適用 — 会話は続いたまま切り替わる）`}
            onChange={(e) => change(opt, e.currentTarget)}
          >
            <For each={opt.choices}>
              {(c) => (
                <option value={c.value} selected={c.value === opt.current_value} title={c.description ?? ''}>
                  {c.name}
                </option>
              )}
            </For>
          </select>
        )}
      </For>
      <Show when={view()?.error}>
        {(error) => (
          <span class="conversation-model-readonly conversation-settings-error" title={error()}>
            ⚠ 切替に失敗
          </span>
        )}
      </Show>
    </Show>
  )
}
