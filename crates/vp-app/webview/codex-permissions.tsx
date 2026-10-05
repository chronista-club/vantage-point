import { createEffect, createSignal, For, Show } from 'solid-js'
import { Portal } from 'solid-js/web'
import type { CodexRuntime } from './src/generated/CodexRuntime'
import type { CodexPermissionChoice } from './src/generated/CodexPermissionChoice'

const labels: Record<string, string> = {
  'read-only': '承認を求める', standard: '標準', 'auto-review': '代わりに承認', 'full-access': 'フルアクセス',
}

export function CodexPermissionMenu(props: {
  runtime: CodexRuntime | null | undefined
  choices: CodexPermissionChoice[] | null | undefined
  busy: boolean
  connected: boolean
  requestOptions: () => boolean
  change: (choice: string, confirmed?: boolean) => void
}) {
  const [open, setOpen] = createSignal(false)
  const [confirmFull, setConfirmFull] = createSignal(false)
  const [loaded, setLoaded] = createSignal(false)
  const [position, setPosition] = createSignal({left:'16px',bottom:'48px'})
  let anchor: HTMLButtonElement | undefined
  createEffect(() => {
    if (!props.connected) { setOpen(false); setConfirmFull(false); setLoaded(false) }
    else if (open() && !props.busy && props.choices) setLoaded(true)
  })
  const label = () => props.runtime ? labels[props.runtime.preset ?? ''] ?? 'カスタム' : '権限未確認'
  const choose = (id: string, confirmed = false) => {
    if (props.busy || !props.connected || !loaded()) return
    if (!props.choices?.some(c => c.id === id && !c.disabled_reason)) return
    if (id === 'full-access' && !confirmed) { setConfirmFull(true); return }
    props.change(id, confirmed)
    setOpen(false); setConfirmFull(false)
  }
  return <div class="codex-permission-menu" onKeyDown={event => {
    if (event.key === 'Escape') { setOpen(false); setConfirmFull(false) }
  }}>
    <button ref={anchor} type="button" class="conversation-model-select" aria-label="Codex permissions"
      title={props.busy ? '応答と待機入力が完了すると変更できます。' : 'この会話の権限を選択します。'}
      aria-expanded={open()} disabled={props.busy || !props.connected || !props.runtime}
      onClick={() => {
        if (open()) { setOpen(false); setConfirmFull(false); return }
        setLoaded(false)
        const rect = anchor?.getBoundingClientRect()
        if (rect) setPosition({left:`${Math.max(16,Math.min(rect.left,window.innerWidth-430))}px`,bottom:`${Math.max(16,window.innerHeight-rect.top+8)}px`})
        if (props.requestOptions()) setOpen(true)
      }}>{label()}{props.connected ? '' : '（最終確認）'}</button>
    <Show when={open()}>
      <Portal>
      <div class="codex-permission-backdrop" onClick={() => {setOpen(false); setConfirmFull(false)}} />
      <div class="codex-permission-popover" style={position()} role="dialog" aria-label="この会話の権限"
        onKeyDown={event => {if(event.key==='Escape') {setOpen(false); setConfirmFull(false)}}}>
        <div class="codex-permission-heading">この会話の権限
          <button type="button" aria-label="権限メニューを閉じる" onClick={() => {setOpen(false); setConfirmFull(false)}}>×</button>
        </div>
        <Show when={loaded() && props.choices} fallback={<p>候補を確認中です。取得できない場合はメニューを開き直してください。</p>}>
          <Show when={!confirmFull()} fallback={<>
            <p>この会話の次の応答から、ファイルとネットワークへのアクセス制限を外します。</p>
            <button type="button" disabled={props.busy || !props.connected} onClick={() => choose('full-access', true)}>フルアクセスに切り替える</button>
            <button type="button" onClick={() => setConfirmFull(false)}>戻る</button>
          </>}>
            <For each={props.choices}>{choice => <button type="button" class="codex-permission-choice"
              aria-pressed={props.runtime?.preset === choice.id || choice.id === `profile:${props.runtime?.profile}`}
              disabled={props.busy || !props.connected || !!choice.disabled_reason}
              onClick={() => choose(choice.id)}>
              <strong>{choice.label}</strong><span>{choice.description}</span>
              <Show when={choice.disabled_reason}><span>{choice.disabled_reason}</span></Show>
            </button>}</For>
            <p>設定に合わせた権限は「カスタム」と表示します。名前付きプロファイルを選んでも config.toml 全体には戻りません。</p>
          </Show>
        </Show>
      </div>
      </Portal>
    </Show>
  </div>
}
