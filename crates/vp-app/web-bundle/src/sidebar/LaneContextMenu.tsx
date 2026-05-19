/**
 * Lane 行の右クリック context menu (singleton popup)。
 *
 * VP-204 PR-1。 柱2 sidebar rebuild で port 漏れした VP-128 の Lane context menu を
 * SolidJS に復旧する。 inline hover ボタン (restart / delete) を廃し、 Lane 操作を
 * 本 menu に一本化する。 項目は Lane kind で変わる:
 *   - Lead Lane … Restart Lead Stand
 *   - Wing Lane … Restart Wing Stand / Delete Wing (2-click 確認)
 * project (Lead Lane) の削除は PR-2 (`process:delete` fullstack) で追加予定。
 *
 * state は Rust mirror の `store` とは別 — pure client-side UI state なので
 * 本 module 内の module-level signal で singleton 管理する。
 */
import { Show, createSignal } from 'solid-js'
import { CreoIcon } from 'creoui-icons-web'
import type { LaneInfo } from '../generated/LaneInfo'
import { sendIpc } from './ipc'
import { isWingLane, laneAddressKey, laneLabel } from './lane'

/** 開いている menu の対象と表示位置。 null = 非表示。 */
type MenuState = { lane: LaneInfo; projectPath: string; x: number; y: number }

const [menu, setMenu] = createSignal<MenuState | null>(null)

/** menu 幅/項目高さの目安 — viewport はみ出しの clamp に使う。 */
const MENU_W = 200
const ITEM_H = 30

/**
 * 当該 Lane に対し menu 項目が 1 つ以上あるか。
 *
 * inactive Lead (= 一時停止中 project の Lead Lane) は PR-1 では操作対象が無い
 * (restart 不可・project 削除は PR-2)。 その場合は menu を開かない。
 */
export function laneHasContextActions(lane: LaneInfo): boolean {
  const active = lane.pid != null
  return active || isWingLane(lane)
}

/** LaneRow の `onContextMenu` から呼ぶ — 右クリック位置に menu を開く。 */
export function openLaneContextMenu(
  lane: LaneInfo,
  projectPath: string,
  x: number,
  y: number,
): void {
  // viewport 右端/下端からはみ出さないよう左/上に寄せる。
  const clampedX = Math.min(x, window.innerWidth - MENU_W - 4)
  const clampedY = Math.min(y, window.innerHeight - ITEM_H * 3 - 4)
  setMenu({ lane, projectPath, x: Math.max(4, clampedX), y: Math.max(4, clampedY) })
}

/** sidebar に 1 つだけ mount する singleton menu。 */
export function LaneContextMenu() {
  const close = () => setMenu(null)

  return (
    <Show when={menu()}>
      {(m) => {
        const lane = () => m().lane
        const addr = () => laneAddressKey(lane())
        const isWing = () => isWingLane(lane())
        const isActive = () => lane().pid != null
        // delete は破壊的 (PTY kill + tmux kill + workspace dir 削除) なので 2-click 確認。
        const [confirmDelete, setConfirmDelete] = createSignal(false)

        const onRestart = () => {
          sendIpc({ t: 'lane:restart', path: m().projectPath, address: addr() })
          close()
        }
        const onDelete = () => {
          if (confirmDelete()) {
            sendIpc({ t: 'lane:delete', path: m().projectPath, address: addr() })
            close()
          } else {
            setConfirmDelete(true)
          }
        }

        return (
          <>
            {/* click-away / 右クリック / で閉じる透明 backdrop。 */}
            <div
              class="vp-ctx-backdrop"
              onClick={close}
              onContextMenu={(e) => {
                e.preventDefault()
                close()
              }}
            />
            <div class="vp-ctx-menu" style={{ left: `${m().x}px`, top: `${m().y}px` }}>
              <div class="vp-ctx-header" title={addr()}>
                {laneLabel(lane())}
              </div>
              <Show when={isActive()}>
                <div class="vp-ctx-item" onClick={onRestart}>
                  <CreoIcon name="ph:arrow-clockwise" size={13} />
                  <span>Restart {isWing() ? 'Wing' : 'Lead'} Stand</span>
                </div>
              </Show>
              <Show when={isWing()}>
                <div
                  class="vp-ctx-item danger"
                  classList={{ confirming: confirmDelete() }}
                  onClick={onDelete}
                >
                  <CreoIcon name={confirmDelete() ? 'ph:check' : 'ph:trash'} size={13} />
                  <span>{confirmDelete() ? 'もう一度クリックで削除' : 'Delete Wing'}</span>
                </div>
              </Show>
            </div>
          </>
        )
      }}
    </Show>
  )
}
