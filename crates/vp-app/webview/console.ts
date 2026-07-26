/**
 * Console facade (doc 33 §4) — Act I/II が同居する Console 面の World B 側 controller。
 *
 * Rust からの供給は **push envelope**（`window.vpDispatch` の単一受け口 → `dispatch.ts` が
 * ここの method へ配る）。`window.vpConsole` は **DevTools 検分用に残してある**だけで、
 * Rust は名前で呼ばない（SSOT = `crates/vp-app/schema/vp-push.kdl`）。
 *
 * - data plane: `console:event` → [`VpConsole.handleEvent`] — repo の EchoesAgentHost が吐く
 *   EchoesEvent（engine 非依存語彙、doc 32 §4）を per-lane ring buffer に蓄積し、
 *   mount 済みの ChatView renderer に届ける（renderer は C2 で登録）。
 * - control plane: `console:act_applied` → [`VpConsole.setSessionAct`] — その session の
 *   Act（見え方）が変わったことの通知（doc 50 §4.6 A6。旧 lane 単位 `setMode` の後継）。
 *   ⚠️ 表示は強制しない（ビューとエンジンは別軸 — Lane 内で Act I/II pane は共存し得る）。
 * - roster: `console:session_list` → [`VpConsole.handleSessionList`]（供給はこの 1 本、doc 53 §11）
 * - 「+」menu: `console:stands` → [`VpConsole.handleStands`]
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
  /** session の「今なにを」自己申告（doc 51 §1 A3b — `vp now` CLI 発、daemon が注入）。
   *  GUI は now-line（名札直下の動的一行）に出し、turn_completed で消す。 */
  | { kind: 'now_line'; text: string }
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
// repo（echoes_session_list）が唯一の真実源。ここはそれを描くための薄い view cache で、
// tab strip の描画基準（focused）と chatview の event filter が参照する。純関数群は document
// 非依存 = vitest でそのままテストできる（session routing の要）。
// ---------------------------------------------------------------------------

/** echoes_session_list の 1 要素（repo `ChatSessionInfo` の手書き mirror）。 */
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
  /** doc 39: この session が lane の root（Act I slot に立ち mailbox を名乗る）か。
   *  root タブは × を隠す（backend の「root は remove 不可」の UI 反映）。
   *  旧 SP は送らない → undefined（後方互換は canCloseSession 側が吸収）。 */
  root?: boolean
  /** doc 50 §4.6 A6: この session の Act（見え方）。roster（lane-panes）が Pane kind を
   *  決める **唯一の入力**で、名札 kind badge の表示もこれに従う。
   *  旧 SP は送らない → undefined（roster 側が "tui" に倒す = 従来の既定）。 */
  act?: 'tui' | 'chat'
  /** doc 50 §4.6 A6 ②: この session を Chat にできるか（能力表は server が SSOT）。
   *  名札の kind badge は false なら Chat への切替を出さない（押しても server に弾かれる
   *  だけの行き止まりを作らない）。旧 SP は送らない → undefined = 不可に倒す。 */
  chat_capable?: boolean
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

// --- doc 47 §6: 共有 bus の相関 id ---------------------------------------------------------------
// `vp:echoes-stands` は broadcast なので、購読側が複数いると「誰の要求への応答か」が判らない
// （doc 46 P2 で「+ New」の要求に chat の「+」menu まで反応した混線 = #838）。
// 要求時に採番した id を round-trip（webview → Rust IPC → stands_list → handleStands）させ、
// 購読側は **自分が出した要求の id と一致した時だけ** 反応する。
//
// bus を要求元ごとに分ける案は採らなかった: 分けても id の round-trip は要るうえ、発火元
// （console.ts = 投影側）が購読側 UI の顔ぶれを列挙することになり、doc 47 §0 の
// 「実体 → 見え方」の向きが逆流する。id なら発火元は要求元を知らないままでいられる。

/** 共有 bus の相関 id（`<要求元 scope>#<連番>`）。 */
export type BusRequestId = string

let busRequestSeq = 0

/** 相関 id を採番する。scope は要求元のラベル（log で読める形にするだけで、照合は完全一致）。 */
export function nextRequestId(scope: string): BusRequestId {
  busRequestSeq += 1
  return `${scope}#${busRequestSeq}`
}

/** 応答が「自分の要求に対するもの」か。純粋 = テスト可能。
 *  ⚠️ 素の `===` にしないのは、要求を出していない購読側（pending = null）に req 無しの応答
 *  （= null）が来た時に一致してしまうため。**要求していない側は常に false** が規約。 */
export function isMyResponse(
  pending: BusRequestId | null,
  req: BusRequestId | null | undefined,
): boolean {
  return pending !== null && req === pending
}

/** `vp:echoes-stands` の detail。req は要求元の相関 id（要求外の発火は null）。 */
export type EchoesStandsDetail<S = unknown> = {
  lane: string
  stands: S[]
  req: BusRequestId | null
}

type LaneSessions = { focused: number; sessions: EchoesSession[] }

const laneSessions = new Map<string, LaneSessions>()

/** envelope の session を正規化する（未指定 = 1）。doc 38 §5.3 の後方互換:
 *  session を持たない旧 SP / 単一 session lane は focused = key 1 に解決する。純粋 = テスト可能。 */
export function normalizeSession(session?: number): number {
  return session ?? 1
}

/** repo の echoes_session_list payload を per-lane cache に取り込む（純粋 = document 非依存 = テスト可能）。 */
export function noteSessionList(lane: string, focused: number, sessions: EchoesSession[]): void {
  laneSessions.set(lane, { focused, sessions })
}

/** tab click の楽観的 focus 切替（chatview の filter を round-trip を待たず即切り替える）。
 *  repo の echoes_session_list が後で authoritative 値で上書きする。純粋 = テスト可能。 */
export function noteFocus(lane: string, session: number): void {
  const cur = laneSessions.get(lane)
  if (cur) cur.focused = session
  else laneSessions.set(lane, { focused: session, sessions: [] })
}

/**
 * act 切替の楽観的な local 反映（[`sessionActOf`] の読み手 cache を即時更新する）。
 *
 * ⚠️ **これが無いと act を読む消費者が旧値で分岐する**。`laneSessions` は
 * `echoes_session_list` の full fetch でしか更新されないが、**act 切替はその fetch を伴わない**
 * （badge click の成功パスは `session_set_act` → `SessionActApplied` で完結する）。
 * 実害は `ink.ts` の送り先判定 — tui→chat の直後に board 注釈を送ると、畳まれた PtySlot へ
 * `term:write` が飛んで**黙って消える**（chat には届かない。エラーはゼロ）。
 *
 * 旧 lane 単位 `setMode` は `laneOf(lane).mode = mode` で自分の読み手を更新していた。A6 で
 * session 単位へ移す際にこの 1 行が落ちた（team-b 9 回目 2026-07-25）。Rust 側は
 * `SessionActApplied` で手元 snapshot を同じ理由で即時更新している — **同じ判断を 2 つの
 * cache に要求されていて、片方だけ満たしていた**。
 *
 * session 一覧を知らない lane では no-op（次の full fetch が埋める）。badge は roster から
 * 描かれるので、実際には一覧が既にある状態でしか呼ばれない。
 */
export function noteSessionAct(
  lane: string,
  session: number,
  act: 'tui' | 'chat',
): void {
  const entry = laneSessions.get(lane)?.sessions.find((s) => s.key === session)
  if (entry) entry.act = act
}

/** lane の focused session key（未知 = 1）。chatview の event filter / tab 強調の基準。 */
export function focusedOf(lane: string): number {
  return laneSessions.get(lane)?.focused ?? 1
}

/** lane の session 一覧（未知 = 空）。**遅れて現れた購読者**が cache から拾うための入口。
 *
 *  doc 53 §11: roster の供給が lanes snapshot 1 本になり、push は **roster が変わった時だけ**
 *  走る（定期 snapshot で撃ち直さない）。そのため「lane を開いた」だけでは新しい event は
 *  来ない — 開いた側が cache を読む。旧実装はここで `echoes:sessions_fetch` を撃っていた
 *  （= 供給路 2 本目の入口そのもの）。 */
export function sessionListOf(lane: string): EchoesSession[] {
  return laneSessions.get(lane)?.sessions ?? []
}

/** その session の act（見え方）。未知 / 旧 SP（act 欠落）は 'tui'（従来の既定）。
 *  doc 50 §4.6 A6: 見え方は session の属性なので、lane 単位 `getMode` の代わりにこれを引く。 */
export function sessionActOf(lane: string, session: number): 'tui' | 'chat' {
  return (
    laneSessions.get(lane)?.sessions.find((s) => s.key === session)?.act ?? 'tui'
  )
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
 * EchoesHeader（pane 名札）が表示する lane の session summary。
 * EchoesEvent 既存流（session_init / turn_completed）だけから畳む — 新しい Rust→JS
 * チャネルは作らない。presence-driven（無ければ chip 非表示）。
 *
 * doc 50: 名札に残るのは **素性だけ**になったので、summary も sessionId 1 本に縮約した。
 * 旧 field（model / permissionMode / engineError / engineDormant）は名札の chip 撤去で
 * 読み手を失った — model / perm は composer の select、engine 異常は status 行の
 * `deriveStatus` が別経路で同じ event から導出しており、ここで畳む必要が無い。
 */
export type EchoesHeaderState = {
  /** cc session id（Act を跨いで同一 session が継続することの可視化）。 */
  sessionId?: string
}

/**
 * header summary への畳み込み（純関数、vitest 対象）。変化があれば true を返す —
 * caller はその時だけ 'vp:echoes-header' event を dispatch する（message_chunk 等の
 * 高頻度 event では飛ばない = ヘッダ再描画は低頻度に保たれる）。
 */
export function foldHeaderState(h: EchoesHeaderState, event: EchoesEvent): boolean {
  switch (event.kind) {
    case 'session_init':
    case 'turn_completed': {
      const changed = h.sessionId !== event.session_id
      h.sessionId = event.session_id
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
 *  repo 側 cc_session なので、ここは直近ウィンドウで足りる）。 */
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
  /** doc 50 §4.6 A6: session の Act（見え方）が変わったことを通知する（'vp:session-act'）。
   *  Rust の `SessionActApplied` が呼ぶ口。roster と kind badge がこれで追従する。 */
  setSessionAct(lane: string, session: number, act: ConsoleMode): void
  /** ChatView (C2) が mount 時に登録。既存 buffer を replay してから live 配信に接続する。 */
  attachRenderer(lane: string, renderer: ConsoleRenderer): void
  detachRenderer(lane: string): void
  /** devtools 検分: 直近 n 件（default 20）。 */
  peek(lane: string, n?: number): EchoesEvent[]
  /** Echoes 共通ヘッダ用 summary の snapshot（copy を返す — caller の signal 更新用）。 */
  headerState(lane: string): EchoesHeaderState
  /** doc 38 Phase 2: repo の echoes_session_list を per-lane cache に取り込み、tab strip へ
   *  'vp:echoes-sessions' CustomEvent を発火する（focused も併せて更新）。 */
  handleSessionList(lane: string, payload: EchoesSessionListPayload): void
  /** doc 38 Phase 2: stands_list を「+」menu へ 'vp:echoes-stands' CustomEvent で中継する。
   *  doc 47 §6: req = 要求元の相関 id（IPC の `req` を Rust が往復させたもの）。購読側は
   *  自分の id と一致した時だけ反応する。 */
  handleStands(lane: string, payload: EchoesStandsPayload, req?: BusRequestId | null): void
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
    setSessionAct(lane, session, act) {
      // doc 50 §4.6 A6: 見え方は **session の属性**。roster（lane-panes）と名札の kind badge
      // （EchoesHeader）がこの bus を購読して、その session の Pane kind を入れ替える。
      // lane 単位の mode cache は触らない（root の追従は sidebar snapshot が持つ）。
      //
      // ⚠️ **自分の読み手 cache を先に更新する**（`sessionActOf` の供給元）。bus の購読者は
      // 各自の cache を更新するが、`laneSessions` は full fetch でしか埋まらず、act 切替は
      // fetch を伴わない — 更新を忘れると `ink.ts` が旧 act で誤配送する（`noteSessionAct`）。
      noteSessionAct(lane, session, act)
      document.dispatchEvent(
        new CustomEvent('vp:session-act', { detail: { lane, session, act } }),
      )
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
    handleStands(lane, payload, req) {
      const stands = Array.isArray(payload?.stands) ? payload!.stands! : []
      // doc 47 §6: req をそのまま detail に載せる（発火元は要求元が誰かを解釈しない）。
      const detail: EchoesStandsDetail = { lane, stands, req: req ?? null }
      document.dispatchEvent(new CustomEvent('vp:echoes-stands', { detail }))
    },
    // 純関数 focusedOf をそのまま公開（laneSessions cache を参照。property 名は method binding を
    // 作らないので module-level の focusedOf を指す — 自己再帰にはならない）。
    focusedOf,
  }
  ;(window as unknown as { vpConsole: VpConsole }).vpConsole = api
  return api
}
