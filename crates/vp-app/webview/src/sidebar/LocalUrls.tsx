/** R sidebar の Lane リンク。検出 URL と PR は一時表示、登録は native の保存結果だけを確定値にする。 */
import { For, Show, createEffect, createSignal, on, onCleanup } from 'solid-js'
import { requestLocalUrls, probeLabel, type LocalUrlEntry, type UrlAction, type UrlProbe, type UrlResult } from './local-urls'
import { extractLoopbackUrls } from '../../lane-link-discovery'
import type { Pr } from '../../pr-list'

export function LocalUrls(props: { repoPath: string; address: string; active?: boolean; discovered?: string[]; pr?: Pr | null; addRequest?: number }) {
  const [entries, setEntries] = createSignal<LocalUrlEntry[]>([])
  const [probes, setProbes] = createSignal<Record<string, UrlProbe>>({})
  const [busy, setBusy] = createSignal(false)
  const [loaded, setLoaded] = createSignal(false)
  const [error, setError] = createSignal('')
  const [editing, setEditing] = createSignal<string | null>(null)
  const [url, setUrl] = createSignal('')
  const [name, setName] = createSignal('')
  const [label, setLabel] = createSignal('')
  const [discoveredProbes, setDiscoveredProbes] = createSignal<Record<string, UrlProbe>>({})
  const [expanded, setExpanded] = createSignal(false)
  let form: HTMLFormElement | undefined
  let nameInput: HTMLInputElement | undefined
  let generation = 0
  let probingGeneration: number | null = null
  const visible = () => props.active !== false && !!props.repoPath && !!props.address
  const openLink = (url: string) => window.ipc?.postMessage(JSON.stringify({ t: 'open-url', url }))
  const candidates = () => (props.discovered ?? []).filter(url => !entries().some(entry => extractLoopbackUrls(entry.url)[0] === url))
  createEffect(on([() => props.repoPath, () => props.address, () => props.active], () => {
    const path = props.repoPath, address = props.address
    const current = ++generation
    setEntries([]); setProbes({}); setDiscoveredProbes({}); setExpanded(false); setEditing(null); setLoaded(false); setError(''); setBusy(false)
    if (!visible()) return
    setBusy(true)
    requestLocalUrls(path, address, { action: 'load' }).then(result => {
      if (generation !== current) return
      setEntries(result.entries ?? []); setLoaded(true)
    }).catch(e => { if (generation === current) setError(String(e.message ?? e)) })
      .finally(() => { if (generation === current) setBusy(false) })
    const refreshTimer = window.setInterval(() => void refresh(), 3000)
    const refreshOnFocus = () => void refresh()
    window.addEventListener('focus', refreshOnFocus)
    onCleanup(() => {
      generation++
      window.clearInterval(refreshTimer)
      window.removeEventListener('focus', refreshOnFocus)
    })
  }))
  async function act(action: UrlAction): Promise<UrlResult | null> {
    if (!visible() || busy()) return null
    const current = generation
    setBusy(true); setError('')
    try {
      const result = await requestLocalUrls(props.repoPath, props.address, action)
      return generation === current ? result : null
    } catch (e) { if (generation === current) setError(e instanceof Error ? e.message : String(e)); return null }
    finally { if (generation === current) setBusy(false) }
  }
  async function save(next: LocalUrlEntry[]) {
    const result = await act({ action: 'save', expected: entries(), entries: next })
    if (!result?.entries) return
    setEntries(result.entries); setProbes({}); setEditing(null)
  }
  function edit(entry?: LocalUrlEntry, discovered = '') {
    setEditing(entry?.id ?? ''); setName(entry?.id ?? ''); setUrl(entry?.url ?? discovered); setLabel(entry?.label ?? ''); if (loaded()) setError('')
    const current = generation
    queueMicrotask(() => {
      if (current !== generation || !visible() || editing() === null || !form?.isConnected) return
      nameInput?.focus({ preventScroll: true })
      form.scrollIntoView({ block: 'nearest' })
    })
  }
  // 登録の更新は編集中の draft を置き換えない。probe は検出 URL の独立した列で行う。
  async function refresh() {
    if (!visible() || !loaded() || busy() || editing() !== null || error()) return
    const result = await act({ action: 'load' })
    if (result?.entries && editing() === null && JSON.stringify(result.entries) !== JSON.stringify(entries())) {
      setEntries(result.entries); setProbes({})
    }
  }
  async function probeDiscovered() {
    if (!visible() || !loaded() || probingGeneration === generation) return
    const current = generation
    probingGeneration = current
    try {
      // 同じ lane 内でも順番に確認。非表示 / 切替後には次の URL を要求しない。
      for (;;) {
        if (current !== generation || !visible()) return
        const url = candidates().find(value => !discoveredProbes()[value])
        if (!url) return
        let probe: UrlProbe
        try {
          const result = await requestLocalUrls(props.repoPath, props.address, { action: 'probe_url', url })
          probe = result.probe ?? { state: 'failed', message: '応答を確認できませんでした' }
        } catch (e) { probe = { state: 'failed', message: e instanceof Error ? e.message : String(e) } }
        if (current !== generation) return
        setDiscoveredProbes(previous => ({ ...previous, [url]: probe }))
      }
    } finally { if (probingGeneration === current) probingGeneration = null }
  }
  createEffect(on([() => props.discovered, () => props.address, () => props.repoPath, () => props.active, loaded, entries], () => { void probeDiscovered() }))
  createEffect(on(() => props.addRequest, request => { if (request) edit() }))
  return <><style>{LOCAL_URLS_CSS}</style><div class="vp-local-urls" onClick={e => e.stopPropagation()} onContextMenu={e => e.stopPropagation()} onDragStart={e => e.stopPropagation()}>
    <h3>リンク</h3>
    <div class="vp-local-url-body">
      <Show when={editing() !== null}>
        <form ref={form} class="vp-local-url-form" onSubmit={e => {
          e.preventDefault(); e.stopPropagation()
          const entry = { id: editing() || name().trim(), url: url().trim(), label: label().trim() }
          void save(editing() ? entries().map(old => old.id === editing() ? entry : old) : [...entries(), entry])
        }}>
          <label>名前<input ref={nameInput} aria-label="名前" required maxlength={64} pattern="[a-z0-9][a-z0-9_-]{0,63}" value={name()} placeholder="preview" onInput={e => setName(e.currentTarget.value)} disabled={busy() || !!editing()} /></label>
          <label>URL<input aria-label="URL" required value={url()} placeholder="http://localhost:12889" onInput={e => setUrl(e.currentTarget.value)} disabled={busy()} /></label>
          <label>用途<input aria-label="用途" required maxlength={120} value={label()} placeholder="Editor preview" onInput={e => setLabel(e.currentTarget.value)} disabled={busy()} /></label>
          <div class="vp-local-url-actions"><button type="submit" disabled={busy() || !loaded()}>保存</button><button type="button" disabled={busy()} onClick={() => setEditing(null)}>キャンセル</button></div>
        </form>
      </Show>
      <Show when={error()}><p class="vp-local-url-error" role="alert" title={error()}>{error()}</p><button type="button" disabled={busy()} onClick={async () => { const result = await act({action:'load'}); if(result?.entries){setEntries(result.entries);setLoaded(true);setProbes({})} }}>再読み込み</button></Show>
      <div class="vp-link-heading">登録済み</div>
      <For each={entries()}>{entry => <div class="vp-local-url-entry">
        <button type="button" class="vp-local-url-open" title={`${entry.url} をブラウザで開く`} disabled={busy()} onClick={() => void act({ action: 'open', id: entry.id })}>{entry.label}</button>
        <span class="vp-local-url-name" title={`登録名: ${entry.id}`}>{entry.id}</span>
        <span class="vp-local-url-address" title={entry.url}>{entry.url}</span>
        <span class="vp-local-url-state" title={probes()[entry.id]?.state === 'failed' ? (probes()[entry.id] as {message: string}).message : undefined}>{probeLabel(probes()[entry.id])}</span>
        <div class="vp-local-url-actions">
          <button type="button" disabled={busy()} onClick={async () => { const result = await act({ action: 'probe', id: entry.id }); if (result?.probe) setProbes(p => ({ ...p, [entry.id]: result.probe! })) }}>確認</button>
          <button type="button" disabled={busy()} onClick={() => edit(entry)}>編集</button>
          <button type="button" disabled={busy()} onClick={() => void save(entries().filter(e => e.id !== entry.id))}>削除</button>
        </div>
      </div>}</For>
      <Show when={editing() === null}><button type="button" disabled={busy() || !loaded()} onClick={() => edit()}>URLを追加</button></Show>
      <Show when={props.pr}>{pr => <>
        <div class="vp-link-heading">この Lane の PR</div>
        <button type="button" class="vp-local-url-open" title={pr().url} onClick={() => openLink(pr().url)}>#{pr().number} {pr().title}</button>
      </>}</Show>
      <div class="vp-link-heading">見つかった URL <Show when={candidates().length}><button type="button" onClick={() => { setDiscoveredProbes({}); void probeDiscovered() }}>再確認</button></Show></div>
      <For each={expanded() ? candidates() : candidates().slice(0, 5)} fallback={<span class="vp-link-empty">まだ見つかっていません</span>}>{url => <div class="vp-discovered-url">
        <span class="vp-link-dot" data-state={discoveredProbes()[url]?.state ?? 'unknown'} role="img" aria-label={probeLabel(discoveredProbes()[url])} title={probeLabel(discoveredProbes()[url])}>●</span>
        <button type="button" class="vp-local-url-open" title={url} onClick={() => openLink(url)}>{url}</button>
        <button type="button" disabled={busy() || !loaded()} onClick={() => edit(undefined, url)}>登録</button>
      </div>}</For>
      <Show when={candidates().length > 5}>
        <button type="button" aria-expanded={expanded()} onClick={() => setExpanded(value => !value)}>{expanded() ? '折りたたむ' : `さらに ${candidates().length - 5} 件`}</button>
      </Show>
      <Show when={busy()}><span role="status">処理中…</span></Show>
    </div>
  </div></>
}

export const LOCAL_URLS_CSS = `
.vp-local-urls{flex:0 0 100%;min-width:0;padding:6px 10px;font-size:var(--sb-text-meta,11px);cursor:default;color:var(--color-text-secondary,#a8b1bb)}
.vp-local-urls h3{font-size:12px;font-weight:600;margin:4px 0}
.vp-link-heading{display:flex;align-items:center;justify-content:space-between;font-size:11px;margin-top:4px;color:var(--color-text-tertiary,#88929d)}
.vp-link-empty{font-size:11px;color:var(--color-text-tertiary,#88929d)}
.vp-discovered-url{display:flex;gap:6px;align-items:center;min-width:0}.vp-discovered-url .vp-local-url-open{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.vp-link-dot{font-size:9px;color:var(--color-text-tertiary,#88929d)}.vp-link-dot[data-state=responding]{color:#4ade80}.vp-link-dot[data-state=refused]{color:#fbbf24}
.vp-local-urls summary{cursor:pointer;padding:3px 0;user-select:none;font-size:var(--sb-text-micro,10px)}
.vp-local-urls summary span{font-variant-numeric:tabular-nums;opacity:.7}
.vp-local-url-body{display:flex;flex-direction:column;gap:8px;padding:6px 0}
.vp-local-url-entry{display:flex;flex-direction:column;gap:3px;border-bottom:1px solid var(--color-surface-border,#ffffff16);padding-bottom:8px;min-width:0}
.vp-local-urls button{font:inherit;color:inherit;border:1px solid var(--color-surface-border,#ffffff20);border-radius:4px;background:transparent;padding:3px 7px;cursor:pointer;text-align:left}
.vp-local-urls button:hover{background:#ffffff0c}.vp-local-urls button:disabled{opacity:.45;cursor:default}
.vp-local-urls .vp-local-url-open{border:0;padding:0;color:var(--color-text-primary,#e7edf0);text-decoration:underline;text-underline-offset:3px}
.vp-local-url-name{font-size:10px;opacity:.7;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.vp-local-url-address{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--typography-family-mono,monospace);font-size:10px}
.vp-local-url-state{font-size:10px;opacity:.8}.vp-local-url-actions{display:flex;gap:5px;flex-wrap:wrap}
.vp-local-url-form{display:flex;flex-direction:column;gap:7px}.vp-local-url-form label{display:flex;flex-direction:column;gap:3px}
.vp-local-url-form input{box-sizing:border-box;width:100%;min-width:0;font:inherit;color:var(--color-text-primary,#e7edf0);background:var(--color-surface-bg-base,#15191d);border:1px solid var(--color-surface-border,#ffffff30);border-radius:4px;padding:6px}
.vp-local-url-error{color:var(--color-text-secondary,#a8b1bb);margin:0;font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
`
