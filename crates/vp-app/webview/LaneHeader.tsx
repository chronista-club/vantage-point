/**
 * LaneHeader — tui/gui 共通の lane-local context strip（creo memo `vp-pane-common-header`）。
 *
 * pane-host 上端の `#lane-header`（main_area.rs が mount 点だけ提供）に SolidJS で mount され、
 * どの PaneKind / Mode を表示していても「lane の Conversation」の情報が載り続ける
 * （帰属は PaneKind ではなく lane の Conversation —「Conversation は一つ、視え方が二つ」の UI 体現）。
 * chips は presence-driven — 値があるものだけ並ぶ。
 *
 * データ源（既存経路への相乗りのみ、新しい Rust→JS 配信チャネルは作らない）:
 * - lane 名 / cwd / branch / Mode 初期値: setActivePane payload（entry.tsx bridge が setLane で届ける）
 * - cc session id / permission mode / engine 途絶: vpConsole の header summary
 *   （session_init / turn_completed / error の畳み込み。'vp:lane-header' event で追従）
 * - Mode（見え方）は載せない: doc 50 §4.6 A6 で session の属性になり、切替は各 pane の
 *   名札 kind badge が持つ（lane の名札は「この lane が何であるか」だけを載せる）
 *
 * ## 載せるもの / 載せないもの（pane の縦軸、doc 29/30 の Edge Ring を pane に再適用）
 *
 * これは **lane の名札**（doc 51 §1 の lane-plate）。「この lane が何であるか」= 居る間
 * 変わらない素性（lane 名 / cwd / branch = 台上の全員が共有するもの）と、root session の
 * 表示器 = 操作器（session chip → picker）、そして **+ New**（台に器械を足す = 場所への
 * 操作なので場所の名札に住む — mako 2026-07-24「一旦 Lane のヘッダ右側に」）を載せる。
 *
 * session 単位の「今の文脈」（状態・model・permission・engine 異常）は各 pane の
 * **計器盤**（chatview の status 行 / composer）が持つ。かつては上段にも併置されていたが、
 * 同じ役割の二重実装だったので撤去した:
 * - `⏹ Stop` → 入力欄の「停止」（同じ `conversation:interrupt`）
 * - perm chip → status 行の perm select（表示だけの chip は操作器に含まれる）
 * - `⚠ engine` / `💤 休眠` → status 行（`deriveStatus` が同じ event を畳んでいる）
 *
 * 旧・下端の帯（`#pane-tabs`）は doc 51 §1 A1 で退役 — pane chip は tiling 既定で存在理由が
 * 消え、+ New はここへ、Mode 切替（避難路、doc 51 §2）は root picker の「見え方」行へ移設。
 */

import { render } from 'solid-js/web'
import { createSignal, For, Show } from 'solid-js'
import { CreoIcon } from '@chronista-club/creo-ui-icons-web'
import {
  isMyResponse,
  nextRequestId,
  sessionListOf,
  type BusRequestId,
  type AgentsDetail,
  type VpConsole,
  type LaneHeaderState,
  type ConversationSession,
} from './console'
// ⚠️ 循環 import（lane-panes → sessionChipPrefix / ここ → newPaneChoices）だが、双方とも
// hoist される関数宣言を handler 内で遅延参照するだけなので ESM 的に安全。
import { newPaneChoices } from './lane-panes'
import { STAND_ICON } from './icons/stand'

/** `vp:conversation-agents` bus が運ぶ agent entry（newPaneChoices の入力と同形）。 */
type PaneStand = { name: string; label?: string; chat_capable?: boolean }

// ---------------------------------------------------------------------------
// 純関数（vitest 対象）
// ---------------------------------------------------------------------------

/** $HOME 相当 prefix を ~ に畳む（Mac 主軸: /Users/<u>。Linux の /home/<u> も受ける）。 */
export function tildify(path: string): string {
  return path.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, '~')
}

/** 中略: maxLen 超過時に頭を残し末尾を厚めに取って '…' で繋ぐ（path は末尾ほど情報が濃い）。 */
export function middleEllipsis(s: string, maxLen = 42): string {
  if (s.length <= maxLen) return s
  const tail = Math.ceil((maxLen - 1) * 0.6)
  const head = maxLen - 1 - tail
  return `${s.slice(0, head)}…${s.slice(s.length - tail)}`
}

/**
 * LaneAddress::Display 形から表示用短名を導く。
 * `<repo>/root` → "root"、`<repo>/performer/<name>` → name。
 * legacy `lead` / `wing` も受理（entry.tsx laneNameFromAddress と同じ語彙）。
 */
export function laneShortName(addr: string): string {
  if (addr.endsWith('/root') || addr.endsWith('/lead')) return 'conductor'
  const m = addr.match(/\/(?:performer|wing)\/(.+)$/)
  return m?.[1] ?? addr
}

/**
 * session chip の engine 別 prefix（doc 37: engine = session 束縛の identity なので、
 * chip 自体が「どの engine の session か」を兼ねる）。
 * claude は歴史的な `cc`（Claude Code）を維持、未知 engine（撤去済み cursor / agy 含む）は
 * 中立の `sid`。
 */
export function sessionChipPrefix(agent: string | null | undefined): string {
  switch (agent) {
    case 'claude':
    case 'hd':
      return 'cc'
    case 'codex':
      return 'cdx'
    case 'grok':
      return 'grok'
    case 'opencode':
      return 'oc'
    default:
      return 'sid'
  }
}

/** Root 切替 picker の 1 行（doc 39 P3 → P4）。 */
export type RootPickerItem = {
  key: number
  /** `cc:3d91933b` 形。会話 id が未発行（Draft / 未発話）の session は `cc:新品`。 */
  label: string
  isRoot: boolean
  /** 切替不可。実質の理由は「**resume を持たない**」（doc 50 §4.0 — shell 層に落ちる
   *  session は `--resume` で slot に張り替えられない）。判定は chip prefix `sid`
   *  （撤去済み / legacy agent）を代理指標にしている — shell が正規の投げる先になる時は
   *  この代理を「resume capability」の実表現に置き換えること。 */
  disabled: boolean
}

/**
 * conversation_session_list の sessions を picker の表示行へ畳む（doc 39 P3 → P4 — Root 切替 picker）。
 * 並びは repo の登録順そのまま（key 昇順 = 生成順、tab strip と同じ秩序）。全 session を列挙する。
 * doc 39 P4: slot の respawn が root session の agent で engine を決めるようになった（P4-A）ため、
 * cross-engine の Root 切替が解禁された。disabled にするのは **engine が未知**（chip prefix が
 * `sid` = 撤去済み cursor/agy や legacy agent — shell 層に落ちて resume が効かない）行のみ。
 * backend の `prepare_switch_root_session`（未知 engine を Err）と二重防御。
 */
export function rootPickerItems(sessions: ConversationSession[]): RootPickerItem[] {
  return sessions.map((s) => ({
    key: s.key,
    label: `${sessionChipPrefix(s.agent)}:${s.engine_session_id ? s.engine_session_id.slice(0, 8) : '新品'}`,
    isRoot: s.root === true,
    disabled: sessionChipPrefix(s.agent) === 'sid',
  }))
}

// ---------------------------------------------------------------------------
// component
// ---------------------------------------------------------------------------

/** setActivePane（Rust push_active_view）が運ぶ lane 文脈。null = lane 未選択。 */
export type HeaderLaneCtx = {
  addr: string
  /** LaneInfo.name（無ければ addr から laneShortName で導出） */
  name: string | null
  cwd: string | null
  branch: string | null
  /** root mode == "gui"（Mode の初期値。Rust 側が sessions から導出 — doc 53 R1。
   *  以降は vp:console-mode event で追従） */
  chat: boolean
  /** active engine の session id（LaneInfo.engine_session_id 由来）。
   *  tui は ConversationEvent が流れないため、この供給路が無いと chip が出ない
   *  （bug mem_1Cd3icsvKiGsQ8TtX8t1FR）。gui では event 由来の真値が優先される。 */
  sessionId: string | null
  /** root session の agent（= slot に載る engine 種別）。session chip の prefix 導出用。
   *  doc 39 P4-C: Rust push_active_view が agent_name（root の engine）優先で解決した値
   *  （cross-engine root でも chip が slot の engine を映す。無ければ lane 固定 agent）。 */
  agent: string | null
}

export type LaneHeaderApi = {
  setLane(ctx: HeaderLaneCtx | null): void
}

function copyText(text: string): void {
  const fallback = () => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify({ t: 'copy', d: text }))
  }
  try {
    // webview の permission policy で silent fail することがあるため IPC fallback 併用
    //（main_area.rs doCopy と同じ二段構え）。
    navigator.clipboard.writeText(text).catch(fallback)
  } catch {
    fallback()
  }
}

export function mountLaneHeader(mount: HTMLElement, vpConsole: VpConsole): LaneHeaderApi {
  const [ctx, setCtx] = createSignal<HeaderLaneCtx | null>(null)
  // doc 50 §4.6 A6: lane 単位 mode の signal は退役（見え方は session の属性 = 各 pane の
  // 名札が持つ）。lane の名札は「この lane が何であるか」だけを載せる。
  const [summary, setSummary] = createSignal<LaneHeaderState>({})
  /** click copy の視覚 feedback（copied class を短時間付ける対象 chip の key）。 */
  const [copiedKey, setCopiedKey] = createSignal<string | null>(null)
  let copiedTimer: number | undefined

  const copy = (key: string, text: string): void => {
    copyText(text)
    setCopiedKey(key)
    if (copiedTimer) clearTimeout(copiedTimer)
    copiedTimer = window.setTimeout(() => setCopiedKey(null), 900)
  }

  // doc 39 P3: Root 切替 picker（session chip click で開く dropdown）。
  // sessions は 'vp:conversation-sessions'（handleSessionList の既存 bus）から受ける — 新配信路は作らない。
  const [pickerOpen, setPickerOpen] = createSignal(false)
  const [pickerPos, setPickerPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 })
  const [sessions, setSessions] = createSignal<ConversationSession[]>([])

  // doc 51 §1 A1: + New（engine × Mode で新 session = 台に器械を足す）。旧・下端の帯から移設。
  // null = 閉。agents は click → `conversation:stands_fetch` 要求 → 応答（相関 id 照合）で開く。
  const [newMenu, setNewMenu] = createSignal<PaneStand[] | null>(null)
  const [newMenuPos, setNewMenuPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 })
  let newMenuReq: BusRequestId | null = null

  const sendIpc = (payload: Record<string, unknown>): void => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify(payload))
  }

  /** + New click: menu が開いていれば閉じ、閉じていれば agents を取り直して開く
   *  （開くたび authoritative — picker の openPicker と同じ流儀）。 */
  const toggleNewMenu = (btn: HTMLElement): void => {
    if (newMenu()) {
      setNewMenu(null)
      return
    }
    const lane = ctx()?.addr
    if (!lane) return
    const rect = btn.getBoundingClientRect()
    // 右端の button なので menu は右揃えで下に開く（x = 右端。CSS が translateX で寄せる）。
    setNewMenuPos({ x: rect.right, y: rect.bottom + 4 })
    newMenuReq = nextRequestId('pane-new')
    sendIpc({ t: 'conversation:stands_fetch', lane, req: newMenuReq })
  }

  /** menu 行 click: backend が新しい session id を採番し、lane の cwd から始める
   *  （doc 46 P2 要件 5）。tiling 既定なので新 pane はそのまま台に並ぶ。 */
  const createPane = (engine: string, mode: 'tui' | 'gui'): void => {
    const lane = ctx()?.addr
    setNewMenu(null)
    if (!lane) return
    sendIpc({ t: 'console:new_session', lane, engine, mode })
  }

  // doc 47 §6: `vp:conversation-agents` は共有 bus。自分の要求（相関 id）への応答だけで menu を
  // 開く（chat composer の要求では開かない）。連打しても最新 id 以外の応答は捨てられる。
  document.addEventListener('vp:conversation-agents', (e) => {
    const d = (e as CustomEvent<AgentsDetail<PaneStand>>).detail
    if (!isMyResponse(newMenuReq, d?.req)) return
    newMenuReq = null
    setNewMenu(d?.agents ?? [])
  })

  /** chip click（tui）: picker を開き、一覧を repo から取り直す（開くたび authoritative）。 */
  const openPicker = (chip: HTMLElement): void => {
    const lane = ctx()?.addr
    if (!lane) return
    const rect = chip.getBoundingClientRect()
    setPickerPos({ x: rect.left, y: rect.bottom + 4 })
    setPickerOpen(true)
    // doc 53 §11: roster は lanes snapshot が運ぶ（旧: ここで `echoes:sessions_fetch` を撃って
    // いた = 供給路 2 本目の入口）。picker は cache から拾う。
    setSessions(sessionListOf(lane))
  }

  /** picker 行 click: 既存 session へ root を向け替え（backend が slot を Resume respawn）。 */
  const switchRoot = (key: number): void => {
    const lane = ctx()?.addr
    setPickerOpen(false)
    if (!lane) return
    sendIpc({ t: 'console:switch_root', lane, session: key })
  }

  // doc 50 §4.6 A6: 「✨ 新 ID から」（旧 `newRoot`）は picker から撤去した — Add（Conversation を
  // 足す）と Reborn（その場で始め直す）の合成でしかなく、同じことをする口を 2 つ作らない。

  // doc 50 §4.6 A6: 見え方の乗り換えは **各 pane の名札 kind badge**（chatview の
  // `requestSessionMode`）へ移設。lane の名札はこの操作を持たない（Mode は lane ではなく
  // session の属性 — 旧 `requestModeSwitch` は同 PR で撤去した）。

  // 外側 click で閉じる（picker / chip の内側は除外 — chip click は toggle が担う）。
  document.addEventListener('click', (ev) => {
    const target = ev.target as HTMLElement | null
    if (pickerOpen() && !target?.closest('.eh-root-picker, .eh-session')) {
      setPickerOpen(false)
    }
    if (newMenu() && !target?.closest('.eh-new-menu, .eh-new')) {
      setNewMenu(null)
    }
  })

  // doc 50 §4.6 A6: 'vp:console-mode' 追従は退役（lane の名札は Mode を表示しない）。
  // session 一覧追従（handleSessionList の既存 bus。picker が閉じていても cache しておく）。
  document.addEventListener('vp:conversation-sessions', (e) => {
    const d = (e as CustomEvent<{ lane: string; sessions?: ConversationSession[] }>).detail
    if (d?.lane && d.lane === ctx()?.addr) setSessions(d.sessions ?? [])
  })
  // session summary 追従（console.ts の畳み込みが変化した時だけ飛ぶ低頻度 event）。
  document.addEventListener('vp:lane-header', (e) => {
    const d = (e as CustomEvent<{ lane: string }>).detail
    if (d?.lane && d.lane === ctx()?.addr) setSummary(vpConsole.headerState(d.lane))
  })

  const Header = () => (
    <div class="eh-root" classList={{ 'eh-empty': !ctx() }}>
      <Show when={ctx()}>
        {(c) => (
          <>
            <span class="eh-chip eh-lane" title={c().addr}>
              <CreoIcon name={STAND_ICON.claude.default} size={13} />
              {c().name ?? laneShortName(c().addr)}
            </span>
            <Show when={c().cwd}>
              <button
                type="button"
                class="eh-chip eh-cwd"
                classList={{ copied: copiedKey() === 'cwd' }}
                title={`${c().cwd}（click で full path を copy）`}
                onClick={() => copy('cwd', c().cwd!)}
              >
                {middleEllipsis(tildify(c().cwd!))}
              </button>
            </Show>
            <Show when={c().branch}>
              <span class="eh-chip eh-branch" title="git branch">
                <CreoIcon name="ph:git-branch" size={12} />
                {c().branch}
              </span>
            </Show>
            {/* session chip: root session の表示器 = 操作器（doc 39 P3、2026-07-18 mako 決定）。
                かつては tui 限定だった（gui は tab strip が識別と切替を担っていた）が、
                tab strip は doc 50 P1 の session = Pane で退役 — 担い手が消えたので両 Mode で
                出す。picker は root の名札 menu（Root 切替 + 見え方の乗り換え = 避難路）。
                供給路は setActivePane 相乗りの engine_session_id（ctx）— tui は
                ConversationEvent が流れないため summary は空になりうる（OR merge で両対応）。
                prefix は engine 別（cc/cdx/grok/oc、doc 37: chip が engine indicator を兼ねる）。 */}
            <Show when={summary().sessionId ?? c().sessionId}>
              {(sid) => (
                <button
                  type="button"
                  class="eh-chip eh-session"
                  title={`${sessionChipPrefix(c().agent)} session ${sid()}（click で Root 切替）`}
                  onClick={(ev) => {
                    if (pickerOpen()) setPickerOpen(false)
                    else openPicker(ev.currentTarget as HTMLElement)
                  }}
                >
                  {sessionChipPrefix(c().agent)}:{sid().slice(0, 8)}
                  <CreoIcon name="ph:caret-down" size={10} class="eh-session-caret" />
                </button>
              )}
            </Show>
            {/* doc 39 P3: Root 切替 picker。ヘッダ strip は overflow:hidden なので
                position:fixed で clipping の外に出す（開いた時点の chip 位置に固定）。 */}
            <Show when={pickerOpen()}>
              <div
                class="eh-root-picker"
                style={{ left: `${pickerPos().x}px`, top: `${pickerPos().y}px` }}
              >
                <div class="eh-rp-title">Root agent</div>
                <For
                  each={rootPickerItems(sessions())}
                  fallback={<div class="eh-rp-empty">読み込み中…</div>}
                >
                  {(item) => (
                    <button
                      type="button"
                      class="eh-rp-row"
                      classList={{ 'eh-rp-root': item.isRoot, 'eh-rp-disabled': item.disabled }}
                      title={
                        item.isRoot
                          ? '今の slot（root）'
                          : item.disabled
                            ? 'この session は resume を持たないため root に切替不可'
                            : 'この session を root にする（slot を resume で張り替え）'
                      }
                      onClick={() => {
                        if (item.disabled) return
                        if (item.isRoot) setPickerOpen(false)
                        else switchRoot(item.key)
                      }}
                    >
                      <CreoIcon
                        name={item.isRoot ? 'ph:circle-fill' : 'ph:circle'}
                        size={9}
                      />
                      {item.label}
                      <span class="eh-rp-key">#{item.key}</span>
                      <Show when={item.isRoot}>
                        <span class="eh-rp-now">今の slot</span>
                      </Show>
                    </button>
                  )}
                </For>
                {/* doc 50 §4.6 A6: 「✨ 新 ID から」は撤去した。
                    「新しい session を作る」は 2 つの操作に分かれ、この行はその合成でしかない:
                    ① lane の名札の **Add**（Conversation を足す = pane が増える）
                    ② その pane の **Reborn**（同じ場所で新しく始める = pane 数は不変）
                    root を新しい session にしたいなら「root pane で Reborn」で到達できる
                    （A6 で switch_root の tui 限定 gate も外れたので、どの session でも代表にできる）。
                    picker は **既存の Conversation から代表を選ぶ**ことに専念する。 */}
                <div class="eh-rp-divider" />
                <Show when={summary().sessionId ?? c().sessionId}>
                  {(sid) => (
                    <button
                      type="button"
                      class="eh-rp-row"
                      onClick={() => {
                        copy('sid', sid())
                        setPickerOpen(false)
                      }}
                    >
                      <CreoIcon name="ph:copy" size={11} />
                      今の id を copy
                    </button>
                  )}
                </Show>
                {/* doc 50 §4.6 A6: 「見え方」行はここから退役した。見え方は session の属性に
                    なり、切替は**各 pane の名札 kind badge**が持つ（§4.6 ② — Mode = Pane の
                    kind なので「この pane が何であるか」を名乗る名札が住処）。この picker は
                    root の付け替え（lane の代表を誰にするか）に専念する。 */}
              </div>
            </Show>
            {/* + New: 台に器械を足す（場所への操作 = 場所の名札の右端、mako 2026-07-24）。
                click で agents を取り直し（相関 id）、応答で menu が開く — 同期では開かない
                ので stopPropagation は不要（#836/#837: document click を無条件に止めない）。 */}
            <button
              type="button"
              class="eh-chip eh-new"
              title="Engine と Mode を選んで、この台に新しいコンソールを作る"
              onClick={(ev) => toggleNewMenu(ev.currentTarget as HTMLElement)}
            >
              <CreoIcon name="ph:plus" size={11} />
              New
            </button>
            <Show when={newMenu()}>
              {(agents) => (
                <div
                  class="eh-root-picker eh-new-menu"
                  style={{ left: `${newMenuPos().x}px`, top: `${newMenuPos().y}px` }}
                >
                  <For
                    each={newPaneChoices(agents())}
                    fallback={<div class="eh-rp-empty">engine なし</div>}
                  >
                    {(nc) => (
                      <button
                        type="button"
                        class="eh-rp-row"
                        onClick={() => createPane(nc.engine, nc.mode)}
                      >
                        {/* Mode は「Console（tui）」「Chat（gui）」で見せる — 内部語（tui/chat）は出さない */}
                        <CreoIcon
                          name={nc.mode === 'gui' ? 'ph:chat-circle' : 'ph:terminal-window'}
                          size={11}
                        />
                        {nc.engineLabel} · {nc.mode === 'gui' ? 'Chat' : 'Console'}
                      </button>
                    )}
                  </For>
                </div>
              )}
            </Show>
          </>
        )}
      </Show>
    </div>
  )

  render(() => <Header />, mount)

  return {
    setLane(next) {
      setCtx(next)
      setSummary(next ? vpConsole.headerState(next.addr) : {})
      // lane が替わったら picker / + New menu は閉じ、一覧は **新 lane の cache** に張り替える
      //（別 lane の session を誤表示しない / doc 53 §11: push は roster 変化時だけなので
      // 「開いた時」は自分で cache を読む）。
      setPickerOpen(false)
      setNewMenu(null)
      setSessions(next ? sessionListOf(next.addr) : [])
      // strip の開閉（World A の DOM に触れる唯一の接点）: lane 文脈がある時だけ
      // #pane-lane に .lane-header-active を付け、main_area.rs の --lane-header-h を
      // 0→30px にして strip を開く。無い時は高さ 0 = xterm/chat が全面（regression なし）。
      // これが無いと strip は #lane-header の overflow:hidden で永遠に高さ 0 = 不可視になる。
      document
        .getElementById('pane-lane')
        ?.classList.toggle('lane-header-active', !!next)
    },
  }
}

// ---------------------------------------------------------------------------
// CSS（entry.tsx が <style> で注入 — CHATVIEW_CSS と同 pattern）
// ---------------------------------------------------------------------------

export const LANE_HEADER_CSS = `
/* Conversation の名札（pane 上段）。素性だけを載せる 1 行 — 高さ・地色・境界は Pane 共通の
   名札 token（main_area.rs の --vp-nameplate-*）を参照し、.pane-header と同一の見えにする。
   strip 自体は window drag 面、chip / button は no-drag（.pane-header と同じ規約）。 */
#lane-header .eh-root{ display:flex; align-items:center; gap:6px;
  height:var(--vp-nameplate-h); padding:0 var(--vp-nameplate-pad-x);
  font-size:var(--vp-nameplate-font-size); background:var(--vp-nameplate-bg);
  border-bottom:var(--vp-nameplate-border);
  color:var(--color-text-secondary);
  font-family:var(--vp-font-sans),var(--typography-family-sans); font-weight:300;
  user-select:none; -webkit-app-region:drag; overflow:hidden; white-space:nowrap; }
#lane-header .eh-chip{ -webkit-app-region:no-drag; display:inline-flex; align-items:center; gap:4px;
  flex:none; padding:1px 8px; border-radius:9999px; background:transparent;
  border:1px solid var(--color-surface-border-subtle); color:var(--color-text-secondary);
  font:inherit; line-height:1.6; max-width:36ch; overflow:hidden; text-overflow:ellipsis; }
#lane-header button.eh-chip{ cursor:pointer; }
#lane-header button.eh-chip:hover{ background:var(--color-surface-bg-emphasis); color:var(--color-text-primary); }
#lane-header .eh-chip.copied{ border-color:var(--color-brand-primary); color:var(--color-text-primary); }
#lane-header .eh-lane{ color:var(--color-text-primary); border-color:transparent; padding-left:0; font-weight:500; }
#lane-header .eh-cwd, #lane-header .eh-session{
  font-family:var(--vp-font-mono),var(--typography-family-mono); font-size:10.5px; }
/* doc 39 P3: Root 切替 picker。strip の overflow:hidden を position:fixed で脱出する。 */
#lane-header .eh-session-caret{ opacity:.6; margin-left:2px; }
#lane-header .eh-root-picker{ position:fixed; z-index:1000; min-width:230px; padding:4px;
  background:var(--color-surface-surface); border:1px solid var(--color-surface-border-subtle);
  border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.35); -webkit-app-region:no-drag;
  font-family:var(--vp-font-sans),var(--typography-family-sans); }
#lane-header .eh-rp-title{ padding:4px 8px 2px; color:var(--color-text-secondary); font-size:10.5px; }
#lane-header .eh-rp-row{ display:flex; align-items:center; gap:6px; width:100%; text-align:left;
  padding:5px 8px; border:0; border-radius:6px; background:transparent; cursor:pointer;
  color:var(--color-text-primary);
  font-family:var(--vp-font-mono),var(--typography-family-mono); font-size:10.5px; line-height:1.6; }
#lane-header .eh-rp-row:hover{ background:var(--color-surface-bg-emphasis); }
#lane-header .eh-rp-key{ color:var(--color-text-secondary); }
#lane-header .eh-rp-row.eh-rp-root{ color:var(--color-brand-primary); }
#lane-header .eh-rp-row.eh-rp-disabled{ opacity:.45; cursor:default; }
#lane-header .eh-rp-row.eh-rp-disabled:hover{ background:transparent; }
#lane-header .eh-rp-now{ margin-left:auto; color:var(--color-text-secondary); font-size:10px; }
#lane-header .eh-rp-divider{ height:1px; margin:4px 6px; background:var(--color-surface-border-subtle); }
#lane-header .eh-rp-empty{ padding:6px 8px; color:var(--color-text-secondary); font-size:10.5px; }
/* doc 51 §1 A1: + New（台に器械を足す）。名札の右端 = margin-left:auto、作成の入口なので
   chip とは破線で区別（旧・帯の .pane-new と同じ記号論）。 */
#lane-header .eh-new{ margin-left:auto; border-style:dashed; }
/* + New の menu。器は root picker と同じ（.eh-root-picker を継承）。右端の button から
   開くので、fixed の基準点 x = button 右端 → translateX で右揃えにする。 */
#lane-header .eh-new-menu{ transform:translateX(-100%); }
`
