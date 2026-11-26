// engine usage meter（spec mem_1ChMLKCMUvC73JHTLjG3xg）— MachineStrip と sub lane 作成の engine 選択が
// sidebar store の `engine_usage` を実際に描くこと（純関数の test は src/sidebar/usage.test.ts）。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLSelectElement } from 'happy-dom'

/** 2026-11-24 の実測に近い形（Fable 枠だけ 100%、grok は未取得で理由あり）。 */
const SNAPSHOT = {
  engines: [
    { engine: 'claude', plan: 'Ultra', tier: 'default_claude_max_20x', fetched_at_ms: 1, source: 'cache', error: null,
      windows: [
        { id: 'weekly', label: 'weekly', used_pct: 88, resets_at_ms: null, severity: 'warning', scope: null },
        { id: 'model:fable', label: 'weekly · Fable', used_pct: 100, resets_at_ms: null, severity: 'critical', scope: 'Fable' },
      ] },
    { engine: 'codex', plan: 'pro', tier: null, fetched_at_ms: 1, source: 'poll', error: null,
      windows: [{ id: 'weekly', label: 'weekly', used_pct: 95, resets_at_ms: null, severity: 'critical', scope: null }] },
    { engine: 'agy', plan: null, tier: null, fetched_at_ms: 1, source: 'poll', error: null,
      windows: [{ id: 'gemini-5h', label: 'Gemini · 5h', used_pct: 17.3, resets_at_ms: null, severity: 'normal', scope: null }] },
    { engine: 'grok', plan: null, tier: null, fetched_at_ms: null, source: 'none', error: 'grok が見つかりません（未インストール）', windows: [] },
  ],
  updated_at_ms: 1,
}

async function mount(component: string, props: string) {
  const result = await build({ stdin: { contents: `
    import { render } from '@solidjs/web'
    import { ${component} } from './src/sidebar/${component === 'MachineStrip' ? 'DaemonWidget' : component}'
    import { applySidebarState, emptyState } from './src/sidebar/store'
    import { reportAgentsResult } from './src/sidebar/feedback'
    window.sent = []; window.ipc = { postMessage: (m) => window.sent.push(JSON.parse(m)) }
    window.setUsage = (u) => applySidebarState({ ...emptyState(), engine_usage: u ?? undefined })
    window.reportAgents = reportAgentsResult
    render(() => ${component}(${props}), document.body)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife',
    conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const win = new Window()
  evalDom(win, result.outputFiles[0]!.text)
  return win as unknown as Window & {
    sent: any[]; setUsage: (u: unknown) => void; reportAgents: (p: unknown) => void
  }
}

it('MachineStrip: 畳んだ行に最も逼迫しているエンジン、展開部に 4 エンジンと「更新」', async () => {
  const win = await mount('MachineStrip', '')
  try {
    const doc = win.document
    expect(doc.querySelector('.vp-usage-chip'), '未着の間はチップを出さない').toBeNull()
    expect(doc.querySelector('.vp-usage-empty')?.textContent).toContain('未着')

    callDom(win, () => win.setUsage(SNAPSHOT))
    const chip = doc.querySelector('.vp-usage-chip')!
    expect(chip.textContent).toBe('Cdx 95%')
    expect(chip.className).toContain('critical')

    const rows = [...doc.querySelectorAll('.vp-usage-row')].map((r) => r.textContent)
    expect(rows).toEqual(['CC88%', 'Cdx95%', 'Gem17%', 'Grk—'])
    // Fable 100% は部分枠として 1 段静かに出す（Claude 全体の % にはしない）
    const fable = [...doc.querySelectorAll('.vp-usage-window.scoped')].map((w) => w.textContent)
    expect(fable).toEqual(['weekly · Fable100%'])
    expect(doc.querySelector('.vp-usage-error')?.textContent).toBe('grok が見つかりません（未インストール）')

    ;clickDom((doc.querySelector('.vp-usage-refresh') as unknown as { click(): void }))
    expect(win.sent).toEqual([{ t: 'usage:refresh' }])
  } finally {
    await win.happyDOM.close()
  }
})

it('AddSub: engine の選択肢に使用率を添え、未取得の engine は名前だけ', async () => {
  const win = await mount('AddSub', `{ repoPath: '/r', onClose: () => {}, onPendingChange: () => {} }`)
  try {
    const agents = [
      { name: 'claude', description: 'Claude Code' },
      { name: 'agy', description: 'Antigravity' },
      { name: 'grok', description: 'Grok' },
      { name: 'shell', description: 'login shell' },
    ]
    callDom(win, () => win.reportAgents({ repo_path: '/r', agents }))
    const labels = () =>
      [...win.document.querySelector<HTMLSelectElement>('.vp-add-sub-agent')!.options].map((o) => o.textContent)
    expect(labels(), 'usage 未着は従来どおり').toEqual(['claude', 'agy', 'grok', 'shell'])

    callDom(win, () => win.setUsage(SNAPSHOT))
    expect(labels()).toEqual(['claude (88%)', 'agy (17%)', 'grok', 'shell'])
  } finally {
    await win.happyDOM.close()
  }
})
