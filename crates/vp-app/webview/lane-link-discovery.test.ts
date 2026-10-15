// mem_1Cg4Mh2158eTTSiPPymwWp
import { describe, expect, it, vi } from 'vitest'
import { extractLoopbackUrls, LoopbackUrlHistory, observeConversationUrls, observeTerminalUrls } from './lane-link-discovery'

describe('出力のローカル URL', () => {
  it.each([false, true])('32 KiB 本文の先頭 URL を捨てない（complete=%s）', complete => {
    const history = new LoopbackUrlHistory()
    history.feed('large', 'pty:1', 'Local: http://localhost:5173/\n' + 'x'.repeat(32768) + '\n', complete)
    expect(history.get('large')).toEqual(['http://localhost:5173/'])
  })
  it('秘密情報らしい query key の URL を検出しないが通常の query は保持する', () => {
    const keys = ['token', 'api_key', 'client_secret', 'password', 'auth', 'X-Amz-Signature', 'accessToken', '%74oken']
    expect(extractLoopbackUrls(keys.map(key => `http://localhost:8888/?${key}=value`).join(' '))).toEqual([])
    expect(extractLoopbackUrls('http://localhost:8888/?page=2')).toEqual(['http://localhost:8888/?page=2'])
  })
  it('CSI のカーソル移動をまたいで URL と別の文字列を連結しない', () => {
    expect(extractLoopbackUrls('http://localhost:3000\x1b[2;1H/foo')).toEqual(['http://localhost:3000/'])
  })
  it('ANSI と句読点を除き、wildcard を接続先に直し、末尾 / と重複を正規化する', () => {
    expect(extractLoopbackUrls('\x1b[32mLocal: http://localhost:5173/\x1b[0m\n(http://localhost:5173), http://0.0.0.0:8080/ http://127.0.0.1:8080 https://[::1]:8443/')).toEqual([
      'http://localhost:5173/', 'http://127.0.0.1:8080/', 'https://[::1]:8443/',
    ])
  })
  it('port と path/query を保ち、localhost 風の外部 host・資格情報・不正 port を拾わない', () => {
    expect(extractLoopbackUrls('http://localhost:3000 http://localhost:3001/a?q=1 http://127.0.0.2:8080/ https://example.com http://localhost.evil:3000 http://192.168.1.1:3000 http://user:pass@localhost:3000 http://localhost:0 http://localhost:99999 ftp://localhost:3000')).toEqual([
      'http://localhost:3000/', 'http://localhost:3001/a?q=1', 'http://127.0.0.2:8080/',
    ])
  })
  it('chunk を session ごとにつなぎ、別 lane の URL を混ぜず、重複と件数を抑える', () => {
    const history = new LoopbackUrlHistory(2)
    history.feed('a', 'pty:1', '\x1b[32mhttp://local')
    history.feed('b', 'pty:1', 'http://localhost:9999/\n')
    history.feed('a', 'pty:2', 'host:1234\n')
    expect(history.get('a')).toEqual([])
    history.feed('a', 'pty:1', 'host:3000/\x1b[0m\n')
    history.feed('a', 'tool:1', 'http://localhost:3000 http://127.0.0.1:4000', true)
    expect(history.get('a')).toEqual(['http://localhost:3000/', 'http://127.0.0.1:4000/'])
    history.feed('a', 'pty:1', 'http://localhost:5000\n')
    expect(history.get('a')).toEqual(['http://127.0.0.1:4000/', 'http://localhost:5000/'])
    expect(history.get('b')).toEqual(['http://localhost:9999/'])
  })
  it('chat の text / tool 出力と履歴を拾い、user 入力や tool 引数を拾わない', () => {
    const history = new LoopbackUrlHistory()
    const observe = (event: Parameters<typeof observeConversationUrls>[1], session = 1) => observeConversationUrls('chat', event, session, history)
    observe({kind:'user_message',text:'http://localhost:1000'})
    observe({kind:'tool_call',id:'one',name:'shell',input:{command:'curl http://localhost:2000'}})
    observe({kind:'message_chunk',text:'Preview: http://local'})
    observe({kind:'message_chunk',text:'host:3000'})
    expect(history.get('chat')).toEqual([])
    observe({kind:'turn_completed',session_id:'s'})
    observe({kind:'tool_call_update',tool_use_id:'one',content:'Listening http://0.0.0.0:4000',is_error:false})
    observe({kind:'subagent_message',parent_tool_use_id:'sub',role:'text',text:'http://localhost:4500'})
    observe({kind:'codex_history',thread_id:'t',user_message_ids:[],in_flight:false,truncated:false,events:[{kind:'codex_message',item_id:'m',text:'http://[::1]:5000/',questions:[],append:false}]},2)
    expect(history.get('chat')).toEqual(['http://localhost:3000/','http://127.0.0.1:4000/','http://localhost:4500/','http://[::1]:5000/'])
  })
  it('PTY の分割 UTF-8 と改行のない末尾 URL を取り込み、partial port を残さない', () => {
    vi.useFakeTimers()
    try {
      const history = new LoopbackUrlHistory()
      const bytes = new TextEncoder().encode('\x1b[32mhttp://localhost:5173/日本\x1b[0m')
      const split = bytes.indexOf(0xe6) + 1
      observeTerminalUrls('pty', 1, bytes.slice(0, split), history)
      observeTerminalUrls('pty', 1, bytes.slice(split), history)
      expect(history.get('pty')).toEqual([])
      vi.runAllTimers()
      expect(history.get('pty')).toEqual(['http://localhost:5173/%E6%97%A5%E6%9C%AC'])
    } finally { vi.useRealTimers() }
  })
  it('Codex の全文置換と replay の開始で、古い未完 chunk を連結しない', () => {
    const history = new LoopbackUrlHistory()
    observeConversationUrls('replace',{kind:'codex_message',item_id:'m',text:'http://localhost:30',questions:[],append:true},1,history)
    observeConversationUrls('replace',{kind:'codex_message',item_id:'m',text:'http://localhost:3000',questions:[],append:false},1,history)
    expect(history.get('replace')).toEqual(['http://localhost:3000/'])
    observeConversationUrls('replace',{kind:'message_chunk',text:'http://local'},1,history)
    observeConversationUrls('replace',{kind:'replay_start'},1,history)
    observeConversationUrls('replace',{kind:'message_chunk',text:'http://localhost:4000\n'},1,history)
    expect(history.get('replace')).toEqual(['http://localhost:3000/','http://localhost:4000/'])
  })
})
