/**
 * engine 別 settings panel の共有部 — context 型・IPC 送信・model select 部品。
 *
 * panel（claude-settings-panel / codex-settings-panel / vpcode-settings-panel）はここだけを
 * import する。束ね役（engine-settings-panel）は panel を import する。この向きを守ると
 * 循環 import が生まれない。
 */
import { For, Show } from 'solid-js'
import type { LaneChat } from './chat-model'
import type { ConversationSession, PickerChoice } from './console'
import type { EngineSettings } from './src/generated/EngineSettings'

/** panel が受け取る文脈（(lane, session) 単位の chat store + roster entry）。 */
export type SettingsContext = {
  lane: string
  session: number
  lc: LaneChat
  /** この session の roster entry（picker の catalog の供給源 = server 能力表明）。 */
  rosterEntry: () => ConversationSession | undefined
}

type Ipc = { postMessage(m: string): void }
// window でなく globalThis を引く（webview では同一。vitest の node 環境でも差し替えられる）
const ipc = (): Ipc | undefined => (globalThis as unknown as { ipc?: Ipc }).ipc

/**
 * `conversation:set_settings` を送る。settings は engine 所有の形（`{claude: {...}}` 等）で
 * vp-app は透過、server は variant で dispatch する（文字列で engine を分岐しない）。
 * null = engine 既定へ戻す。戻り値 = 送れたか（IPC 不在 = false）。
 */
export function postSettings(
  ctx: Pick<SettingsContext, 'lane' | 'session'>,
  settings: EngineSettings | null,
  requestId?: string,
): boolean {
  const target = ipc()
  if (!target) return false
  target.postMessage(
    JSON.stringify({
      t: 'conversation:set_settings',
      lane: ctx.lane,
      session: ctx.session,
      settings,
      request_id: requestId,
    }),
  )
  return true
}

/** 任意の IPC message を送る（panel の permission mode 等）。 */
export function postIpc(message: Record<string, unknown>): boolean {
  const target = ipc()
  if (!target) return false
  target.postMessage(JSON.stringify(message))
  return true
}

/** 切替不可のときの read-only 表示（実測 model があるときだけ）。表に無い engine と、
 *  catalog が空に落ちた engine（vpcode の endpoint 不在）の両方が使う。 */
export function ReadonlyModel(props: { model: string }) {
  return (
    <Show when={props.model}>
      <span class="conversation-model-readonly" title="model は engine 側で選択します（VP からは切替不可）">
        {props.model}
      </span>
    </Show>
  )
}

/**
 * catalog 駆動の model select。server catalog + 実測 model の動的追加（一覧に無い実測値は
 * option を足して真実を見せる）。catalog 空なら描かない（呼び手が Show で gate する）。
 */
export function ModelSelect(props: {
  choices: ReadonlyArray<PickerChoice>
  /** 実測 model（session_init の header.model）。picker の「現在値」はこれが正。 */
  current: string
  disabled: boolean
  title: string
  onChange: (value: string) => void
}) {
  const choices = (): ReadonlyArray<PickerChoice> => {
    const m = props.current
    return m && props.choices.length > 0 && !props.choices.some((c) => c.value === m)
      ? [...props.choices, { value: m, label: m }]
      : props.choices
  }
  return (
    <select
      class="conversation-model-select"
      disabled={props.disabled}
      title={props.title}
      onChange={(e) => props.onChange(e.currentTarget.value)}
    >
      <For each={choices()}>
        {(c) => (
          <option value={c.value} selected={c.value === props.current}>
            {c.label}
          </option>
        )}
      </For>
    </select>
  )
}
