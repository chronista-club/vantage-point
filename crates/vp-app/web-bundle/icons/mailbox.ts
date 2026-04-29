// VP Mailbox glyph mapping (creo-ui-icons-web の VP-domain alias)。
//
// Mailbox = ECS 的メッセージング primitive (VP-24)。 inbox / send /
// broadcast / notify 等の semantic action を icon で表現。
// state-driven: default = regular, active = fill (unread indicator 等)。
//
// 参考 memory: feedback_creo_ui_icon_dual_axis.md (2026-04-29)
// 参考 spec: project_mailbox_address_spec.md、 vp_mailbox_monitor_agent_inbox.md

import type { IconName } from 'creo-ui-icons-web'

export type MailboxAction =
  | 'inbox'      // 受信箱 (incoming messages)
  | 'send'       // 送信 (outgoing message)
  | 'broadcast'  // 全体通知
  | 'notify'     // notification bell
  | 'thread'     // thread 表示 (chat 風)
  | 'archive'    // archive
  | 'delete'     // delete

export interface MailboxIconSet {
  default: IconName
  active: IconName
}

export const MAILBOX_ICON: Record<MailboxAction, MailboxIconSet> = {
  inbox: {
    default: 'mingcute:inbox-line',
    active: 'mingcute:inbox-fill',
  },
  send: {
    default: 'mingcute:send-line',
    active: 'mingcute:send-fill',
  },
  broadcast: {
    default: 'ph:broadcast',
    active: 'ph:broadcast-fill',
  },
  notify: {
    default: 'mingcute:notification-line',
    active: 'mingcute:notification-fill',
  },
  thread: {
    default: 'ph:chat-circle',
    active: 'ph:chat-circle-fill',
  },
  archive: {
    default: 'mingcute:archive-line',
    active: 'mingcute:archive-fill',
  },
  delete: {
    default: 'mingcute:delete-line',
    active: 'mingcute:delete-fill',
  },
}

export function iconForMailbox(
  action: MailboxAction,
  state: 'default' | 'active' = 'default',
): IconName {
  return MAILBOX_ICON[action][state]
}
