/**
 * Lane (Lead / Wing) 1 行の描画 component。
 *
 * v1.0 柱 2。 旧 SIDEBAR_HTML の `.vp-lane-row` 構築ロジックを SolidJS に port。
 * 描画 (PR-2): stand icon / label / wing git meta / awaiting dot / mailbox icon /
 * session title (2 行目)。 click 選択 (PR-3): row click → `lane:select` IPC で
 * main area を当該 Lane に切り替え。 context menu・restart/delete 操作は後続。
 */
import { Show, createSignal } from 'solid-js'
import { CreoIcon } from 'creoui-icons-web'
import type { LaneInfo } from '../generated/LaneInfo'
import type { WingStatusWire } from '../generated/WingStatusWire'
import { sidebar } from './store'
import { sendIpc } from './ipc'
import { isWingLane, laneAddressKey, laneLabel, standDisplayName, standIcon } from './lane'

/** Wing Lane の git 状態 (branch · ahead/behind · dirty · merged)。 */
function WingMeta(props: { ws: WingStatusWire }) {
  const ahead = () => props.ws.ahead | 0
  const behind = () => props.ws.behind | 0
  const dirty = () => props.ws.dirty_count | 0
  return (
    <span class="vp-lane-meta">
      <Show when={props.ws.branch}>
        <span>{props.ws.branch}</span>
      </Show>
      <Show when={ahead() > 0}>
        <span class="ahead">↑{ahead()}</span>
      </Show>
      <Show when={behind() > 0}>
        <span class="behind">↓{behind()}</span>
      </Show>
      <Show when={dirty() > 0}>
        <span class="dirty">{dirty()}M</span>
      </Show>
      <Show when={props.ws.is_merged}>
        <span class="merged">merged</span>
      </Show>
    </span>
  )
}

export function LaneRow(props: { lane: LaneInfo; projectPath: string }) {
  const addr = () => laneAddressKey(props.lane)
  const isActive = () => sidebar.active_lane_address === addr()
  // Pane (Echoes) 不在 = pid:null は disk-only Lane (workspace dir のみ)、 dim 表示。
  const isInactive = () => props.lane.pid == null
  const isWing = () => isWingLane(props.lane)
  const icon = () => standIcon(props.lane.stand, isActive())
  // mailbox inbox: entry がある Lane のみ icon 表示 (mailbox infra が active)。
  const inbox = () => sidebar.lane_inboxes?.[addr()]
  // OSC 99 由来の入力待ち。 active lane は即読扱いで dot を出さない。 inactive も除外。
  const isAwaiting = () => !isInactive() && !isActive() && !!sidebar.awaiting_input[addr()]
  // cc `/rename` の custom-title (2 行目)。 未設定 lane は dimmed "—"。
  const sessionTitle = () => sidebar.session_titles?.[addr()]

  // row click → main area を当該 Lane に切り替え。 Inactive Lane (pid:null) は SP に
  // PtySlot が無く、 select すると WS 1006 切断 → reconnect loop に入るためガード
  // (旧 SIDEBAR_HTML と同じ挙動)。
  const onSelect = () => {
    if (isInactive()) return
    sendIpc({ t: 'lane:select', path: props.projectPath, address: addr() })
  }

  // restart は respawn で復帰可能なので即時。 操作ボタンの click は row の
  // lane:select に伝播させない (stopPropagation)。
  const onRestart = (e: MouseEvent) => {
    e.stopPropagation()
    sendIpc({ t: 'lane:restart', path: props.projectPath, address: addr() })
  }
  // delete は破壊的 (PTY kill + tmux kill + workspace dir 削除) なので 2-click 確認:
  // 1 回目で確認状態、 2 回目で実行。 row から mouse が離れると自動解除。
  const [confirmDelete, setConfirmDelete] = createSignal(false)
  const onDelete = (e: MouseEvent) => {
    e.stopPropagation()
    if (confirmDelete()) {
      sendIpc({ t: 'lane:delete', path: props.projectPath, address: addr() })
    } else {
      setConfirmDelete(true)
    }
  }

  return (
    <div
      class="vp-lane-row"
      classList={{ active: isActive(), inactive: isInactive() }}
      onClick={onSelect}
      onMouseLeave={() => setConfirmDelete(false)}
    >
      <Show when={icon()}>
        <span class="vp-lane-icon" title={standDisplayName(props.lane.stand)}>
          <CreoIcon name={icon()!} size={14} />
        </span>
      </Show>
      <Show when={inbox()}>
        <span
          class="vp-lane-msg"
          classList={{ unread: ((inbox()!.unread_count | 0) > 0) }}
          title={`mailbox: ${addr()}`}
        >
          <CreoIcon
            name={(inbox()!.unread_count | 0) > 0 ? 'ph:envelope-fill' : 'ph:envelope'}
            size={13}
          />
        </span>
      </Show>
      <span class="vp-lane-label">{laneLabel(props.lane)}</span>
      <Show when={isWing() && props.lane.wing_status}>
        <WingMeta ws={props.lane.wing_status!} />
      </Show>
      <Show when={isAwaiting()}>
        <span class="vp-lane-awaiting" title="Claude is waiting for input" />
      </Show>
      <Show when={!isInactive()}>
        <button class="vp-lane-btn" title="Restart Lane Stand" onClick={onRestart}>
          <CreoIcon name="ph:arrow-clockwise" size={12} />
        </button>
      </Show>
      <Show when={isWing()}>
        <button
          class="vp-lane-btn vp-lane-btn-danger"
          classList={{ confirming: confirmDelete() }}
          title={confirmDelete() ? 'もう一度クリックで削除' : 'Delete Wing'}
          onClick={onDelete}
        >
          <CreoIcon name={confirmDelete() ? 'ph:check' : 'ph:x'} size={12} />
        </button>
      </Show>
      <Show
        when={sessionTitle()}
        fallback={
          <div class="vp-lane-line2 empty" title="/rename で session 名を設定すると表示されます">
            —
          </div>
        }
      >
        <div class="vp-lane-line2" title={sessionTitle()!}>
          {sessionTitle()}
        </div>
      </Show>
    </div>
  )
}
