/**
 * EdgeRail — lane 級動詞の家（右端の縦帯、doc 56 前夜の prototype、2026-07-30 mako × Claude）。
 *
 * ## 原理: 動詞の級 = 住所
 *
 * - **lane 級**（lane 全体に効く、対象を選ばない）だけがここに住む: New（session を足す）/
 *   board 取っ手（#board-handle は main_area.rs の静的 DOM — 配線は board-view.ts のまま、
 *   住所だけ帯の中へ）。rail を見れば「この lane に対して何ができるか」が一覧できる =
 *   初見への self-documentation（B 形 = 不透明な帯が既定。A 形 = 浮遊は将来の設定トグル）
 * - **pane 級**（Clear / ✕ / form 切替）は各 pane の名札のまま
 * - **app 級**（⚙ 設定）はサイドバー下部 — ここには置かない
 *
 * ## + New の移設（LaneHeader → ここ）
 *
 * 「二つ動線は混乱する」（mako 2026-07-30）— LaneHeader の + New は撤去し、動線をここに
 * 一本化した。機構は移設そのまま: click → `conversation:agents_fetch`（相関 id）→
 * `vp:conversation-agents` 応答で menu が開く（同期では開かない）。menu は帯の左横に開く。
 *
 * ## 層の分離
 *
 * - **data**: lane signal（entry.tsx の applyActivePane が setLane で注入）+ menu 状態
 * - **calculations**: 選択肢の展開は `newPaneChoices`（lane-panes.ts、vitest 済）に委譲
 * - **actions**: mountEdgeRail（Solid render + IPC 送信 + 相関 id 照合 + 外側 click）
 */
import { render } from 'solid-js/web'
import { createSignal, For, Show } from 'solid-js'
import { CreoIcon } from '@chronista-club/creo-ui-icons-web'
import {
  isMyResponse,
  nextRequestId,
  type BusRequestId,
  type AgentsDetail,
} from './console'
import { newPaneChoices } from './lane-panes'
import { emitOpenNewLane } from './new-verbs-bridge'

/** `vp:conversation-agents` bus が運ぶ agent entry（newPaneChoices の入力と同形）。 */
type RailStand = { name: string; label?: string; chat_capable?: boolean }

export interface EdgeRailApi {
  /** 表示 lane の追従。null = lane 不在（帯ごと隠す — rail は lane 級動詞の家なので）。 */
  setLane(addr: string | null): void
}

/**
 * rail の Solid 部（New ボタン + menu）を mount する。
 *
 * @param railRoot #edge-rail（帯そのもの。lane 不在時の表示制御に使う）
 * @param mount    #edge-rail-new-host（Solid の render 先）
 */
export function mountEdgeRail(railRoot: HTMLElement, mount: HTMLElement): EdgeRailApi {
  const [lane, setLaneSignal] = createSignal<string | null>(null)
  // null = 閉。応答（相関 id 照合）で開く — LaneHeader 時代の流儀そのまま。
  const [menu, setMenu] = createSignal<RailStand[] | null>(null)
  const [menuPos, setMenuPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 })
  let menuReq: BusRequestId | null = null

  const sendIpc = (payload: Record<string, unknown>): void => {
    const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
    ipc?.postMessage(JSON.stringify(payload))
  }

  /** New click: 開いていれば閉じ、閉じていれば agents を取り直して開く（開くたび authoritative）。 */
  const toggleMenu = (btn: HTMLElement): void => {
    if (menu()) {
      setMenu(null)
      return
    }
    const addr = lane()
    if (!addr) return
    const rect = btn.getBoundingClientRect()
    // 帯の左横に開く（menu は CSS の translateX(-100%) で右端 = ボタン左縁に揃う）
    setMenuPos({ x: rect.left - 8, y: rect.top })
    menuReq = nextRequestId('rail-new')
    sendIpc({ t: 'conversation:agents_fetch', lane: addr, req: menuReq })
  }

  /** menu 行 click: backend が新しい session id を採番し、lane の cwd から始める
   *  （doc 46 P2 要件 5）。tiling 既定なので新 pane はそのまま台に並ぶ。 */
  const createPane = (engine: string, mode: 'tui' | 'gui'): void => {
    const addr = lane()
    setMenu(null)
    if (!addr) return
    sendIpc({ t: 'console:new_session', lane: addr, engine, mode })
  }

  // 共有 bus（doc 47 §6）: 自分の要求（相関 id）への応答だけで menu を開く。
  document.addEventListener('vp:conversation-agents', (e) => {
    const d = (e as CustomEvent<AgentsDetail<RailStand>>).detail
    if (!isMyResponse(menuReq, d?.req)) return
    menuReq = null
    setMenu(d?.agents ?? [])
  })

  // 外側 click で閉じる（menu / New ボタンの内側は除外 — ボタン click は toggle が担う）。
  document.addEventListener('click', (ev) => {
    const target = ev.target as HTMLElement | null
    if (menu() && !target?.closest('.er-menu, .rail-new')) setMenu(null)
  })

  const Rail = () => (
    <>
      <button
        type="button"
        class="rail-btn rail-new"
        data-label="New — Engine と Mode を選んで新しいコンソール"
        onClick={(ev) => toggleMenu(ev.currentTarget as HTMLElement)}
      >
        <CreoIcon name="ph:plus" size={15} />
      </button>
      <Show when={menu()}>
        {(agents) => (
          <div
            class="er-menu"
            style={{ left: `${menuPos().x}px`, top: `${menuPos().y}px` }}
          >
            <For
              each={newPaneChoices(agents())}
              fallback={<div class="er-empty">engine なし</div>}
            >
              {(nc) => (
                <button
                  type="button"
                  class="er-row"
                  onClick={() => createPane(nc.engine, nc.mode)}
                >
                  {/* Mode は「Console（tui）」「Chat（gui）」で見せる — 内部語は出さない */}
                  <CreoIcon
                    name={nc.mode === 'gui' ? 'ph:chat-circle' : 'ph:terminal-window'}
                    size={11}
                  />
                  {nc.engineLabel} · {nc.mode === 'gui' ? 'Chat' : 'Console'}
                </button>
              )}
            </For>
            {/* doc 58 ④: 産む動詞は級を跨いで New menu に集まる（sidebar の常設 + は撤去）。
                上の節 = この lane に console を足す / ここから下 = 新しい場所を産む。 */}
            <div class="er-sep" />
            <button
              type="button"
              class="er-row"
              onClick={() => {
                setMenu(null)
                // active repo の AddSub form（名簿内、ephemeral）を開く — `n` directive と同じ入口。
                emitOpenNewLane()
              }}
            >
              <CreoIcon name="ph:git-branch" size={11} />
              lane を作る…
            </button>
            <button
              type="button"
              class="er-row"
              onClick={() => {
                setMenu(null)
                // native folder picker → 登録（sidebar channel の既存 handler に届く —
                // 単一 WebView の ipc は tag で振り分けられるため bundle は関係ない）。
                sendIpc({ t: 'repo:add' })
              }}
            >
              <CreoIcon name="ph:folder-plus" size={11} />
              repo を登録…
            </button>
          </div>
        )}
      </Show>
    </>
  )

  render(() => <Rail />, mount)

  return {
    setLane(addr) {
      setLaneSignal(addr)
      setMenu(null) // lane が変われば menu の文脈も失効
      railRoot.style.display = addr ? '' : 'none'
    },
  }
}

/** rail の menu スタイル（帯・ボタン本体の CSS は main_area.rs の #edge-rail 系）。 */
export const EDGE_RAIL_CSS = `
.er-menu{ position:fixed; z-index:1000; min-width:230px; padding:4px;
  transform:translateX(-100%);
  background:var(--color-surface-surface); border:1px solid var(--color-surface-border-subtle);
  border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.35); -webkit-app-region:no-drag;
  font-family:var(--vp-font-sans),var(--typography-family-sans); }
.er-row{ display:flex; align-items:center; gap:6px; width:100%; text-align:left;
  padding:5px 8px; border:0; border-radius:6px; background:transparent; cursor:pointer;
  color:var(--color-text-primary);
  font-family:var(--vp-font-mono),var(--typography-family-mono); font-size:10.5px; line-height:1.6; }
.er-row:hover{ background:var(--color-surface-bg-emphasis); }
.er-empty{ padding:6px 8px; color:var(--color-text-secondary); font-size:10.5px; }
.er-sep{ margin:4px 6px; border-top:1px solid var(--color-surface-border-subtle); }
`
