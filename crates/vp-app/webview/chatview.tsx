/**
 * ChatView (doc 33 C2) — Echoes Act II の Console 面 GUI（SolidJS）。
 *
 * World B。`window.vpConsole`（console.ts）が届ける [`EchoesEvent`] を per-lane store に
 * 畳み込み、active lane の会話を message stream として描画する。入力は IPC `echoes:submit`
 * で SP へ送る。marked で markdown、motion は CSS（prefers-reduced-motion 尊重）。
 *
 * 設計: 単一 ChatView が active lane の store を表示。store は lane ごとに永続（lane 切替で
 * 再 replay しない）。renderer は lane 初出時に一度だけ attach（console.ts が buffer を replay）。
 */

import { render } from 'solid-js/web'
import { createSignal, For, Show, type Accessor } from 'solid-js'
import { createStore, produce, type SetStoreFunction } from 'solid-js/store'
import { marked } from 'marked'
import type { EchoesEvent, PlanEntry, VpConsole } from './console'

// ---------------------------------------------------------------------------
// 会話モデル — flat item stream（EchoesEvent を UI 単位に畳む）
// ---------------------------------------------------------------------------

type ChatItem =
  | { kind: 'user'; text: string }
  | { kind: 'assistant'; text: string } // message_chunk を末尾 assistant に append
  | { kind: 'thinking'; text: string } // thought_chunk を末尾 thinking に append
  | { kind: 'tool'; id: string; name: string; done: boolean; error: boolean }

type ChatState = {
  header: { model?: string; sessionId?: string } | null
  items: ChatItem[]
  plan: PlanEntry[]
  streaming: boolean
  cost: number | null
}

type LaneChat = {
  state: ChatState
  set: SetStoreFunction<ChatState>
}

const laneChats = new Map<string, LaneChat>()
const [activeLane, setActiveLane] = createSignal<string | null>(null)

function laneChat(lane: string): LaneChat {
  let lc = laneChats.get(lane)
  if (!lc) {
    const [state, set] = createStore<ChatState>(emptyChatState())
    lc = { state, set }
    laneChats.set(lane, lc)
  }
  return lc
}

/**
 * EchoesEvent を ChatState に畳み込む純粋 mutation（reducer 本体）。
 *
 * solid の `produce` draft でも plain object でも同じに動く（＝ store 非依存 = 単体テスト可能）。
 * 会話モデリングの肝: message_chunk / thought_chunk は末尾同種 item に append（accumulate）、
 * tool_call_update は id 一致で done 化。ここが Act II の描画正しさの中核。
 */
export function foldInto(s: ChatState, ev: EchoesEvent): void {
  switch (ev.kind) {
    case 'replay_start':
      // 以降は transcript replay（過去会話の再送）。会話を一度クリアしてから畳み直す。
      // backend は「新規 attach」と「reconnect / demand 再発火」を区別できないため、reset せず
      // 追記すると再接続のたび会話が二重化する（terminal replay の clear-prefix と同型の問題）。
      // reset → 再構築なら cold-start でも reconnect でも同じ最終状態に収束する（= 冪等）。
      // header は live engine の session_init が持つ情報なので保持する。
      s.items = []
      s.plan = []
      s.streaming = false
      s.cost = null
      break
    case 'user_message':
      // replay 専用（live は submit 時に ChatView が optimistic に足す）。常に新 bubble。
      s.items.push({ kind: 'user', text: ev.text })
      break
    case 'session_init':
      s.header = { model: ev.model, sessionId: ev.session_id }
      break
    case 'message_chunk': {
      s.streaming = true
      const last = s.items[s.items.length - 1]
      if (last && last.kind === 'assistant') last.text += ev.text
      else s.items.push({ kind: 'assistant', text: ev.text })
      break
    }
    case 'thought_chunk': {
      // thinking も active turn の一部（extended thinking は message より前に来る）。
      // streaming を立てることで末尾 thinking の live 判定 = shimmer 演出に使える。
      s.streaming = true
      const last = s.items[s.items.length - 1]
      if (last && last.kind === 'thinking') last.text += ev.text
      else s.items.push({ kind: 'thinking', text: ev.text })
      break
    }
    case 'tool_call':
      s.items.push({ kind: 'tool', id: ev.id, name: ev.name, done: false, error: false })
      break
    case 'tool_call_update': {
      const t = s.items.find((i) => i.kind === 'tool' && i.id === ev.tool_use_id) as
        | Extract<ChatItem, { kind: 'tool' }>
        | undefined
      if (t) {
        t.done = true
        t.error = ev.is_error ?? false
      }
      break
    }
    case 'plan':
      s.plan = ev.entries
      break
    case 'turn_completed':
      s.streaming = false
      s.cost = ev.cost_usd ?? s.cost
      break
    case 'error':
      s.streaming = false
      s.items.push({ kind: 'assistant', text: `\n\n⚠️ **engine error**: ${ev.message}` })
      break
  }
}

/** EchoesEvent を lane の store に畳み込む（console.ts の renderer 本体）。 */
function foldEvent(lane: string, ev: EchoesEvent): void {
  laneChat(lane).set(produce((s) => foldInto(s, ev)))
}

/** 空の ChatState（store 初期値 + テスト用）。 */
export function emptyChatState(): ChatState {
  return { header: null, items: [], plan: [], streaming: false, cost: null }
}

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------

function mdToHtml(text: string): string {
  return marked.parse(text) as string
}

function ThinkingBlock(props: { text: string; active: () => boolean }) {
  const [open, setOpen] = createSignal(false)
  return (
    <div class="echoes-thinking">
      <button
        class="echoes-thinking-toggle"
        classList={{ live: props.active() }}
        onClick={() => setOpen(!open())}
      >
        <span class="echoes-thinking-caret" classList={{ open: open() }}>
          ▸
        </span>
        {/* active 中はラベルを shimmer で光らせる（考え中の質感）。 */}
        <span class="echoes-thinking-label">thinking</span>
      </button>
      <Show when={open()}>
        <div class="echoes-thinking-body">{props.text}</div>
      </Show>
    </div>
  )
}

function ToolRow(props: { name: string; done: boolean; error: boolean }) {
  return (
    <div class="echoes-tool" classList={{ done: props.done, error: props.error }}>
      <span class="echoes-tool-spinner" />
      <span class="echoes-tool-icon">🔧</span>
      <span class="echoes-tool-name">{props.name}</span>
      <span class="echoes-tool-status">
        {props.error ? 'error' : props.done ? '✓' : '実行中…'}
      </span>
    </div>
  )
}

function PlanWidget(props: { entries: Accessor<PlanEntry[]> }) {
  return (
    <Show when={props.entries().length > 0}>
      <div class="echoes-plan">
        <div class="echoes-plan-title">Plan</div>
        <For each={props.entries()}>
          {(e) => (
            <div class="echoes-plan-item" classList={{ [e.status]: true }}>
              <span class="echoes-plan-dot" />
              <span class="echoes-plan-text">
                {e.status === 'in_progress' ? (e.active_form ?? e.content) : e.content}
              </span>
            </div>
          )}
        </For>
      </div>
    </Show>
  )
}

/** model picker の選択肢（value = `--model` に渡る id、'' = claude default）。
 *  session_init が返す実測 model が一覧に無い場合は動的に option を足して真実を見せる。 */
const MODEL_CHOICES: ReadonlyArray<readonly [string, string]> = [
  ['', 'Default'],
  ['claude-fable-5', 'Fable 5'],
  ['claude-opus-4-8', 'Opus 4.8'],
  ['claude-sonnet-5', 'Sonnet 5'],
  ['claude-haiku-4-5-20251001', 'Haiku 4.5'],
]

function ChatView() {
  const current = (): LaneChat | null => {
    const l = activeLane()
    return l ? laneChat(l) : null
  }
  const state = (): ChatState | null => current()?.state ?? null

  // Act II モデル切替（spec: セッション進行中でも切替可能）。SP が engine を --resume +
  // 新 --model で入れ替える = 会話コンテキスト継続でモデル交換。適用の視覚確認は
  // 新 engine の session_init が header.model を更新することで得る（picker は実測値に追従）。
  // streaming 中は disable — engine drop が進行中 turn を切るのを UI で抑止する。
  const currentModel = (): string => state()?.header?.model ?? ''
  const modelChoices = (): ReadonlyArray<readonly [string, string]> => {
    const m = currentModel()
    return m && !MODEL_CHOICES.some(([v]) => v === m)
      ? [...MODEL_CHOICES, [m, m] as const]
      : MODEL_CHOICES
  }
  const setModel = (model: string) => {
    const lane = activeLane()
    if (!lane) return
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({ t: 'console:set_model', lane, model: model || null }),
    )
  }

  const [draft, setDraft] = createSignal('')
  const submit = () => {
    const lane = activeLane()
    const text = draft().trim()
    if (!lane || !text) return
    // optimistic: user bubble を即描画
    laneChat(lane).set(produce((s) => s.items.push({ kind: 'user', text })))
    setDraft('')
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify({ t: 'echoes:submit', lane, prompt: text }))
  }

  return (
    <div class="echoes-chat">
      <Show
        when={state()}
        fallback={<div class="echoes-empty">Console (Act II) — lane 未選択</div>}
      >
        <div class="echoes-header">
          <span class="echoes-header-label">model</span>
          <select
            class="echoes-model-select"
            disabled={state()!.streaming}
            onChange={(e) => setModel(e.currentTarget.value)}
          >
            <For each={modelChoices()}>
              {([v, label]) => (
                <option value={v} selected={v === currentModel()}>
                  {label}
                </option>
              )}
            </For>
          </select>
        </div>
        <PlanWidget entries={() => state()!.plan} />
        <div class="echoes-stream">
          <For each={state()!.items}>
            {(item, index) => {
              if (item.kind === 'thinking')
                return (
                  <ThinkingBlock
                    text={item.text}
                    // 末尾 thinking かつ turn 進行中 = 今まさに考え中 → shimmer。
                    active={() => index() === state()!.items.length - 1 && state()!.streaming}
                  />
                )
              if (item.kind === 'tool') {
                return <ToolRow name={item.name} done={item.done} error={item.error} />
              }
              return (
                <div class="echoes-msg" classList={{ user: item.kind === 'user' }}>
                  <div class="echoes-msg-body" innerHTML={mdToHtml(item.text)} />
                </div>
              )
            }}
          </For>
          <Show when={state()!.streaming}>
            <div class="echoes-cursor" />
          </Show>
        </div>
        <div class="echoes-input">
          <textarea
            class="echoes-input-box"
            placeholder="メッセージを入力（⌘Enter で送信）"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault()
                submit()
              }
            }}
          />
          <button class="echoes-send" onClick={submit} disabled={!draft().trim()}>
            送信
          </button>
        </div>
      </Show>
    </div>
  )
}

// ---------------------------------------------------------------------------
// install — entry.tsx から呼ぶ
// ---------------------------------------------------------------------------

/** ChatView の scoped CSS。entry.tsx が `<style>` で注入する（pp.ts の style 注入と同型）。
 *  色は creo-ui token（--color-* 系）に寄せ、無い環境でも読める fallback を持つ。 */
export const CHATVIEW_CSS = `
.echoes-chat { position:absolute; inset:0; display:flex; flex-direction:column;
  background: var(--color-bg, #0f1115); color: var(--color-text, #e6e9ef);
  font-family: var(--font-ui, system-ui, -apple-system, sans-serif); overflow:hidden; }
.echoes-empty { margin:auto; color: var(--color-text-tertiary, #616b80); font-size:13px; }
.echoes-stream { flex:1; overflow-y:auto; padding:16px 18px; display:flex; flex-direction:column; gap:12px; }
.echoes-msg { max-width:100%; animation: echoes-fade .18s ease-out; }
.echoes-msg.user { align-self:flex-end; background: var(--color-accent-soft, #1c2333);
  border:1px solid var(--color-border, #2a3040); border-radius:12px 12px 3px 12px; padding:8px 13px; max-width:80%; }
.echoes-msg-body { font-size:13.5px; line-height:1.6; word-break:break-word; }
.echoes-msg-body :first-child { margin-top:0; } .echoes-msg-body :last-child { margin-bottom:0; }
.echoes-msg-body pre { background: var(--color-bg-elevated, #16191f); border:1px solid var(--color-border,#2a3040);
  border-radius:8px; padding:10px 12px; overflow-x:auto; font-size:12px; }
.echoes-msg-body code { font-family: var(--font-mono, ui-monospace, monospace); }
.echoes-thinking { align-self:flex-start; font-size:12px; }
.echoes-thinking-toggle { background:none; border:none; color: var(--color-text-tertiary,#8b93a7);
  cursor:pointer; font-size:12px; padding:2px 0; display:flex; align-items:center; gap:5px; }
.echoes-thinking-caret { transition: transform .15s ease; display:inline-block; }
.echoes-thinking-caret.open { transform: rotate(90deg); }
.echoes-thinking-label { display:inline-block; }
/* active（末尾 thinking かつ turn 進行中）: 文字を gradient sweep で shimmer させ「考え中」を伝える。 */
.echoes-thinking-toggle.live .echoes-thinking-label {
  background: linear-gradient(100deg, var(--color-text-tertiary,#8b93a7) 30%,
    var(--color-text,#e6e9ef) 50%, var(--color-text-tertiary,#8b93a7) 70%);
  background-size: 220% 100%; -webkit-background-clip:text; background-clip:text;
  -webkit-text-fill-color:transparent; color:transparent;
  animation: echoes-shimmer 1.5s linear infinite; }
.echoes-thinking-body { margin:4px 0 0 16px; padding:8px 12px; border-left:2px solid var(--color-border,#2a3040);
  color: var(--color-text-secondary,#a8b0c0); white-space:pre-wrap; font-size:12px; line-height:1.55; }
.echoes-tool { align-self:flex-start; display:flex; align-items:center; gap:8px; font-size:12px;
  color: var(--color-text-secondary,#a8b0c0); background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--color-border,#2a3040); border-radius:8px; padding:5px 11px; animation: echoes-fade .18s ease-out; }
.echoes-tool-spinner { width:9px; height:9px; border-radius:50%; border:1.5px solid var(--color-accent,#3b82f6);
  border-top-color: transparent; animation: echoes-spin .7s linear infinite; }
.echoes-tool.done .echoes-tool-spinner, .echoes-tool.error .echoes-tool-spinner { display:none; }
.echoes-tool.done { color: var(--color-text-tertiary,#616b80); } .echoes-tool.error { color:#f0a3a3; }
.echoes-tool-name { font-family: var(--font-mono, ui-monospace, monospace); }
.echoes-tool-status { margin-left:auto; font-size:11px; }
.echoes-cursor { width:7px; height:15px; background: var(--color-accent,#3b82f6); border-radius:1px;
  animation: echoes-blink 1s step-start infinite; align-self:flex-start; }
.echoes-plan { border-bottom:1px solid var(--color-border,#2a3040); padding:10px 18px; background: var(--color-bg-elevated,#13161c); }
.echoes-plan-title { font-size:10px; text-transform:uppercase; letter-spacing:.08em; color: var(--color-text-tertiary,#616b80); margin-bottom:6px; }
.echoes-plan-item { display:flex; align-items:center; gap:8px; font-size:12.5px; padding:2px 0; transition: color .2s ease; }
.echoes-plan-dot { width:7px; height:7px; border-radius:50%; background: var(--color-text-tertiary,#616b80); transition: background .2s ease; }
.echoes-plan-item.in_progress { color: var(--color-text,#e6e9ef); } .echoes-plan-item.in_progress .echoes-plan-dot { background: var(--color-accent,#e2b96f); }
.echoes-plan-item.completed { color: var(--color-text-tertiary,#616b80); } .echoes-plan-item.completed .echoes-plan-dot { background: var(--color-success,#6fe2a8); }
.echoes-plan-item.completed .echoes-plan-text { text-decoration: line-through; }
.echoes-input { display:flex; gap:8px; padding:12px 14px; border-top:1px solid var(--color-border,#2a3040); }
.echoes-input-box { flex:1; resize:none; min-height:38px; max-height:160px; padding:9px 12px; font-size:13px;
  font-family: var(--font-ui, system-ui, sans-serif); color: var(--color-text,#e6e9ef);
  background: var(--color-bg-elevated,#16191f); border:1px solid var(--color-border,#2a3040); border-radius:9px; outline:none; }
.echoes-input-box:focus { border-color: var(--color-accent,#3b82f6); }
.echoes-send { align-self:flex-end; padding:9px 16px; font-size:13px; border-radius:9px; border:none; cursor:pointer;
  background: var(--color-accent,#3b82f6); color:#fff; }
.echoes-send:disabled { opacity:.4; cursor:default; }
.echoes-act-toggle { position:absolute; top:8px; right:12px; z-index:10; font-size:11px; padding:4px 11px;
  border-radius:14px; border:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#16191f);
  color: var(--color-text-secondary,#a8b0c0); cursor:pointer; opacity:.75; transition: opacity .15s ease; }
.echoes-act-toggle:hover { opacity:1; }
/* console 右上の操作群（New Session + Act toggle）。container が位置を持ち、子は並ぶだけ。 */
.echoes-console-actions { position:absolute; top:8px; right:12px; z-index:10; display:flex; gap:8px; }
.echoes-console-actions .echoes-act-toggle { position:static; }
/* New Session の armed 状態（2 クリック確認の 1 段目）: 誤爆防止の視覚合図。 */
.echoes-new-session.armed { color: var(--color-accent,#e2b96f); border-color: var(--color-accent,#e2b96f); opacity:1; }
.echoes-header { display:flex; align-items:center; gap:8px; padding:7px 14px;
  border-bottom:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#13161c); }
.echoes-header-label { font-size:10px; text-transform:uppercase; letter-spacing:.08em;
  color: var(--color-text-tertiary,#616b80); }
.echoes-model-select { font-size:11px; padding:3px 7px; border-radius:7px; outline:none; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#16191f);
  color: var(--color-text-secondary,#a8b0c0); }
.echoes-model-select:disabled { opacity:.45; cursor:default; }
@keyframes echoes-fade { from { opacity:0; transform: translateY(4px); } to { opacity:1; transform:none; } }
@keyframes echoes-spin { to { transform: rotate(360deg); } }
@keyframes echoes-blink { 50% { opacity:0; } }
@keyframes echoes-shimmer { from { background-position: 220% 0; } to { background-position: -120% 0; } }
@media (prefers-reduced-motion: reduce) {
  .echoes-msg, .echoes-tool { animation:none; }
  .echoes-tool-spinner { animation-duration: 1.5s; } .echoes-cursor { animation:none; opacity:.6; }
  /* motion off: shimmer は止めるが text-fill:transparent のままだと消えるので色を戻す。 */
  .echoes-thinking-toggle.live .echoes-thinking-label { animation:none; background:none;
    -webkit-text-fill-color: currentColor; color: var(--color-text,#e6e9ef); }
}
`

export type ChatViewApi = {
  /** lane を active にして表示（初出なら vpConsole renderer を attach）。 */
  showLane(lane: string): void
}

export function installChatView(mount: HTMLElement, vpConsole: VpConsole): ChatViewApi {
  render(() => <ChatView />, mount)
  const attached = new Set<string>()
  return {
    showLane(lane: string) {
      if (!attached.has(lane)) {
        attached.add(lane)
        laneChat(lane) // store を先に用意（replay が流し込む）
        vpConsole.attachRenderer(lane, (ev) => foldEvent(lane, ev))
      }
      setActiveLane(lane)
    },
  }
}
