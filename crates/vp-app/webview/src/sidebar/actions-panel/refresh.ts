import { createSignal } from 'solid-js'
import { sendIpc } from '../ipc'
import { newActionId } from './store'
import type { ActionsRefreshResult } from '../../generated/SidebarIpc'

export const [actionsRefreshing,setActionsRefreshing] = createSignal(false)
export const [actionsRefreshError,setActionsRefreshError] = createSignal('')
let pending: {id: string; timer: ReturnType<typeof setTimeout>} | undefined
export function receiveActionsRefresh(result: ActionsRefreshResult): void {
  if(!pending || result.req!==pending.id) return
  clearTimeout(pending.timer);pending=undefined
  setActionsRefreshError(result.error ?? '')
  setActionsRefreshing(false)
}
/** App-wide single request: reload never touches drafts, selection, or the write outbox. */
export function refreshActions(): void {
  if(pending) return
  setActionsRefreshError('')
  if(!window.ipc) {setActionsRefreshError('アプリに接続していません');return}
  const id=newActionId()
  pending={id,timer:setTimeout(()=>receiveActionsRefresh({req:id,error:'再読み込みの応答を確認できませんでした'}),65000)}
  setActionsRefreshing(true)
  try {sendIpc({t:'actions:refresh',req:id})}
  catch(e) {receiveActionsRefresh({req:id,error:e instanceof Error?e.message:String(e)})}
}
