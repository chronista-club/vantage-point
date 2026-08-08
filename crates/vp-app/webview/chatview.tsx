/**
 * ChatView (doc 33 C2) — Conversation gui の Console 面 GUI（SolidJS）。
 *
 * World B。`window.vpConsole`（console.ts）が届ける [`ConversationEvent`] を per-lane store に
 * 畳み込み、active lane の会話を message stream として描画する。入力は IPC `conversation:submit`
 * で repo へ送る。marked で markdown、motion は CSS（prefers-reduced-motion 尊重）。
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
  type JSX,
} from 'solid-js'
import { createStore, produce, type SetStoreFunction } from 'solid-js/store'
import { CreoIcon } from '@chronista-club/creo-ui-icons-web'
import { Marked } from 'marked'
import type {
  ConversationEvent,
  ConversationSession,
  PickerChoice,
  PlanEntry,
  QuestionSpec,
  VpConsole,
} from './console'
// doc 38 Phase 2: focused 判定 / 楽観的 focus 切替は console.ts の per-lane registry を共有する
// （repo が真実源、ここは view）。session chip の prefix 規則は LaneHeader を SSOT として再利用。
// doc 47 §6: 共有 bus の相関 id（採番 + 照合）も console.ts が SSOT。
import { focusedOf, noteFocus, syncHeaderSessionId } from './console'
import { sessionChipPrefix } from './LaneHeader'
import { isImeKeystroke } from './ime'
import { applyCompletion, filterSlashCommands, moveSelection, slashQuery } from './slash'

// ---------------------------------------------------------------------------
// 会話モデル — flat item stream（ConversationEvent を UI 単位に畳む）
// ---------------------------------------------------------------------------

type ChatItem =
  | { kind: 'user'; text: string }
  | { kind: 'assistant'; text: string; sealed?: boolean } // append 先。sealed=turn 境界（§5.1、次 turn は新バブル）
  | { kind: 'thinking'; text: string; at?: number } // thought_chunk を末尾 thinking に append。at = live 受信時刻（doc 57 §4.2、replay では刻まない）
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
      /** live 受信/settle 時刻（doc 57 §4.2 経過時間の材料）。replay では刻まない = 偽らない。 */
      at?: number
      doneAt?: number
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
 * ねらい: Agent 等が連続で回ったとき N 行を占有せず「wrench + Agent ×N」の 1 行に畳む。
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
  /** context ゲージ（tui statusline の bar :context 相当）。turn_completed で更新。 */
  contextTokens: number | null
  contextWindow: number | null
  /** doc 35 PR3/PR4: engine の permission mode（session_init.permission_mode 由来）。per-lane。 */
  permissionMode?: string
  /**
   * この session で打てる slash command（`session_init.slash_commands` 由来）。
   *
   * ⚠️ **per-session で持つ**。skill / plugin / MCP の読み込みで session ごとに増減するので、
   * lane 横断で 1 つ持つと嘘になる（doc 32 が「非同期ロードでブレる noise」と実測している）。
   * ⚠️ `/` は付いていない素の名前で来る（`chronista-style:codeflow` のような形も混じる）。
   */
  slashCommands: string[]
  /**
   * slash command の説明（`session_init.command_docs` 由来）。
   *
   * ⚠️ **候補の源ではない**。一覧の正は `slashCommands` で、こちらは引ければ添えるだけ。
   * 実測で 160 個中 86 個しか埋まらない = **説明の無い候補が普通に混ざる**。
   */
  commandDocs: Record<string, string>
  /** doc 35 §5.1: streaming 中に送られた type-ahead。turn 閉で flush（表示順=処理順の不変条件）。 */
  pending: string | null
  /** status 同期: 最後に畳んだイベント種別（foldInto で全イベント更新）。 */
  lastEvent: string | null
  /** status 同期: 最後にイベントを受けた時刻 ms（foldEvent で Date.now。hang 検出の時間軸）。 */
  lastEventAt: number | null
  /** transcript replay（attach/reconnect 時の過去会話再送）進行中か。replay_start→true /
   *  replay_end→false。コーナーの再同期ローディングアニメ（resync-loader）の可視条件。 */
  replaying: boolean
  /** now-line の契約供給（doc 51 §1 A3b — AI が自分の今を報告する口）。null = 契約報告なし
   *  = deriveNowLine の機械導出（A3a の保険）が下支えする。turn_completed で消える（「今」は
   *  turn より長生きしない）。書き手は A3b の `now_line` event（PR2 で配線 — 受け皿を先に置く
   *  reader-first）。 */
  nowLine: string | null
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
// repo（conversation_session_list）が真実源。ここは 'vp:conversation-sessions' bus を映すだけの view cache で
// state を持たない。focused の真値は console.ts の registry（focusedOf）— ここは reactive 表示用の鏡。
// ---------------------------------------------------------------------------

export type LaneSessionsView = { focused: number; sessions: ConversationSession[] }
const [sessionViews, setSessionViews] = createSignal<Record<string, LaneSessionsView>>({})

/** lane の session 一覧 view（reactive）。 */
function sessionsOf(lane: string): LaneSessionsView | null {
  return sessionViews()[lane] ?? null
}

/** session に focus を移す（pane click / 旧 tab click の移植）。
 *  楽観更新（noteFocus + local signal）+ IPC。authoritative は後続の conversation_session_list。 */
export function focusChatSession(lane: string, session: number): void {
  if (session === (sessionsOf(lane)?.focused ?? focusedOf(lane))) return
  // doc 38 §4.3: focus 切替で再同期ローダーを必ず一度下ろす（旧 focused の replay_end を
  // 取りこぼしていても固着させない）。直後の demand_start → ReplayStart が必要なら立て直す。
  clearReplaying(lane)
  noteFocus(lane, session)
  // D1: 既存 session 間の切替でも名札の session chip を即追従させる（authoritative は
  // conversation_session_list → handleSessionList 側の sync が上書き）。
  if (syncHeaderSessionId(lane)) {
    document.dispatchEvent(new CustomEvent('vp:lane-header', { detail: { lane } }))
  }
  setSessionViews((prev) => {
    const cur = prev[lane] ?? { focused: session, sessions: [] }
    return { ...prev, [lane]: { ...cur, focused: session } }
  })
  const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
  ipc?.postMessage(JSON.stringify({ t: 'conversation:session_focus', lane, session }))
}

/** session を閉じる（session ↔ Pane 1:1 なので pane ごと消える）。backend（conversation_session_remove）
 *  が registry から除去 → 除去後の focus 先を返し、app.rs が list 再取得 + demand_start する。
 *  最後の 1 本 / root は backend が Err で拒否（UI 側 canCloseSession と多重防御）。 */
export function removeChatSession(lane: string, session: number): void {
  const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
  ipc?.postMessage(JSON.stringify({ t: 'conversation:session_remove', lane, session }))
}

/** doc 50 §4.6 A6 ②: 名札 kind badge → session の Mode（見え方）切替を要求する。
 *
 * IPC を直接撃たず `vp:mode-switch-request` に流すのは、handoff overlay と二重切替 lock を
 * entry.tsx が一元管理しているため（名札の実装と overlay の DOM / timer を絡ませない —
 * doc 51 §2 で root picker から event 依頼にした規律をそのまま引き継ぐ）。
 * 宛先 session は **引数で運ぶ**（focus に依存しない — 「focus してから送る」型の分割は
 * 別 IPC なので順序保証が無く、別 session に届くレースを作る。doc 50 §4.3 の警告）。
 * 応答は Rust の `SessionModeApplied` → `vpConsole.setSessionMode` → 'vp:session-mode' で返り、
 * roster がその session の Pane kind を入れ替える。 */
export function requestSessionMode(
  lane: string,
  session: number,
  mode: 'tui' | 'gui',
): void {
  document.dispatchEvent(
    new CustomEvent('vp:mode-switch-request', {
      detail: { lane, session, target: mode },
    }),
  )
}

// ---------------------------------------------------------------------------
// doc 38 §4.3 — 再同期ローダー（resync-loader）の固着防止
//
// `replaying` は replay_start→true / replay_end→false で駆動するが、replay_end が来ない経路
// （tui lane / error 中断 / engine 途絶）では立ちっぱなしになれる。表示を「focused session の
// attach 状態機械」に束縛する:
//  ① lane/Mode/tab 切替で必ず解除（clearReplaying を各遷移点で呼ぶ）
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
 * ConversationEvent を ChatState に畳み込む純粋 mutation（reducer 本体）。
 *
 * solid の `produce` draft でも plain object でも同じに動く（＝ store 非依存 = 単体テスト可能）。
 * 会話モデリングの肝: message_chunk / thought_chunk は末尾同種 item に append（accumulate）、
 * tool_call_update は id 一致で done 化。ここが gui の描画正しさの中核。
 */
export function foldInto(s: ChatState, ev: ConversationEvent): void {
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
      // この session で打てる slash command。⚠️ CLI 側で「対話端末なしで動くもの」に
      // **絞り込み済み**なので、VP 側で除外リストを持たない（公式 agent-sdk/slash-commands）。
      if (ev.slash_commands) s.slashCommands = ev.slash_commands
      if (ev.command_docs) s.commandDocs = ev.command_docs
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
      else
        s.items.push({
          kind: 'thinking',
          text: ev.text,
          at: s.replaying ? undefined : Date.now(),
        })
      break
    }
    case 'tool_call':
      // tool 実行も active turn（text を挟まず tool に直行する turn がある — chunk だけを
      // streaming の契機にすると、その間 status / 灯が「待機中」と嘘をつく。A2 の灯で顕在化）。
      s.streaming = true
      s.items.push({
        kind: 'tool',
        id: ev.id,
        name: ev.name,
        done: false,
        error: false,
        input: ev.input,
        at: s.replaying ? undefined : Date.now(),
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
        // 経過時間の材料（doc 57 §4.2）。replay では実時間を偽れないので刻まない。
        if (!s.replaying) t.doneAt = Date.now()
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
    case 'now_line':
      // AI の自己申告（doc 51 §1 A3b — `vp now` 発）。deriveNowLine が質問要旨の次に読む。
      s.nowLine = ev.text
      break
    case 'turn_completed':
      s.streaming = false
      s.cost = ev.cost_usd ?? s.cost
      // 欠落 turn（engine が値を運ばない版）では前値を保つ — ゲージが点滅しないように。
      s.contextTokens = ev.context_tokens ?? s.contextTokens
      s.contextWindow = ev.context_window ?? s.contextWindow
      s.nowLine = null // 契約の「今」は turn より長生きしない（doc 51 §1 A3）
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

/** ConversationEvent を **その session の** store に畳み込む（console.ts の renderer 本体）。
 *
 *  doc 50 §4.3 #2: 旧実装は `session !== focusedOf(lane)` で背景 session の event を**捨てて**
 *  いた（lane に会話が 1 本しか無い前提）。session ↔ Pane 1:1 では N 本が同時に生きるので、
 *  捨てずに **session ごとの store へ振り分ける**。背景 session の stream が focused の会話に
 *  混ざる心配は、store が別なので構造的に消える（旧 filter が担っていた役割は key が担う）。
 *  session は console.ts で正規化済み（未指定 = focused = 1、旧 SP 互換）。 */
function foldEvent(lane: string, ev: ConversationEvent, session: number): void {
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
export function isTurnClosingEvent(kind: ConversationEvent['kind']): boolean {
  return kind === 'turn_completed' || kind === 'error' || kind === 'engine_exited'
}

/** doc 35 §5.1: buffer した type-ahead を engine に流す（対象 = turn を閉じた (lane, session)）。
 *  doc 50 P2: `conversation:submit` が session を運ぶようになったので、background session の
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
  ipc?.postMessage(JSON.stringify({ t: 'conversation:submit', lane, session, prompt: text }))
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
    slashCommands: [],
    commandDocs: {},
    pending: null,
    lastEvent: null,
    lastEventAt: null,
    replaying: false,
    nowLine: null,
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

/** kind badge（doc 50 §4.6 A6 ②）で `target` の見え方に切り替えられるか。
 *
 * **能力表引きで判定し、engine 名の型分岐は書かない**（§4.6 ② — shell の chat が
 * 「原理不可」ではなく「host 未実装」であるように、能力は engine 側の申告で変わる。
 * 実装された日に client を触らず badge が生えるのが正しい形）。判定材料は server が
 * session ごとに送る `chat_capable` 一本で、能力表の SSOT は server（`EngineKind`）。
 *
 * - **→chat**: その session が gui host を持つ engine か。未申告（旧 SP）は**不可**に倒す
 *   — 押しても server に弾かれるだけの行き止まりを出さない（`newPaneChoices` と同じ規律）。
 * - **→tui**: 常に可。tui は login shell に engine を流し込むだけなのでどの engine でも
 *   成立する（doc 50 §4.0 帰結 1「login shell は劣化ケースではなく正規の投げる先」）。
 */
export function canSwitchTo(target: 'tui' | 'gui', chatCapable?: boolean): boolean {
  return target === 'tui' ? true : chatCapable === true
}

/** 進行中の mode 切替（handoff）を (lane, session) で引くための key。
 *
 * doc 50 §4.6 A6: 切替は **pane 単位**になったので、lock も pane（= session）単位で持つ。
 * 「どれか 1 つでも進行中なら全部弾く」にすると、**無関係な pane の click を無言で落とす**
 * （A6 で全 pane が badge を持つため実際に起こる。team-b review 2026-07-25 score 85 —
 * 解除側は (lane, session) を照合していたのに入口だけ素の存在チェックで非対称だった）。 */
export function handoffKey(lane: string, session: number): string {
  return `${lane}#${session}`
}

// ---------------------------------------------------------------------------
// agent status 導出（doc 35 §5.1 診断用の常時可視化ブロック）— 純粋関数 = テスト可能
// ---------------------------------------------------------------------------

export type ConversationStatus = {
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
export function deriveStatus(s: ChatState | null, nowMs = 0): ConversationStatus {
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

/** 灯の 3 状態（doc 51 §1 A2 — 並行性を支える視点の視覚言語）。
 *  動いている（run = 緑・脈動）/ 待っている（off = 無灯）/ あなたが要る（need = 赤・速い脈動）。 */
export type SessionLamp = 'run' | 'off' | 'need'

/** ConversationStatus → 灯（純関数）。細かい状態語（thinking / tool / 停滞…）は計器盤（status 行）の
 *  領分で、灯は「横目で読む」ための 3 値に畳む:
 *  - need = ボールが人にある（質問 / 承認）+ engine 異常（介入が要る点で同じ側）
 *  - run  = engine が動いている（streaming / thinking / tool）。stalled は run のまま —
 *    8s 無イベントは平常でも起きるので灯を赤にせず、嘘の告発は status 行の文字に任せる
 *  - off  = 待っている（待機中 / 💤 休眠） */
export function lampOf(status: ConversationStatus): SessionLamp {
  if (status.kind === 'awaiting' || status.kind === 'error') return 'need'
  if (status.kind === 'streaming' || status.kind === 'thinking' || status.kind === 'tool') return 'run'
  return 'off'
}

/** now-line の 1 行を 1 行らしく整える（先頭行のみ + 長すぎは切る。純関数）。 */
export function clampNowLine(text: string, maxLen = 60): string {
  const line = text.split('\n', 1)[0].trim()
  return line.length <= maxLen ? line : `${line.slice(0, maxLen - 1)}…`
}

/**
 * now-line（doc 51 §1 A3 — 名札直下の「今なにを」動的一行。純関数）。
 *
 * 優先順（上ほど「今」が濃い）:
 * 1. 質問 / 承認の要旨 — ボールが人にある時は、AI の自己報告（過去の turn 内作業）より濃い
 * 2. **契約**（s.nowLine — A3b で AI が自分の今を報告する口。メイン供給）
 * 3. 機械導出（A3a の保険 — 報告しない engine / turn でも行が死なない下支え）:
 *    実行中の tool 名 → turn 中の頼まれごと（直近 user prompt の先頭）
 * 4. null = 待っている pane に「今」は無い — 空なら描かない（doc 50 §2）
 */
export function deriveNowLine(s: ChatState | null): string | null {
  if (!s) return null
  const waiting = s.items.find((i) => i.kind === 'prompt' && !i.answered) as
    | Extract<ChatItem, { kind: 'prompt' }>
    | undefined
  if (waiting) {
    if (waiting.permission) return `承認待ち: ${waiting.permission.toolName}`
    const q = waiting.questions[0]?.question
    return q ? clampNowLine(q) : '質問待ち'
  }
  if (s.nowLine) return clampNowLine(s.nowLine)
  if (!s.streaming) return null
  const last = s.items[s.items.length - 1]
  if (last?.kind === 'tool' && !last.done) return `${last.name} を実行中`
  const lastUser = [...s.items].reverse().find((i) => i.kind === 'user') as
    | Extract<ChatItem, { kind: 'user' }>
    | undefined
  return lastUser ? clampNowLine(lastUser.text) : null
}

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------

/** [[slug]] → creo の memory slug 直 URL（mako 2026-07-31 — wiki-link 記法を chat で踏める）。
 *  /m/{slug} は creo B-2 Phase 5「slug の表面化」で実機検証済みの恒久 route。存在しない
 *  slug は creo 側の not-found に落ちる（wiki の dangling link と同じ扱いで許容）。 */
const CREO_MEMORY_BASE = 'https://app.creo-memories.in/m/'

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

/** [[name]] の描画 HTML（純関数、テスト対象）。表示は記法そのまま = 出自が一目で分かる。 */
export function creoLinkHtml(name: string): string {
  return `<a class="conversation-creo-link" href="${CREO_MEMORY_BASE}${encodeURIComponent(name)}">[[${escapeHtml(name)}]]</a>`
}

// [[name]] を inline token として登録する。生テキストの事前置換だと code span / fence 内まで
// 誤爆するが、marked の拡張なら code の tokenize が先に勝つので構造的に安全。
// name に [ ] 改行は許さない（memory slug の実態に合わせた保守的な受理）。
// ⚠️ chat 専用の Marked instance に閉じる: `marked.use()`（singleton）だと同 bundle の
// board-render.ts の描画にまで拡張が波及する（board は <a> click の open-url 配線を
// 持たないので、リンク化すると webview ごと遷移しうる — review 指摘 2026-07-31）。
const chatMarked = new Marked()
chatMarked.use({
  extensions: [
    {
      name: 'creoLink',
      level: 'inline',
      start(src: string) {
        return src.indexOf('[[')
      },
      tokenizer(src: string) {
        const m = /^\[\[([^[\]\n]+)\]\]/.exec(src)
        if (!m) return undefined
        return { type: 'creoLink', raw: m[0], name: m[1].trim() }
      },
      renderer(token) {
        return creoLinkHtml((token as unknown as { name: string }).name)
      },
    },
  ],
})

/** ChatView 本文の markdown → HTML（[[name]] は上の拡張で creo リンク化される）。 */
export function mdToHtml(text: string): string {
  // breaks: true で単一改行を <br> に変換する。marked 既定（CommonMark）は段落内の単一 \n を
  // 空白に潰すため、engine が返す改行が gui のチャット表示で消えていた。gfm は既定 true だが明示。
  return chatMarked.parse(text, { breaks: true, gfm: true }) as string
}

/**
 * chat メッセージ内リンクを OS ブラウザで開くための `open-url` IPC ペイロード判定（純関数 = calc）。
 *
 * http(s) の href なら tui の xterm と同じ `open-url` IPC の JSON 文字列を返し、それ以外
 * （相対 / `file:` / `javascript:` / 空）は null を返す。非 http(s) を絶対に通さない一次弾き —
 * webview に `file://` 等を開かせないための多層防御（scheme 検証の SSOT は Rust 側 terminal.rs、
 * ここは webview 内遷移を止めるための前段）。terminal.rs と揃えて小文字 scheme を前方一致で見る。
 */
export function linkOpenPayload(href: string): string | null {
  if (!href.startsWith('http://') && !href.startsWith('https://')) return null
  return JSON.stringify({ t: 'open-url', url: href })
}

function ThinkingBlock(props: { text: string; active: () => boolean; liner?: boolean }) {
  const [open, setOpen] = createSignal(false)
  return (
    <div class="conversation-thinking">
      <button
        class="conversation-thinking-toggle"
        classList={{ live: props.active() }}
        onClick={() => setOpen(!open())}
      >
        <span class="conversation-thinking-caret" classList={{ open: open() }}>
          ▸
        </span>
        {/* active 中はラベルを shimmer で光らせる（考え中の質感）。 */}
        <span class="conversation-thinking-label">thinking</span>
        {/* 木の中では冒頭 1 行を添える（doc 57 §6-3 — 畳んだまま思考の流れが読める）。 */}
        <Show when={props.liner && headLine(props.text)}>
          {(t) => <span class="conversation-tool-oneliner">{t()}</span>}
        </Show>
      </button>
      <Show when={open()}>
        <div class="conversation-thinking-body">{props.text}</div>
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

/** 複数行 text の先頭の意味ある 1 行（1 ライナーの正規化 / thinking 節の冒頭、doc 57 §6-3）。 */
export function headLine(s: string): string | null {
  const t = s.split('\n').find((l) => l.trim().length > 0)
  return t ? t.trim() : null
}

/**
 * path を「親 1 段 + basename」へ短縮する純関数（doc 57 §6-1）。
 * 末尾（ファイル名）が情報の主役で幅は有限 — full path は展開した input 詳細が持つ。
 * 区切りは `/` と `\` の両対応（Windows path が来ても basename 側を残す。ellipsis は
 * 末尾を削るので、無短縮だと肝心のファイル名から欠ける）。
 */
export function shortenPath(p: string): string {
  const seg = p.split(/[\\/]/).filter((s) => s.length > 0)
  return seg.length <= 2 ? seg.join('/') : seg.slice(-2).join('/')
}

/**
 * tool の 1 ライナーを input から導く純関数（doc 57 §4.4 の表駆動）。
 *
 * ピルの情報密度の源: tool 名だけでは「Bash ✓」の壁になる（doc 57 §1）。null = 出せる
 * 情報がない → 呼び出し側は従来どおり名前のみ表示。長さの clamp は CSS ellipsis に任せ、
 * ここでは複数行入力を先頭の意味ある 1 行（headLine — 空行はスキップ）に正規化する。
 */
export function toolOneLiner(name: string, input: unknown): string | null {
  const firstLine = headLine
  if (typeof input === 'string') return firstLine(input)
  if (input === null || typeof input !== 'object') return null
  const rec = input as Record<string, unknown>
  const str = (k: string): string | null => {
    const v = rec[k]
    return typeof v === 'string' && v.trim().length > 0 ? v.trim() : null
  }
  switch (name) {
    case 'Bash': {
      // description は CC が送る人間向け説明（日本語）。無ければ command の先頭行。
      const s = str('description') ?? str('command')
      return s ? firstLine(s) : null
    }
    case 'Edit':
    case 'Write':
    case 'Read': {
      const p = str('file_path')
      return p ? shortenPath(p) : null
    }
    case 'NotebookEdit': {
      const p = str('notebook_path') ?? str('file_path')
      return p ? shortenPath(p) : null
    }
    case 'Grep':
    case 'Glob':
      return str('pattern')
    case 'Agent': {
      const s = str('description') ?? str('prompt')
      return s ? firstLine(s) : null
    }
    case 'Skill':
      return str('skill')
    case 'WebFetch':
      return str('url')
    case 'WebSearch':
      return str('query')
    default: {
      // mcp__* / その他: 最初の意味ある string field（表に無い tool の best-effort）。
      // ⚠️「最初」= JSON の serialize 順。server の serde_json は BTreeMap（preserve_order
      // 無効）なので現状 alphabetical — 呼び出し時の引数順ではない。将来 preserve_order が
      // feature unification で有効化されると無音で挿入順に変わる（表示だけの best-effort
      // なので許容だが、選ばれる field が変わったらまずここを疑う）。
      for (const v of Object.values(rec)) {
        if (typeof v === 'string' && v.trim().length > 0) return firstLine(v)
      }
      return null
    }
  }
}

/** activity item（塊 = 木の対象、doc 57 §4.1）。text / prompt（HITL）は区切り側 = 幹に残る。 */
function isActivityItem(it: ChatItem): boolean {
  return it.kind === 'tool' || it.kind === 'thinking'
}

/** activity run（tool/thinking の連続塊）における位置の役割（doc 57 §4.1）。 */
export type ActivityRunRole =
  | { role: 'plain' } // run 化しない → 従来描画（単発 tool / thinking のみの塊 / 非対象 kind）
  | { role: 'head'; run: ChatItem[] } // run 先頭 → 塊全体を ActivityTree 1 本で描く
  | { role: 'member' } // head に吸収（非描画）

/**
 * items[idx] が属する activity run での役割を返す純粋関数（doc 57 §4.1）。
 *
 * classifyToolRun の一般化。同じく**描画時のみ**の集約で reducer は触らない（§C2 不変）。
 * root 化は「長さ ≥2 かつ tool を 1 つ以上含む」run のみ:
 * - 単発 tool に root を被せると行が増えるだけ（従来の ToolRow のまま）
 * - thinking だけの塊は作業でなく思考の流れ → 従来の ThinkingBlock のまま
 */
export function classifyActivityRun(items: ChatItem[], idx: number): ActivityRunRole {
  const it = items[idx]
  if (!it || !isActivityItem(it)) return { role: 'plain' }
  let start = idx
  while (start - 1 >= 0 && isActivityItem(items[start - 1])) start--
  let end = idx
  while (end + 1 < items.length && isActivityItem(items[end + 1])) end++
  const run = items.slice(start, end + 1)
  if (run.length < 2 || !run.some((r) => r.kind === 'tool')) return { role: 'plain' }
  return idx === start ? { role: 'head', run } : { role: 'member' }
}

/** ms → `45s` / `3m12s` / `1h02m`（完了 root の経過時間表示、doc 57 §4.2 / §6-2）。 */
export function fmtElapsed(ms: number): string {
  const sec = Math.max(0, Math.round(ms / 1000))
  if (sec < 60) return `${sec}s`
  const m = Math.floor(sec / 60)
  if (m < 60) return `${m}m${String(sec % 60).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}m`
}

/** 塊の root 行に出す集約 status（doc 57 §4.2）。 */
export type ActivityStatus = {
  running: boolean
  doneTools: number
  totalTools: number
  agents: number
  anyError: boolean
  /** 走行中の「現在」の 1 行 = 未 done の最新 tool（無ければ末尾 item）。完了時は null。 */
  liner: string | null
  /** 完了時のみ。live で時刻が揃った塊だけ（replay 混じりは null = 測っていない時間を出さない）。 */
  elapsedMs: number | null
}

/**
 * activity run の root 行の集約を導く純粋関数（doc 57 §4.2）。
 *
 * toolGroupStatus と同じ「エンジン状態を偽らない」規律: 1 件でも未 done なら running。
 * tailStreaming = 塊が items 末尾かつ turn 進行中 — tool 間の thinking 中も「作業中」を
 * 維持し、途中で ✓ に落として嘘をつかない。
 */
export function activityRunStatus(run: ChatItem[], tailStreaming = false): ActivityStatus {
  const tools = run.filter((r): r is ToolItem => r.kind === 'tool')
  const doneTools = tools.filter((t) => t.done).length
  const running = doneTools < tools.length || tailStreaming
  const agents = tools.filter((t) => t.name === 'Agent').length
  const anyError = tools.some((t) => t.error)
  let liner: string | null = null
  if (running) {
    const current = [...tools].reverse().find((t) => !t.done)
    if (current) liner = toolOneLiner(current.name, current.input) ?? current.name
    else {
      const last = run[run.length - 1]
      if (last)
        liner =
          last.kind === 'thinking'
            ? headLine(last.text)
            : last.kind === 'tool'
              ? (toolOneLiner(last.name, last.input) ?? last.name)
              : null
    }
  }
  let elapsedMs: number | null = null
  if (!running && run.length > 0) {
    const first = run[0]
    const t0 = first.kind === 'tool' || first.kind === 'thinking' ? first.at : undefined
    const ends = tools.map((t) => t.doneAt)
    if (t0 !== undefined && ends.length > 0 && ends.every((e) => e !== undefined)) {
      elapsedMs = Math.max(...(ends as number[]), t0) - t0
    }
  }
  return { running, doneTools, totalTools: tools.length, agents, anyError, liner, elapsedMs }
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
    <div class="conversation-tool-detail">
      <div class="conversation-tool-detail-label">{props.label}</div>
      <pre class="conversation-tool-detail-body">{clamped().text}</pre>
      <Show when={clamped().omitted > 0}>
        <div class="conversation-tool-detail-omitted">…{clamped().omitted} 文字省略</div>
      </Show>
    </div>
  )
}

/** subagent（Agent の子）の発話列。role ごとにラベルを付けて縦に積む。 */
function SubagentBlock(props: { entries: SubagentEntry[] }) {
  return (
    <div class="conversation-tool-detail">
      <div class="conversation-tool-detail-label">subagent</div>
      <For each={props.entries}>
        {(e) => {
          const clamped = createMemo(() => clampToolDetail(e.text))
          return (
            <div class="conversation-subagent-entry" classList={{ [e.role]: true }}>
              <span class="conversation-subagent-role">{e.role}</span>
              <pre class="conversation-tool-detail-body">{clamped().text}</pre>
              <Show when={clamped().omitted > 0}>
                <div class="conversation-tool-detail-omitted">…{clamped().omitted} 文字省略</div>
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
  const oneLiner = createMemo(() => toolOneLiner(props.name, props.input))
  const hasDetail = createMemo(
    () => inputText() !== null || resultText() !== null || subagent().length > 0,
  )
  return (
    <div class="conversation-tool" classList={{ done: props.done, error: props.error }}>
      <button
        class="conversation-tool-head"
        classList={{ clickable: hasDetail() }}
        onClick={() => hasDetail() && setOpen(!open())}
      >
        <Show when={hasDetail()}>
          <span class="conversation-thinking-caret" classList={{ open: open() }}>
            ▸
          </span>
        </Show>
        <span class="conversation-tool-spinner" />
        <span class="conversation-tool-icon">
          <CreoIcon name="ph:wrench" size={11} />
        </span>
        <span class="conversation-tool-name">{props.name}</span>
        <Show when={oneLiner()}>
          {(t) => <span class="conversation-tool-oneliner">{t()}</span>}
        </Show>
        <span class="conversation-tool-status">
          {props.error ? 'error' : props.done ? '✓' : '実行中…'}
        </span>
      </button>
      <Show when={open() && hasDetail()}>
        <div class="conversation-tool-body">
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
 * 既定は畳んだ状態: header が「wrench + {name} ×{count} {status}」で進捗を要約する。in-flight 中は
 * spinner + 完了数「{done}/{count}」を出し（畳んだままでも何本終わったかが分かる）、全 tool が
 * 終わると ✓（1 件でも error なら error）に変わる。展開で個別 ToolRow を並べる。
 * props.tools は reactive accessor（run は末尾に伸び、各 tool の done/error も後から変異する）。
 */
function ToolGroupRow(props: { name: string; tools: Accessor<ToolItem[]> }) {
  const [open, setOpen] = createSignal(false)
  const count = () => props.tools().length
  const status = () => toolGroupStatus(props.tools())
  const anyError = () => props.tools().some((t) => t.error)
  // 代表 = 先頭の 1 ライナー（doc 57 §4.4）。run は同名なので先頭が種類を代表できる。
  const headLiner = createMemo(() => {
    const first = props.tools()[0]
    return first ? toolOneLiner(first.name, first.input) : null
  })
  return (
    <div
      class="conversation-toolgroup"
      classList={{ done: !status().running && !anyError(), error: !status().running && anyError() }}
    >
      <button class="conversation-toolgroup-toggle" onClick={() => setOpen(!open())}>
        <span class="conversation-thinking-caret" classList={{ open: open() }}>
          ▸
        </span>
        <Show when={status().running}>
          <span class="conversation-tool-spinner" />
        </Show>
        <span class="conversation-tool-icon">
          <CreoIcon name="ph:wrench" size={11} />
        </span>
        <span class="conversation-tool-name">{props.name}</span>
        <span class="conversation-toolgroup-count">×{count()}</span>
        <Show when={headLiner()}>
          {(t) => <span class="conversation-tool-oneliner">{t()} ほか</span>}
        </Show>
        <span class="conversation-tool-status">{status().label}</span>
      </button>
      <Show when={open()}>
        <div class="conversation-toolgroup-body">
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

/** 塊（activity run）の開閉の記憶。key = 先頭 tool の id — component 再生成や replay の
 *  再畳み込みを跨いで user の選択を保つ（doc 57 §4.5 表示所有権）。 */
const activityOpen = new Map<string, boolean>()

/**
 * activity run（tool/thinking の連続塊）を 1 root の木に畳む（doc 57 §4.2-4.3、P2 本丸）。
 *
 * 走行中は畳んだまま root が「現在の 1 ライナー + {done}/{total}」を生更新し、完了で
 * 「✓ N tools · M agent · 経過」に収束する。既定 = 畳み。user が開いたら stream 追記でも
 * turn 完了でも閉じない（表示所有権は user、システムは初期値だけ — doc 55 継承）。
 * 中身は既存部品の再利用: 同名連続 = ToolGroupRow / 単発 = ToolRow / thinking = ThinkingBlock。
 */
function ActivityTree(props: { run: Accessor<ChatItem[]>; tailStreaming: () => boolean }) {
  // 先頭 tool は run の存在条件（classifyActivityRun）なので必ず居る。
  const key = (props.run().find((i) => i.kind === 'tool') as ToolItem | undefined)?.id ?? ''
  const [open, setOpenRaw] = createSignal(activityOpen.get(key) ?? false)
  const setOpen = (v: boolean) => {
    activityOpen.set(key, v)
    setOpenRaw(v)
  }
  const st = createMemo(() => activityRunStatus(props.run(), props.tailStreaming()))
  const doneLabel = () => {
    const s = st()
    const parts = [`${s.totalTools} tools`]
    if (s.agents > 0) parts.push(`${s.agents} agent`)
    if (s.elapsedMs !== null) parts.push(fmtElapsed(s.elapsedMs))
    return `${s.anyError ? '✗' : '✓'} ${parts.join(' · ')}`
  }
  return (
    <div
      class="conversation-activity"
      classList={{
        done: !st().running && !st().anyError,
        error: !st().running && st().anyError,
      }}
    >
      <button class="conversation-activity-head" onClick={() => setOpen(!open())}>
        <span class="conversation-thinking-caret" classList={{ open: open() }}>
          ▸
        </span>
        <Show
          when={st().running}
          fallback={<span class="conversation-activity-summary">{doneLabel()}</span>}
        >
          <span class="conversation-tool-spinner" />
          <span class="conversation-tool-oneliner">{st().liner ?? '作業中'}</span>
          <span class="conversation-activity-count">
            ({st().doneTools}/{st().totalTools}
            {st().agents > 0 ? ` · agent ${st().agents}` : ''})
          </span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="conversation-activity-body">
          <For each={props.run()}>
            {(item, i) => {
              if (item.kind === 'thinking')
                return (
                  <ThinkingBlock
                    text={item.text}
                    liner
                    active={() => props.tailStreaming() && i() === props.run().length - 1}
                  />
                )
              if (item.kind !== 'tool') return null // run は tool/thinking のみ（型の防御）
              // 塊の中でも同名連続は従来どおり ×N に畳む（doc 57 §4.3、孫 = 個別 ToolRow）。
              const role = createMemo(() => classifyToolRun(props.run(), i()))
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
            }}
          </For>
        </div>
      </Show>
    </div>
  )
}

function PlanWidget(props: { entries: Accessor<PlanEntry[]> }) {
  return (
    <Show when={props.entries().length > 0}>
      <div class="conversation-plan">
        <div class="conversation-plan-title">Plan</div>
        <For each={props.entries()}>
          {(e) => (
            <div class="conversation-plan-item" classList={{ [e.status]: true }}>
              <span class="conversation-plan-dot" />
              <span class="conversation-plan-text">
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
 * 「Other」= 自由記述の擬似選択肢を表す内部 label（Claude Code 本体の UI と同じ振る舞い）。
 *
 * ⚠️ **engine には決して送らない**。送るのは入力欄の中身そのもの（`answers` は
 * `{question → 任意文字列}` で、host が verbatim で `updatedInput` にマージするため、
 * 選択肢に無い文字列も素通しできる）。選択状態を持つためだけの sentinel。
 *
 * 値は実選択肢と衝突しないよう内部専用の prefix 付きにしてある（`label` は AI が書く
 * 表示文字列なので、素の "Other" だと AI が同名の選択肢を出したとき誤爆する）。
 */
export const OTHER_LABEL = '__vp_other__'

/**
 * 選択状態 → engine に送る回答文字列（純関数、calculation）。
 *
 * - `OTHER_LABEL` は sentinel なので `otherText` の中身へ置換する（前後の空白は落とす）。
 * - 空になった要素は捨てる（Other を選んだだけで未入力のとき、空文字を混ぜない）。
 * - multiSelect は `", "` 結合の単一 string（回答 wire 形は doc §8 で未決のため保守的な形）。
 *
 * 戻り値が空文字 = **未回答**。呼び手はこれで確定可否を判定する（Other を選んだだけの
 * 状態を「答えた」と見なすと、AI には空欄が回答として届いてしまう）。
 */
export function resolveAnswer(
  labels: string[],
  otherText: string,
  multiSelect: boolean,
): string {
  const kept = labels
    .map((l) => (l === OTHER_LABEL ? otherText.trim() : l))
    .filter((l) => l !== '')
  return multiSelect ? kept.join(', ') : (kept[0] ?? '')
}

/**
 * PromptCard（doc 35 §4）— HITL 質問（AskUserQuestion 横取り）の選択肢 UI。
 *
 * 各 question を見出し + 選択肢ボタンで描く。single-select は radio（クリックで置換）、
 * multiSelect は toggle（複数選択）。全質問に選択が付いたら「確定」で `answers` を組んで
 * onAnswer に渡す（親が conversation:respond を送り、カードを回答済み表示へ折りたたむ）。
 *
 * 選択肢の `description` は**可視要素として描く**（旧実装は `title` = tooltip のみで、
 * 選択の判断材料が hover しないと読めなかった。型は Rust `QuestionOption.description` から
 * 既に運ばれていたので、描画だけの欠落だった）。
 *
 * 末尾には「Other」を足し、選ぶと自由記述欄が開く（cc 本体と同じ）。加えて「キャンセル」で
 * 質問自体を取り下げられる（`behavior:"deny"` レール = PR3 で tool 承認用に敷かれた既存経路を
 * 質問側へ引き戻したもの）。
 */
function PromptCard(props: {
  item: Extract<ChatItem, { kind: 'prompt' }>
  onAnswer: (requestId: string, answers: Record<string, string>) => void
  onCancel: (requestId: string) => void
}) {
  // 各質問の選択（label 配列）。single は 1 要素、multi は複数。
  const [sel, setSel] = createSignal<Record<string, string[]>>({})
  // Other を選んだ質問の自由記述（question → text）。
  const [other, setOther] = createSignal<Record<string, string>>({})

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

  /** その質問の回答文字列（組み立ては純関数 [`resolveAnswer`] に委譲）。 */
  const answerOf = (q: QuestionSpec): string =>
    resolveAnswer(sel()[q.question] ?? [], other()[q.question] ?? '', !!q.multi_select)

  // 全質問が回答済みなら確定可。Other 選択時は**入力が空だと未回答扱い**
  // （選んだだけで空文字を送ると、AI には「答えた」と見えてしまうため）。
  const canConfirm = (): boolean =>
    props.item.questions.every((q) => answerOf(q) !== '')

  const confirm = () => {
    if (!canConfirm()) return
    const answers: Record<string, string> = {}
    for (const q of props.item.questions) {
      answers[q.question] = answerOf(q)
    }
    props.onAnswer(props.item.requestId, answers)
  }

  return (
    <div class="conversation-prompt" classList={{ answered: props.item.answered }}>
      <Show
        when={!props.item.answered}
        fallback={
          <div class="conversation-prompt-answered">
            {/* キャンセル済みは回答行を出さない（答えていないので空欄が並ぶだけになる）。 */}
            <Show
              when={props.item.decision !== 'deny'}
              fallback={<div class="conversation-prompt-cancelled">キャンセルしました</div>}
            >
              <For each={props.item.questions}>
                {(q) => (
                  <div class="conversation-prompt-arow">
                    <span class="conversation-prompt-ahead">{q.header}</span>
                    <span class="conversation-prompt-aval">{props.item.answers?.[q.question] ?? ''}</span>
                  </div>
                )}
              </For>
            </Show>
          </div>
        }
      >
        <For each={props.item.questions}>
          {(q) => (
            <div class="conversation-prompt-q">
              <div class="conversation-prompt-header">{q.header}</div>
              <div class="conversation-prompt-question">{q.question}</div>
              <div class="conversation-prompt-options">
                <For each={q.options}>
                  {(opt) => (
                    <button
                      class="conversation-prompt-opt"
                      classList={{ selected: isSelected(q, opt.label) }}
                      onClick={() => toggle(q, opt.label)}
                    >
                      <span class="conversation-prompt-opt-label">{opt.label}</span>
                      {/* description は選択の判断材料なので可視で描く（tooltip では読まれない）。 */}
                      <Show when={opt.description}>
                        <span class="conversation-prompt-opt-desc">{opt.description}</span>
                      </Show>
                    </button>
                  )}
                </For>
                {/* Other = 自由記述（cc 本体と同じ末尾配置）。選ぶと下に入力欄が開く。 */}
                <button
                  class="conversation-prompt-opt other"
                  classList={{ selected: isSelected(q, OTHER_LABEL) }}
                  onClick={() => toggle(q, OTHER_LABEL)}
                >
                  <span class="conversation-prompt-opt-label">Other</span>
                  <span class="conversation-prompt-opt-desc">自分で書く</span>
                </button>
              </div>
              <Show when={isSelected(q, OTHER_LABEL)}>
                <input
                  class="conversation-prompt-other-input"
                  type="text"
                  placeholder="回答を入力…"
                  value={other()[q.question] ?? ''}
                  onInput={(e) =>
                    setOther((prev) => ({ ...prev, [q.question]: e.currentTarget.value }))
                  }
                  // Enter で確定（全質問が埋まっている時のみ。IME 変換確定の Enter は
                  // isImeKeystroke で判別 — WKWebView は keyCode 229 で来る）。
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !isImeKeystroke(e) && canConfirm()) {
                      e.preventDefault()
                      confirm()
                    }
                  }}
                />
              </Show>
            </div>
          )}
        </For>
        <div class="conversation-prompt-actions">
          <button class="conversation-prompt-confirm" disabled={!canConfirm()} onClick={confirm}>
            確定
          </button>
          {/* 質問自体の取り下げ。engine には deny + 理由が返り、AI は別の進め方を探れる。 */}
          <button
            class="conversation-prompt-cancel"
            onClick={() => props.onCancel(props.item.requestId)}
          >
            キャンセル
          </button>
        </div>
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
    <div class="conversation-prompt" classList={{ answered: props.item.answered }}>
      <Show
        when={!props.item.answered}
        fallback={
          <div class="conversation-prompt-answered">
            <span class="conversation-prompt-ahead">{perm().toolName}</span>
            <span class="conversation-prompt-aval">
              {props.item.decision === 'deny' ? '✗ 却下' : '✓ 許可'}
            </span>
          </div>
        }
      >
        <div class="conversation-prompt-header">tool 承認</div>
        <div class="conversation-prompt-question">
          <code class="conversation-perm-tool">{perm().toolName}</code> の実行を許可しますか？
        </div>
        <div class="conversation-perm-input">{inputSummary()}</div>
        <div class="conversation-perm-actions">
          <button
            class="conversation-perm-allow"
            onClick={() => props.onDecide(props.item.requestId, 'allow')}
          >
            許可
          </button>
          <button
            class="conversation-perm-deny"
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
    <div class="conversation-prompt conversation-plan-card" classList={{ answered: props.item.answered }}>
      <Show
        when={!props.item.answered}
        fallback={
          <div class="conversation-prompt-answered">
            <span class="conversation-prompt-ahead">plan</span>
            <span class="conversation-prompt-aval">
              {props.item.decision === 'deny' ? '✗ 却下' : '✓ 承認'}
            </span>
          </div>
        }
      >
        <div class="conversation-prompt-header">plan 承認</div>
        <div class="conversation-plan-body" innerHTML={mdToHtml(planText())} />
        <div class="conversation-perm-actions">
          <button
            class="conversation-perm-allow"
            onClick={() => props.onDecide(props.item.requestId, 'allow')}
          >
            承認して実行
          </button>
          <button
            class="conversation-perm-deny"
            onClick={() => props.onDecide(props.item.requestId, 'deny')}
          >
            却下
          </button>
        </div>
      </Show>
    </div>
  )
}

// model / permission picker の選択肢は **server の catalog**（LaneSessionEntryWire →
// session list の model_choices / permission_choices）から並べる。client にリストを
// hardcode しない（mako 裁定 2026-07-27 — engine ごとに catalog を持ち、model の成長 /
// 多 engine 化に client 改修なしで追従する。chat_capable と同じ server 能力表明の一般化）。

/**
 * session 名札（pane 上端） — **term / chat 共通**（doc 50 §4.6 A6 ②）。
 *
 * この pane が「何であるか」= session の素性を名乗る 1 行。全 pane が同じ顔で名乗ることで
 * 「どれが root か」が一目で読める（3 pane 並ぶと名札が無い pane は識別不能になる —
 * 2026-07-25 実機で mako が踏んだ）。
 *
 * 載せるもの（doc 50 §2「上段 = この pane が何であるか」）:
 *  - 灯（slot 注入。**chat 固有** — term は ConversationEvent stream を持たないので出さない）
 *  - session ラベル / root chip / 会話 id = 素性
 *  - kind badge = 切り替え先（click で `session_set_mode` → in-place 変身。表示は現在形でなく行き先）
 *  - ✕ = この session を閉じる（root は不可）
 *
 * 供給は `sessionsOf(lane)`（`conversation_session_list` の cache）— term / chat どちらの pane でも
 * 同じ 1 本の真実源から引く。
 */
export function SessionPlate(props: {
  lane: string
  session: number
  /** この pane の見え方。badge の表示と切替先を決める。 */
  mode: 'tui' | 'gui'
  focused: boolean
  /** 活動の灯（chat のみ。term は供給が無いので省略 = 描かない）。 */
  lamp?: JSX.Element
}) {
  const info = (): ConversationSession | undefined =>
    sessionsOf(props.lane)?.sessions.find((s) => s.key === props.session)
  const label = (): string => `${sessionChipPrefix(info()?.agent)}#${props.session}`
  /** badge を押した時の切替先（今の見え方の逆）。 */
  const target = (): 'tui' | 'gui' => (props.mode === 'gui' ? 'tui' : 'gui')
  /** badge を押せるか（= 切替先に行けるか）。 */
  const canSwitchMode = (): boolean => canSwitchTo(target(), info()?.chat_capable)

  return (
    <div class="conversation-session-plate" classList={{ focused: props.focused }}>
      {props.lamp}
      <span class="conversation-session-plate-label">{label()}</span>
      {/* root = lane の代表（mailbox / pid、doc 40 §4-1）。素性なので名札に出す —
          これが無いと「なぜこの pane だけ × が無いのか」（root は close 不可）が読めない。 */}
      <Show when={info()?.root}>
        <span
          class="conversation-session-plate-root"
          title="root session（lane の代表 — 閉じられない。素に戻すのは sidebar の Reset Lane）"
        >
          <CreoIcon name="ph:anchor-simple" size={10} />
          root
        </span>
      </Show>
      <Show when={info()?.engine_session_id}>
        {(sid) => <span class="conversation-session-plate-sid">{sid().slice(0, 8)}</span>}
      </Show>
      {/* focus は **chat の概念**（replay demand の宛先。送信はどの pane からも可）。
          term pane は focus を World B が持たない（keyboard focus は xterm 側）ので、
          この hint を出すと「押しても何も起きない」誤誘導になる — chat のときだけ出す。 */}
      <Show when={props.mode === 'gui' && !props.focused}>
        <span class="conversation-session-plate-hint">click で focus</span>
      </Show>
      <span class="conversation-session-plate-spacer" />
      {/* kind badge（doc 50 §4.6 A6 ②）: 押すと切り替わる**行き先**を見せる（Chat pane には
          「Console」、Console pane には「Chat」— 現在形でなく目的地。mako 裁定 2026-07-27）。
          click で session_set_mode → repo が resume handoff → **同じ往復路**が別の面として
          立ち上がる（位置と share は renamePane が保つ = in-place 変身）。
          ⚠️ term 側にも必ず出すこと — chat pane が 0 枚になると gui へ戻る入口が消える
          （2026-07-25 に実際に片道ドアを作った）。

          切替できない session（shell 等、gui host を持たない engine）は **押せる見た目を
          出さない** — 押しても server に弾かれるだけの行き止まりになる（2026-07-25 実機で
          「押しても無言」を踏んだ）。可否の判定は server が送る `chat_capable` 一本で、
          engine 名の型分岐は client に持たせない（§4.6 ② の能力表引き）。 */}
      <Show
        when={canSwitchMode()}
        fallback={
          <span
            class="conversation-session-plate-kind static"
            title={`${info()?.agent ?? 'この engine'} は Chat（gui）の受け口を持ちません`}
          >
            <CreoIcon name="ph:terminal-window" size={9} />
            Console
          </span>
        }
      >
        <button
          type="button"
          class="conversation-session-plate-kind"
          title={
            target() === 'tui'
              ? 'Console（tui）に切り替える — 会話はそのまま resume で続く'
              : 'Chat（gui）に切り替える — 会話はそのまま resume で続く'
          }
          onClick={(e) => {
            e.stopPropagation()
            requestSessionMode(props.lane, props.session, target())
          }}
        >
          <CreoIcon
            name={target() === 'gui' ? 'ph:chat-circle' : 'ph:terminal-window'}
            size={9}
          />
          {target() === 'gui' ? 'Chat' : 'Console'}
        </button>
      </Show>
      <Show when={canCloseSession(sessionsOf(props.lane)?.sessions.length ?? 0, info()?.root)}>
        <button
          type="button"
          class="conversation-session-plate-close"
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
  )
}

/** 1 枚 = 1 session の chat pane（doc 46 §1.5 session ↔ Pane 1:1）。(lane, session) は mount 時に
 *  固定 — lane 切替は pane host ごと作り直す（lane-panes が dispose → mount）。
 *  doc 50 P2: chat 動詞（submit / respond / perm / interrupt / model）は session を運ぶ =
 *  どの pane からも打てる（model の旧 focused 制限は conversation_set_model の session 化で
 *  撤去 — 2026-07-27、mako 裁定「model も permission も session に紐づく」）。 */
function SessionChatView(props: { lane: string; session: number }) {
  const lc = laneChat(props.lane, props.session)
  const state = (): ChatState => lc.state
  /** この pane が lane の focused session か（= chat 動詞の宛先か）。 */
  const isFocused = (): boolean => (sessionsOf(props.lane)?.focused ?? 1) === props.session
  // 名札まわり（label / root chip / 会話 id / badge / ✕）は `SessionPlate` に移管した
  // （doc 50 §4.6 A6 — term pane と共有するため）。

  // gui モデル切替（spec: セッション進行中でも切替可能）。repo が engine を --resume +
  // 新 --model で入れ替える = 会話コンテキスト継続でモデル交換。適用の視覚確認は
  // 新 engine の session_init が header.model を更新することで得る（picker は実測値に追従）。
  // streaming 中は disable — engine drop が進行中 turn を切るのを UI で抑止する。
  const currentModel = (): string => state()?.header?.model ?? ''
  /** この session の roster entry（picker の catalog / intent の供給源 = server 能力表明）。 */
  const rosterEntry = (): ConversationSession | undefined =>
    sessionsOf(props.lane)?.sessions.find((s) => s.key === props.session)
  /** server catalog + 実測 model の動的追加（一覧に無い実測値は option を足して真実を見せる）。
   *  catalog 空 = この engine は VP から切替不可（picker を出さず read-only 表示に落とす）。 */
  const modelChoices = (): ReadonlyArray<PickerChoice> => {
    const catalog = rosterEntry()?.model_choices ?? []
    const m = currentModel()
    return m && catalog.length > 0 && !catalog.some((c) => c.value === m)
      ? [...catalog, { value: m, label: m }]
      : catalog
  }
  const permissionChoices = (): ReadonlyArray<PickerChoice> =>
    rosterEntry()?.permission_choices ?? []
  const setModel = (model: string) => {
    const lane = props.lane
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    // session 明示（doc 50 session=Pane — model は session 単位、2026-07-27 に root/lane
    // 単位から移行。focused 制限も同時に消えた = どの pane も自分の session を切替できる）。
    ipc?.postMessage(
      JSON.stringify({
        t: 'conversation:set_model',
        lane,
        session: props.session,
        model: model || null,
      }),
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
      JSON.stringify({ t: 'conversation:set_permission_mode', lane, session: props.session, mode }),
    )
  }

  // context ゲージ（tui statusline の bar :context 相当）。分子分母が揃うまで非表示。
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

  // ---- slash command 補完（判断は slash.ts、ここは配線だけ）-------------------
  /** 候補一覧。空 = palette を出さない（行頭が `/` でない / 引数を打ち始めた / 一致なし）。 */
  const slashHits = () => {
    const q = slashQuery(draft())
    if (q === null) return []
    return filterSlashCommands(state()?.slashCommands ?? [], q)
  }
  const [slashAt, setSlashAt] = createSignal(0)
  const slashOpen = () => slashHits().length > 0
  /** 候補を入力欄へ入れる。⚠️ 送信はしない — 引数を続けて打てるようにする。 */
  const acceptSlash = (name: string) => {
    setDraft(applyCompletion(name))
    setSlashAt(0)
    queueMicrotask(() => {
      if (!inputRef) return
      inputRef.focus()
      const end = inputRef.value.length
      inputRef.setSelectionRange(end, end)
      autosize(inputRef)
    })
  }
  // history 最下部の常時 status バー。全イベント同期 + 無反応(hang)検出のため 1s 毎に now を更新。
  const [nowMs, setNowMs] = createSignal(Date.now())
  onMount(() => {
    const id = setInterval(() => setNowMs(Date.now()), 1000)
    onCleanup(() => clearInterval(id))
  })
  const statusLine = () => deriveStatus(state(), nowMs())
  // 灯 3 状態（doc 51 §1 A2）: status の畳み込み。名札の dot が読む。
  const lamp = () => lampOf(statusLine())
  // now-line（doc 51 §1 A3）: 名札直下の「今なにを」。null = 行ごと描かない。
  const nowLine = () => deriveNowLine(state())
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
      JSON.stringify({ t: 'conversation:submit', lane, session: props.session, prompt: text }),
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
      JSON.stringify({ t: 'conversation:interrupt', lane: props.lane, session: props.session }),
    )
  }

  // doc 35 PR1: PromptCard 回答。カードを回答済み表示へ折りたたみ、conversation:respond で repo に戻す
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
        t: 'conversation:respond', lane, session: props.session, request_id: requestId, answers,
      }),
    )
  }

  // 質問自体の取り下げ。PR3 で tool 承認用に敷かれた `behavior:"deny"` レールをそのまま使う
  //（repo 側 `handle_conversation_respond` は deny を種別non-依存で受けるので Rust 変更は不要）。
  // message は engine に渡り、AI は「聞くのをやめた」ことを踏まえて別の進め方を選べる。
  const cancelPrompt = (requestId: string) => {
    const lane = props.lane
    lc.set(
      produce((s) => {
        const it = s.items.find((i) => i.kind === 'prompt' && i.requestId === requestId)
        if (it && it.kind === 'prompt') {
          it.answered = true
          it.decision = 'deny'
        }
      }),
    )
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(
      JSON.stringify({
        t: 'conversation:respond', lane, session: props.session, request_id: requestId,
        behavior: 'deny',
        message: 'ユーザーが質問をキャンセルしました（回答なしで進めてください）',
      }),
    )
  }

  // doc 35 PR3: permission 承認/却下。カードを decision 表示へ折りたたみ、conversation:respond {behavior} で戻す。
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
        t: 'conversation:respond', lane, session: props.session, request_id: requestId, behavior,
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

  // marked 描画済み HTML 内の <a> クリックを conversation-stream の 1 listener で捌く（イベント委譲 =
  // メッセージ毎に listener を張らない）。default では webview 内遷移（SPA が localhost リンクで
  // 飛ぶ事故）になるので preventDefault で止め、http(s) は tui の xterm と同じ `open-url` IPC で
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
  // chat 非表示時（tui 表示中 = streamEl が display:none 配下 → offsetParent=null）は
  // 一切介入せず、xterm 等にキーを渡す。
  const onDocKey = (e: KeyboardEvent): void => {
    if (!streamEl || streamEl.offsetParent === null) return // chat 非表示 → 素通し
    const key = e.key
    // doc 35 §5: Esc で走行中 turn を中断（作文中の textarea では抑制 = Home/End と同じ棲み分け）。
    if (key === 'Escape') {
      const inTextarea = document.activeElement?.classList.contains('conversation-input-box') ?? false
      if (!inTextarea && state()?.streaming) {
        interrupt()
        e.preventDefault()
      }
      return
    }
    if (key !== 'Home' && key !== 'End' && key !== 'PageUp' && key !== 'PageDown') return
    const inTextarea = document.activeElement?.classList.contains('conversation-input-box') ?? false
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
      class="chat-view"
      classList={{ focused: isFocused() }}
      onClick={() => {
        if (!isFocused()) focusChatSession(props.lane, props.session)
      }}
    >
      {/* session 名札（pane 上端）: この pane = この session の素性。doc 46 §1.3 の帰結で
          タブ strip は撤去 — session の識別は pane 自身が名乗り、切替は pane click が担う。
          engine 選択付きの新規作成は LaneHeader（lane の名札）の「+ New」一本
          （doc 46 P2 の canonical 入口。旧・下端の帯は doc 51 §1 A1 で退役）。
          実体は term pane と共有する `SessionPlate`（doc 50 §4.6 A6 — 全 pane が同じ顔で
          名乗る。灯だけは chat 固有なので slot で渡す）。 */}
      <SessionPlate
        lane={props.lane}
        session={props.session}
        mode="gui"
        focused={isFocused()}
        lamp={
          /* 灯 3 状態（doc 51 §1 A2）: 動いている（緑脈動）/ 待っている（無灯）/ あなたが要る
             （赤速脈動）。旧「live なら緑点」を置換 — presence でなく活動を灯す（lampOf）。
             細かい状態語は下段の status 行が持つ（灯は横目の認知、文字は精読の認知）。
             term pane はこの供給（ConversationEvent stream）を持たないので灯を出さない。 */
          <span
            class="conversation-lamp"
            classList={{ run: lamp() === 'run', need: lamp() === 'need' }}
            title={statusLine().label}
          />
        }
      />
      {/* now-line（doc 51 §1 A3）: 名札（素性・不変）と区別された「今」の帯。名札の直下。
          供給 = 質問要旨 > 契約（A3b）> 機械導出（A3a 保険）。空なら描かない（doc 50 §2）。 */}
      <Show when={nowLine()}>
        {(line) => (
          <div class="conversation-now-line" title={line()}>
            {line()}
          </div>
        )}
      </Show>
              <PlanWidget entries={() => state().plan} />
        <div
          class="conversation-stream"
          ref={streamEl}
          tabindex={0}
          onScroll={onStreamScroll}
          onClick={onStreamLinkClick}
        >
          <For each={state().items}>
            {(item, index) => {
              if (item.kind === 'thinking' || item.kind === 'tool') {
                // doc 57 P2: tool/thinking の連続塊は ActivityTree 1 本に畳む。plain（単発 tool /
                // thinking のみの塊）は従来描画。items/index を reactive に読むので stream 追記で
                // plain→head へ昇格する（classifyToolRun と同じ手筋）。
                const arole = createMemo(() => classifyActivityRun(state().items, index()))
                // 塊が items 末尾かつ turn 進行中 = tool 間の thinking 中も「作業中」を維持。
                const tailStreaming = () => {
                  const r = arole()
                  return (
                    r.role === 'head' &&
                    r.run[r.run.length - 1] === state().items[state().items.length - 1] &&
                    state().streaming
                  )
                }
                return (
                  <Switch>
                    <Match when={arole().role === 'member'}>{null}</Match>
                    <Match when={arole().role === 'head'}>
                      <ActivityTree
                        run={() => (arole() as { role: 'head'; run: ChatItem[] }).run}
                        tailStreaming={tailStreaming}
                      />
                    </Match>
                    <Match when={true}>
                      {(() => {
                        if (item.kind === 'thinking')
                          return (
                            <ThinkingBlock
                              text={item.text}
                              // 末尾 thinking かつ turn 進行中 = 今まさに考え中 → shimmer。
                              active={() =>
                                index() === state().items.length - 1 && state().streaming
                              }
                            />
                          )
                        // 塊にならなかった tool は常に単発（連続 2 本以上は必ず run になる）。
                        return (
                          <ToolRow
                            name={item.name}
                            done={item.done}
                            error={item.error}
                            input={item.input}
                            result={item.result}
                            subagent={item.subagent}
                          />
                        )
                      })()}
                    </Match>
                  </Switch>
                )
              }
              if (item.kind === 'prompt') {
                if (!item.permission)
                  return <PromptCard item={item} onAnswer={answerPrompt} onCancel={cancelPrompt} />
                return item.permission.toolName === 'ExitPlanMode' ? (
                  <PlanCard item={item} onDecide={decidePlan} />
                ) : (
                  <PermissionCard item={item} onDecide={decidePrompt} />
                )
              }
              return (
                <div class="conversation-msg" classList={{ user: item.kind === 'user' }}>
                  <div class="conversation-msg-body" innerHTML={mdToHtml(item.text)} />
                </div>
              )
            }}
          </For>
          <Show when={state().streaming}>
            <div class="conversation-cursor" />
          </Show>
          <Show when={state().pending}>
            <div
              class="conversation-msg user pending"
              classList={{ editable: canEditPending(), locked: !canEditPending() }}
              onClick={editPending}
              title={
                canEditPending()
                  ? 'クリックで入力欄に戻して編集'
                  : '編集するには入力欄を空にしてください'
              }
            >
              <div class="conversation-msg-body" innerHTML={mdToHtml(state().pending!)} />
              <span class="conversation-pending-badge">
                {canEditPending() ? '送信待ち · クリックで編集' : '送信待ち · turn 完了後に送信'}
              </span>
            </div>
          </Show>
        </div>
        {/* status bar — **入力の上**（stream に隣接）。engine が今何をしているかの読み取り専用の
            計器で、操作は持たない。context 残量も「読み取り」なのでここ。 */}
        <div
          class={`conversation-status s-${statusLine().kind}`}
          classList={{ stalled: statusLine().stalled }}
        >
          <span class="conversation-status-dot" />
          <span class="conversation-status-label">{statusLine().label}</span>
          <Show when={statusLine().detail}>
            <span class="conversation-status-detail">{statusLine().detail}</span>
          </Show>
          <Show when={statusLine().stalled}>
            <span class="conversation-status-stalled">反応無 {statusLine().idleSec}s</span>
          </Show>
          <Show when={statusLine().lastEvent}>
            <span class="conversation-status-event">· {statusLine().lastEvent}</span>
          </Show>
          <Show when={statusLine().pending}>
            <span class="conversation-status-pending">
              <CreoIcon name="ph:pencil-simple" size={11} /> 送信待ち
            </span>
          </Show>
          <Show when={ctxPct() !== null}>
            <span
              class="conversation-context"
              classList={{ warn: ctxPct()! >= 60, crit: ctxPct()! >= 85 }}
              title={ctxTitle()}
            >
              <span class="conversation-context-bar">
                <span class="conversation-context-fill" style={{ width: `${ctxPct()}%` }} />
              </span>
              <span class="conversation-context-pct">{ctxPct()}%</span>
            </span>
          </Show>
        </div>
        {/* composer — 入力とその操作を 1 つの器にまとめる。上 = 打つ場所、下 = 操作。
            model / permission も「送る前に決める操作」なのでここ（読み取りの status とは分ける）。 */}
        <div class="conversation-composer">
          {/* slash command の候補。⚠️ **入力欄の上**に出す（下は model / permission の操作列で、
              そこに被せると押そうとした物が入れ替わる）。source は session_init が広告した
              一覧そのもの — CLI 側で「この経路で打てるもの」に絞り込み済み。 */}
          <Show when={slashOpen()}>
            <div class="conversation-slash">
              <For each={slashHits().slice(0, 12)}>
                {(name, i) => (
                  <button
                    type="button"
                    class="conversation-slash-item"
                    classList={{ at: i() === Math.min(slashAt(), slashHits().length - 1) }}
                    // ⚠️ mousedown で拾う。click だと先に textarea の blur が走り、
                    // 選ぶ前に palette が閉じて空振りする。
                    onMouseDown={(e) => {
                      e.preventDefault()
                      acceptSlash(name)
                    }}
                  >
                    <span class="conversation-slash-name">/{name}</span>
                    {/* ⚠️ 説明は**引けた候補にだけ**付く（160 中 86）。無い側が欠けて
                        見えないよう、名前と同じ行に流し込み、幅は内容任せにする。 */}
                    <Show when={state()?.commandDocs?.[name]}>
                      <span class="conversation-slash-desc">
                        {state()?.commandDocs?.[name]}
                      </span>
                    </Show>
                  </button>
                )}
              </For>
              <Show when={slashHits().length > 12}>
                <span class="conversation-slash-more">ほか {slashHits().length - 12} 件</span>
              </Show>
            </div>
          </Show>
          {/* 既定は **1 行**。打った分だけ scrollHeight に合わせて伸び、max-height で頭打ち
              （CSS だけでは textarea は内容に追随しないので、伸縮はここで行う）。 */}
          <textarea
            ref={inputRef}
            class="conversation-input-box"
            rows={1}
            placeholder="メッセージを入力（Enter で送信 / Shift+Enter で改行）"
            value={draft()}
            onInput={(e) => {
              setDraft(e.currentTarget.value)
              autosize(e.currentTarget)
            }}
            onKeyDown={(e) => {
              // ⚠️ **IME の判定を最初に**置く。palette も送信も、変換中の打鍵に反応しては
              // いけない（変換中の ↑↓ は候補選択、Enter は確定）。
              if (isImeKeystroke(e)) return

              // slash palette が開いている間は、移動 / 確定 / 中断を先に食う。
              // ⚠️ 開いていない時は**素通り**させる — 下の送信規約を変えない。
              if (slashOpen()) {
                const hits = slashHits()
                if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                  e.preventDefault()
                  setSlashAt(moveSelection(slashAt(), e.key === 'ArrowDown' ? 1 : -1, hits.length))
                  return
                }
                // Tab / Enter で確定。⚠️ Shift+Enter は改行のままにする（下の規約と揃える）。
                if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) {
                  e.preventDefault()
                  const pick = hits[Math.min(slashAt(), hits.length - 1)]
                  if (pick) acceptSlash(pick)
                  return
                }
                if (e.key === 'Escape') {
                  // 候補を畳むだけ（入力は消さない）。`/` を消して閉じるより手数が少ない。
                  e.preventDefault()
                  setSlashAt(0)
                  setDraft(`${draft()} `)
                  return
                }
              }

              // Enter = 送信（Claude Desktop と同じ既定。mako 裁定 2026-07-30 — ⌘Enter 送信は
              // 「送信するつもりで空行が入る」誤操作が多かった）。
              if (e.key !== 'Enter') return
              // Shift+Enter = 改行（textarea の既定挙動に任せる）。
              if (e.shiftKey) return
              // 素の Enter / ⌘Enter / Ctrl+Enter = 送信（⌘Enter は旧来の筋肉記憶を残す）。
              e.preventDefault()
              submit()
            }}
          />
          <div class="conversation-actions">
            {/* model picker: catalog（server 能力表明）が非空の engine だけ出す。
                空 + 実測 model あり = read-only 表示（「今どの model か」の情報は保ちつつ、
                押しても server に弾かれる行き止まりを作らない）。 */}
            <Show
              when={modelChoices().length > 0}
              fallback={
                <Show when={currentModel()}>
                  <span
                    class="conversation-model-readonly"
                    title="model は engine 側で選択します（VP からは切替不可）"
                  >
                    {currentModel()}
                  </span>
                </Show>
              }
            >
              <select
                class="conversation-model-select"
                disabled={state().streaming}
                title="model（この session に適用 — 会話は resume で継続したまま入れ替わる）"
                onChange={(e) => setModel(e.currentTarget.value)}
              >
                <For each={modelChoices()}>
                  {(c) => (
                    <option value={c.value} selected={c.value === currentModel()}>
                      {c.label}
                    </option>
                  )}
                </For>
              </select>
            </Show>
            {/* permission picker: 同じく catalog 駆動（claude は TUI と同一表記の英語 4 mode）。
                空 = 対話承認の概念なし → 出さない。 */}
            <Show when={permissionChoices().length > 0}>
              <select
                class="conversation-model-select"
                title="permission mode（この session に適用。表記は TUI と同一）"
                onChange={(e) => setPermissionMode(e.currentTarget.value)}
              >
                <For each={permissionChoices()}>
                  {(c) => (
                    <option value={c.value} selected={currentPermMode() === c.value}>
                      {c.label}
                    </option>
                  )}
                </For>
              </select>
            </Show>
            <div class="conversation-actions-spacer" />
            <Show when={state().streaming}>
              <button class="conversation-stop" onClick={interrupt} title="turn を中断 (Esc)">
                <CreoIcon name="ph:stop" size={11} /> 停止
              </button>
            </Show>
            <button class="conversation-send" onClick={submit} disabled={!draft().trim()}>
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

/** ChatView の scoped CSS。entry.tsx が `<style>` で注入する（board-render.ts の style 注入と同型）。
 *  色は creo-ui token（--color-* 系）に寄せ、無い環境でも読める fallback を持つ。 */
export const CHATVIEW_CSS = `
/* chat Live Token (--chat-text-*) の定義は :root に置く。適用 (use site) は .chat-view 以下に
   閉じているので他 pane を汚染しない。:root 定義にする理由 = creo-ui Editor Mode
   (entry.tsx ChatTokenBinds) の slider が documentElement.style.setProperty で書くため、
   より近い祖先に定義があると「近い祖先の定義が勝つ」で書き込みがマスクされる
   (sidebar Shell.tsx の --sb-text-* と同型)。 */
:root{--chat-text-body:15px;--chat-text-user:13.5px;--chat-text-tool:12px;--chat-text-meta:11px;--chat-text-micro:10px;}
.chat-view { position:absolute; inset:0; display:flex; flex-direction:column;
  background: var(--color-bg, #0f1115); color: var(--color-text, #e6e9ef);
  font-family: var(--vp-font-sans),var(--typography-family-sans); overflow:hidden; }
.conversation-empty { margin:auto; color: var(--color-text-tertiary, #616b80); font-size:13px; }
.conversation-stream { flex:1; overflow-y:auto; padding:16px 18px; display:flex; flex-direction:column; gap:12px; }
/* スクロールバー常時表示（mako 2026-07-24）: 既定の overlay scrollbar は「スクロール中だけ」
   なので現在地が読めない。custom style を当てると常時表示になる（WebKit 仕様）。細く控えめに。 */
.conversation-stream::-webkit-scrollbar { width:8px; }
.conversation-stream::-webkit-scrollbar-track { background:transparent; }
.conversation-stream::-webkit-scrollbar-thumb { background:var(--color-border,#2a3040); border-radius:4px; }
.conversation-stream::-webkit-scrollbar-thumb:hover { background:var(--color-text-tertiary,#8b93a7); }
/* history は tabindex=0 で focus 可能（Home/End/PgUp/PgDn 用）。領域全体を囲む outline は
   目障りなので抑制する（focus 合図は scrollbar 操作で十分伝わる）。 */
.conversation-stream:focus, .conversation-stream:focus-visible { outline:none; }
.conversation-msg { max-width:100%; animation: conversation-fade .18s ease-out; }
/* user bubble も左寄せ（mako 裁定 2026-07-31: 横幅方向に表示物を動かさない —
   全要素が左端に揃い、視線が左右にジャンプしない）。しっぽは左下へ。
   assistant との見分けは背景色・枠・幅 80% が担う */
.conversation-msg.user { align-self:flex-start; background: var(--color-accent-soft, #1c2333);
  border:1px solid var(--color-border, #2a3040); border-radius:12px 12px 12px 3px; padding:8px 13px; max-width:80%; }
/* §5.1: 送信待ち type-ahead。半透明 + 破線で「まだ送っていない」を伝える。 */
.conversation-msg.user.pending { opacity:.62; border-style:dashed; transition: opacity .12s ease, border-color .12s ease; }
/* dequeue-to-composer: composer が空なら「クリックで入力欄に戻して編集」可（hover で明るく）。 */
.conversation-msg.user.pending.editable { cursor:pointer; }
.conversation-msg.user.pending.editable:hover { opacity:.9; border-color: var(--color-accent, #e2b96f); }
/* composer に打ちかけ下書きがある間は編集不可 = グレーアウト（下書きを潰さないための MVP ガード）。 */
.conversation-msg.user.pending.locked { opacity:.38; cursor:not-allowed; }
.conversation-pending-badge { display:block; margin-top:4px; font-size:10.5px; color: var(--color-text-tertiary, #8b93a7); }
/* status bar（入力の上）: engine の現況の**読み取り専用**計器。操作は composer 側が持つ。 */
.conversation-status { display:flex; align-items:center; gap:8px; padding:4px 14px; min-height:24px; font-size:var(--chat-text-meta,11px);
  font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-text-tertiary,#8b93a7);
  border-top:1px solid var(--color-border,#2a3040); background: var(--color-bg,#0f1115); }
.conversation-status-dot { width:7px; height:7px; border-radius:50%; flex:none; background: var(--color-text-tertiary,#616b80); }
.conversation-status-label { letter-spacing:.03em; }
.conversation-status-detail { color: var(--color-text-secondary,#a8b0c0); }
.conversation-status-pending { color: var(--color-accent,#e2b96f); }
.conversation-status.s-streaming .conversation-status-dot { background: var(--color-success,#6fe2a8); animation: conversation-status-pulse 1.2s ease-in-out infinite; }
.conversation-status.s-thinking .conversation-status-dot { background:#8fb0ff; animation: conversation-status-pulse 1.2s ease-in-out infinite; }
.conversation-status.s-tool .conversation-status-dot { background: var(--color-accent,#e2b96f); animation: conversation-status-pulse 1.2s ease-in-out infinite; }
.conversation-status.s-awaiting .conversation-status-dot { background:#f0a3a3; animation: conversation-status-pulse .8s ease-in-out infinite; }
.conversation-status.s-error .conversation-status-dot { background:#f0a3a3; }
.conversation-status.stalled .conversation-status-dot { background:#f0a3a3 !important; animation: conversation-status-pulse .6s ease-in-out infinite; }
.conversation-status-stalled { color:#f0a3a3; font-weight:600; }
.conversation-status-event { color: var(--color-text-tertiary,#616b80); opacity:.65; }
@keyframes conversation-status-pulse { 50% { opacity:.32; } }
.conversation-msg-body { font-size:var(--chat-text-user,13.5px); line-height:1.6; word-break:break-word; }
/* 返信（assistant）の本文だけ拡大 = --chat-text-body（自分の入力バブルは --chat-text-user のまま）。
   line-height は unitless なので font-size に追従してスケールする。 */
.conversation-msg:not(.user) .conversation-msg-body { font-size:var(--chat-text-body,15px); }
.conversation-msg-body :first-child { margin-top:0; } .conversation-msg-body :last-child { margin-bottom:0; }
/* [[name]] creo リンク: wiki 記法をそのまま見せつつ踏める（破線下線 = 記法由来のリンクの合図）。 */
.conversation-creo-link { color: var(--color-accent,#3b82f6); text-decoration:none;
  border-bottom:1px dashed color-mix(in srgb, var(--color-accent,#3b82f6), transparent 40%); }
.conversation-creo-link:hover { border-bottom-style:solid; }
.conversation-msg-body pre { background: var(--color-bg-elevated, #16191f); border:1px solid var(--color-border,#2a3040);
  border-radius:8px; padding:10px 12px; overflow-x:auto; font-size:var(--chat-text-tool,12px); }
.conversation-msg-body code { font-family: var(--vp-font-mono),var(--typography-family-mono); }
.conversation-thinking { align-self:flex-start; font-size:var(--chat-text-tool,12px); }
.conversation-thinking-toggle { background:none; border:none; color: var(--color-text-tertiary,#8b93a7);
  cursor:pointer; font-size:var(--chat-text-tool,12px); padding:2px 0; display:flex; align-items:center; gap:5px; }
.conversation-thinking-caret { transition: transform .15s ease; display:inline-block; }
.conversation-thinking-caret.open { transform: rotate(90deg); }
.conversation-thinking-label { display:inline-block; }
/* active（末尾 thinking かつ turn 進行中）: 文字を gradient sweep で shimmer させ「考え中」を伝える。 */
.conversation-thinking-toggle.live .conversation-thinking-label {
  background: linear-gradient(100deg, var(--color-text-tertiary,#8b93a7) 30%,
    var(--color-text,#e6e9ef) 50%, var(--color-text-tertiary,#8b93a7) 70%);
  background-size: 220% 100%; -webkit-background-clip:text; background-clip:text;
  -webkit-text-fill-color:transparent; color:transparent;
  animation: conversation-shimmer 1.5s linear infinite; }
.conversation-thinking-body { margin:4px 0 0 16px; padding:8px 12px; border-left:2px solid var(--color-border,#2a3040);
  color: var(--color-text-secondary,#a8b0c0); white-space:pre-wrap; font-size:var(--chat-text-tool,12px); line-height:1.55; }
/* ToolRow: tool 1 件。container / head(pill 1 行) / body(詳細) の 3 層は toolgroup と同型。 */
.conversation-tool { align-self:flex-start; font-size:var(--chat-text-tool,12px); animation: conversation-fade .18s ease-out; }
.conversation-tool-head { display:flex; align-items:center; gap:8px; width:100%; text-align:left;
  font-family:inherit; font-size:var(--chat-text-tool,12px);
  color: var(--color-text-secondary,#a8b0c0); background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--color-border,#2a3040); border-radius:8px; padding:5px 11px; }
/* tool 種の印: Phosphor wrench（旧 🔧 絵文字 — SVG は currentColor 継承で done/error の
   色変化に追従し、絵文字のようにプラットフォーム差の出る字形にならない）。 */
.conversation-tool-icon { display:inline-flex; align-items:center; flex:none; }
/* 詳細を持つ tool だけ押せる（持たない行は見た目そのまま・無反応）。 */
.conversation-tool-head.clickable { cursor:pointer; }
.conversation-tool-spinner { width:9px; height:9px; border-radius:50%; border:1.5px solid var(--color-accent,#3b82f6);
  border-top-color: transparent; animation: conversation-spin .7s linear infinite; }
.conversation-tool.done .conversation-tool-spinner, .conversation-tool.error .conversation-tool-spinner { display:none; }
.conversation-tool.done .conversation-tool-head { color: var(--color-text-tertiary,#616b80); }
.conversation-tool.error .conversation-tool-head { color:#f0a3a3; }
.conversation-tool-name { font-family: var(--vp-font-mono),var(--typography-family-mono); flex:none; }
/* 1 ライナー（doc 57 §4.4）: 情報密度の源。幅が尽きたら ellipsis（clamp は CSS の仕事）。 */
.conversation-tool-oneliner { min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;
  color: var(--color-text-tertiary,#8b93a7); }
.conversation-tool-status { margin-left:auto; flex:none; font-size:var(--chat-text-meta,11px); }
/* 展開部: thinking-body と同じ左罫線の入れ子表現で input / result を積む。 */
.conversation-tool-body { display:flex; flex-direction:column; gap:6px; margin:5px 0 0 16px;
  padding-left:8px; border-left:2px solid var(--color-border,#2a3040); }
.conversation-tool-detail-label { font-size:var(--chat-text-micro,10px); letter-spacing:.06em; text-transform:uppercase;
  color: var(--color-text-tertiary,#616b80); margin-bottom:2px; }
.conversation-tool-detail-body { margin:0; max-height:260px; overflow:auto; white-space:pre-wrap;
  word-break:break-word; font-family: var(--vp-font-mono),var(--typography-family-mono);
  font-size:var(--chat-text-meta,11px); line-height:1.5; color: var(--color-text-secondary,#a8b0c0); }
.conversation-tool-detail-omitted { font-size:var(--chat-text-micro,10px); color: var(--color-text-tertiary,#616b80); margin-top:2px; }
/* subagent の発話: role でラベル分け。thinking は親の thinking と同じ「控えめ」の質感に寄せる。 */
.conversation-subagent-entry { margin-top:4px; }
.conversation-subagent-role { font-size:9px; letter-spacing:.06em; text-transform:uppercase;
  color: var(--color-text-tertiary,#616b80); border:1px solid var(--color-border,#2a3040);
  border-radius:4px; padding:0 4px; }
.conversation-subagent-entry.thinking .conversation-tool-detail-body { color: var(--color-text-tertiary,#616b80); font-style:italic; }
.conversation-subagent-entry.prompt .conversation-tool-detail-body { color: var(--color-text-tertiary,#8b93a7); }
/* ActivityTree（doc 57 P2）: 塊の root + 展開 body。root は tool 行と同じ質感の 1 行。 */
.conversation-activity { align-self:flex-start; max-width:100%; font-size:var(--chat-text-tool,12px);
  animation: conversation-fade .18s ease-out; }
.conversation-activity-head { display:flex; align-items:center; gap:8px; max-width:100%; text-align:left;
  cursor:pointer; font-family:inherit; font-size:var(--chat-text-tool,12px);
  color: var(--color-text-secondary,#a8b0c0); background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--color-border,#2a3040); border-radius:8px; padding:5px 11px; }
/* 走行中の root の 1 ライナーは主役なので secondary（子行の tertiary より一段立てる）。 */
.conversation-activity-head .conversation-tool-oneliner { color: var(--color-text-secondary,#a8b0c0); }
.conversation-activity.done .conversation-activity-head { color: var(--color-text-tertiary,#616b80); }
.conversation-activity.error .conversation-activity-head { color:#f0a3a3; }
.conversation-activity-count { flex:none; font-size:var(--chat-text-meta,11px);
  font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-text-tertiary,#8b93a7); }
/* 展開 body: thinking-body / tool-body と同じ左罫線の入れ子表現。 */
.conversation-activity-body { display:flex; flex-direction:column; gap:6px; margin:5px 0 0 16px;
  padding-left:8px; border-left:2px solid var(--color-border,#2a3040); }
/* ToolGroupRow: 連続同名 tool（Agent ×N 等）を畳む accordion。畳んだ header は ToolRow と同じ枠で 1 行。 */
.conversation-toolgroup { align-self:flex-start; font-size:var(--chat-text-tool,12px); animation: conversation-fade .18s ease-out; }
.conversation-toolgroup-toggle { display:flex; align-items:center; gap:8px; width:100%; cursor:pointer;
  font-size:var(--chat-text-tool,12px); color: var(--color-text-secondary,#a8b0c0); background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--color-border,#2a3040); border-radius:8px; padding:5px 11px; }
.conversation-toolgroup.done .conversation-toolgroup-toggle { color: var(--color-text-tertiary,#616b80); }
.conversation-toolgroup.error .conversation-toolgroup-toggle { color:#f0a3a3; }
.conversation-toolgroup-count { font-family: var(--vp-font-mono),var(--typography-family-mono);
  color: var(--color-text-tertiary,#8b93a7); font-size:var(--chat-text-meta,11px); }
/* 展開部: 個別 ToolRow を段付きで縦に並べる（thinking-body と同じ左罫線の入れ子表現）。 */
.conversation-toolgroup-body { display:flex; flex-direction:column; gap:5px; margin:5px 0 0 16px;
  padding-left:8px; border-left:2px solid var(--color-border,#2a3040); }
.conversation-cursor { width:7px; height:15px; background: var(--color-accent,#3b82f6); border-radius:1px;
  animation: conversation-blink 1s step-start infinite; align-self:flex-start; }
/* PromptCard（doc 35 §4）: HITL 質問。engine が人を待っている合図として左寄せカードで settle。 */
.conversation-prompt { align-self:flex-start; max-width:88%; display:flex; flex-direction:column; gap:12px;
  padding:13px 15px; border-radius:12px; background: var(--color-bg-elevated,#16191f);
  border:1px solid var(--sb-conn-hitl,#FF4A2D); box-shadow:0 0 0 1px color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 78%);
  animation: conversation-fade .18s ease-out; }
.conversation-prompt.answered { border-color: var(--color-border,#2a3040); box-shadow:none; opacity:.9; }
.conversation-prompt-q { display:flex; flex-direction:column; gap:7px; }
.conversation-prompt-header { font-size:10px; text-transform:uppercase; letter-spacing:.08em;
  color: var(--sb-conn-hitl,#FF4A2D); }
.conversation-prompt-question { font-size:14px; line-height:1.5; color: var(--color-text,#e6e9ef); }
/* description を可視で描くため、選択肢は横並びの pill から縦積みカードへ。
   1 行に詰めると説明文が読めず、結局 tooltip と同じ（= 読まれない）になる。 */
.conversation-prompt-options { display:flex; flex-direction:column; gap:6px; }
.conversation-prompt-opt { display:flex; flex-direction:column; gap:3px; text-align:left; width:100%;
  font-size:12.5px; padding:8px 13px; border-radius:8px; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg,#0f1115);
  color: var(--color-text-secondary,#a8b0c0); transition: border-color .15s ease, background .15s ease, color .15s ease; }
.conversation-prompt-opt:hover { border-color: var(--color-text-tertiary,#616b80); color: var(--color-text,#e6e9ef); }
.conversation-prompt-opt.selected { border-color: var(--sb-conn-hitl,#FF4A2D); color: var(--color-text,#e6e9ef);
  background: color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 86%); }
.conversation-prompt-opt-label { font-weight:500; }
.conversation-prompt-opt-desc { font-size:11.5px; line-height:1.45; color: var(--color-text-tertiary,#616b80); }
.conversation-prompt-opt.selected .conversation-prompt-opt-desc { color: var(--color-text-secondary,#a8b0c0); }
/* Other = 自由記述。選択肢ではなく「逃げ道」なので破線で地味に置く。 */
.conversation-prompt-opt.other { border-style:dashed; }
.conversation-prompt-other-input { width:100%; box-sizing:border-box; padding:7px 11px; font-size:12.5px;
  border-radius:8px; border:1px solid var(--sb-conn-hitl,#FF4A2D); background: var(--color-bg,#0f1115);
  color: var(--color-text,#e6e9ef); font-family:inherit; }
.conversation-prompt-other-input:focus { outline:none; box-shadow:0 0 0 2px color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 80%); }
.conversation-prompt-actions { display:flex; align-items:center; justify-content:flex-end; gap:8px; }
.conversation-prompt-confirm { padding:7px 16px; font-size:12.5px; border-radius:8px;
  border:none; cursor:pointer; background: var(--sb-conn-hitl,#FF4A2D); color:#fff; }
.conversation-prompt-confirm:disabled { opacity:.4; cursor:default; }
/* キャンセルは destructive ではなく「答えずに進む」なので、確定より一段弱い見た目に。 */
.conversation-prompt-cancel { padding:7px 14px; font-size:12.5px; border-radius:8px; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background:transparent; color: var(--color-text-tertiary,#616b80); }
.conversation-prompt-cancel:hover { color: var(--color-text-secondary,#a8b0c0); border-color: var(--color-text-tertiary,#616b80); }
.conversation-prompt-cancelled { font-size:12.5px; color: var(--color-text-tertiary,#616b80); }
/* 回答済み: 見出し + 選んだ値だけの静かな折りたたみ表示。 */
.conversation-prompt-answered { display:flex; flex-direction:column; gap:5px; }
.conversation-prompt-arow { display:flex; gap:9px; align-items:baseline; font-size:12.5px; }
.conversation-prompt-ahead { font-size:10px; text-transform:uppercase; letter-spacing:.06em;
  color: var(--color-text-tertiary,#616b80); min-width:0; }
.conversation-prompt-aval { color: var(--color-text,#e6e9ef); font-weight:500; }
.conversation-plan { border-bottom:1px solid var(--color-border,#2a3040); padding:10px 18px; background: var(--color-bg-elevated,#13161c); }
.conversation-plan-title { font-size:10px; text-transform:uppercase; letter-spacing:.08em; color: var(--color-text-tertiary,#616b80); margin-bottom:6px; }
.conversation-plan-item { display:flex; align-items:center; gap:8px; font-size:12.5px; padding:2px 0; transition: color .2s ease; }
.conversation-plan-dot { width:7px; height:7px; border-radius:50%; background: var(--color-text-tertiary,#616b80); transition: background .2s ease; }
.conversation-plan-item.in_progress { color: var(--color-text,#e6e9ef); } .conversation-plan-item.in_progress .conversation-plan-dot { background: var(--color-accent,#e2b96f); }
.conversation-plan-item.completed { color: var(--color-text-tertiary,#616b80); } .conversation-plan-item.completed .conversation-plan-dot { background: var(--color-success,#6fe2a8); }
.conversation-plan-item.completed .conversation-plan-text { text-decoration: line-through; }
/* composer: 入力（上）と操作（下）を 1 つの器に。枠は器が持ち、textarea は枠なしで中に敷く。 */
/* slash command の候補列。⚠️ 会話本文より前に出る唯一の overlay なので、
   composer の器の中に収めて「入力の一部」に見せる（別の面に見せない）。 */
/* ⚠️ 説明が付いた時点で横並びは破綻する（1 件が長くなり折り返しが荒れる）ので**縦積み**。
   説明の無い候補が混ざっても、名前だけの行として自然に見える。 */
/* ⚠️ **flex:none が要る**。composer は flex-direction:column + overflow:hidden で、
   overflow-y を持つ子は flex の既定（min-height:auto が効かない）で 0 高さまで潰れ、
   そのまま親の hidden に飲まれて**何も出なくなる**（2026-08-09 に実際に踏んだ）。
   横並びだった頃は flex-wrap が高さを内容に従わせていたので露見しなかった。
   ⚠️ この CSS は template literal の中なので、コメントに backtick を書くと文字列が閉じる。 */
.conversation-slash { flex:none; display:flex; flex-direction:column; gap:1px;
  padding:6px 6px 2px; max-height:180px; overflow-y:auto; }
.conversation-slash-item { display:flex; align-items:baseline; gap:8px; width:100%;
  padding:3px 8px; border:none; border-radius:6px; cursor:pointer; text-align:left;
  background:transparent; color:var(--lg-mute,#5C7A85); font:inherit; font-size:12px;
  transition:background .1s ease,color .1s ease; }
.conversation-slash-name { font-family:var(--font-mono,ui-monospace,monospace); flex:none; }
/* 説明は一段沈める。長い時は 1 行で切る（候補行の高さを揃えて走査しやすくする）。 */
.conversation-slash-desc { flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis;
  white-space:nowrap; font-size:11px; color:var(--lg-mute-2,#38525b); }
.conversation-slash-item:hover { background:#ffffff12; color:var(--lg-hot,#EAFBFF); }
/* 選択中。keyboard で動かしている位置を hover と別に示す（両方同時に見えてよい）。 */
.conversation-slash-item.at { background:var(--lg-cyan-dim,#1C6C7C); color:var(--lg-hot,#EAFBFF); }
.conversation-slash-more { align-self:center; font-size:11px; color:var(--lg-mute-2,#38525b); }
.conversation-composer { display:flex; flex-direction:column; margin:8px 14px 10px; border-radius:10px;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#161a20);
  overflow:hidden; }
.conversation-composer:focus-within { border-color: var(--color-accent,#3b82f6); }
/* 操作の行（入力の下）: 左 = 送る前に決める設定、右 = 実行。 */
.conversation-actions { display:flex; align-items:center; gap:6px; padding:4px 6px 5px 8px; }
.conversation-actions-spacer { flex:1; }
/* 既定 1 行（min-height は置かず、rows=1 + autosize が高さを決める）。伸びる上限だけ CSS が持つ。 */
.conversation-input-box { flex:1; resize:none; max-height:160px; padding:8px 10px 4px; font-size:13px; line-height:1.5;
  font-family: var(--vp-font-sans),var(--typography-family-sans); color: var(--color-text,#e6e9ef);
  /* 枠と地色は composer(器) が持つ — textarea 自身は素で敷く（二重枠にしない）。 */
  background:transparent; border:none; outline:none; }
.conversation-send { display:inline-flex; align-items:center; gap:4px; padding:4px 11px; font-size:12px;
  border-radius:7px; border:none; cursor:pointer; background: var(--color-accent,#3b82f6); color:#fff; }
.conversation-send:disabled { opacity:.4; cursor:default; }
.conversation-stop { display:inline-flex; align-items:center; gap:4px; padding:4px 10px; font-size:12px;
  border-radius:7px; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background:transparent; color: var(--color-text-secondary,#a8b0c0); }
.conversation-stop:hover { border-color:#f0a3a3; color:#f0a3a3; }
/* Mode 切替（見え方の乗り換え = 避難路）は LaneHeader の root picker「見え方」行へ
   （doc 51 §2 — 旧 lane-level Mode toggle と下端の帯は doc 51 §1 A1 で退役）。 */
/* session 名札（pane 上端）: この pane = この session の素性。tab strip（doc 38 仮置き）の
   後継 — session ↔ Pane 1:1（doc 46 §1.5 / doc 50 P1）で pane 自身が名乗る。
   Pane 共通の名札 token（--vp-nameplate-*）に乗せて、全 pane の上端と同じ見えにする。 */
.conversation-session-plate { display:flex; align-items:center; gap:6px; flex:none;
  height:calc(var(--vp-nameplate-h) - 4px); padding:0 var(--vp-nameplate-pad-x);
  font-size:10.5px; font-family: var(--vp-font-mono),var(--typography-family-mono);
  color: var(--color-text-tertiary,#8b93a7); background: var(--vp-nameplate-bg);
  border-bottom: var(--vp-nameplate-border); user-select:none; }
.conversation-session-plate.focused { color: var(--color-text-secondary,#a8b0c0); }
.conversation-session-plate-label { font-weight:500; }
.conversation-session-plate-root { display:inline-flex; align-items:center; gap:2px; padding:0 5px;
  border-radius:9999px; border:1px solid var(--color-surface-border-subtle,#2a3040);
  font-size:9.5px; opacity:.8; }
.conversation-session-plate-sid { opacity:.65; }
.conversation-session-plate-hint { opacity:.5; font-family: var(--vp-font-sans),var(--typography-family-sans); }
.conversation-session-plate-spacer { flex:1; }
/* 既定 opacity .55 は暗い名札上で沈んで「削除の動線が無い」ように見えた（2026-07-24 実機）。
   常時視認できる濃さに上げ、hover で確定的に立てる。 */
.conversation-session-plate-close { flex:none; display:inline-flex; align-items:center; padding:2px 4px;
  line-height:1; border:none; border-radius:4px; background:transparent; cursor:pointer;
  color: var(--color-text-secondary,#a8b0c0); opacity:.85; }
.conversation-session-plate-close:hover { opacity:1; color: var(--color-text,#e6e9ef);
  background: var(--color-bg,#0f1115); }
/* kind badge（doc 50 §4.6 A6 ②）: 押すと切り替わる行き先を見せる乗り換えボタン。
   pane の素性に関わる操作なので名札（上段）に住む。§2.1 の規律で名札は静かに保ち、
   hover で操作可能だと分かる程度に立てる（root chip と同じ pill 形。あちらは表示専用、
   こちらは押せる = hover の差で区別する）。 */
.conversation-session-plate-kind { flex:none; display:inline-flex; align-items:center; gap:3px;
  padding:1px 6px; border-radius:9999px; cursor:pointer;
  border:1px solid var(--color-surface-border-subtle,#2a3040); background:transparent;
  font-size:9.5px; font-family:inherit; color: var(--color-text-tertiary,#8b93a7); opacity:.8; }
.conversation-session-plate-kind:hover { opacity:1; color: var(--color-text,#e6e9ef);
  border-color: var(--color-accent,#3b82f6); background: var(--color-bg,#0f1115); }
/* 切替できない session（gui host を持たない engine）の kind 表示。素性としては出すが
   **押せる見た目を出さない**（cursor / hover を持たない = 行き止まりに誘わない、§4.6 ②）。 */
.conversation-session-plate-kind.static { cursor:default; opacity:.55; }
/* focus されていない pane は全体をわずかに沈める（どこに打てるかを一目で）。 */
.chat-view:not(.focused) { opacity:.82; }
/* 灯 3 状態（doc 51 §1 A2）: 動いている = 緑・脈動 / 待っている = 無灯（地の色の点）/
   あなたが要る = 赤・速い脈動。脈動の速さが緊急度を運ぶ（mock workbench-v2 の視覚言語）。 */
.conversation-lamp { width:7px; height:7px; border-radius:50%; flex:none;
  background: var(--color-border,#2a3040); }
.conversation-lamp.run { background: var(--color-success,#6fe2a8);
  animation: conversation-lamp-pulse 1.2s ease-in-out infinite; }
.conversation-lamp.need { background: var(--color-error,#f0a3a3);
  animation: conversation-lamp-pulse .7s ease-in-out infinite; }
@keyframes conversation-lamp-pulse { 50% { opacity:.3; } }
@media (prefers-reduced-motion: reduce){ .conversation-lamp { animation: none !important; } }
/* now-line（doc 51 §1 A3）: 名札直下の「今なにを」動的一行。名札（素性）より一段引いた地色で
   「変わる情報」であることを見せる（mock workbench-v2 の now-line と同じ階調）。 */
.conversation-now-line { flex:none; padding:2px 10px 2px 8px; font-size:10.5px;
  color: var(--color-text-tertiary,#8b93a7);
  font-family: var(--vp-font-sans),var(--typography-family-sans);
  background: color-mix(in srgb, var(--vp-nameplate-bg,#141622) 55%, var(--color-bg,#0f1115));
  border-bottom: 1px solid var(--color-border,#2a3040);
  white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.conversation-now-line::before { content:"▸ "; color: var(--color-text-quaternary,#616b80); }
/* 旧 .lane-header（model/perm の独立行）は計器盤へ畳んで撤去。select は下段の高さに収まる
   よう一段小さくする（行が status と共用になったため）。 */
.conversation-model-select { font-size:10.5px; padding:1px 5px; border-radius:6px; outline:none; cursor:pointer;
  border:1px solid var(--color-border,#2a3040); background: var(--color-bg-elevated,#16191f);
  color: var(--color-text-secondary,#a8b0c0); font-family:inherit; }
.conversation-model-select:disabled { opacity:.45; cursor:default; }
/* catalog 空 engine の read-only model 表示（select と同じ枠感、押せない見た目 = cursor/border なし）。 */
.conversation-model-readonly { font-size:10.5px; padding:1px 5px; border-radius:6px;
  color: var(--color-text-secondary,#a8b0c0); opacity:.7; }
/* PR3: permission 承認カード（allow/deny）。question カードと同じ枠、action だけ差し替え。 */
.conversation-perm-tool { font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-accent,#e2b96f); }
.conversation-perm-input { font-family: var(--vp-font-mono),var(--typography-family-mono); font-size:11.5px;
  color: var(--color-text-tertiary,#8b93a7); background: var(--color-bg,#0f1115); border:1px solid var(--color-border,#2a3040);
  border-radius:6px; padding:6px 9px; margin:6px 0; overflow-x:auto; white-space:pre-wrap; word-break:break-all; }
.conversation-perm-actions { display:flex; gap:8px; margin-top:8px; }
.conversation-perm-allow, .conversation-perm-deny { font-size:12.5px; padding:6px 16px; border-radius:8px; cursor:pointer; border:1px solid var(--color-border,#2a3040); }
.conversation-perm-allow { background: var(--color-success,#6fe2a8); color:#06231a; border-color: var(--color-success,#6fe2a8); }
.conversation-perm-deny { background: var(--color-bg-elevated,#16191f); color:#f0a3a3; }
.conversation-perm-deny:hover { border-color:#f0a3a3; }
/* PR4: plan 承認カード。plan 本文は markdown で描き、accent 枠で「あなたの承認を待つ」を伝える。 */
.conversation-plan-card { border-color: var(--color-accent,#e2b96f); }
.conversation-plan-body { font-size:13px; line-height:1.6; max-height:280px; overflow-y:auto; margin:6px 0;
  padding:8px 10px; background: var(--color-bg,#0f1115); border:1px solid var(--color-border,#2a3040); border-radius:6px; }
.conversation-plan-body :first-child { margin-top:0; } .conversation-plan-body :last-child { margin-bottom:0; }
/* context ゲージ（tui statusline の bar :context 相当）。ヘッダー右端に寄せる。 */
/* status bar の右端へ寄せる（読み取り計器の並びの末尾）。 */
.conversation-context { margin-left:auto; display:flex; align-items:center; gap:6px; }
.conversation-context-bar { width:52px; height:5px; border-radius:3px; overflow:hidden;
  background: var(--color-bg,#0f1115); border:1px solid var(--color-border,#2a3040); }
.conversation-context-fill { display:block; height:100%; border-radius:2px;
  background: var(--color-success,#6fe2a8); transition: width .3s ease, background .3s ease; }
.conversation-context-pct { font-size:10.5px; min-width:32px; text-align:right;
  font-family: var(--vp-font-mono),var(--typography-family-mono); color: var(--color-text-tertiary,#8b93a7); }
.conversation-context.warn .conversation-context-fill { background: var(--color-accent,#e2b96f); }
.conversation-context.crit .conversation-context-fill { background: #f0a3a3; }
.conversation-context.crit .conversation-context-pct { color: #f0a3a3; }
@keyframes conversation-fade { from { opacity:0; transform: translateY(4px); } to { opacity:1; transform:none; } }
@keyframes conversation-spin { to { transform: rotate(360deg); } }
@keyframes conversation-blink { 50% { opacity:0; } }
@keyframes conversation-shimmer { from { background-position: 220% 0; } to { background-position: -120% 0; } }
@media (prefers-reduced-motion: reduce) {
  .conversation-msg, .conversation-tool, .conversation-prompt { animation:none; }
  .conversation-tool-spinner { animation-duration: 1.5s; } .conversation-cursor { animation:none; opacity:.6; }
  /* motion off: shimmer は止めるが text-fill:transparent のままだと消えるので色を戻す。 */
  .conversation-thinking-toggle.live .conversation-thinking-label { animation:none; background:none;
    -webkit-text-fill-color: currentColor; color: var(--color-text,#e6e9ef); }
}
`

export type ChatViewApi = {
  /** lane を active にする（初出なら vpConsole renderer を attach + session 一覧を取得）。 */
  showLane(lane: string): void
  /** doc 38 §4.3: 指定 lane の再同期ローダーを明示的に下ろす（tui 切替時に entry.tsx が呼ぶ）。 */
  clearReplaying(lane: string): void
  /** chat session pane を host に mount する（lane-panes の動的 host 生成から呼ばれる）。
   *  返り値 = dispose（lane 切替 / session close で host ごと破棄する時に呼ぶ）。 */
  mountSession(host: HTMLElement, lane: string, session: number): () => void
  /** doc 50 §4.6 A6 ②: term pane（xterm）の host に名札だけを差し込む。返り値 = dispose。 */
  mountTermPlate(host: HTMLElement, lane: string, session: number): () => void
}

export function installChatView(vpConsole: VpConsole): ChatViewApi {
  // session 一覧 bus → module signal（全 SessionChatView が共有）。install は起動時 1 回。
  document.addEventListener('vp:conversation-sessions', (e) => {
    const d = (
      e as CustomEvent<{ lane: string; focused: number; sessions: ConversationSession[] }>
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
        // replay の demand は**消費者が構えたここ**からも撃つ（2026-07-24 根治の第 2 弾）。
        // Rust の subscribe 直後 demand（attach 時）だけだと、配送が bundle 読込前に届いた場合
        // `window.vpConsole && …` guard で黙って捨てられる（Rust 側に buffer なし）— session id
        // は request/response で復帰するのに会話だけ空、の非対称が起きる。renderer を張った
        // 直後なら受け手が確実に居る。attached gate で page-load ごと lane 1 回 = 切替 spam なし。
        // 二重 replay は ReplayStart の clear-prefix で収束（無害）。
        const bootIpc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
        bootIpc?.postMessage(JSON.stringify({ t: 'conversation:demand_start', lane }))
      }
      // doc 38 §4.3: 離れる lane の再同期ローダーを掃除する（replay_end 取りこぼしで stuck した
      // まま戻って来ても固着させない）。新 lane が本当に再同期するなら attach / demand の
      // replay_start が立て直す。
      const prev = activeLane()
      if (prev && prev !== lane) clearReplaying(prev)
      setActiveLane(lane)
      // doc 53 §11: roster（focused の確定 + pane の顔ぶれ）は lanes snapshot が運ぶ。
      // lane-panes / chatview は lane ごとに roster を持ち続けるので、attach 時に取り直す
      // 必要が無い（旧: ここで `echoes:sessions_fetch` を撃っていた = 供給路 2 本目の入口）。
    },
    clearReplaying,
    mountSession(host, lane, session) {
      laneChat(lane, session) // store を先に用意（mount 前に届く replay の取りこぼし防止）
      return render(() => <SessionChatView lane={lane} session={session} />, host)
    },
    mountTermPlate(host, lane, session) {
      // doc 50 §4.6 A6 ②: term pane の名札。host と中の xterm は World A の持ち物なので
      // **名札用の div を足すだけ**（xterm container には触れない — doc 33 §8）。
      // `.has-term-plate` を host に付けると World A 側の CSS が xterm を名札分下げる。
      const mount = document.createElement('div')
      mount.className = 'term-plate'
      host.appendChild(mount)
      host.classList.add('has-term-plate')
      const dispose = render(
        () => (
          <SessionPlate
            lane={lane}
            session={session}
            mode="tui"
            // term pane は focus 状態を World B が持たない（keyboard focus は xterm 側）。
            // 名札の focus 強調は chat の「打つ宛先」を示すものなので、term では常に false。
            focused={false}
          />
        ),
        mount,
      )
      return () => {
        dispose()
        mount.remove()
        host.classList.remove('has-term-plate')
      }
    },
  }
}
