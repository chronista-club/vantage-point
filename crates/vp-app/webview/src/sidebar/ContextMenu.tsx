/**
 * sidebar の右クリック context menu (generic singleton popup)。
 *
 * VP-204 PR-1。 柱2 sidebar rebuild で port 漏れした VP-128 の context menu を
 * SolidJS に復旧する。 Lane 行 / project ヘッダ 共通の popup — caller が `openContextMenu`
 * に header + 項目配列 + 右クリック座標を渡す。 破壊的項目は `confirm` 指定で 2-click 確認。
 *
 * state は Rust mirror の `store` とは別 — pure client-side UI state なので
 * 本 module 内の module-level signal で singleton 管理する。
 */
import { For, Show, createSignal } from 'solid-js'
import { CreoIcon } from 'creoui-icons-web'
import type { IconName } from 'creoui-icons-web'

/** context menu の 1 項目。 */
export type ContextMenuItem = {
  /** 通常時ラベル。 */
  label: string
  /** 通常時アイコン。 */
  icon: IconName
  /** danger 配色 (削除系)。 */
  danger?: boolean
  /**
   * 破壊的操作の 2-click 確認。 指定すると 1 回目クリックで確認状態
   * (label/icon が confirm 側に切り替わる)、 2 回目クリックで `onSelect` 実行。
   */
  confirm?: { label: string; icon: IconName }
  /** 選択時の動作。 */
  onSelect: () => void
}

/** 開いている menu の内容と表示位置。 null = 非表示。 */
type MenuState = { header: string; items: ContextMenuItem[]; x: number; y: number }

const [menu, setMenu] = createSignal<MenuState | null>(null)

/** menu 幅/項目高さの目安 — viewport はみ出しの clamp に使う。 */
const MENU_W = 200
const ITEM_H = 30

/**
 * 右クリック位置に menu を開く。 `items` が空なら開かない (操作対象なし)。
 * viewport 右端/下端からはみ出さないよう左/上に clamp する。
 */
export function openContextMenu(
  header: string,
  items: ContextMenuItem[],
  x: number,
  y: number,
): void {
  if (items.length === 0) return
  const clampedX = Math.min(x, window.innerWidth - MENU_W - 4)
  const clampedY = Math.min(y, window.innerHeight - ITEM_H * (items.length + 1) - 4)
  setMenu({ header, items, x: Math.max(4, clampedX), y: Math.max(4, clampedY) })
}

/** sidebar に 1 つだけ mount する singleton menu。 */
export function ContextMenu() {
  const close = () => setMenu(null)

  return (
    <Show when={menu()}>
      {(m) => {
        // 2-click 確認中の項目 index (confirm 指定項目のみ)。 null = 確認状態なし。
        const [confirming, setConfirming] = createSignal<number | null>(null)

        const onItem = (item: ContextMenuItem, idx: number) => {
          // confirm 指定項目は 1 回目で確認状態に入り、 2 回目で実行。
          if (item.confirm && confirming() !== idx) {
            setConfirming(idx)
            return
          }
          item.onSelect()
          close()
        }

        return (
          <>
            {/* click-away / 右クリックで閉じる透明 backdrop。 */}
            <div
              class="vp-ctx-backdrop"
              onClick={close}
              onContextMenu={(e) => {
                e.preventDefault()
                close()
              }}
            />
            <div class="vp-ctx-menu" style={{ left: `${m().x}px`, top: `${m().y}px` }}>
              <div class="vp-ctx-header">{m().header}</div>
              <For each={m().items}>
                {(item, idx) => {
                  const isConfirming = () => confirming() === idx()
                  const label = () =>
                    isConfirming() && item.confirm ? item.confirm.label : item.label
                  const icon = () =>
                    isConfirming() && item.confirm ? item.confirm.icon : item.icon
                  return (
                    <div
                      class="vp-ctx-item"
                      classList={{ danger: !!item.danger, confirming: isConfirming() }}
                      onClick={() => onItem(item, idx())}
                    >
                      <CreoIcon name={icon()} size={13} />
                      <span>{label()}</span>
                    </div>
                  )
                }}
              </For>
            </div>
          </>
        )
      }}
    </Show>
  )
}
