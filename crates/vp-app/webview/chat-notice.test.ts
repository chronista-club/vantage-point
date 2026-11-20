// mem_1ChDPjq74fG87DYtSn3ns3 — 届いた notice の表示と寿命。
import { describe, expect, it } from 'vitest'
import * as model from './chat-model'

const notice = (message: string) => ({ kind: 'notice' as const, message })
const start = { kind: 'replay_start' as const }
const end = { kind: 'replay_end' as const, in_flight: false }

describe('chat notices', () => {
  it.each(['error', 'loader cleared'] as const)('再同期ローダーが %s で下りても、replay の通知は次の配信で消す', reason => {
    const s = model.emptyChatState()
    model.foldInto(s, start)
    if (reason === 'error') model.foldInto(s, { kind: 'error', message: '過去のエラー' })
    else s.replaying = false // focus 変更・watchdog が下ろす表示状態。
    model.foldInto(s, notice('replay 内の知らせ'))
    model.foldInto(s, end)
    expect(s.replayNotices).toEqual(['replay 内の知らせ'])
    expect(s.liveNotices).toEqual([])
    model.foldInto(s, start)
    expect(s.replayNotices).toEqual([])
  })

  it('replay 内の通知は次の replay で置き換わり、同文を積まない', () => {
    const s = model.emptyChatState()
    model.foldInto(s, start)
    model.foldInto(s, notice('古い部分を省いています'))
    model.foldInto(s, notice('古い部分を省いています'))
    model.foldInto(s, end)
    expect(s.replayNotices).toEqual(['古い部分を省いています'])
    model.foldInto(s, start)
    expect(s.replayNotices).toEqual([])
    model.foldInto(s, notice('別の知らせ'))
    model.foldInto(s, end)
    expect(s.replayNotices).toEqual(['別の知らせ'])
    expect(s.items).toEqual([])
  })

  it('最初の replay 前と replay 後の通知は、再配信・turn 完了でも残る', () => {
    const s = model.emptyChatState()
    model.foldInto(s, notice('新しい会話を始めました'))
    model.foldInto(s, start)
    model.foldInto(s, end)
    model.foldInto(s, notice('後からの知らせ'))
    model.foldInto(s, { kind: 'turn_completed', session_id: 's' })
    model.foldInto(s, start)
    model.foldInto(s, end)
    expect(s.liveNotices).toEqual(['新しい会話を始めました', '後からの知らせ'])
    expect(s.items).toEqual([])
    expect(s.streaming).toBe(false)
  })

  it('同文が replay の内外で届いても、閉じるまで残す帯に一本化する', () => {
    const s = model.emptyChatState()
    model.foldInto(s, start)
    model.foldInto(s, notice('知らせ'))
    model.foldInto(s, end)
    model.foldInto(s, notice('知らせ'))
    model.foldInto(s, notice('知らせ'))
    expect(s.liveNotices).toEqual(['知らせ'])
    expect(s.replayNotices).toEqual([])
    model.foldInto(s, start)
    model.foldInto(s, notice('知らせ'))
    model.foldInto(s, end)
    expect(s.liveNotices).toEqual(['知らせ'])
    expect(s.replayNotices).toEqual([])
  })

  it('閉じた通知だけを除き、再び届いた通知は表示する', () => {
    const s = model.emptyChatState()
    model.foldInto(s, notice('閉じる'))
    model.foldInto(s, notice('残す'))
    expect(s.liveNotices).toEqual(['閉じる', '残す'])
    model.dismissNotice(s, '閉じる')
    model.foldInto(s, start)
    model.foldInto(s, end)
    expect(s.liveNotices).toEqual(['残す'])
    model.foldInto(s, notice('閉じる'))
    expect(s.liveNotices).toEqual(['残す', '閉じる'])
  })

  it('通知は session ごとに分かれ、新しい state には引き継がない', () => {
    const first = model.emptyChatState()
    model.foldInto(first, notice('この session の知らせ'))
    expect(first.liveNotices).toEqual(['この session の知らせ'])
    const second = model.emptyChatState()
    expect(second.liveNotices).toEqual([])
    expect(second.replayNotices).toEqual([])
  })
})
