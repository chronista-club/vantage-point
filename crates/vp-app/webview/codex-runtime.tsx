import { For, Show } from 'solid-js'
import type { CodexRuntime } from './src/generated/CodexRuntime'
import type { CodexPermissionChoice } from './src/generated/CodexPermissionChoice'
import { CodexPermissionMenu } from './codex-permissions'

export function CodexRuntimePanel(props: {
  runtime: CodexRuntime | null | undefined
  busy: boolean
  connected: boolean
  changeMode: (mode: string) => void
  permissionChoices: CodexPermissionChoice[] | null | undefined
  requestPermissions: () => boolean
  changePermissions: (choice: string, confirmed?: boolean) => void
}) {
  return <>
    <select class="conversation-model-select" aria-label="Codex mode"
      title="次の応答の進め方。Plan は計画、通常は実行。権限の設定は変更しません。"
      disabled={props.busy || !props.connected}
      onChange={event => {
        const selected = event.currentTarget.value
        event.currentTarget.value = props.runtime?.mode ?? ''
        if (!props.busy && props.connected) props.changeMode(selected)
      }}>
      <Show when={!['plan', 'default'].includes(props.runtime?.mode ?? '')}>
        <option value="" selected disabled>モード未確認</option>
      </Show>
      <option value="default" selected={props.runtime?.mode === 'default'}>通常</option>
      <option value="plan" selected={props.runtime?.mode === 'plan'}>Plan</option>
    </select>
    <CodexPermissionMenu runtime={props.runtime} choices={props.permissionChoices} busy={props.busy}
      connected={props.connected} requestOptions={props.requestPermissions} change={props.changePermissions} />
    <Show when={props.runtime} fallback={<span class="conversation-model-readonly">実効権限を確認中…</span>}>
      {runtime => <details class="conversation-model-readonly">
        <summary>実効権限{props.connected ? '' : '（最終確認）'}: {runtime().approval} / {runtime().sandbox}</summary>
        <div>承認先: {runtime().reviewer ?? '未確認'}</div>
        <div>ネットワーク: {runtime().network_access == null ? '未確認' : runtime().network_access ? '許可' : '制限あり'}</div>
        <Show when={runtime().profile}><div>プロファイル: {runtime().profile}</div></Show>
        <Show when={runtime().writable_roots.length}>
          <div>書き込み可能な場所:</div>
          <For each={runtime().writable_roots}>{path => <div>{path}</div>}</For>
        </Show>
      </details>}
    </Show>
  </>
}
