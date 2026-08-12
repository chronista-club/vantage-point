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
 * 表示器 = 操作器（session chip → picker）を載せる。**+ New は edge rail へ移設した**
 * （doc 56、2026-07-30 — lane 級動詞の家は右端の帯。名札は「読む帯」へ純化が進む）。
 *
 * session 単位の「今の文脈」（状態・model・permission・engine 異常）は各 pane の
 * **計器盤**（chatview の status 行 / composer）が持つ。かつては上段にも併置されていたが、
 * 同じ役割の二重実装だったので撤去した:
 * - `⏹ Stop` → 入力欄の「停止」（同じ `conversation:interrupt`）
 * - perm chip → status 行の perm select（表示だけの chip は操作器に含まれる）
 * - `⚠ engine` / `💤 休眠` → status 行（`deriveStatus` が同じ event を畳んでいる）
 *
 * 旧・下端の帯（`#pane-tabs`）は doc 51 §1 A1 で退役 — pane chip は tiling 既定で存在理由が
 * 消えた。+ New は一時ここに住んだ後、doc 56 で edge rail へ移設（動線一本化）。
 */

import { MAIN_DISPLAY_NAME, subNameOfAddress } from './lane-address'
import { render } from 'solid-js/web'
import { createSignal, For, Show } from 'solid-js'
import { CreoIcon } from '@chronista-club/creo-ui-icons-web'
import {
  sessionListOf,
  type VpConsole,
  type LaneHeaderState,
  type ConversationSession,
} from './console'
// （+ New の machinery — newPaneChoices / 相関 id / PaneStand 型 — は EdgeRail.tsx へ移設済み。
//  副産物: 旧 lane-panes ↔ LaneHeader の循環 import は newPaneChoices の移動で解消し、
//  依存は lane-panes → LaneHeader の片方向になった）
import { COMPONENT_ICON } from './icons/component'
import { copyText } from './clipboard'

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
 * address から表示用の短名を導く。Main は `main`、Sub は lane 名。
 *
 * ⚠️ 分解は `lane-address.ts` の 1 箇所に畳んである（旧実装は `/sub/` を**探す**形で、
 * address が `<repo>/lane/<name>` になると一致せず**address 全体をヘッダに出して**いた）。
 */
export function laneShortName(addr: string): string {
  return subNameOfAddress(addr) ?? MAIN_DISPLAY_NAME
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

  // + New（engine × Mode で新 session）は edge rail へ移設（doc 56 prototype、2026-07-30 —
  // 「二つ動線は混乱する」）。machinery（agents_fetch / 相関 id / menu）ごと EdgeRail.tsx に住む。

  const sendIpc = (payload: Record<string, unknown>): void => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify(payload))
  }

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
              <CreoIcon name={COMPONENT_ICON.claude.default} size={13} />
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
            {/* + New は edge rail（右端の帯）へ移設した（doc 56 prototype、mako 2026-07-30
                「二つ動線は混乱する」— 動線一本化。lane 級動詞の家は rail、名札は読む帯へ純化）。 */}
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
      // lane が替わったら picker は閉じ、一覧は **新 lane の cache** に張り替える
      //（別 lane の session を誤表示しない / doc 53 §11: push は roster 変化時だけなので
      // 「開いた時」は自分で cache を読む）。+ New menu の同処理は EdgeRail.setLane が担う。
      setPickerOpen(false)
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
/* + New の CSS（.eh-new / .eh-new-menu）は edge rail への移設に伴い撤去（EdgeRail.tsx の
   EDGE_RAIL_CSS が後継、doc 56 prototype 2026-07-30）。 */
`
