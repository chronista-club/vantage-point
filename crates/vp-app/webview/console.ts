/**
 * Console facade (doc 33 §4) — Act I/II が同居する Console 面の World B 側 controller。
 *
 * - data plane: `window.vpConsole.handleEvent(lane, event)` — SP の EchoesAgentHost が吐く
 *   EchoesEvent（engine 非依存語彙、doc 32 §4）を per-lane ring buffer に蓄積し、
 *   mount 済みの ChatView renderer に届ける（renderer は C2 で登録）。
 * - control plane: `window.vpConsole.setMode(lane, mode)` — エンジンモードの通知。
 *   ⚠️ 表示は強制しない（ビューとエンジンは別軸 — Lane 内で Act I/II pane は共存し得る）。
 * - 検分: `window.vpConsole.peek(lane)` — devtools から buffer を覗く（throwaway debug pane を
 *   作らないための恒久 API）。
 *
 * World A（main_area.rs インライン xterm JS）には触れない — 境界規律（doc 33 §8）。
 */

// ---------------------------------------------------------------------------
// EchoesEvent 型 — SSOT は Rust `crates/vantage-point/src/echoes/event.rs`（PR1 で凍結）。
// vp-app Rust はこれを serde_json::Value で素通しするため ts-rs 経路が無く、手書きで mirror
// する（変更時は event.rs と同時に更新すること）。
// ---------------------------------------------------------------------------

export type ConsoleMode = 'tui' | 'chat'

export type PlanEntry = {
  content: string
  status: 'pending' | 'in_progress' | 'completed' | string
  active_form?: string | null
}

/** AskUserQuestion の 1 選択肢（doc 35 §3）。 */
export type QuestionOption = {
  label: string
  description?: string
}

/** AskUserQuestion の 1 質問（doc 35 §3）。multiSelect は複数選択 + 確定ボタン。 */
export type QuestionSpec = {
  question: string
  header: string
  options: QuestionOption[]
  multi_select?: boolean
}

export type EchoesEvent =
  | {
      kind: 'session_init'
      session_id: string
      model?: string
      permission_mode?: string
      cwd?: string
      tools?: string[]
      mcp_servers?: string[]
      slash_commands?: string[]
    }
  /** transcript replay の開始マーカー。受信側は会話表示 + buffer をクリアしてから後続を畳む
   *  （replay を冪等にする = reconnect / demand 再発火で会話が二重化しない）。 */
  | { kind: 'replay_start' }
  /** transcript replay の終端マーカー（replay_start と対、replay 列の最後に 1 回）。
   *  in_flight = 直後に本当に生成中の turn があるか。GUI はこれで streaming を確定する
   *  （過去発話の message_chunk が立てた streaming を打ち消す）。 */
  | { kind: 'replay_end'; in_flight: boolean }
  /** user 自身の過去発話（transcript replay 専用。live では ChatView が submit 時に足す）。 */
  | { kind: 'user_message'; text: string }
  | { kind: 'message_chunk'; text: string }
  | { kind: 'thought_chunk'; text: string }
  | { kind: 'tool_call'; id: string; name: string; input: unknown }
  | { kind: 'tool_call_update'; tool_use_id: string; content: string; is_error?: boolean }
  /**
   * subagent（Agent tool が回した子）の発話。engine が --forward-subagent-text 付きの時だけ来る。
   * parent_tool_use_id は親の tool_call.id と一致するので、GUI は該当 tool 行の中に入れ子で描く。
   * ⚠️ delta ではなく「block 1 個ぶんの完成テキスト」（subagent は snapshot でしか流れてこない）。
   */
  | {
      kind: 'subagent_message'
      parent_tool_use_id: string
      role: 'prompt' | 'thinking' | 'text'
      text: string
    }
  | { kind: 'plan'; entries: PlanEntry[] }
  /** context_tokens/window = Act I statusline 相当の context ゲージ（省略時 GUI は前値を保つ）。 */
  | {
      kind: 'turn_completed'
      session_id: string
      cost_usd?: number
      context_tokens?: number
      context_window?: number
    }
  | { kind: 'error'; message: string }
  /** engine プロセスの終了（途絶）= 回復可能な休眠。error（本物の異常）と別語彙で、
   *  GUI は「💤 休眠（送信で起動）」と穏当に出す。次の submit / reconnect demand で復活する。 */
  | { kind: 'engine_exited'; message: string }
  /** clarifying question（AskUserQuestion の can_use_tool 横取り、doc 35 PR1）。
   *  GUI は PromptCard で選択肢を描き、回答を echoes:respond {request_id, answers} で戻す。 */
  | { kind: 'question'; request_id: string; questions: QuestionSpec[] }
  /** tool 承認要求（permission-mode=default 時の can_use_tool、doc 35 PR3）。
   *  GUI は PromptCard で allow/deny を描き、echoes:respond {request_id, behavior} で戻す。 */
  | { kind: 'permission_request'; request_id: string; tool_name: string; input: unknown }

/** ChatView（C2）が lane ごとに登録する renderer。
 *  doc 38 Phase 2: 第 2 引数 session = EchoesEvent envelope 由来の VP 採番 key（1 Lane = N session）。
 *  renderer 側は `session !== focusedOf(lane)` を fold しないことで背景 session の混入を防ぐ。 */
export type ConsoleRenderer = (event: EchoesEvent, session: number) => void

// ---------------------------------------------------------------------------
// doc 38 Phase 2 — per-lane session registry（1 Lane = N session）
//
// SP（echoes_session_list）が唯一の真実源。ここはそれを描くための薄い view cache で、
// tab strip の描画基準（focused）と chatview の event filter が参照する。純関数群は document
// 非依存 = vitest でそのままテストできる（session routing の要）。
// ---------------------------------------------------------------------------

/** echoes_session_list の 1 要素（SP `ChatSessionInfo` の手書き mirror）。 */
export type EchoesSession = {
  /** VP 採番のローカル key（<lane>#<n> の n）。 */
  key: number
  /** engine 種別（session chip / tab の prefix 導出用）。 */
  stand: string
  /** engine の会話 id（cc_session 等。Draft = null、doc 38 §1.1）。 */
  engine_session_id: string | null
  /** chat host が現在生きているか（in-memory slot の有無）。 */
  live: boolean
  focused: boolean
  /** doc 39: この session が lane の root（床に化身し mailbox を名乗る）か。
   *  root タブは × を隠す（backend の「root は remove 不可」の UI 反映）。
   *  旧 SP は送らない → undefined（後方互換は canCloseSession 側が吸収）。 */
  root?: boolean
}

/** echoes_session_list の生 payload（Rust `handle_echoes_session_list` の返り値 mirror）。 */
export type EchoesSessionListPayload = {
  lane?: string
  /** focused session key。session が無い lane では null。 */
  focused?: number | null
  sessions?: EchoesSession[]
}

/** stands_list の生 payload（`{stands:[{name, description}]}`）。 */
export type EchoesStandsPayload = {
  stands?: unknown[]
}

type LaneSessions = { focused: number; sessions: EchoesSession[] }

const laneSessions = new Map<string, LaneSessions>()

/** envelope の session を正規化する（未指定 = 1）。doc 38 §5.3 の後方互換:
 *  session を持たない旧 SP / 単一 session lane は focused = key 1 に解決する。純粋 = テスト可能。 */
export function normalizeSession(session?: number): number {
  return session ?? 1
}

/** SP の echoes_session_list payload を per-lane cache に取り込む（純粋 = document 非依存 = テスト可能）。 */
export function noteSessionList(lane: string, focused: number, sessions: EchoesSession[]): void {
  laneSessions.set(lane, { focused, sessions })
}

/** tab click の楽観的 focus 切替（chatview の filter を round-trip を待たず即切り替える）。
 *  SP の echoes_session_list が後で authoritative 値で上書きする。純粋 = テスト可能。 */
export function noteFocus(lane: string, session: number): void {
  const cur = laneSessions.get(lane)
  if (cur) cur.focused = session
  else laneSessions.set(lane, { focused: session, sessions: [] })
}

/** lane の focused session key（未知 = 1）。chatview の event filter / tab 強調の基準。 */
export function focusedOf(lane: string): number {
  return laneSessions.get(lane)?.focused ?? 1
}

/** focused session の engine_session_id を共通ヘッダの chip に同期する（変化時 true —
 *  caller はその時だけ 'vp:echoes-header' を dispatch する）。
 *
 *  D1（解剖 memory `cc-session-pointer-self-destruction` / F5）: chip は従来 session_init /
 *  turn_completed でしか動かず、新 Draft を focus しても旧 session の id が chip に残り続けた
 *  （「New しても id が変わらない」— 実体は新品なのに表示だけが嘘をつく）。
 *  Draft（engine_session_id = null）は chip を消し、初回 submit の session_init が新 id を灯す。 */
export function syncHeaderSessionId(lane: string): boolean {
  const cur = laneSessions.get(lane)
  if (!cur) return false
  const sid = cur.sessions.find((s) => s.key === cur.focused)?.engine_session_id ?? undefined
  const h = laneOf(lane).header
  if (h.sessionId === sid) return false
  h.sessionId = sid
  return true
}

// ---------------------------------------------------------------------------
// Echoes 共通ヘッダ用の per-lane summary（creo memo `vp-pane-common-header`）
// ---------------------------------------------------------------------------

/**
 * EchoesHeader（共通ヘッダ strip）が表示する lane の session summary。
 * EchoesEvent 既存流（session_init / turn_completed / error）だけから畳む —
 * 新しい Rust→JS チャネルは作らない。全 field presence-driven（無ければ chip 非表示）。
 */
export type EchoesHeaderState = {
  /** cc session id（Act を跨いで同一 session が継続することの可視化）。 */
  sessionId?: string
  model?: string
  permissionMode?: string
  /** 直近の engine 異常（turn crash / 翻訳失敗など「本物の error」）。⚠ engine（警告）で出す。
   *  session_init（engine 復帰）/ turn_completed（生存証拠）で clear。 */
  engineError?: string
  /** engine プロセスの休眠（途絶 = 回復可能）。💤 休眠 で穏当に出す。error とは排他
   *  （engine_exited は clean exit なので engineError を消す）。session_init / turn_completed で clear。 */
  engineDormant?: string
}

/**
 * header summary への畳み込み（純関数、vitest 対象）。変化があれば true を返す —
 * caller はその時だけ 'vp:echoes-header' event を dispatch する（message_chunk 等の
 * 高頻度 event では飛ばない = ヘッダ再描画は低頻度に保たれる）。
 */
export function foldHeaderState(h: EchoesHeaderState, event: EchoesEvent): boolean {
  switch (event.kind) {
    case 'session_init': {
      const changed =
        h.sessionId !== event.session_id ||
        (event.model !== undefined && h.model !== event.model) ||
        (event.permission_mode !== undefined && h.permissionMode !== event.permission_mode) ||
        h.engineError !== undefined ||
        h.engineDormant !== undefined
      h.sessionId = event.session_id
      if (event.model !== undefined) h.model = event.model
      if (event.permission_mode !== undefined) h.permissionMode = event.permission_mode
      // engine 復帰 = error / 休眠 の両方を下ろす。
      h.engineError = undefined
      h.engineDormant = undefined
      return changed
    }
    case 'turn_completed': {
      const changed =
        h.sessionId !== event.session_id || h.engineError !== undefined || h.engineDormant !== undefined
      h.sessionId = event.session_id
      // 生存証拠 = error / 休眠 の両方を下ろす。
      h.engineError = undefined
      h.engineDormant = undefined
      return changed
    }
    case 'error': {
      // 本物の異常 → engineError。休眠表示とは排他。
      const changed = h.engineError !== event.message || h.engineDormant !== undefined
      h.engineError = event.message
      h.engineDormant = undefined
      return changed
    }
    case 'engine_exited': {
      // 途絶 = 回復可能な休眠 → engineDormant。clean exit なので engineError は消す。
      const changed = h.engineDormant !== event.message || h.engineError !== undefined
      h.engineDormant = event.message
      h.engineError = undefined
      return changed
    }
    default:
      return false
  }
}

// ---------------------------------------------------------------------------
// per-lane 状態（buffer / mode / renderer）
// ---------------------------------------------------------------------------

/** ring buffer 上限。ChatView mount 前の取りこぼし救済 + devtools 検分用（会話全体の SSOT は
 *  SP 側 cc_session なので、ここは直近ウィンドウで足りる）。 */
const BUFFER_CAP = 1000

/** ring buffer の 1 要素。doc 38 Phase 2: どの session の event かを envelope として保持し、
 *  attach 時の replay で renderer に session を渡せるようにする。 */
type BufferedEvent = { event: EchoesEvent; session: number }

type LaneConsole = {
  buffer: BufferedEvent[]
  mode: ConsoleMode
  renderer: ConsoleRenderer | null
  /** Echoes 共通ヘッダ用 summary（session_init / turn_completed / error の畳み込み）。 */
  header: EchoesHeaderState
}

const lanes = new Map<string, LaneConsole>()

function laneOf(lane: string): LaneConsole {
  let entry = lanes.get(lane)
  if (!entry) {
    entry = { buffer: [], mode: 'tui', renderer: null, header: {} }
    lanes.set(lane, entry)
  }
  return entry
}

// ---------------------------------------------------------------------------
// facade 本体
// ---------------------------------------------------------------------------

export type VpConsole = {
  /** doc 38 Phase 2: session = envelope 由来の VP 採番 key（未指定 = focused = 1、旧 SP 互換）。 */
  handleEvent(lane: string, event: EchoesEvent, session?: number): void
  setMode(lane: string, mode: ConsoleMode): void
  getMode(lane: string): ConsoleMode
  /** ChatView (C2) が mount 時に登録。既存 buffer を replay してから live 配信に接続する。 */
  attachRenderer(lane: string, renderer: ConsoleRenderer): void
  detachRenderer(lane: string): void
  /** devtools 検分: 直近 n 件（default 20）。 */
  peek(lane: string, n?: number): EchoesEvent[]
  /** Echoes 共通ヘッダ用 summary の snapshot（copy を返す — caller の signal 更新用）。 */
  headerState(lane: string): EchoesHeaderState
  /** ChatView の permission mode optimistic 切替をヘッダにも同期する（engine は即時 event を
   *  返さないため。respawn 時は session_init.permission_mode の真値が上書きする）。 */
  notePermissionMode(lane: string, mode: string): void
  /** doc 38 Phase 2: SP の echoes_session_list を per-lane cache に取り込み、tab strip へ
   *  'vp:echoes-sessions' CustomEvent を発火する（focused も併せて更新）。 */
  handleSessionList(lane: string, payload: EchoesSessionListPayload): void
  /** doc 38 Phase 2: stands_list を「+」menu へ 'vp:echoes-stands' CustomEvent で中継する。 */
  handleStands(lane: string, payload: EchoesStandsPayload): void
  /** doc 38 Phase 2: lane の focused session key（未知 = 1）。chatview の event filter が参照。 */
  focusedOf(lane: string): number
}

export function installConsole(): VpConsole {
  const api: VpConsole = {
    handleEvent(lane, event, session) {
      const s = normalizeSession(session)
      const entry = laneOf(lane)
      // replay 開始 = 該当 session の過去会話再送。doc 38 Phase 2: replay は session 単位なので、
      // その session の buffer 分だけ捨てる（他 session の buffer を巻き込まない）。ChatView 未
      // mount のまま 2 回 replay された場合に後着 renderer が二重の会話を畳むのを防ぐ。N=1 では
      // 全消去と等価（旧挙動）。
      if (event.kind === 'replay_start') {
        entry.buffer = entry.buffer.filter((b) => b.session !== s)
      }
      entry.buffer.push({ event, session: s })
      if (entry.buffer.length > BUFFER_CAP) {
        entry.buffer.splice(0, entry.buffer.length - BUFFER_CAP)
      }
      // doc 33 §9: session_init = engine が resume を確定した瞬間。切替の progress を
      // ここで clear する（「resume してから切替完了」= 安全なハンドオフ）。
      if (event.kind === 'session_init') {
        document.dispatchEvent(
          new CustomEvent('vp:console-ready', { detail: { lane } }),
        )
      }
      // Echoes 共通ヘッダ summary。変化した時だけ通知（chunk 系では飛ばない）。
      // NB(doc 38): header は lane 単位の presence-driven 表示で、session ごとの scoping は
      // Phase 3 以降の磨き。今は全 session を跨いで畳み、N=1 の既存挙動を保つ。
      if (foldHeaderState(entry.header, event)) {
        document.dispatchEvent(
          new CustomEvent('vp:echoes-header', { detail: { lane } }),
        )
      }
      if (entry.renderer) {
        try {
          entry.renderer(event, s)
        } catch (e) {
          console.warn('[vpConsole] renderer error', lane, e)
        }
      }
    },
    setMode(lane, mode) {
      laneOf(lane).mode = mode
      // 表示切替は ChatView / layout 側の判断（ビューとエンジンは別軸）。通知だけ流す。
      document.dispatchEvent(
        new CustomEvent('vp:console-mode', { detail: { lane, mode } }),
      )
    },
    getMode(lane) {
      return laneOf(lane).mode
    },
    attachRenderer(lane, renderer) {
      const entry = laneOf(lane)
      entry.renderer = renderer
      // mount 前に届いた分を replay（subscribe→submit 順と合わせ、取りこぼしゼロ）。
      // doc 38 Phase 2: 各 buffered event の session を renderer に渡す（filter が効く）。
      for (const b of entry.buffer) {
        try {
          renderer(b.event, b.session)
        } catch (e) {
          console.warn('[vpConsole] replay error', lane, e)
          break
        }
      }
    },
    detachRenderer(lane) {
      laneOf(lane).renderer = null
    },
    peek(lane, n = 20) {
      return laneOf(lane).buffer.slice(-n).map((b) => b.event)
    },
    headerState(lane) {
      return { ...laneOf(lane).header }
    },
    notePermissionMode(lane, mode) {
      const h = laneOf(lane).header
      if (h.permissionMode === mode) return
      h.permissionMode = mode
      document.dispatchEvent(
        new CustomEvent('vp:echoes-header', { detail: { lane } }),
      )
    },
    handleSessionList(lane, payload) {
      const focused = normalizeSession(
        typeof payload?.focused === 'number' ? payload.focused : undefined,
      )
      const sessions = Array.isArray(payload?.sessions) ? payload!.sessions! : []
      noteSessionList(lane, focused, sessions)
      // D1: chip を focused session の真値に追従させる（draft = null → chip 消灯、
      //  syncHeaderSessionId の doc 参照）。list が authoritative な同期点。
      if (syncHeaderSessionId(lane)) {
        document.dispatchEvent(
          new CustomEvent('vp:echoes-header', { detail: { lane } }),
        )
      }
      // tab strip（chatview）へ。'vp:console-ready'（:201 相当）と同じ CustomEvent bus パターン。
      document.dispatchEvent(
        new CustomEvent('vp:echoes-sessions', { detail: { lane, focused, sessions } }),
      )
    },
    handleStands(lane, payload) {
      const stands = Array.isArray(payload?.stands) ? payload!.stands! : []
      document.dispatchEvent(
        new CustomEvent('vp:echoes-stands', { detail: { lane, stands } }),
      )
    },
    // 純関数 focusedOf をそのまま公開（laneSessions cache を参照。property 名は method binding を
    // 作らないので module-level の focusedOf を指す — 自己再帰にはならない）。
    focusedOf,
  }
  ;(window as unknown as { vpConsole: VpConsole }).vpConsole = api
  return api
}
