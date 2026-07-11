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
  /** user 自身の過去発話（transcript replay 専用。live では ChatView が submit 時に足す）。 */
  | { kind: 'user_message'; text: string }
  | { kind: 'message_chunk'; text: string }
  | { kind: 'thought_chunk'; text: string }
  | { kind: 'tool_call'; id: string; name: string; input: unknown }
  | { kind: 'tool_call_update'; tool_use_id: string; content: string; is_error?: boolean }
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

/** ChatView（C2）が lane ごとに登録する renderer。 */
export type ConsoleRenderer = (event: EchoesEvent) => void

// ---------------------------------------------------------------------------
// per-lane 状態（buffer / mode / renderer）
// ---------------------------------------------------------------------------

/** ring buffer 上限。ChatView mount 前の取りこぼし救済 + devtools 検分用（会話全体の SSOT は
 *  SP 側 cc_session なので、ここは直近ウィンドウで足りる）。 */
const BUFFER_CAP = 1000

type LaneConsole = {
  buffer: EchoesEvent[]
  mode: ConsoleMode
  renderer: ConsoleRenderer | null
}

const lanes = new Map<string, LaneConsole>()

function laneOf(lane: string): LaneConsole {
  let entry = lanes.get(lane)
  if (!entry) {
    entry = { buffer: [], mode: 'tui', renderer: null }
    lanes.set(lane, entry)
  }
  return entry
}

// ---------------------------------------------------------------------------
// facade 本体
// ---------------------------------------------------------------------------

export type VpConsole = {
  handleEvent(lane: string, event: EchoesEvent): void
  setMode(lane: string, mode: ConsoleMode): void
  getMode(lane: string): ConsoleMode
  /** ChatView (C2) が mount 時に登録。既存 buffer を replay してから live 配信に接続する。 */
  attachRenderer(lane: string, renderer: ConsoleRenderer): void
  detachRenderer(lane: string): void
  /** devtools 検分: 直近 n 件（default 20）。 */
  peek(lane: string, n?: number): EchoesEvent[]
}

export function installConsole(): VpConsole {
  const api: VpConsole = {
    handleEvent(lane, event) {
      const entry = laneOf(lane)
      // replay 開始 = 過去会話の再送。 buffer も捨てて張り直す（ChatView 未 mount のまま
      // 2 回 replay された場合に、 後で attach した renderer が二重の会話を畳むのを防ぐ）。
      if (event.kind === 'replay_start') entry.buffer.length = 0
      entry.buffer.push(event)
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
      if (entry.renderer) {
        try {
          entry.renderer(event)
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
      for (const ev of entry.buffer) {
        try {
          renderer(ev)
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
      return laneOf(lane).buffer.slice(-n)
    },
  }
  ;(window as unknown as { vpConsole: VpConsole }).vpConsole = api
  return api
}
