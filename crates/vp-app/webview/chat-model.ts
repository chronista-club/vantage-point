/**
 * chat model — ConversationEvent を UI 単位（ChatItem 列 + 派生状態）に畳む純粋層。
 *
 * ⚠️ **SolidJS / DOM / IPC に依存しない**（棚卸し 2026-09-07 項目 4 で chatview.tsx から分離）。
 * ここにあるのは「型」「reducer（foldInto / beginSubmission）」「導出（deriveStatus / lampOf /
 * deriveNowLine …）」だけで、store の生成・timer・IPC 送信・描画は chatview.tsx（controller /
 * view）が持つ。solid の `produce` draft でも plain object でも同じに動くので、component を
 * import せずに vitest で検証できる。
 *
 * 移設時に振る舞いは変えていない（イベント順・replay の意味・HITL の streaming 停止は据え置き）。
 */
import type { ConversationEvent, PlanEntry, QuestionSpec } from './console'
import type { toWirePayload } from './paste-image'

// ---------------------------------------------------------------------------
// 会話モデル — flat item stream（ConversationEvent を UI 単位に畳む）
// ---------------------------------------------------------------------------

export type ChatItem =
  | { kind: 'user'; text: string; submissionId?: string; clientId?: string }
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

export type ChatState = {
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
  submission: Submission | null
  /** status 同期: 最後に畳んだイベント種別（foldInto で全イベント更新）。 */
  lastEvent: string | null
  /** status 同期: 最後にイベントを受けた時刻 ms（foldEvent で Date.now。hang 検出の時間軸）。 */
  lastEventAt: number | null
  /** transcript replay（attach/reconnect 時の過去会話再送）進行中か。replay_start→true /
   *  replay_end→false。コーナーの再同期ローディングアニメ（resync-loader）の可視条件。 */
  replaying: boolean
  historyTruncated?: boolean
  historyThreadId?: string
  codexConfig?: Extract<ConversationEvent, { kind: 'codex_config' }>['config']
  codexSettingsRequest?: string | null
  codexSettingsError?: string | null
  /** now-line の契約供給（doc 51 §1 A3b — AI が自分の今を報告する口）。null = 契約報告なし
   *  = deriveNowLine の機械導出（A3a の保険）が下支えする。turn_completed で消える（「今」は
   *  turn より長生きしない）。書き手は A3b の `now_line` event（PR2 で配線 — 受け皿を先に置く
   *  reader-first）。 */
  nowLine: string | null
}

export type Submission = {
  id: string
  text: string
  images: ReturnType<typeof toWirePayload>
  status: 'sending' | 'failed'
  error: string | null
}
/**
 * ConversationEvent を ChatState に畳み込む純粋 mutation（reducer 本体）。
 *
 * solid の `produce` draft でも plain object でも同じに動く（＝ store 非依存 = 単体テスト可能）。
 * 会話モデリングの肝: message_chunk / thought_chunk は末尾同種 item に append（accumulate）、
 * tool_call_update は id 一致で done 化。ここが gui の描画正しさの中核。
 */
export function foldInto(s: ChatState, ev: ConversationEvent): void {
  if (ev.kind === 'codex_config') {
    if (ev.request_id && ev.request_id !== s.codexSettingsRequest) return
    if (ev.config && !ev.request_id) s.codexConfig = ev.config
    if (ev.request_id) {
      s.codexSettingsRequest = null
      s.codexSettingsError = ev.error
    }
    return
  }
  // Acknowledgements are scoped by request as well as lane/session. They are not
  // turn-closing engine events and must never flush type-ahead on rejection.
  if (ev.kind === 'submit_result') {
    const submission = s.submission
    if (!submission || submission.id !== ev.request_id || submission.status !== 'sending') return
    if (ev.error !== null) {
      submission.status = 'failed'
      submission.error = ev.error
      s.replaying = false
      s.items = s.items.filter((item) => item.kind !== 'user' || item.submissionId !== submission.id)
    } else {
      for (const item of s.items) {
        if (item.kind === 'user' && item.submissionId === submission.id) delete item.submissionId
      }
      s.submission = null
    }
    return
  }
  s.lastEvent = ev.kind // 拾える全イベント種別を status に同期（時刻は foldEvent が Date.now で付す）
  switch (ev.kind) {
    case 'codex_history': {
      const included = new Set(ev.user_message_ids)
      const local = s.historyThreadId && s.historyThreadId !== ev.thread_id ? [] : s.items.filter(
        item => item.kind === 'user' && item.clientId && !included.has(item.clientId),
      )
      foldInto(s, { kind: 'replay_start' })
      for (const event of ev.events) {
        // 表示データのみ。過去の承認・送信・snapshot を再帰実行しない。
        if (['user_message', 'message_chunk', 'thought_chunk', 'tool_call', 'tool_call_update', 'turn_completed'].includes(event.kind)) {
          foldInto(s, event)
        }
      }
      s.items.push(...local)
      foldInto(s, { kind: 'replay_end', in_flight: ev.in_flight })
      s.historyThreadId = ev.thread_id
      s.header = { ...s.header, sessionId: ev.thread_id }
      s.historyTruncated = ev.truncated
      s.lastEvent = ev.kind
      break
    }
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
      s.historyTruncated = false
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
/** Keep the exact payload in memory until accepted or explicitly recovered. */
export function beginSubmission(
  s: ChatState, id: string, text: string, images: Submission['images'],
): boolean {
  if (s.submission) return false
  s.submission = { id, text, images, status: 'sending', error: null }
  s.items.push({ kind: 'user', text, submissionId: id, clientId: id })
  return true
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
    submission: null,
    lastEvent: null,
    lastEventAt: null,
    replaying: false,
    nowLine: null,
  }
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
