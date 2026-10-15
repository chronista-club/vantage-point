/** 保存結果の IPC は sidebar bundle が所有する。Lane 節の host へ同じ bundle から描く。 */
import { createMemo, createSignal } from 'solid-js'
import { render } from 'solid-js/web'
import { onLaneLinksView, type LaneLinksView } from '../../lane-links-bridge'
import { LocalUrls } from './LocalUrls'
import { sidebar } from './store'
import { laneAddressKey } from './lane'

export function installLaneLinks(): () => void {
  const [view, setView] = createSignal<LaneLinksView | null>(null)
  let host: HTMLElement | null = null
  let dispose: (() => void) | undefined
  const unsubscribe = onLaneLinksView(next => {
    setView(next)
    if (host === next.host) return
    dispose?.()
    host = next.host
    dispose = render(() => {
      // PR / 検出結果だけが変わった時に load effect と編集中の draft を再初期化しない。
      const address = createMemo(() => view()?.lane ?? '')
      const active = createMemo(() => view()?.visible ?? false)
      const repoPath = createMemo(() => Object.entries(sidebar.lanes_by_repo).find(([, lanes]) => lanes.some(lane => laneAddressKey(lane) === address()))?.[0] ?? '')
      return <LocalUrls repoPath={repoPath()} address={address()} active={active()} discovered={view()?.urls ?? []} pr={view()?.pr} />
    }, host)
  })
  return () => { unsubscribe(); dispose?.() }
}
