/**
 * `sidebar/dispatch.ts` — Rust → sidebar bundle の押し込みの単一受け口。
 *
 * ここで守るのは main bundle の `dispatch.test.ts` と同じ **「押し込みが黙って落ちない」**。
 * 旧来 `window.renderSidebarState(...)` は bundle 準備前なら no-op で「成功」していた。
 *
 * ⚠️ **この 9 面のうち実機で目視できたのは `sidebar:state` だけ**（他は overlay を開く操作が
 * 要る）。残り 7 面の自動検証はここが唯一なので、arm を足したら test も足すこと。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { IpcEventEnvelope } from '../generated/SidebarIpc'
import type { SidebarPushHandlers } from './dispatch'

/** 呼ばれた動詞を到着順に記録する handler。 */
function recordingHandlers(): { calls: string[]; handlers: SidebarPushHandlers } {
  const calls: string[] = []
  return {
    calls,
    handlers: {
      state: (s) => calls.push(`state:${(s as { version?: number }).version}`),
      error: (m) => calls.push(`error:${m}`),
      subCreateResult: (p, n, e) => calls.push(`sub:${p}/${n}:${e}`),
      agentsResult: (p, s, e) => calls.push(`agents:${p}:${s.length}:${e}`),
      wireResult: (p) => calls.push(`wire:${(p as { total?: number }).total}`),
      clonePathPicked: (p) => calls.push(`clone:${p}`),
      settingsResult: (dev, locked, root, resolved) =>
        calls.push(`settings:${dev}:${locked}:${root}:${resolved}`),
    },
  }
}

const dispatch = (msg: IpcEventEnvelope): void => {
  ;(window as unknown as { vpSidebarDispatch: (m: IpcEventEnvelope) => void }).vpSidebarDispatch(
    msg,
  )
}

describe('sidebar dispatch', () => {
  beforeEach(() => {
    // vitest の environment は node。dispatch.ts が触るのは `window.vpSidebarDispatch` だけ。
    ;(globalThis as unknown as { window: unknown }).window = globalThis
    vi.resetModules()
  })

  it('install 前に届いた push を到着順で流す', async () => {
    // module state（pending / handlers）は module 単位なので毎回読み直す。
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()

    dispatch({ t: 'sidebar:state', state: { version: 1 } })
    dispatch({ t: 'sidebar:error', message: 'boom' })

    const { calls, handlers } = recordingHandlers()
    // install 前は 1 件も実行されていない（= 落としてもいない）。
    expect(calls).toEqual([])

    mod.installSidebarDispatch(handlers)
    expect(calls).toEqual(['state:1', 'error:boom'])
  })

  it('install 後は保留を経由せず直通で呼ばれる', async () => {
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)

    dispatch({ t: 'clone:path_picked', path: '/tmp/x' })
    expect(calls).toEqual(['clone:/tmp/x'])
  })

  it('保留分と直通分が 1 本の順序に並ぶ', async () => {
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    dispatch({ t: 'clone:path_picked', path: '/tmp/a' })

    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)
    dispatch({ t: 'clone:path_picked', path: '/tmp/b' })

    expect(calls).toEqual(['clone:/tmp/a', 'clone:/tmp/b'])
  })

  it('optional field の省略は null として渡る（成功 = error 無し）', async () => {
    // `error` は schema で optional。旧実装は JS へ `null` を直書きしていたので、
    // ここが崩れると「成功なのに error あり」と受け手が誤読する。
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)

    dispatch({ t: 'sub:create_result', repo_path: '/p', name: 'w1' })
    dispatch({ t: 'sub:create_result', repo_path: '/p', name: 'w2', error: 'ng' })
    dispatch({ t: 'agents:result', repo_path: '/p', agents: [{}, {}] })

    expect(calls).toEqual(['sub:/p/w1:null', 'sub:/p/w2:ng', 'agents:/p:2:null'])
  })

  it('オブジェクトを分解した面が元の形で受け手に届く', async () => {
    // Rust 側は envelope の top-level field に展開し、受け手は旧来どおり 1 オブジェクトで
    // 受ける。組み立て直しは `ipc.ts` の handler の仕事で、ここは分解側の順序を固定する。
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)

    dispatch({
      t: 'agents:result',
      repo_path: '/p',
      agents: [{}, {}, {}],
      error: 'ng',
    })
    dispatch({ t: 'wire:result', payload: { total: 7 } })

    expect(calls).toEqual(['agents:/p:3:ng', 'wire:7'])
  })

  it('設定の確定値は 4 引数の順序どおりに届く（doc 59 P1）', async () => {
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)

    dispatch({
      t: 'settings:result',
      developer_mode: true,
      developer_mode_locked: false,
      default_repo_root: '/Users/x/repos',
      resolved_repo_root: '/Users/x/repos',
    })

    expect(calls).toEqual(['settings:true:false:/Users/x/repos:/Users/x/repos'])
  })

  it('⚠️ 未設定の path は空文字に潰れる（「未設定」と「空」を受け手で分岐させない）', async () => {
    // schema 上 optional な `default_repo_root` / `resolved_repo_root` が省略されたとき、
    // 受け手に undefined を渡すと入力欄が uncontrolled に化ける。空文字で正規化する。
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)

    dispatch({ t: 'settings:result', developer_mode: false, developer_mode_locked: true })

    expect(calls).toEqual(['settings:false:true::'])
  })

  it('install 前に届いた分は二重に流れない', async () => {
    const mod = await import('./dispatch')
    mod.openSidebarDispatch()
    dispatch({ t: 'sidebar:error', message: 'once' })

    const { calls, handlers } = recordingHandlers()
    mod.installSidebarDispatch(handlers)
    expect(calls).toEqual(['error:once'])

    // 2 度目の install でも保留は空なので、過去分が再生されない。
    mod.installSidebarDispatch(handlers)
    expect(calls).toEqual(['error:once'])
  })
})
