import { For, Show } from 'solid-js'
import type { CodexInteraction } from './src/generated/CodexInteraction'

/** MCP の入力値は下書きでは文字列、型への変換と制約検証は host が所有する。 */
export function McpElicitationPanel(props: {
  request: CodexInteraction
  sending: boolean
  answers: Record<string, string>
  setAnswer: (id: string, value: string) => void
  respond: (id: string, behavior: 'allow' | 'deny', answers?: Record<string, string>) => void
}) {
  const model = () => props.request.elicitation
  const fields = () => model()?.fields ?? []
  const value = (id: string) => props.answers[`omit:${id}`] === 'true' ? undefined
    : props.answers[id] ?? fields().find(f => f.id === id)?.default_value ?? undefined
  const multiple = (id: string): string[] => {
    try { const result: unknown = JSON.parse(value(id) ?? '[]'); return Array.isArray(result) ? result.filter(v => typeof v === 'string') : [] }
    catch { return [] }
  }
  const canAnswer = () => props.request.can_accept && (model()?.url
    ? props.answers['url-complete'] === 'true'
    : fields().every(f => !f.required || (value(f.id) !== undefined && (f.kind === 'string' || value(f.id) !== ''))))
  const send = () => {
    if (props.sending || !canAnswer()) return
    const answers: Record<string, string> = {}
    for (const field of fields()) {
      const v = value(field.id)
      if (v !== undefined && (field.kind === 'string' || v !== '')) answers[field.id] = v
    }
    props.respond(props.request.request_id, 'allow', model()?.url ? { action: 'accept' } : answers)
  }
  return <>
    <Show when={model()}>{m => <>
      <div class="conversation-prompt-header">{m().server_name}</div>
      <p style={{ 'white-space': 'pre-wrap' }}>{m().message}</p>
    </>}</Show>
    <fieldset disabled={props.sending} style={{border:'none',padding:'0',margin:'0'}}>
      <Show when={model()?.url}>{url => <>
        <a href={url()} rel="noopener noreferrer" aria-disabled={props.sending} onClick={event => {
          event.preventDefault()
          event.stopPropagation()
          if (props.sending || !/^https?:\/\//i.test(url())) return
          const ipc = (window as unknown as {ipc?: {postMessage(message:string):void}}).ipc
          ipc?.postMessage(JSON.stringify({t:'open-url',url:url()}))
        }}>{url()}</a>
        <p><label><input type="checkbox" checked={props.answers['url-complete'] === 'true'}
          onChange={e => props.setAnswer('url-complete', String(e.currentTarget.checked))} />外部ページで手続きを完了しました</label></p>
      </>}</Show>
      <For each={fields()}>{field => <div class="conversation-prompt-q">
        <label for={`${props.request.request_id}-${field.id}`}>{field.title}（{field.required ? '必須' : '任意'}）</label>
        <Show when={field.description}><p style={{'white-space':'pre-wrap'}}>{field.description}</p></Show>
        <Show when={field.kind === 'array'} fallback={
          <Show when={field.kind === 'boolean' || field.options.length > 0} fallback={
            <input id={`${props.request.request_id}-${field.id}`} class="conversation-prompt-other-input" autocomplete="off"
              type={field.kind === 'string' ? 'text' : 'number'} step={field.kind === 'integer' ? '1' : 'any'}
              value={value(field.id) ?? ''} onInput={e => props.setAnswer(field.id, e.currentTarget.value)} />
          }>
            <Show when={field.kind === 'boolean'} fallback={
              <select id={`${props.request.request_id}-${field.id}`} value={String(field.options.findIndex(o => o.value === value(field.id)))}
                onChange={e => {
                  const option = field.options[Number(e.currentTarget.value)]
                  props.setAnswer(`omit:${field.id}`, String(!option))
                  if (option) props.setAnswer(field.id, option.value)
                }}>
                <option value="-1" disabled={field.required}>未回答</option>
                <For each={field.options}>{(option, index) => <option value={String(index())}>{option.label}</option>}</For>
              </select>
            }>
              <select id={`${props.request.request_id}-${field.id}`} value={value(field.id) ?? ''}
                onChange={e => props.setAnswer(field.id, e.currentTarget.value)}>
                <option value="">未回答</option><option value="true">はい</option><option value="false">いいえ</option>
              </select>
            </Show>
          </Show>
        }>
          <For each={field.options}>{option => <label style={{display:'block'}}><input type="checkbox"
            checked={multiple(field.id).includes(option.value)} onChange={e => {
              const next = multiple(field.id).filter(v => v !== option.value)
              if (e.currentTarget.checked) next.push(option.value)
              props.setAnswer(field.id, JSON.stringify(next))
            }} />{option.label}</label>}</For>
        </Show>
      </div>}</For>
      <div class="conversation-prompt-actions">
        <button class="conversation-prompt-confirm" disabled={!canAnswer()} onClick={send}>{props.sending ? '送信中…' : model()?.url ? '完了を伝える' : '回答する'}</button>
        <button class="conversation-prompt-cancel" onClick={() => props.respond(props.request.request_id,'deny')}>回答を辞退する</button>
        <button data-mcp-cancel onClick={() => props.respond(props.request.request_id,'allow',{action:'cancel'})}>手続きをキャンセル</button>
      </div>
    </fieldset>
  </>
}
