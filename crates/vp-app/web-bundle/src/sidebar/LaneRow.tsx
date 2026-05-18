/**
 * Lane (Lead / Worker) 1 行の描画 component。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の `.vp-lane-row` 構築ロジックを SolidJS に port。
 * PR-2 は **読み取り描画のみ** — stand icon / label / worker git meta / awaiting dot /
 * mailbox icon / session title (2 行目)。 click 選択・context menu・restart/delete 操作は
 * PR-3 (操作) で追加する。
 */
import { Show } from 'solid-js'
import { CreoIcon } from 'creoui-icons-web'
import type { LaneInfo } from '../generated/LaneInfo'
import type { WorkerStatusWire } from '../generated/WorkerStatusWire'
import { sidebar } from './store'
import { isWorkerLane, laneAddressKey, laneLabel, standDisplayName, standIcon } from './lane'

/** Worker Lane の git 状態 (branch · ahead/behind · dirty · merged)。 */
function WorkerMeta(props: { ws: WorkerStatusWire }) {
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

export function LaneRow(props: { lane: LaneInfo }) {
  const addr = () => laneAddressKey(props.lane)
  const isActive = () => sidebar.active_lane_address === addr()
  // Pane (Echoes) 不在 = pid:null は disk-only Lane (workspace dir のみ)、 dim 表示。
  const isInactive = () => props.lane.pid == null
  const isWorker = () => isWorkerLane(props.lane)
  const icon = () => standIcon(props.lane.stand, isActive())
  // mailbox inbox: entry がある Lane のみ icon 表示 (mailbox infra が active)。
  const inbox = () => sidebar.lane_inboxes?.[addr()]
  // OSC 99 由来の入力待ち。 active lane は即読扱いで dot を出さない。 inactive も除外。
  const isAwaiting = () => !isInactive() && !isActive() && !!sidebar.awaiting_input[addr()]
  // cc `/rename` の custom-title (2 行目)。 未設定 lane は dimmed "—"。
  const sessionTitle = () => sidebar.session_titles?.[addr()]

  return (
    <div class="vp-lane-row" classList={{ active: isActive(), inactive: isInactive() }}>
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
      <Show when={isWorker() && props.lane.worker_status}>
        <WorkerMeta ws={props.lane.worker_status!} />
      </Show>
      <Show when={isAwaiting()}>
        <span class="vp-lane-awaiting" title="Claude is waiting for input" />
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
