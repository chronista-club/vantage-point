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
import {
  createSignal,
  createMemo,
  createEffect,
  onMount,
  onCleanup,
  For,
  Show,
  Switch,
  Match,
  type Accessor,
} from 'solid-js'
import { createStore, produce, type SetStoreFunction } from 'solid-js/store'
import { CreoIcon } from '@chronista-club/creo-ui-icons-web'
import { marked } from 'marked'
import type {
  BusRequestId,
  EchoesEvent,
  EchoesSession,
  PlanEntry,
  QuestionSpec,
  VpConsole,
} from './console'
// doc 38 Phase 2: focused 判定 / 楽観的 focus 切替は console.ts の per-lane registry を共有する
// （SP が真実源、ここは view）。session chip の prefix 規則は EchoesHeader を SSOT として再利用。
// doc 47 §6: 共有 bus の相関 id（採番 + 照合）も console.ts が SSOT。
import { focusedOf, noteFocus, syncHeaderSessionId } from './console'
import { sessionChipPrefix } from './EchoesHeader'

// ---------------------------------------------------------------------------
// 会話モデル — flat item stream（EchoesEvent を UI 単位に畳む）
// ---------------------------------------------------------------------------

type ChatItem =
  | { kind: 'user'; text: string }
  | { kind: 'assistant'; text: string; sealed?: boolean } // append 先。sealed=turn 境界（§5.1、次 turn は新バブル）
  | { kind: 'thinking'; text: string } // thought_chunk を末尾 thinking に append
  // tool。input/result は詳細展開の表示源。backend は最初から ToolCall{input} /
  // ToolCallUpdate{content} を送っているので、view が保持するだけで詳細が開ける。
  // subagent は Agent tool が回した子の発話（--forward-subagent-text 有効時のみ）。
  | {
      kind: 'tool'
      id: string
      name: string
      done: boolean
      error: boolean
      input?: unknown
      result?: string
      subagent?: SubagentEntry[]
    }
  // doc 35 PR1/PR3: HITL PromptCard。question（選択肢）or permission（allow/deny）。answered で折りたたむ。
  | {
      kind: 'prompt'
      requestId: string
      questions: QuestionSpec[]
      answered: boolean
      answers?: Record<string, string>
      // PR3: permission（tool 承認）。存在すれば allow/deny UI を描く。
      permission?: { toolName: string; input: unknown }
      decision?: 'allow' | 'deny'
    }

/**
 * subagent（Agent tool の子）の発話 1 節。
 *
 * engine は親子を 1 本の stream に混ぜて流し、`parent_tool_use_id` だけが両者を分ける。
 * ここでは親の tool item にぶら下げて保持する = 「誰の発話か」を構造で保証する。
 */
export type SubagentEntry = { role: 'prompt' | 'thinking' | 'text'; text: string }

/** tool アイテム（accordion 集約の対象）。 */
export type ToolItem = Extract<ChatItem, { kind: 'tool' }>

/**
 * 連続同名 tool の run における、ある位置の役割。
 * - `single`: run 長 1 → 従来どおり 1 行（ToolRow）
 * - `head`: run 長 ≥2 の先頭 → run 全体を 1 行の accordion（ToolGroupRow）に畳む
 * - `member`: run 長 ≥2 の 2 件目以降 → 先頭 group に吸収されるので描画しない（null）
 */
export type ToolRunRole =
  | { role: 'single' }
  | { role: 'head'; run: ToolItem[] }
  | { role: 'member' }

/**
 * items[idx]（tool 前提）が属する「連続同名 tool run」での役割を返す純粋関数。
 *
 * ねらい: Agent 等が連続で回ったとき N 行を占有せず「🔧 Agent ×N」の 1 行に畳む。
 * reducer（foldInto）は一切触らず描画時のみ集約するので、transcript replay や孤児
 * tool_call_update 処理の不変条件（§C2「描画正しさの中核」）に影響しない。
 *
 * foldInto は append-only（tool は push、状態変化は in-place 変異）なので、既存 item の
 * index・run 所属は不変で、run は末尾にだけ伸びる。呼び出し側が items/index を reactive に
 * 読むことで、stream 追記に追従して single→head へ昇格する。
 */
export function classifyToolRun(items: ChatItem[], idx: number): ToolRunRole {
  const it = items[idx]
  if (!it || it.kind !== 'tool') return { role: 'single' } // 防御（呼ばれない前提）
  const name = it.name
  // run 先頭を左へ探索
  let start = idx
  while (start - 1 >= 0) {
    const p = items[start - 1]
    if (p.kind !== 'tool' || p.name !== name) break
    start--
  }
  // run 終端を右へ探索
  let end = idx
  while (end + 1 < items.length) {
    const n = items[end + 1]
    if (n.kind !== 'tool' || n.name !== name) break
    end++
  }
  if (end - start + 1 < 2) return { role: 'single' }
  if (idx === start) return { role: 'head', run: items.slice(start, end + 1) as ToolItem[] }
  return { role: 'member' }
}

/**
 * ToolGroupRow header の集約 status を導く純粋関数（テスト可能）。
 *
 * 「エンジン状態を偽らない」方針の実装点: 1 件でも未 done なら running=true（走行中）を返し、
 * その間は完了数 `{done}/{count}` を label にする。run 内の 1 件が error で終わっても、他が
 * in-flight なら error/✓ には落とさない（deriveStatus / stall 表示と同じ価値観）。全 tool が
 * settle して初めて、error があれば `error`、無ければ `✓` を返す。
 */
export function toolGroupStatus(tools: ToolItem[]): { running: boolean; label: string } {
  const doneCount = tools.filter((t) => t.done).length
  if (doneCount < tools.length) return { running: true, label: `${doneCount}/${tools.length}` }
  return { running: false, label: tools.some((t) => t.error) ? 'error' : '✓' }
}

type ChatState = {
  header: { model?: string; sessionId?: string } | null
  items: ChatItem[]
  plan: PlanEntry[]
  streaming: boolean
  cost: number | null
  /** context ゲージ（Act I statusline の bar :context 相当）。turn_completed で更新。 */
  contextTokens: number | null
  contextWindow: number | null
  /** doc 35 PR3/PR4: engine の permission mode（session_init.permission_mode 由来）。per-lane。 */
  permissionMode?: string
  /** doc 35 §5.1: streaming 中に送られた type-ahead。turn 閉で flush（表示順=処理順の不変条件）。 */
  pending: string | null
  /** status 同期: 最後に畳んだイベント種別（foldInto で全イベント更新）。 */
  lastEvent: string | null
  /** status 同期: 最後にイベントを受けた時刻 ms（foldEvent で Date.now。hang 検出の時間軸）。 */
  lastEventAt: number | null
  /** transcript replay（attach/reconnect 時の過去会話再送）進行中か。replay_start→true /
   *  replay_end→false。コーナーの再同期ローディングアニメ（resync-loader）の可視条件。 */
  replaying: boolean
}

type LaneChat = {
  state: ChatState
  set: SetStoreFunction<ChatState>
}

/**
 * 会話 store は **(lane, session) 単位**（doc 50 §4.3 #1）。
 *
 * 旧実装は lane 単位で、focused 以外の session の event は `foldEvent` が捨てていた
 * （= 1 lane に 1 つの会話しか持てない）。session ↔ Pane 1:1（doc 46 §1.5）では N 枚の
 * chat Pane が同時に生きるので、Rust 側 P5 の `pty_slots` re-key と**同型の re-key** を
 * JS 側にも入れる。
 *
 * key は `lane` と `session` を NUL で連結する。`#` や `:` は lane 名や engine session id に
 * 現れうるので衝突源になる。NUL はどの経路の lane 名にも入らない。
 * ⚠️ source では必ずエスケープ（`\u0000`）で書く — リテラルの不可視文字を置くと grep も diff も
 * 読めなくなる（この commit で実際に踏んだ）。
 */
const laneChats = new Map<string, LaneChat>()
const [activeLane, setActiveLane] = createSignal<string | null>(null)

/** store の key（純関数 = テスト対象）。lane 名に何が入っても衝突しない区切りを使う。 */
export function chatKey(lane: string, session: number): string {
  return `${lane}\u0000${session}`
}

function laneChat(lane: string, session: number): LaneChat {
  const k = chatKey(lane, session)
  let lc = laneChats.get(k)
  if (!lc) {
    const [state, set] = createStore<ChatState>(emptyChatState())
    lc = { state, set }
    laneChats.set(k, lc)
  }
  return lc
}

/**
 * lane の **focused session** の store。
 *
 * ⚠️ 過渡的な helper。doc 50 P1 の残り（ChatView を (lane, session) の props で受け、session
 * ごとに 1 枚ずつ mount する）が入ると呼び出し側は自分の session を知っているので不要になる。
 * 今は「**store は session 別 / 描画はまだ focused の 1 枚**」の段階。
 */
function focusedChat(lane: string): LaneChat {
  return laneChat(lane, focusedOf(lane))
}

// ---------------------------------------------------------------------------
// session view registry（module-level — 全 SessionChatView と installChatView が共有）
// SP（echoes_session_list）が真実源。ここは 'vp:echoes-sessions' bus を映すだけの view cache で
// state を持たない。focused の真値は console.ts の registry（focusedOf）— ここは reactive 表示用の鏡。
// ---------------------------------------------------------------------------

export type LaneSessionsView = { focused: number; sessions: EchoesSession[] }
const [sessionViews, setSessionViews] = createSignal<Record<string, LaneSessionsView>>({})

/** lane の session 一覧 view（reactive）。 */
function sessionsOf(lane: string): LaneSessionsView | null {
  return sessionViews()[lane] ?? null
}

/** session に focus を移す（pane click / 旧 tab click の移植）。
 *  楽観更新（noteFocus + local signal）+ IPC。authoritative は後続の echoes_session_list。 */
export function focusChatSession(lane: string, session: number): void {
  if (session === (sessionsOf(lane)?.focused ?? focusedOf(lane))) return
  // doc 38 §4.3: focus 切替で再同期ローダーを必ず一度下ろす（旧 focused の replay_end を
  // 取りこぼしていても固着させない）。直後の demand_start → ReplayStart が必要なら立て直す。
  clearReplaying(lane)
  noteFocus(lane, session)
  // D1: 既存 session 間の切替でも名札の session chip を即追従させる（authoritative は
  // echoes_session_list → handleSessionList 側の sync が上書き）。
  if (syncHeaderSessionId(lane)) {
    document.dispatchEvent(new CustomEvent('vp:echoes-header', { detail: { lane } }))
  }
  setSessionViews((prev) => {
    const cur = prev[lane] ?? { focused: session, sessions: [] }
    return { ...prev, [lane]: { ...cur, focused: session } }
  })
  const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
  ipc?.postMessage(JSON.stringify({ t: 'echoes:session_focus', lane, session }))
}

/** session を閉じる（session ↔ Pane 1:1 なので pane ごと消える）。backend（echoes_session_remove）
 *  が registry から除去 → 除去後の focus 先を返し、app.rs が list 再取得 + demand_start する。
 *  最後の 1 本 / root は backend が Err で拒否（UI 側 canCloseSession と多重防御）。 */
export function removeChatSession(lane: string, session: number): void {
  const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
  ipc?.postMessage(JSON.stringify({ t: 'echoes:session_remove', lane, session }))
}

// ---------------------------------------------------------------------------
// doc 38 §4.3 — 再同期ローダー（resync-loader）の固着防止
//
// `replaying` は replay_start→true / replay_end→false で駆動するが、replay_end が来ない経路
// （Act I lane / error 中断 / engine 途絶）では立ちっぱなしになれる。表示を「focused session の
// attach 状態機械」に束縛する:
//  ① lane/Act/tab 切替で必ず解除（clearReplaying を各遷移点で呼ぶ）
//  ② replay_start ごとに watchdog を張り、REPLAY_WATCHDOG_MS 無応答なら強制解除 + warn（安全網）
//  ③ watchdog は **(lane, session) 単位**（doc 50 §4.3 #2 で store を session 別にしたのに合わせる）。
//     旧実装は lane 単位で、fold 側が background session を捨てていたので衝突しなかった。
//     捨てるのをやめた今は lane 単位のままだと、別 session の replay_start が
//     前の session の watchdog を解除してしまう（= 固着の検出を取りこぼす）。
// ---------------------------------------------------------------------------

/** replay_end が来ない時に replaying を強制解除するまでの猶予 ms（安全網）。 */
const REPLAY_WATCHDOG_MS = 10_000
const replayWatchdogs = new Map<string, ReturnType<typeof setTimeout>>()

/**
 * 入力欄の高さを内容に合わせる（既定 1 行 → 打った分だけ伸び、CSS の max-height で頭打ち）。
 * `height:auto` で一度潰してから scrollHeight を測るのは、縮む方向にも追随させるため
 *（先に潰さないと scrollHeight が前回の高さに引きずられて減らない）。
 */
function autosize(el: HTMLTextAreaElement): void {
  el.style.height = 'auto'
  el.style.height = `${el.scrollHeight}px`
}

/** 張ってある watchdog を取り消す（replay_end / error / 明示解除の時）。 */
function clearReplayWatchdog(lane: string, session: number): void {
  const k = chatKey(lane, session)
  const t = replayWatchdogs.get(k)
  if (t !== undefined) {
    clearTimeout(t)
    replayWatchdogs.delete(k)
  }
}

/** replay_start を受けた時に watchdog を張り直す（REPLAY_WATCHDOG_MS 後に強制解除）。 */
function armReplayWatchdog(lane: string, session: number): void {
  clearReplayWatchdog(lane, session)
  const k = chatKey(lane, session)
  replayWatchdogs.set(
    k,
    setTimeout(() => {
      replayWatchdogs.delete(k)
      const lc = laneChats.get(k)
      if (lc && lc.state.replaying) {
        // replay_end 未達 = 固着。強制解除して warn（console IPC 経由で Rust log にも載る）。
        console.warn(
          `[chatview] resync watchdog: replay_end 未達で再同期ローダーを強制解除 (lane=${lane} session=${session})`,
        )
        lc.set('replaying', false)
      }
    }, REPLAY_WATCHDOG_MS),
  )
}

/** 指定 lane の**全 session** の再同期表示を明示的に下ろす（lane 切替で呼ぶ）。watchdog も取り消す。
 *  cache 未作成の session は no-op（空 entry を作らない）。 */
function clearReplaying(lane: string): void {
  const prefix = `${lane}\u0000`
  for (const [k, lc] of laneChats) {
    if (!k.startsWith(prefix)) continue
    const t = replayWatchdogs.get(k)
    if (t !== undefined) {
      clearTimeout(t)
      replayWatchdogs.delete(k)
    }
    if (lc.state.replaying) lc.set('replaying', false)
  }
}

/**
 * EchoesEvent を ChatState に畳み込む純粋 mutation（reducer 本体）。
 *
 * solid の `produce` draft でも plain object でも同じに動く（＝ store 非依存 = 単体テスト可能）。
 * 会話モデリングの肝: message_chunk / thought_chunk は末尾同種 item に append（accumulate）、
 * tool_call_update は id 一致で done 化。ここが Act II の描画正しさの中核。
 */
export function foldInto(s: ChatState, ev: EchoesEvent): void {
  s.lastEvent = ev.kind // 拾える全イベント種別を status に同期（時刻は foldEvent が Date.now で付す）
  switch (ev.kind) {
    case 'replay_start':
      // 以降は transcript replay（過去会話の再送）。会話を一度クリアしてから畳み直す。
      // backend は「新規 attach」と「reconnect / demand 再発火」を区別できないため、reset せず
      // 追記すると再接続のたび会話が二重化する（terminal replay の clear-prefix と同型の問題）。
      // reset → 再構築なら cold-start でも reconnect でも同じ最終状態に収束する（= 冪等）。
      // header / context ゲージは live engine 由来の session 状態（会話 item ではない）なので
      // 保持する — transcript replay は turn_completed / session_init を運ばないため、消すと
      // reconnect のたびゲージ・ヘッダーが空に戻ってしまう。
      //
      // replay 列は `transcript(commit 済み) ++ in-flight tail(生成中の未 commit 増分)`。
      // よって生成の真っ最中に着地しても、末尾には「途中まで書かれた assistant バブル」が
      // 再構築される。復帰後の message_chunk はそこへ自然に append される（= 文の途中から
      // 新バブルが立つことはない）。tail が streaming を立て直すのでカーソルも戻る。
      s.items = []
      s.plan = []
      s.streaming = false
      s.cost = null
      s.replaying = true // 再同期ローディング表示 ON（replay_end で OFF）
      break
    case 'replay_end':
      s.replaying = false // 再同期完了 → ローディング表示 OFF
      // replay 終端で streaming の真値を確定する。replay は過去の assistant 発話も message_chunk で
      // 送るため fold で streaming が立つが、replay 列は turn_completed を運ばない。生成中 turn が
      // 無ければここで下ろさないと、engine が idle でも「応答中」が永久に残り、turn 完了契機の処理
      //（type-ahead の flush 等）が二度と発火しない。
      s.streaming = ev.in_flight
      break
    case 'user_message':
      // replay 専用（live は submit 時に ChatView が optimistic に足す）。常に新 bubble。
      s.items.push({ kind: 'user', text: ev.text })
      break
    case 'session_init':
      s.header = { model: ev.model, sessionId: ev.session_id }
      // review #2: permission mode の真値を per-lane に反映（engine は respawn 時 bypassPermissions
      // で立ち上がるので、select が実態とズレないよう session_init の値で上書きする）。
      s.permissionMode = ev.permission_mode
      break
    case 'message_chunk': {
      s.streaming = true
      const last = s.items[s.items.length - 1]
      if (last && last.kind === 'assistant' && !last.sealed) last.text += ev.text
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
      s.items.push({
        kind: 'tool',
        id: ev.id,
        name: ev.name,
        done: false,
        error: false,
        input: ev.input,
      })
      break
    case 'tool_call_update': {
      const t = s.items.find((i) => i.kind === 'tool' && i.id === ev.tool_use_id) as
        | Extract<ChatItem, { kind: 'tool' }>
        | undefined
      if (t) {
        t.done = true
        t.error = ev.is_error ?? false
        // 結果本文を保持。in-place 変異なので、開いたままの詳細にライブで流れ込む。
        t.result = ev.content
      } else {
        // 結び先の無い update。backend 側で「replay 列に孤児は現れない」を不変条件にした
        // （transcript の切り詰めが ToolCall/Update のペアを割らない、in-flight tail は
        // ToolCall を二重に持たない）。ここに来たら配送順序のバグなので、黙って捨てず残す。
        console.warn('[chatview] 孤児 tool_call_update（結び先の tool_call が無い）', ev.tool_use_id)
      }
      break
    }
    case 'subagent_message': {
      // 親 tool（Agent）にぶら下げる。親の発話列には決して混ぜない。
      const t = s.items.find((i) => i.kind === 'tool' && i.id === ev.parent_tool_use_id) as
        | Extract<ChatItem, { kind: 'tool' }>
        | undefined
      if (!t) {
        // 親が居ない = backend の隔離漏れ or replay 切り詰めで親が落ちた。孤児 tool_call_update と
        // 同じく、既存 item を壊さず捨てる（最終防衛線）。
        console.warn('[chatview] 親 tool の無い subagent_message', ev.parent_tool_use_id)
        break
      }
      const list = (t.subagent ??= [])
      const last = list[list.length - 1]
      // 連続同 role は 1 節に畳む（thinking が細切れに見えない）。delta ではないので改行で継ぐ。
      if (last && last.role === ev.role) last.text += `\n${ev.text}`
      else list.push({ role: ev.role, text: ev.text })
      break
    }
    case 'plan':
      s.plan = ev.entries
      break
    case 'turn_completed':
      s.streaming = false
      s.cost = ev.cost_usd ?? s.cost
      // 欠落 turn（engine が値を運ばない版）では前値を保つ — ゲージが点滅しないように。
      s.contextTokens = ev.context_tokens ?? s.contextTokens
      s.contextWindow = ev.context_window ?? s.contextWindow
      sealLastAssistant(s) // 次 turn の chunk と融合させない（§5.1）
      break
    case 'error':
      s.streaming = false
      s.replaying = false // replay window 中に error が割り込んでも再同期ローダーを固着させない（streaming と同じ防御）
      sealLastAssistant(s) // error バブルを前 turn と分ける（§5.1）
      s.items.push({ kind: 'assistant', text: `\n\n⚠️ **engine error**: ${ev.message}` })
      break
    case 'engine_exited':
      // engine の休眠（途絶 = 回復可能）。error と違い会話バブルは足さない（休眠は会話本文ではなく
      // ヘッダの 💤 休眠 / status で出す）。streaming / replaying は下ろす（error と同じ防御）。
      s.streaming = false
      s.replaying = false
      sealLastAssistant(s) // 復活後の chunk と前 turn を融合させない（§5.1）
      break
    case 'question':
      // engine が turn を pause して選択を待つ（HITL）。カーソル点滅（streaming）は止める。
      // 回答すると turn が継続し、後続 message_chunk が streaming を立て直す。
      s.streaming = false
      s.items.push({
        kind: 'prompt',
        requestId: ev.request_id,
        questions: ev.questions,
        answered: false,
      })
      break
    case 'permission_request':
      // engine が turn を pause して tool 承認を待つ（HITL）。カーソル点滅を止める。
      s.streaming = false
      s.items.push({
        kind: 'prompt',
        requestId: ev.request_id,
        questions: [],
        answered: false,
        permission: { toolName: ev.tool_name, input: ev.input },
      })
      break
  }
}

function sealLastAssistant(s: ChatState): void {
  const last = s.items[s.items.length - 1]
  if (last && last.kind === 'assistant') last.sealed = true
}

/** EchoesEvent を **その session の** store に畳み込む（console.ts の renderer 本体）。
 *
 *  doc 50 §4.3 #2: 旧実装は `session !== focusedOf(lane)` で背景 session の event を**捨てて**
 *  いた（lane に会話が 1 本しか無い前提）。session ↔ Pane 1:1 では N 本が同時に生きるので、
 *  捨てずに **session ごとの store へ振り分ける**。背景 session の stream が focused の会話に
 *  混ざる心配は、store が別なので構造的に消える（旧 filter が担っていた役割は key が担う）。
 *  session は console.ts で正規化済み（未指定 = focused = 1、旧 SP 互換）。 */
function foldEvent(lane: string, ev: EchoesEvent, session: number): void {
  const lc = laneChat(lane, session)
  lc.set(produce((s) => foldInto(s, ev)))
  lc.set('lastEventAt', Date.now()) // 全イベントで時刻を同期（hang 検出の時間軸）
  // doc 38 §4.3: replay window の watchdog を張り替える。replay_start で arm、replay_end / error で
  // 解除（foldInto は既に replaying を下ろしている — ここは timer の後始末）。10s 無応答なら強制解除。
  if (ev.kind === 'replay_start') armReplayWatchdog(lane, session)
  else if (ev.kind === 'replay_end' || ev.kind === 'error' || ev.kind === 'engine_exited')
    clearReplayWatchdog(lane, session) // engine 途絶 = 続きの replay はもう来ない → watchdog を固着させない
  // doc 35 §5.1: turn が閉じた event を契機に pending を flush。派生状態 streaming===false は見ない
  //（replay_start / question / permission_request も false にするため — それらで流すと順序が壊れる）。
  if (isTurnClosingEvent(ev.kind)) flushPending(lane, session)
}

/** turn が閉じた（= pending flush を発火してよい）event か（doc 35 §5.1、vitest 対象）。
 *  engine_exited も含む（旧 error 相乗り時代の自己修復経路の継承）: pending の submit が
 *  engine respawn のトリガになる = 「メッセージ送信で再開」が type-ahead でも成立する。 */
export function isTurnClosingEvent(kind: EchoesEvent['kind']): boolean {
  return kind === 'turn_completed' || kind === 'error' || kind === 'engine_exited'
}

/** doc 35 §5.1: buffer した type-ahead を engine に流す（対象 = turn を閉じた (lane, session)）。
 *  doc 50 P2: `echoes:submit` が session を運ぶようになったので、background session の
 *  pending もその session 自身へ安全に流せる（旧 focused guard は撤去）。 */
function flushPending(lane: string, session: number): void {
  const lc = laneChat(lane, session)
  const text = lc.state.pending
  if (!text) return
  lc.set(
    produce((s) => {
      s.items.push({ kind: 'user', text })
      s.pending = null
    }),
  )
  const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
  ipc?.postMessage(JSON.stringify({ t: 'echoes:submit', lane, session, prompt: text }))
}

/**
 * 送信待ち type-ahead を composer へ戻せる条件（dequeue-to-composer の MVP ガード）。純粋 = テスト可能。
 *
 * 「編集開始 = キューから取り出して入力欄へ戻す」設計（todo 2026-07-14）:
 * 取り出した時点で pending は空 → ただの下書きに戻るので turn 完了後の flushPending は何も送らない
 *（`if (!text) return`）= 自動送信が起きずレースが消滅する。
 * ただし composer に打ちかけ下書きがある時に戻すと下書きを潰す → MVP は「composer が空のときだけ可」。
 */
export function canDequeuePending(draftText: string, pending: string | null): boolean {
  return draftText.trim() === '' && pending != null && pending !== ''
}

/** 空の ChatState（store 初期値 + テスト用）。 */
export function emptyChatState(): ChatState {
  return {
    header: null,
    items: [],
    plan: [],
    streaming: false,
    cost: null,
    contextTokens: null,
    contextWindow: null,
    permissionMode: undefined,
    pending: null,
    lastEvent: null,
    lastEventAt: null,
    replaying: false,
  }
}

/** active lane の **focused session** が transcript replay（再同期）中か。
 *  resync-loader の可視条件（reactive）。ローダーは lane 全面に出る 1 枚なので、
 *  代表として focused を見る（背景 session の replay で全面を覆わない）。 */
export function activeLaneReplaying(): boolean {
  const l = activeLane()
  return l ? laneChat(l, focusedOf(l)).state.replaying : false
}

// ---------------------------------------------------------------------------
// doc 38 Phase 3 — session tab strip の純粋ロジック（document 非依存 = vitest 対象）
// ---------------------------------------------------------------------------


/** doc 38 Phase 3 → doc 39: session tab の × を出してよいか。純粋 = テスト可能。
 *  - 2 本以上でのみ close 可（1 本 = 素に戻すのは Reset lane の役目）
 *  - root タブは隠す（backend も root の remove を Err で拒否する — doc 39 §6。隠さないと
 *    「クリックしたのに閉じない」無言 no-op になる）。旧 SP は root を送らない（undefined）→
 *    従来挙動（本数のみ）に倒す。 */
export function canCloseSession(sessionCount: number, isRoot?: boolean): boolean {
  return sessionCount >= 2 && isRoot !== true
}

// ---------------------------------------------------------------------------
// agent status 導出（doc 35 §5.1 診断用の常時可視化ブロック）— 純粋関数 = テスト可能
// ---------------------------------------------------------------------------

export type EchoesStatus = {
  kind: 'idle' | 'streaming' | 'thinking' | 'tool' | 'awaiting' | 'error'
  label: string
  detail?: string
  pending: boolean // 送信待ち type-ahead を抱えているか（待機中 + pending = flush 失敗の兆候）
  lastEvent?: string // 最後に受けたイベント種別（細かく追う用）
  idleSec?: number // 最終イベントからの経過秒
  stalled: boolean // streaming なのに一定時間イベントが来ない = engine hang の兆候（応答中の嘘を暴く）
}

/** イベントが来なくなってから「無反応」と見なすまでの猶予 ms。 */
const STALL_MS = 8000

/** ChatState から現在の agent 状態を導く（純粋、nowMs は呼び手が渡す＝テスト可能）。 */
export function deriveStatus(s: ChatState | null, nowMs = 0): EchoesStatus {
  if (!s) return { kind: 'idle', label: '—', pending: false, stalled: false }
  const pending = !!s.pending
  const lastEvent = s.lastEvent ?? undefined
  const idleSec =
    s.lastEventAt != null && nowMs > 0 ? Math.max(0, Math.round((nowMs - s.lastEventAt) / 1000)) : undefined
  const base = { pending, lastEvent, idleSec }
  // 未回答の HITL prompt（質問 / 承認）が最優先 = ユーザーにボールがある。
  const waiting = s.items.find((i) => i.kind === 'prompt' && !i.answered) as
    | Extract<ChatItem, { kind: 'prompt' }>
    | undefined
  if (waiting)
    return { ...base, kind: 'awaiting', label: waiting.permission ? '承認待ち' : '質問待ち', stalled: false }
  // streaming なのに最終イベントから STALL 超過 = engine hang を正直に出す（応答中を鵜呑みにしない）。
  const stalled = s.streaming && s.lastEventAt != null && nowMs > 0 && nowMs - s.lastEventAt >= STALL_MS
  const last = s.items[s.items.length - 1]
  if (s.streaming) {
    if (last?.kind === 'thinking') return { ...base, kind: 'thinking', label: '考え中…', stalled }
    if (last?.kind === 'tool' && !last.done) return { ...base, kind: 'tool', label: '実行中', detail: last.name, stalled }
    return { ...base, kind: 'streaming', label: '応答中…', stalled }
  }
  // engine の休眠（途絶 = 回復可能）は idle 扱いで「💤 休眠」と穏当に出す（error とは別）。
  if (lastEvent === 'engine_exited') return { ...base, kind: 'idle', label: '💤 休眠', stalled: false }
  // 本物の engine 異常（turn crash / 翻訳失敗）は「エラー」と正直に出す。
  if (lastEvent === 'error') return { ...base, kind: 'error', label: 'エラー', stalled: false }
  return { ...base, kind: 'idle', label: '待機中', stalled: false }
}

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------

function mdToHtml(text: string): string {
  // breaks: true で単一改行を <br> に変換する。marked 既定（CommonMark）は段落内の単一 \n を
  // 空白に潰すため、engine が返す改行が Act II のチャット表示で消えていた。gfm は既定 true だが明示。
  return marked.parse(text, { breaks: true, gfm: true }) as string
}

/**
 * chat メッセージ内リンクを OS ブラウザで開くための `open-url` IPC ペイロード判定（純関数 = calc）。
 *
 * http(s) の href なら Act I の xterm と同じ `open-url` IPC の JSON 文字列を返し、それ以外
 * （相対 / `file:` / `javascript:` / 空）は null を返す。非 http(s) を絶対に通さない一次弾き —
 * webview に `file://` 等を開かせないための多層防御（scheme 検証の SSOT は Rust 側 terminal.rs、
 * ここは webview 内遷移を止めるための前段）。terminal.rs と揃えて小文字 scheme を前方一致で見る。
 */
export function linkOpenPayload(href: string): string | null {
  if (!href.startsWith('http://') && !href.startsWith('https://')) return null
  return JSON.stringify({ t: 'open-url', url: href })
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

/** tool 詳細（input/result）を DOM に流し込む上限。超過分は「省略」を明示する。 */
const TOOL_DETAIL_MAX = 4000

/**
 * tool の input を表示用テキストへ整形する純関数。
 *
 * input は tool ごとに形が違う生 JSON（Bash なら `{command,description}`）なので素直に
 * pretty JSON にする。空（`{}` / `[]` / null / undefined / 空文字）は「詳細なし」= null。
 * 呼び出し側はこれを見て caret を出すかを決める（開いても空、を作らないため）。
 */
export function formatToolInput(input: unknown): string | null {
  if (input === undefined || input === null) return null
  if (typeof input === 'string') return input.length > 0 ? input : null
  try {
    const s = JSON.stringify(input, null, 2)
    if (!s || s === '{}' || s === '[]') return null
    return s
  } catch {
    // 循環参照など JSON 化できない入力でも落とさない（詳細は best-effort）。
    return String(input)
  }
}

/** tool の result を表示用テキストへ。空文字は「詳細なし」= null。 */
export function formatToolResult(result: string | undefined): string | null {
  if (result === undefined || result === null) return null
  return result.length > 0 ? result : null
}

/**
 * 巨大 detail の clamp（純関数）。省略した文字数を返すので、UI は「黙って切った」ように
 * 見せずに済む（no silent truncation — 切ったなら切ったと言う）。
 */
export function clampToolDetail(
  text: string,
  max = TOOL_DETAIL_MAX,
): { text: string; omitted: number } {
  if (text.length <= max) return { text, omitted: 0 }
  return { text: text.slice(0, max), omitted: text.length - max }
}

/** tool detail の 1 節（input / result）。長文は clamp し、省略数を明示する。 */
function ToolDetail(props: { label: string; text: string }) {
  const clamped = createMemo(() => clampToolDetail(props.text))
  return (
    <div class="echoes-tool-detail">
      <div class="echoes-tool-detail-label">{props.label}</div>
      <pre class="echoes-tool-detail-body">{clamped().text}</pre>
      <Show when={clamped().omitted > 0}>
        <div class="echoes-tool-detail-omitted">…{clamped().omitted} 文字省略</div>
      </Show>
    </div>
  )
}

/** subagent（Agent の子）の発話列。role ごとにラベルを付けて縦に積む。 */
function SubagentBlock(props: { entries: SubagentEntry[] }) {
  return (
    <div class="echoes-tool-detail">
      <div class="echoes-tool-detail-label">subagent</div>
      <For each={props.entries}>
        {(e) => {
          const clamped = createMemo(() => clampToolDetail(e.text))
          return (
            <div class="echoes-subagent-entry" classList={{ [e.role]: true }}>
              <span class="echoes-subagent-role">{e.role}</span>
              <pre class="echoes-tool-detail-body">{clamped().text}</pre>
              <Show when={clamped().omitted > 0}>
                <div class="echoes-tool-detail-omitted">…{clamped().omitted} 文字省略</div>
              </Show>
            </div>
          )
        }}
      </For>
    </div>
  )
}

/**
 * tool 1 件の行。詳細（input/result/subagent）があれば開閉できる accordion になる。
 *
 * 単発（single）でも group（ToolGroupRow）の中の 1 件でも同じ形 — 「まとめて見る」と
 * 「個別に掘る」を両立させる要。詳細が無い tool は caret を出さず従来どおりの 1 行。
 * result は tool_call_update で後から in-place に入るので、開いたままでも流れ込む。
 */
function ToolRow(props: {
  name: string
  done: boolean
  error: boolean
  input?: unknown
  result?: string
  subagent?: SubagentEntry[]
}) {
  const [open, setOpen] = createSignal(false)
  const inputText = createMemo(() => formatToolInput(props.input))
  const resultText = createMemo(() => formatToolResult(props.result))
  const subagent = createMemo(() => props.subagent ?? [])
  const hasDetail = createMemo(
    () => inputText() !== null || resultText() !== null || subagent().length > 0,
  )
  return (
    <div class="echoes-tool" classList={{ done: props.done, error: props.error }}>
      <button
        class="echoes-tool-head"
        classList={{ clickable: hasDetail() }}
        onClick={() => hasDetail() && setOpen(!open())}
      >
        <Show when={hasDetail()}>
          <span class="echoes-thinking-caret" classList={{ open: open() }}>
            ▸
          </span>
        </Show>
        <span class="echoes-tool-spinner" />
        <span class="echoes-tool-icon">🔧</span>
        <span class="echoes-tool-name">{props.name}</span>
        <span class="echoes-tool-status">
          {props.error ? 'error' : props.done ? '✓' : '実行中…'}
        </span>
      </button>
      <Show when={open() && hasDetail()}>
        <div class="echoes-tool-body">
          <Show when={inputText()}>{(t) => <ToolDetail label="input" text={t()} />}</Show>
          {/* subagent は Agent 行の中に入れ子で置く = 「誰の発話か」を構造で示す。 */}
          <Show when={subagent().length > 0}>
            <SubagentBlock entries={subagent()} />
          </Show>
          <Show when={resultText()}>{(t) => <ToolDetail label="result" text={t()} />}</Show>
        </div>
      </Show>
    </div>
  )
}

/**
 * 連続同名 tool run（Agent ×N 等）を 1 行に畳む accordion。ThinkingBlock と同じ開閉 UI。
 * 既定は畳んだ状態: header が「🔧 {name} ×{count} {status}」で進捗を要約する。in-flight 中は
 * spinner + 完了数「{done}/{count}」を出し（畳んだままでも何本終わったかが分かる）、全 tool が
 * 終わると ✓（1 件でも error なら error）に変わる。展開で個別 ToolRow を並べる。
 * props.tools は reactive accessor（run は末尾に伸び、各 tool の done/error も後から変異する）。
 */
function ToolGroupRow(props: { name: string; tools: Accessor<ToolItem[]> }) {
  const [open, setOpen] = createSignal(false)
  const count = () => props.tools().length
  const status = () => toolGroupStatus(props.tools())
  const anyError = () => props.tools().some((t) => t.error)
  return (
    <div
      class="echoes-toolgroup"
      classList={{ done: !status().running && !anyError(), error: !status().running && anyError() }}
    >
      <button class="echoes-toolgroup-toggle" onClick={() => setOpen(!open())}>
        <span class="echoes-thinking-caret" classList={{ open: open() }}>
          ▸
        </span>
        <Show when={status().running}>
          <span class="echoes-tool-spinner" />
        </Show>
        <span class="echoes-tool-icon">🔧</span>
        <span class="echoes-tool-name">{props.name}</span>
        <span class="echoes-toolgroup-count">×{count()}</span>
        <span class="echoes-tool-status">{status().label}</span>
      </button>
      <Show when={open()}>
        <div class="echoes-toolgroup-body">
          <For each={props.tools()}>
            {(t) => (
              <ToolRow
                name={t.name}
                done={t.done}
                error={t.error}
                input={t.input}
                result={t.result}
                subagent={t.subagent}
              />
            )}
          </For>
        </div>
      </Show>
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

/**
 * PromptCard（doc 35 §4）— HITL 質問（AskUserQuestion 横取り）の選択肢 UI。
 *
 * 各 question を見出し + 選択肢ボタンで描く。single-select は radio（クリックで置換）、
 * multiSelect は toggle（複数選択）。全質問に選択が付いたら「確定」で `answers` を組んで
 * onAnswer に渡す（親が echoes:respond を送り、カードを回答済み表示へ折りたたむ）。
 */
function PromptCard(props: {
  item: Extract<ChatItem, { kind: 'prompt' }>
  onAnswer: (requestId: string, answers: Record<string, string>) => void
}) {
  // 各質問の選択（label 配列）。single は 1 要素、multi は複数。
  const [sel, setSel] = createSignal<Record<string, string[]>>({})

  const toggle = (q: QuestionSpec, label: string) => {
    setSel((prev) => {
      const cur = prev[q.question] ?? []
      const next = q.multi_select
        ? cur.includes(label)
          ? cur.filter((l) => l !== label)
          : [...cur, label]
        : [label] // single-select = 置換
      return { ...prev, [q.question]: next }
    })
  }

  const isSelected = (q: QuestionSpec, label: string): boolean =>
    (sel()[q.question] ?? []).includes(label)

  // 全質問が 1 つ以上選択済みなら確定可。
  const canConfirm = (): boolean =>
    props.item.questions.every((q) => (sel()[q.question] ?? []).length > 0)

  const confirm = () => {
    if (!canConfirm()) return
    const answers: Record<string, string> = {}
    for (const q of props.item.questions) {
      const labels = sel()[q.question] ?? []
      // multiSelect の回答 wire 形は未確定（doc §8 未決点）。保守的に ", " 結合の単一 string。
      answers[q.question] = q.multi_select ? labels.join(', ') : labels[0]
    }
    props.onAnswer(props.item.requestId, answers)
  }

  return (
    <div class="echoes-prompt" classList={{ answered: props.item.answered }}>
      <Show
        when={!props.item.answered}
        fallback={
          <div class="echoes-prompt-answered">
            <For each={props.item.questions}>
              {(q) => (
                <div class="echoes-prompt-arow">
                  <span class="echoes-prompt-ahead">{q.header}</span>
                  <span class="echoes-prompt-aval">{props.item.answers?.[q.question] ?? ''}</span>
                </div>
              )}
            </For>
          </div>
        }
      >
        <For each={props.item.questions}>
          {(q) => (
            <div class="echoes-prompt-q">
              <div class="echoes-prompt-header">{q.header}</div>
              <div class="echoes-prompt-question">{q.question}</div>
              <div class="echoes-prompt-options">
                <For each={q.options}>
                  {(opt) => (
                    <button
                      class="echoes-prompt-opt"
                      classList={{ selected: isSelected(q, opt.label) }}
                      onClick={() => toggle(q, opt.label)}
                      title={opt.description}
                    >
                      {opt.label}
                    </button>
                  )}
                </For>
              </div>
            </div>
          )}
        </For>
        <button class="echoes-prompt-confirm" disabled={!canConfirm()} onClick={confirm}>
          確定
        </button>
      </Show>
    </div>
  )
}

/** doc 35 §4 / PR3: tool 承認の PromptCard（allow/deny）。permission-mode=default 時の can_use_tool。 */
function PermissionCard(props: {
  item: Extract<ChatItem, { kind: 'prompt' }>
  onDecide: (requestId: string, behavior: 'allow' | 'deny') => void
}) {
  const perm = () => props.item.permission!
  const inputSummary = (): string => {
    try {
      const s = JSON.stringify(perm().input)
      return s.length > 240 ? s.slice(0, 240) + '…' : s
    } catch {
      return ''
    }
  }
  return (
    <div class="echoes-prompt" classList={{ answered: props.item.answered }}>
      <Show
        when={!props.item.answered}
        fallback={
          <div class="echoes-prompt-answered">
            <span class="echoes-prompt-ahead">{perm().toolName}</span>
            <span class="echoes-prompt-aval">
              {props.item.decision === 'deny' ? '✗ 却下' : '✓ 許可'}
            </span>
          </div>
        }
      >
        <div class="echoes-prompt-header">tool 承認</div>
        <div class="echoes-prompt-question">
          <code class="echoes-perm-tool">{perm().toolName}</code> の実行を許可しますか？
        </div>
        <div class="echoes-perm-input">{inputSummary()}</div>
        <div class="echoes-perm-actions">
          <button
            class="echoes-perm-allow"
            onClick={() => props.onDecide(props.item.requestId, 'allow')}
          >
            許可
          </button>
          <button
            class="echoes-perm-deny"
            onClick={() => props.onDecide(props.item.requestId, 'deny')}
          >
            却下
          </button>
        </div>
      </Show>
    </div>
  )
}

/** doc 35 §4 / PR4: plan 承認カード。ExitPlanMode の can_use_tool を plan 本文 + 承認/却下 で描く。 */
function PlanCard(props: {
  item: Extract<ChatItem, { kind: 'prompt' }>
  onDecide: (requestId: string, behavior: 'allow' | 'deny') => void
}) {
  // ExitPlanMode input は `{plan: markdown}`（Claude tool schema）。無ければ raw を出す（robust）。
  const planText = (): string => {
    const input = props.item.permission?.input as { plan?: unknown } | undefined
    if (input && typeof input.plan === 'string') return input.plan
    try {
      return '```json\n' + JSON.stringify(input, null, 2) + '\n```'
    } catch {
      return ''
    }
  }
  return (
    <div class="echoes-prompt echoes-plan-card" classList={{ answered: props.item.answered }}>
      <Show
        when={!props.item.answered}
        fallback={
          <div class="echoes-prompt-answered">
            <span class="echoes-prompt-ahead">plan</span>
            <span class="echoes-prompt-aval">
              {props.item.decision === 'deny' ? '✗ 却下' : '✓ 承認'}
            </span>
          </div>
        }
      >
        <div class="echoes-prompt-header">plan 承認</div>
        <div class="echoes-plan-body" innerHTML={mdToHtml(planText())} />
        <div class="echoes-perm-actions">
          <button
            class="echoes-perm-allow"
            onClick={() => props.onDecide(props.item.requestId, 'allow')}
          >
            承認して実行
          </button>
          <button
            class="echoes-perm-deny"
            onClick={() => props.onDecide(props.item.requestId, 'deny')}
          >
            却下
          </button>
        </div>
      </Show>
    </div>
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

/** 1 枚 = 1 session の chat pane（doc 46 §1.5 session ↔ Pane 1:1）。(lane, session) は mount 時に
 *  固定 — lane 切替は pane host ごと作り直す（lane-panes が dispose → mount）。
 *  doc 50 P2: chat 動詞（submit / respond / perm / interrupt）は session を運ぶ = どの pane
 *  からも打てる。例外は model 切替のみ — SP 側 console_set_model が root slot 単位（engine の
 *  --resume 込み respawn）のため、focused でだけ有効にしている。 */
function SessionChatView(props: { lane: string; session: number }) {
  const lc = laneChat(props.lane, props.session)
  const state = (): ChatState => lc.state
  /** この pane が lane の focused session か（= chat 動詞の宛先か）。 */
  const isFocused = (): boolean => (sessionsOf(props.lane)?.focused ?? 1) === props.session
  /** この pane の session の registry entry（label / live / root 表示用）。 */
  const sessionInfo = (): EchoesSession | undefined =>
    sessionsOf(props.lane)?.sessions.find((v) => v.key === props.session)
  const sessionLabel = (): string =>
    `${sessionChipPrefix(sessionInfo()?.stand)}#${props.session}`

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
    const lane = props.lane
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({ t: 'console:set_model', lane, model: model || null }),
    )
  }

  // doc 35 PR3: permission mode（tool 承認の opt-in）。spawn 既定は bypassPermissions（素通し）。
  // "default" に切替えると Write/Bash 等が承認要求（PermissionRequest）経由になる。
  // doc 35 PR3/PR4: permission mode は per-lane（engine の真値 = session_init.permission_mode）。
  // review #2: 旧実装はグローバル signal で lane 横断共有 + respawn の bypass reset を映さなかった。
  const currentPermMode = (): string => state()?.permissionMode ?? 'bypassPermissions'
  const setPermissionMode = (mode: string) => {
    const lane = props.lane
    // optimistic: 当該 lane に即反映。engine は set_permission_mode を適用し、respawn 時は
    // session_init.permission_mode が真値（通常 bypassPermissions）で上書きする。
    //（旧: notePermissionMode でヘッダ chip にも同期していたが、chip は doc 50 の名札純化で
    //  撤去済み — 同期先ごと消えた）
    lc.set(produce((s) => (s.permissionMode = mode)))
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({ t: 'echoes:set_permission_mode', lane, session: props.session, mode }),
    )
  }

  // context ゲージ（Act I statusline の bar :context 相当）。分子分母が揃うまで非表示。
  // 閾値は cc-status の意味論を踏襲: >=60% warn / >=85% critical。
  const ctxPct = (): number | null => {
    const s = state()
    if (!s || s.contextTokens == null || !s.contextWindow) return null
    return Math.min(100, Math.round((s.contextTokens / s.contextWindow) * 100))
  }
  const ctxTitle = (): string => {
    const s = state()
    if (!s || s.contextTokens == null || !s.contextWindow) return ''
    return `context ${s.contextTokens.toLocaleString()} / ${s.contextWindow.toLocaleString()} tokens`
  }

  const [draft, setDraft] = createSignal('')
  let inputRef: HTMLTextAreaElement | undefined // dequeue 後に composer へフォーカスを移すため
  // history 最下部の常時 status バー。全イベント同期 + 無反応(hang)検出のため 1s 毎に now を更新。
  const [nowMs, setNowMs] = createSignal(Date.now())
  onMount(() => {
    const id = setInterval(() => setNowMs(Date.now()), 1000)
    onCleanup(() => clearInterval(id))
  })
  const statusLine = () => deriveStatus(state(), nowMs())
  const submit = () => {
    const lane = props.lane
    const text = draft().trim()
    if (!text) return
    setDraft('')
    if (inputRef) autosize(inputRef) // 送信後は 1 行に畳み戻す
    // doc 35 §5.1: streaming 中は engine へ送らず pending に buffer（items[] を触らない = 順序を汚さない）。
    // 走行中の複数送信は改行で連結し、単一 draft = 1 turn として turn 閉時に flush する。
    if (lc.state.streaming) {
      lc.set('pending', (p) => (p ? `${p}\n${text}` : text))
      return
    }
    // idle: 送信順 = 処理順なので optimistic に即描画して送る
    lc.set(produce((s) => s.items.push({ kind: 'user', text })))
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({ t: 'echoes:submit', lane, session: props.session, prompt: text }),
    )
  }

  // 送信待ち type-ahead を入力欄へ戻して編集可能にする（dequeue-to-composer, todo 2026-07-14）。
  // composer が空のときだけ有効（下書きを潰さない）。戻した瞬間 pending は空 = 自動送信されない。
  const canEditPending = () => canDequeuePending(draft(), state().pending ?? null)
  const editPending = () => {
    if (!canDequeuePending(draft(), lc.state.pending)) return
    const text = lc.state.pending as string
    lc.set('pending', null) // 先に空にする → 以降の turn_completed が flush しない（レース消滅）
    setDraft(text)
    queueMicrotask(() => inputRef?.focus()) // すぐ編集できるよう composer にカーソルを移す
  }

  // doc 35 §5: 実行中 turn を中断する（停止ボタン / Esc）。engine は turn を止め、次の submit を受けられる。
  const interrupt = () => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({ t: 'echoes:interrupt', lane: props.lane, session: props.session }),
    )
  }

  // doc 35 PR1: PromptCard 回答。カードを回答済み表示へ折りたたみ、echoes:respond で SP に戻す
  //（host が control_response を stdin に書いて turn が継続する）。
  const answerPrompt = (requestId: string, answers: Record<string, string>) => {
    const lane = props.lane
    lc.set(
      produce((s) => {
        const it = s.items.find((i) => i.kind === 'prompt' && i.requestId === requestId)
        if (it && it.kind === 'prompt') {
          it.answered = true
          it.answers = answers
        }
      }),
    )
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({
        t: 'echoes:respond', lane, session: props.session, request_id: requestId, answers,
      }),
    )
  }

  // doc 35 PR3: permission 承認/却下。カードを decision 表示へ折りたたみ、echoes:respond {behavior} で戻す。
  const decidePrompt = (requestId: string, behavior: 'allow' | 'deny') => {
    const lane = props.lane
    lc.set(
      produce((s) => {
        const it = s.items.find((i) => i.kind === 'prompt' && i.requestId === requestId)
        if (it && it.kind === 'prompt') {
          it.answered = true
          it.decision = behavior
        }
      }),
    )
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({
        t: 'echoes:respond', lane, session: props.session, request_id: requestId, behavior,
      }),
    )
  }

  // doc 35 PR4: plan 承認/却下。承認 = ExitPlanMode を allow + mode を default へ戻す（plan mode を
  // 抜けて承認フローで実行に移る）。却下 = deny（plan に留まる or 再計画）。
  const decidePlan = (requestId: string, behavior: 'allow' | 'deny') => {
    decidePrompt(requestId, behavior)
    if (behavior === 'allow') setPermissionMode('default')
  }

  // --- auto-scroll（sticky bottom）+ キーボードスクロール（Home/End/PgUp/PgDn）---------
  // stream 要素の ref。<Show> 内なので lane 選択時のみ存在する。
  let streamEl: HTMLDivElement | undefined
  // ユーザーが最下部に貼り付いているか。history を遡って読んでいる間は追従を止める
  // （chat の定石: 下端にいる時だけ新着で追う。上にスクロールしたら勝手に引き戻さない）。
  let stuckToBottom = true
  const BOTTOM_EPS = 48 // 「最下部」と見なす許容 px（行の途中でも追従を継続させる遊び）
  const isAtBottom = (el: HTMLElement): boolean =>
    el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_EPS

  // scroll のたびに「下端張り付き」を測り直す。プログラム的 scroll でも発火するが、
  // 最下部へ動かした直後は isAtBottom=true に収束するので振動しない。
  const onStreamScroll = (): void => {
    if (streamEl) stuckToBottom = isAtBottom(streamEl)
  }

  // marked 描画済み HTML 内の <a> クリックを echoes-stream の 1 listener で捌く（イベント委譲 =
  // メッセージ毎に listener を張らない）。default では webview 内遷移（SPA が localhost リンクで
  // 飛ぶ事故）になるので preventDefault で止め、http(s) は Act I の xterm と同じ `open-url` IPC で
  // OS default browser を起動する（Rust: terminal::handle_ipc_message → webbrowser::open）。
  const onStreamLinkClick = (e: MouseEvent): void => {
    const anchor = (e.target as HTMLElement | null)?.closest?.('a') as HTMLAnchorElement | null
    if (!anchor) return
    // webview 内遷移を常に抑止（相対 / anchor リンクでも SPA document を飛ばさない）。
    e.preventDefault()
    // 生の href（getAttribute）で scheme 判定 — .href プロパティは相対を vp-asset:// に解決して濁る。
    const payload = linkOpenPayload(anchor.getAttribute('href') ?? '')
    if (!payload) return
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(payload)
  }

  // 4 キーで history を scroll する実体。対象キー以外は false（呼び側が素通し判定に使う）。
  const scrollByKey = (key: string): boolean => {
    const el = streamEl
    if (!el) return false
    const page = el.clientHeight * 0.9 // 1 画面弱（10% 重ねて文脈を維持）
    switch (key) {
      case 'Home':
        el.scrollTop = 0
        break
      case 'End':
        el.scrollTop = el.scrollHeight
        break
      case 'PageUp':
        el.scrollTop -= page
        break
      case 'PageDown':
        el.scrollTop += page
        break
      default:
        return false
    }
    stuckToBottom = isAtBottom(el)
    return true
  }

  // pane-level（document）keydown: 入力欄で作文しながらでも history を scroll できるように
  // する（ユーザ要望「後者」）。keydown は focus 要素で発火し document へ bubble するので、
  // textarea 入力中でも bubble を document で受ければ拾える（stream 局所ハンドラでは届かない）。
  // テキスト編集を壊さない棲み分け:
  //   - PageUp/PageDown: 常に history へ（小さな textarea では page 移動は無意味なので奪う）
  //   - Home/End: textarea に focus がある間は行内キャレット移動を尊重して奪わない。
  //     それ以外（history/pane に focus）では history 先頭/末尾へ。
  // chat 非表示時（Act I 表示中 = streamEl が display:none 配下 → offsetParent=null）は
  // 一切介入せず、xterm 等にキーを渡す。
  const onDocKey = (e: KeyboardEvent): void => {
    if (!streamEl || streamEl.offsetParent === null) return // chat 非表示 → 素通し
    const key = e.key
    // doc 35 §5: Esc で走行中 turn を中断（作文中の textarea では抑制 = Home/End と同じ棲み分け）。
    if (key === 'Escape') {
      const inTextarea = document.activeElement?.classList.contains('echoes-input-box') ?? false
      if (!inTextarea && state()?.streaming) {
        interrupt()
        e.preventDefault()
      }
      return
    }
    if (key !== 'Home' && key !== 'End' && key !== 'PageUp' && key !== 'PageDown') return
    const inTextarea = document.activeElement?.classList.contains('echoes-input-box') ?? false
    if ((key === 'Home' || key === 'End') && inTextarea) return // 作文中の caret 移動を尊重
    if (scrollByKey(key)) e.preventDefault()
  }
  onMount(() => {
    document.addEventListener('keydown', onDocKey)
    onCleanup(() => document.removeEventListener('keydown', onDocKey))
  })

  // 新着で下端に追従（sticky）。streaming の chunk 追記（末尾 item の text 伸長 = items.length は
  // 不変）にも反応させるため、length だけでなく末尾 text 長も reactive に読む。rev の read 自体が
  // Solid の依存追跡を張る（値は「内容が動いた」印としてのみ使う）。
  createEffect(() => {
    const s = state()
    if (!s) return
    const last = s.items[s.items.length - 1]
    const rev = s.items.length + (last && 'text' in last ? last.text.length : 0)
    if (rev >= 0 && stuckToBottom) {
      // DOM patch 後の実寸で scroll するため rAF に載せる（markdown/font の layout 確定待ち）。
      requestAnimationFrame(() => {
        if (streamEl && stuckToBottom) streamEl.scrollTop = streamEl.scrollHeight
      })
    }
  })

  // mount 時は「この session の最新（下端）」を見せる（履歴の途中で開かない）。
  onMount(() => {
    stuckToBottom = true
    requestAnimationFrame(() => {
      if (streamEl) streamEl.scrollTop = streamEl.scrollHeight
    })
  })

  return (
    <div
      class="echoes-chat"
      classList={{ focused: isFocused() }}
      onClick={() => {
        if (!isFocused()) focusChatSession(props.lane, props.session)
      }}
    >
      {/* session 名札（pane 上端）: この pane = この session の素性。doc 46 §1.3 の帰結で
          タブ strip は撤去 — session の識別は pane 自身が名乗り、切替は pane click が担う。
          engine 選択付きの新規作成は #pane-tabs の「+ New」一本（doc 46 P2 の canonical 入口）。 */}
      <div class="echoes-session-plate" classList={{ focused: isFocused() }}>
        <Show when={sessionInfo()?.live}>
          <span class="echoes-tab-dot" />
        </Show>
        <span class="echoes-session-plate-label">{sessionLabel()}</span>
        <Show when={sessionInfo()?.engine_session_id}>
          {(sid) => <span class="echoes-session-plate-sid">{sid().slice(0, 8)}</span>}
        </Show>
        <Show when={!isFocused()}>
          {/* focus は「Act toggle の対象 / replay demand の宛先」— 送信はどの pane からも可 */}
          <span class="echoes-session-plate-hint">click で focus</span>
        </Show>
        <span class="echoes-session-plate-spacer" />
        <Show
          when={canCloseSession(sessionsOf(props.lane)?.sessions.length ?? 0, sessionInfo()?.root)}
        >
          <button
            type="button"
            class="echoes-session-plate-close"
            title="この session を閉じる（pane ごと消える）"
            onClick={(e) => {
              e.stopPropagation()
              removeChatSession(props.lane, props.session)
            }}
          >
            <CreoIcon name="ph:x" size={9} />
          </button>
        </Show>
      </div>
              <PlanWidget entries={() => state().plan} />
        <div
          class="echoes-stream"
          ref={streamEl}
          tabindex={0}
          onScroll={onStreamScroll}
          onClick={onStreamLinkClick}
        >
          <For each={state().items}>
            {(item, index) => {
              if (item.kind === 'thinking')
                return (
                  <ThinkingBlock
                    text={item.text}
                    // 末尾 thinking かつ turn 進行中 = 今まさに考え中 → shimmer。
                    active={() => index() === state().items.length - 1 && state().streaming}
                  />
                )
              if (item.kind === 'tool') {
                // 連続同名 tool run を畳む。head=accordion / member=非描画 / single=従来 1 行。
                // items/index を reactive に読むので stream 追記で single→head へ昇格する。
                const role = createMemo(() => classifyToolRun(state().items, index()))
                return (
                  <Switch>
                    <Match when={role().role === 'member'}>{null}</Match>
                    <Match when={role().role === 'head'}>
                      <ToolGroupRow
                        name={item.name}
                        tools={() => (role() as { role: 'head'; run: ToolItem[] }).run}
                      />
                    </Match>
                    <Match when={true}>
                      <ToolRow
                        name={item.name}
                        done={item.done}
                        error={item.error}
                        input={item.input}
                        result={item.result}
                        subagent={item.subagent}
                      />
                    </Match>
                  </Switch>
                )
              }
              if (item.kind === 'prompt') {
                if (!item.permission) return <PromptCard item={item} onAnswer={answerPrompt} />
                return item.permission.toolName === 'ExitPlanMode' ? (
                  <PlanCard item={item} onDecide={decidePlan} />
                ) : (
                  <PermissionCard item={item} onDecide={decidePrompt} />
                )
              }
              return (
                <div class="echoes-msg" classList={{ user: item.kind === 'user' }}>
                  <div class="echoes-msg-body" innerHTML={mdToHtml(item.text)} />
                </div>
              )
            }}
          </For>
          <Show when={state().streaming}>
            <div class="echoes-cursor" />
          </Show>
          <Show when={state().pending}>
            <div
              class="echoes-msg user pending"
              classList={{ editable: canEditPending(), locked: !canEditPending() }}
              onClick={editPending}
              title={
                canEditPending()
                  ? 'クリックで入力欄に戻して編集'
                  : '編集するには入力欄を空にしてください'
              }
            >
              <div class="echoes-msg-body" innerHTML={mdToHtml(state().pending!)} />
              <span class="echoes-pending-badge">
                {canEditPending() ? '送信待ち · クリックで編集' : '送信待ち · turn 完了後に送信'}
              </span>
            </div>
          </Show>
        </div>
        {/* status bar — **入力の上**（stream に隣接）。engine が今何をしているかの読み取り専用の
            計器で、操作は持たない。context 残量も「読み取り」なのでここ。 */}
        <div
          class={`echoes-status s-${statusLine().kind}`}
          classList={{ stalled: statusLine().stalled }}
        >
          <span class="echoes-status-dot" />
          <span class="echoes-status-label">{statusLine().label}</span>
          <Show when={statusLine().detail}>
            <span class="echoes-status-detail">{statusLine().detail}</span>
          </Show>
          <Show when={statusLine().stalled}>
            <span class="echoes-status-stalled">反応無 {statusLine().idleSec}s</span>
          </Show>
          <Show when={statusLine().lastEvent}>
            <span class="echoes-status-event">· {statusLine().lastEvent}</span>
          </Show>
          <Show when={statusLine().pending}>
            <span class="echoes-status-pending">
              <CreoIcon name="ph:pencil-simple" size={11} /> 送信待ち
            </span>
          </Show>
          <Show when={ctxPct() !== null}>
            <span
              class="echoes-context"
              classList={{ warn: ctxPct()! >= 60, crit: ctxPct()! >= 85 }}
              title={ctxTitle()}
            >
              <span class="echoes-context-bar">
                <span class="echoes-context-fill" style={{ width: `${ctxPct()}%` }} />
              </span>
              <span class="echoes-context-pct">{ctxPct()}%</span>
            </span>
          </Show>
        </div>
        {/* composer — 入力とその操作を 1 つの器にまとめる。上 = 打つ場所、下 = 操作。
            model / permission も「送る前に決める操作」なのでここ（読み取りの status とは分ける）。 */}
        <div class="echoes-composer">
          {/* 既定は **1 行**。打った分だけ scrollHeight に合わせて伸び、max-height で頭打ち
              （CSS だけでは textarea は内容に追随しないので、伸縮はここで行う）。 */}
          <textarea
            ref={inputRef}
            class="echoes-input-box"
            rows={1}
            placeholder="メッセージを入力（⌘Enter で送信）"
            value={draft()}
            onInput={(e) => {
              setDraft(e.currentTarget.value)
              autosize(e.currentTarget)
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault()
                submit()
              }
            }}
          />
          <div class="echoes-actions">
            <select
              class="echoes-model-select"
              disabled={state().streaming || !isFocused()}
              title={
                isFocused()
                  ? 'model'
                  : 'model 切替は root slot 単位（focus してから）'
              }
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
            <select
              class="echoes-model-select"
              title="permission mode"
              onChange={(e) => setPermissionMode(e.currentTarget.value)}
            >
              <option
                value="bypassPermissions"
                selected={currentPermMode() === 'bypassPermissions'}
              >
                素通し
              </option>
              <option value="default" selected={currentPermMode() === 'default'}>
                承認
              </option>
              <option value="plan" selected={currentPermMode() === 'plan'}>
                計画
              </option>
            </select>
            <div class="echoes-actions-spacer" />
            <Show when={state().streaming}>
              <button class="echoes-stop" onClick={interrupt} title="turn を中断 (Esc)">
                <CreoIcon name="ph:stop" size={11} /> 停止
              </button>
            </Show>
            <button class="echoes-send" onClick={submit} disabled={!draft().trim()}>
              <CreoIcon name="ph:paper-plane-right" size={12} /> 送信
            </button>
          </div>
        </div>
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
  font-family: var(--vp-font-sans),var(--typography-family-sans); overflow:hidden; }
.echoes-empty { margin:auto; color: var(--color-text-tertiary, #616b80); font-size:13px; }
.echoes-stream { flex:1; overflow-y:auto; padding:16px 18px; display:flex; flex-direction:column; gap:12px; }
/* history は tabindex=0 で focus 可能（Home/End/PgUp/PgDn 用）。領域全体を囲む outline は
   目障りなので抑制する（focus 合図は scrollbar 操作で十分伝わる）。 */
.echoes-stream:focus, .echoes-stream:focus-visible { outline:none; }
.echoes-msg { max-width:100%; animation: echoes-fade .18s ease-out; }
.echoes-msg.user { align-self:flex-end; background: var(--color-accent-soft, #1c2333);
  border:1px solid var(--color-border, #2a3040); border-radius:12px 12px 3px 12px; padding:8px 13px; max-width:80%; }
/* §5.1: 送信待ち type-ahead。半透明 + 破線で「まだ送っていない」を伝える。 */
.echoes-msg.user.pending { opacity:.62; border-style:dashed; transition: opacity .12s ease, border-color .12s ease; }
/* dequeue-to-composer: composer が空なら「クリックで入力欄に戻して編集」可（hover で明るく）。 */
.echoes-msg.user.pending.editable { cursor:pointer; }
.echoes-msg.user.pending.editable:hover { opacity:.9; border-color: var(--color-accent, #e2b96f); }
/* composer に打ちかけ下書きがある間は編集不可 = グレーアウト（下書きを潰さないための MVP ガード）。 */
.echoes-msg.user.pending.locked { opacity:.38; cursor:not-allowed; }
.echoes-pending-badge { display:block; margin-top:4px; font-size:10.5px; color: var(--color-text-tertiary, #8b93a7); }
/* status bar（入力の上）: engine の現況の**読み取り専用**計器。操作は composer 側が持つ。 */
.echoes-status { display:flex; align-items:center; gap:8px; padding:4px 14px; min-height:24px; font-size:11px;
  font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-text-tertiary,#8b93a7);
  border-top:1px solid var(--color-border,#2a3040); background: var(--color-bg,#0f1115); }
.echoes-status-dot { width:7px; height:7px; border-radius:50%; flex:none; background: var(--color-text-tertiary,#616b80); }
.echoes-status-label { letter-spacing:.03em; }
.echoes-status-detail { color: var(--color-text-secondary,#a8b0c0); }
.echoes-status-pending { color: var(--color-accent,#e2b96f); }
.echoes-status.s-streaming .echoes-status-dot { background: var(--color-success,#6fe2a8); animation: echoes-status-pulse 1.2s ease-in-out infinite; }
.echoes-status.s-thinking .echoes-status-dot { background:#8fb0ff; animation: echoes-status-pulse 1.2s ease-in-out infinite; }
.echoes-status.s-tool .echoes-status-dot { background: var(--color-accent,#e2b96f); animation: echoes-status-pulse 1.2s ease-in-out infinite; }
.echoes-status.s-awaiting .echoes-status-dot { background:#f0a3a3; animation: echoes-status-pulse .8s ease-in-out infinite; }
.echoes-status.s-error .echoes-status-dot { background:#f0a3a3; }
.echoes-status.stalled .echoes-status-dot { background:#f0a3a3 !important; animation: echoes-status-pulse .6s ease-in-out infinite; }
.echoes-status-stalled { color:#f0a3a3; font-weight:600; }
.echoes-status-event { color: var(--color-text-tertiary,#616b80); opacity:.65; }
@keyframes echoes-status-pulse { 50% { opacity:.32; } }
.echoes-msg-body { font-size:13.5px; line-height:1.6; word-break:break-word; }
/* 返信（assistant）の本文だけ拡大 = 15px（自分の入力バブルは 13.5px のまま）。
   line-height は unitless なので font-size に追従してスケールする。 */
.echoes-msg:not(.user) .echoes-msg-body { font-size:15px; }
.echoes-msg-body :first-child { margin-top:0; } .echoes-msg-body :last-child { margin-bottom:0; }
.echoes-msg-body pre { background: var(--color-bg-elevated, #16191f); border:1px solid var(--color-border,#2a3040);
  border-radius:8px; padding:10px 12px; overflow-x:auto; font-size:12px; }
.echoes-msg-body code { font-family: var(--vp-font-mono),var(--typography-family-mono); }
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
/* ToolRow: tool 1 件。container / head(pill 1 行) / body(詳細) の 3 層は toolgroup と同型。 */
.echoes-tool { align-self:flex-start; font-size:12px; animation: echoes-fade .18s ease-out; }
.echoes-tool-head { display:flex; align-items:center; gap:8px; width:100%; text-align:left;
  font-family:inherit; font-size:12px;
  color: var(--color-text-secondary,#a8b0c0); background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--color-border,#2a3040); border-radius:8px; padding:5px 11px; }
/* 詳細を持つ tool だけ押せる（持たない行は見た目そのまま・無反応）。 */
.echoes-tool-head.clickable { cursor:pointer; }
.echoes-tool-spinner { width:9px; height:9px; border-radius:50%; border:1.5px solid var(--color-accent,#3b82f6);
  border-top-color: transparent; animation: echoes-spin .7s linear infinite; }
.echoes-tool.done .echoes-tool-spinner, .echoes-tool.error .echoes-tool-spinner { display:none; }
.echoes-tool.done .echoes-tool-head { color: var(--color-text-tertiary,#616b80); }
.echoes-tool.error .echoes-tool-head { color:#f0a3a3; }
.echoes-tool-name { font-family: var(--vp-font-mono),var(--typography-family-mono); }
.echoes-tool-status { margin-left:auto; font-size:11px; }
/* 展開部: thinking-body と同じ左罫線の入れ子表現で input / result を積む。 */
.echoes-tool-body { display:flex; flex-direction:column; gap:6px; margin:5px 0 0 16px;
  padding-left:8px; border-left:2px solid var(--color-border,#2a3040); }
.echoes-tool-detail-label { font-size:10px; letter-spacing:.06em; text-transform:uppercase;
  color: var(--color-text-tertiary,#616b80); margin-bottom:2px; }
.echoes-tool-detail-body { margin:0; max-height:260px; overflow:auto; white-space:pre-wrap;
  word-break:break-word; font-family: var(--vp-font-mono),var(--typography-family-mono);
  font-size:11px; line-height:1.5; color: var(--color-text-secondary,#a8b0c0); }
.echoes-tool-detail-omitted { font-size:10px; color: var(--color-text-tertiary,#616b80); margin-top:2px; }
/* subagent の発話: role でラベル分け。thinking は親の thinking と同じ「控えめ」の質感に寄せる。 */
.echoes-subagent-entry { margin-top:4px; }
.echoes-subagent-role { font-size:9px; letter-spacing:.06em; text-transform:uppercase;
  color: var(--color-text-tertiary,#616b80); border:1px solid var(--color-border,#2a3040);
  border-radius:4px; padding:0 4px; }
.echoes-subagent-entry.thinking .echoes-tool-detail-body { color: var(--color-text-tertiary,#616b80); font-style:italic; }
.echoes-subagent-entry.prompt .echoes-tool-detail-body { color: var(--color-text-tertiary,#8b93a7); }
/* ToolGroupRow: 連続同名 tool（Agent ×N 等）を畳む accordion。畳んだ header は ToolRow と同じ枠で 1 行。 */
.echoes-toolgroup { align-self:flex-start; font-size:12px; animation: echoes-fade .18s ease-out; }
.echoes-toolgroup-toggle { display:flex; align-items:center; gap:8px; width:100%; cursor:pointer;
  font-size:12px; color: var(--color-text-secondary,#a8b0c0); background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--color-border,#2a3040); border-radius:8px; padding:5px 11px; }
.echoes-toolgroup.done .echoes-toolgroup-toggle { color: var(--color-text-tertiary,#616b80); }
.echoes-toolgroup.error .echoes-toolgroup-toggle { color:#f0a3a3; }
.echoes-toolgroup-count { font-family: var(--vp-font-mono),var(--typography-family-mono);
  color: var(--color-text-tertiary,#8b93a7); font-size:11px; }
/* 展開部: 個別 ToolRow を段付きで縦に並べる（thinking-body と同じ左罫線の入れ子表現）。 */
.echoes-toolgroup-body { display:flex; flex-direction:column; gap:5px; margin:5px 0 0 16px;
  padding-left:8px; border-left:2px solid var(--color-border,#2a3040); }
.echoes-cursor { width:7px; height:15px; background: var(--color-accent,#3b82f6); border-radius:1px;
  animation: echoes-blink 1s step-start infinite; align-self:flex-start; }
/* PromptCard（doc 35 §4）: HITL 質問。engine が人を待っている合図として左寄せカードで settle。 */
.echoes-prompt { align-self:flex-start; max-width:88%; display:flex; flex-direction:column; gap:12px;
  padding:13px 15px; border-radius:12px; background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--sb-conn-hitl,#FF4A2D); box-shadow:0 0 0 1px color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 78%);
  animation: echoes-fade .18s ease-out; }
.echoes-prompt.answered { border-color: var(--color-border,#2a3040); box-shadow:none; opacity:.9; }
.echoes-prompt-q { display:flex; flex-direction:column; gap:7px; }
.echoes-prompt-header { font-size:10px; text-transform:uppercase; letter-spacing:.08em;
  color: var(--sb-conn-hitl,#FF4A2D); }
.echoes-prompt-question { font-size:14px; line-height:1.5; color: var(--color-text,#e6e9ef); }
.echoes-prompt-options { display:flex; flex-wrap:wrap; gap:8px; }
.echoes-prompt-opt { font-size:12.5px; padding:6px 13px; border-radius:8px; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg,#0f1115);
  color: var(--color-text-secondary,#a8b0c0); transition: border-color .15s ease, background .15s ease, color .15s ease; }
.echoes-prompt-opt:hover { border-color: var(--color-text-tertiary,#616b80); color: var(--color-text,#e6e9ef); }
.echoes-prompt-opt.selected { border-color: var(--sb-conn-hitl,#FF4A2D); color: var(--color-text,#e6e9ef);
  background: color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 86%); }
.echoes-prompt-confirm { align-self:flex-end; padding:7px 16px; font-size:12.5px; border-radius:8px;
  border:none; cursor:pointer; background: var(--sb-conn-hitl,#FF4A2D); color:#fff; }
.echoes-prompt-confirm:disabled { opacity:.4; cursor:default; }
/* 回答済み: 見出し + 選んだ値だけの静かな折りたたみ表示。 */
.echoes-prompt-answered { display:flex; flex-direction:column; gap:5px; }
.echoes-prompt-arow { display:flex; gap:9px; align-items:baseline; font-size:12.5px; }
.echoes-prompt-ahead { font-size:10px; text-transform:uppercase; letter-spacing:.06em;
  color: var(--color-text-tertiary,#616b80); min-width:0; }
.echoes-prompt-aval { color: var(--color-text,#e6e9ef); font-weight:500; }
.echoes-plan { border-bottom:1px solid var(--color-border,#2a3040); padding:10px 18px; background: var(--color-bg-elevated,#13161c); }
.echoes-plan-title { font-size:10px; text-transform:uppercase; letter-spacing:.08em; color: var(--color-text-tertiary,#616b80); margin-bottom:6px; }
.echoes-plan-item { display:flex; align-items:center; gap:8px; font-size:12.5px; padding:2px 0; transition: color .2s ease; }
.echoes-plan-dot { width:7px; height:7px; border-radius:50%; background: var(--color-text-tertiary,#616b80); transition: background .2s ease; }
.echoes-plan-item.in_progress { color: var(--color-text,#e6e9ef); } .echoes-plan-item.in_progress .echoes-plan-dot { background: var(--color-accent,#e2b96f); }
.echoes-plan-item.completed { color: var(--color-text-tertiary,#616b80); } .echoes-plan-item.completed .echoes-plan-dot { background: var(--color-success,#6fe2a8); }
.echoes-plan-item.completed .echoes-plan-text { text-decoration: line-through; }
/* composer: 入力（上）と操作（下）を 1 つの器に。枠は器が持ち、textarea は枠なしで中に敷く。 */
.echoes-composer { display:flex; flex-direction:column; margin:8px 14px 10px; border-radius:10px;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#161a20);
  overflow:hidden; }
.echoes-composer:focus-within { border-color: var(--color-accent,#3b82f6); }
/* 操作の行（入力の下）: 左 = 送る前に決める設定、右 = 実行。 */
.echoes-actions { display:flex; align-items:center; gap:6px; padding:4px 6px 5px 8px; }
.echoes-actions-spacer { flex:1; }
/* 既定 1 行（min-height は置かず、rows=1 + autosize が高さを決める）。伸びる上限だけ CSS が持つ。 */
.echoes-input-box { flex:1; resize:none; max-height:160px; padding:8px 10px 4px; font-size:13px; line-height:1.5;
  font-family: var(--vp-font-sans),var(--typography-family-sans); color: var(--color-text,#e6e9ef);
  /* 枠と地色は composer(器) が持つ — textarea 自身は素で敷く（二重枠にしない）。 */
  background:transparent; border:none; outline:none; }
.echoes-send { display:inline-flex; align-items:center; gap:4px; padding:4px 11px; font-size:12px;
  border-radius:7px; border:none; cursor:pointer; background: var(--color-accent,#3b82f6); color:#fff; }
.echoes-send:disabled { opacity:.4; cursor:default; }
.echoes-stop { display:inline-flex; align-items:center; gap:4px; padding:4px 10px; font-size:12px;
  border-radius:7px; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background:transparent; color: var(--color-text-secondary,#a8b0c0); }
.echoes-stop:hover { border-color:#f0a3a3; color:#f0a3a3; }
/* Act toggle は下段（#pane-tabs）へ移設した — 見た目は隣の chip（.pane-tab）に合わせるため
   main_area.rs の .pane-act-toggle が持つ。旧 floating 定義（.echoes-act-toggle /
   .echoes-console-actions）は置き場ごと消えたので撤去。 */
/* session 名札（pane 上端）: この pane = この session の素性。tab strip（doc 38 仮置き）の
   後継 — session ↔ Pane 1:1（doc 46 §1.5 / doc 50 P1）で pane 自身が名乗る。
   Pane 共通の名札 token（--vp-nameplate-*）に乗せて、全 pane の上端と同じ見えにする。 */
.echoes-session-plate { display:flex; align-items:center; gap:6px; flex:none;
  height:calc(var(--vp-nameplate-h) - 4px); padding:0 var(--vp-nameplate-pad-x);
  font-size:10.5px; font-family: var(--vp-font-mono),var(--typography-family-mono);
  color: var(--color-text-tertiary,#8b93a7); background: var(--vp-nameplate-bg);
  border-bottom: var(--vp-nameplate-border); user-select:none; }
.echoes-session-plate.focused { color: var(--color-text-secondary,#a8b0c0); }
.echoes-session-plate-label { font-weight:500; }
.echoes-session-plate-sid { opacity:.65; }
.echoes-session-plate-hint { opacity:.5; font-family: var(--vp-font-sans),var(--typography-family-sans); }
.echoes-session-plate-spacer { flex:1; }
/* 既定 opacity .55 は暗い名札上で沈んで「削除の動線が無い」ように見えた（2026-07-24 実機）。
   常時視認できる濃さに上げ、hover で確定的に立てる。 */
.echoes-session-plate-close { flex:none; display:inline-flex; align-items:center; padding:2px 4px;
  line-height:1; border:none; border-radius:4px; background:transparent; cursor:pointer;
  color: var(--color-text-secondary,#a8b0c0); opacity:.85; }
.echoes-session-plate-close:hover { opacity:1; color: var(--color-text,#e6e9ef);
  background: var(--color-bg,#0f1115); }
/* focus されていない pane は全体をわずかに沈める（どこに打てるかを一目で）。 */
.echoes-chat:not(.focused) { opacity:.82; }
.echoes-tab-dot { width:6px; height:6px; border-radius:50%; flex:none; background: var(--color-success,#6fe2a8); }
/* 旧 .echoes-header（model/perm の独立行）は計器盤へ畳んで撤去。select は下段の高さに収まる
   よう一段小さくする（行が status と共用になったため）。 */
.echoes-model-select { font-size:10.5px; padding:1px 5px; border-radius:6px; outline:none; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#16191f);
  color: var(--color-text-secondary,#a8b0c0); font-family:inherit; }
.echoes-model-select:disabled { opacity:.45; cursor:default; }
/* PR3: permission 承認カード（allow/deny）。question カードと同じ枠、action だけ差し替え。 */
.echoes-perm-tool { font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-accent,#e2b96f); }
.echoes-perm-input { font-family: var(--vp-font-mono),var(--typography-family-mono); font-size:11.5px;
  color: var(--color-text-tertiary,#8b93a7); background: var(--color-bg,#0f1115); border:1px solid var(--color-border,#2a3040);
  border-radius:6px; padding:6px 9px; margin:6px 0; overflow-x:auto; white-space:pre-wrap; word-break:break-all; }
.echoes-perm-actions { display:flex; gap:8px; margin-top:8px; }
.echoes-perm-allow, .echoes-perm-deny { font-size:12.5px; padding:6px 16px; border-radius:8px; cursor:pointer; border:1px solid var(--color-border,#2a3040); }
.echoes-perm-allow { background: var(--color-success,#6fe2a8); color:#06231a; border-color: var(--color-success,#6fe2a8); }
.echoes-perm-deny { background: var(--color-bg-elevated,#16191f); color:#f0a3a3; }
.echoes-perm-deny:hover { border-color:#f0a3a3; }
/* PR4: plan 承認カード。plan 本文は markdown で描き、accent 枠で「あなたの承認を待つ」を伝える。 */
.echoes-plan-card { border-color: var(--color-accent,#e2b96f); }
.echoes-plan-body { font-size:13px; line-height:1.6; max-height:280px; overflow-y:auto; margin:6px 0;
  padding:8px 10px; background: var(--color-bg,#0f1115); border:1px solid var(--color-border,#2a3040); border-radius:6px; }
.echoes-plan-body :first-child { margin-top:0; } .echoes-plan-body :last-child { margin-bottom:0; }
/* context ゲージ（Act I statusline の bar :context 相当）。ヘッダー右端に寄せる。 */
/* status bar の右端へ寄せる（読み取り計器の並びの末尾）。 */
.echoes-context { margin-left:auto; display:flex; align-items:center; gap:6px; }
.echoes-context-bar { width:52px; height:5px; border-radius:3px; overflow:hidden;
  background: var(--color-bg,#0f1115); border:1px solid var(--color-border,#2a3040); }
.echoes-context-fill { display:block; height:100%; border-radius:2px;
  background: var(--color-success,#6fe2a8); transition: width .3s ease, background .3s ease; }
.echoes-context-pct { font-size:10.5px; min-width:32px; text-align:right;
  font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-text-tertiary,#8b93a7); }
.echoes-context.warn .echoes-context-fill { background: var(--color-accent,#e2b96f); }
.echoes-context.crit .echoes-context-fill { background: #f0a3a3; }
.echoes-context.crit .echoes-context-pct { color: #f0a3a3; }
@keyframes echoes-fade { from { opacity:0; transform: translateY(4px); } to { opacity:1; transform:none; } }
@keyframes echoes-spin { to { transform: rotate(360deg); } }
@keyframes echoes-blink { 50% { opacity:0; } }
@keyframes echoes-shimmer { from { background-position: 220% 0; } to { background-position: -120% 0; } }
@media (prefers-reduced-motion: reduce) {
  .echoes-msg, .echoes-tool, .echoes-prompt { animation:none; }
  .echoes-tool-spinner { animation-duration: 1.5s; } .echoes-cursor { animation:none; opacity:.6; }
  /* motion off: shimmer は止めるが text-fill:transparent のままだと消えるので色を戻す。 */
  .echoes-thinking-toggle.live .echoes-thinking-label { animation:none; background:none;
    -webkit-text-fill-color: currentColor; color: var(--color-text,#e6e9ef); }
}
`

export type ChatViewApi = {
  /** lane を active にする（初出なら vpConsole renderer を attach + session 一覧を取得）。 */
  showLane(lane: string): void
  /** doc 38 §4.3: 指定 lane の再同期ローダーを明示的に下ろす（Act I 切替時に entry.tsx が呼ぶ）。 */
  clearReplaying(lane: string): void
  /** chat session pane を host に mount する（lane-panes の動的 host 生成から呼ばれる）。
   *  返り値 = dispose（lane 切替 / session close で host ごと破棄する時に呼ぶ）。 */
  mountSession(host: HTMLElement, lane: string, session: number): () => void
}

export function installChatView(vpConsole: VpConsole): ChatViewApi {
  // session 一覧 bus → module signal（全 SessionChatView が共有）。install は起動時 1 回。
  document.addEventListener('vp:echoes-sessions', (e) => {
    const d = (
      e as CustomEvent<{ lane: string; focused: number; sessions: EchoesSession[] }>
    ).detail
    if (!d?.lane) return
    setSessionViews((prev) => ({
      ...prev,
      [d.lane]: { focused: d.focused, sessions: d.sessions ?? [] },
    }))
  })
  const attached = new Set<string>()
  return {
    showLane(lane: string) {
      if (!attached.has(lane)) {
        attached.add(lane)
        focusedChat(lane) // store を先に用意（replay が流し込む）
        // doc 38 Phase 2 → doc 50: renderer は session も受け取り、foldEvent が session ごとの
        // store に振り分ける（旧「focused 以外を弾く」は store の re-key で役目を終えた）。
        vpConsole.attachRenderer(lane, (ev, session) => foldEvent(lane, ev, session))
      }
      // doc 38 §4.3: 離れる lane の再同期ローダーを掃除する（replay_end 取りこぼしで stuck した
      // まま戻って来ても固着させない）。新 lane が本当に再同期するなら attach / demand の
      // replay_start が立て直す。
      const prev = activeLane()
      if (prev && prev !== lane) clearReplaying(prev)
      setActiveLane(lane)
      // attach 時に session 一覧を取得（focused の確定 + pane の顔ぶれ（lane-panes）の種）。
      const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
      ipc?.postMessage(JSON.stringify({ t: 'echoes:sessions_fetch', lane }))
    },
    clearReplaying,
    mountSession(host, lane, session) {
      laneChat(lane, session) // store を先に用意（mount 前に届く replay の取りこぼし防止）
      return render(() => <SessionChatView lane={lane} session={session} />, host)
    },
  }
}
