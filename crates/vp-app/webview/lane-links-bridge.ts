/** main の Lane 節と sidebar bundle の登録 UI の接点。同じ document 内で最後の表示を共有する。 */
import type { Pr } from './pr-list'

export interface LaneLinksView {
  host: HTMLElement
  lane: string | null
  visible: boolean
  urls: string[]
  pr: Pr | null
}
const EVENT = 'vp:lane-links-view'
declare global { interface Window { vpLaneLinksView?: LaneLinksView } }

export function publishLaneLinksView(view: LaneLinksView): void {
  window.vpLaneLinksView = view
  window.dispatchEvent(new CustomEvent(EVENT, { detail: view }))
}

/** bundle の評価順に依存しない。後から来た受け手にも現在の表示を一度渡す。 */
export function onLaneLinksView(receive: (view: LaneLinksView) => void): () => void {
  const listener = (event: Event) => receive((event as CustomEvent<LaneLinksView>).detail)
  window.addEventListener(EVENT, listener)
  if (window.vpLaneLinksView) receive(window.vpLaneLinksView)
  return () => window.removeEventListener(EVENT, listener)
}
