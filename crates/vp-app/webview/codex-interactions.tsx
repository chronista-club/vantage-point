import { For, Show, createSignal } from 'solid-js'
import type { CodexInteraction } from './src/generated/CodexInteraction'

/** native の質問 ID を選択状態のキーにする。回答値はこのカードの生存期間だけ保持。 */
export function CodexInteractionCard(props: {
  request: CodexInteraction
  sending: boolean
  error?: string
  respond: (id: string, behavior: 'allow' | 'deny', answers?: Record<string, string>) => void
}) {
  const [answers, setAnswers] = createSignal<Record<string, string>>({})
  const request = () => props.request
  const canAnswer = () => request().can_accept && request().questions.every(q => (answers()[q.id] ?? '').trim().length > 0)
  const answer = () => {
    if (!props.sending && canAnswer()) props.respond(request().request_id, 'allow', answers())
  }
  return <section class="conversation-prompt" aria-label={request().title}>
    <div class="conversation-prompt-header">{request().title}</div>
    <Show when={request().details}>
      <pre style={{ 'white-space': 'pre-wrap', 'overflow-wrap': 'anywhere', 'max-height': '16rem', overflow: 'auto' }}>{request().details}</pre>
    </Show>
    <fieldset disabled={props.sending} style={{ border: 'none', padding: '0', margin: '0' }}>
      <For each={request().questions}>{q => <div class="conversation-prompt-q">
        <div class="conversation-prompt-header">{q.header}</div>
        <label for={`${request().request_id}-${q.id}`}>{q.question}</label>
        <Show when={q.options.length > 0}>
          <div class="conversation-prompt-options">
            <For each={q.options}>{opt => <button type="button" class="conversation-prompt-opt"
              classList={{ selected: answers()[q.id] === opt.label }}
              onClick={() => setAnswers(a => ({ ...a, [q.id]: opt.label }))}>
              <span class="conversation-prompt-opt-label">{opt.label}</span>
              <span class="conversation-prompt-opt-desc">{opt.description}</span>
            </button>}</For>
          </div>
        </Show>
        <input id={`${request().request_id}-${q.id}`} class="conversation-prompt-other-input"
          type={q.is_secret ? 'password' : 'text'} autocomplete="off"
          placeholder="回答を入力…" value={answers()[q.id] ?? ''}
          onInput={e => setAnswers(a => ({ ...a, [q.id]: e.currentTarget.value }))} />
      </div>}</For>
      <div class="conversation-prompt-actions">
        <button class="conversation-prompt-confirm" disabled={!canAnswer()} onClick={answer}>
          {props.sending ? '送信中…' : request().kind === 'question' ? '回答する' : '今回のみ許可'}
        </button>
        <button class="conversation-prompt-cancel" onClick={() => props.respond(request().request_id, 'deny')}>
          {request().kind === 'question' ? '回答を見送る' : '拒否'}
        </button>
      </div>
    </fieldset>
    <Show when={props.error}><div role="alert">{props.error}</div></Show>
  </section>
}
