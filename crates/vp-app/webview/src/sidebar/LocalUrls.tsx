/** Lane ごとの登録 URL。native の保存結果だけを確定値として表示する。 */
import { For, Show, createEffect, createSignal, onCleanup } from 'solid-js'
import { requestLocalUrls, probeLabel, type LocalUrlEntry, type UrlAction, type UrlProbe, type UrlResult } from './local-urls'

export function LocalUrls(props: { repoPath: string; address: string }) {
  const [entries, setEntries] = createSignal<LocalUrlEntry[]>([])
  const [probes, setProbes] = createSignal<Record<string, UrlProbe>>({})
  const [busy, setBusy] = createSignal(false)
  const [loaded, setLoaded] = createSignal(false)
  const [error, setError] = createSignal('')
  const [editing, setEditing] = createSignal<string | null>(null)
  const [url, setUrl] = createSignal('')
  const [label, setLabel] = createSignal('')
  let generation = 0
  createEffect(() => {
    const path = props.repoPath, address = props.address
    const current = ++generation
    setEntries([]); setProbes({}); setEditing(null); setLoaded(false); setError(''); setBusy(true)
    requestLocalUrls(path, address, { action: 'load' }).then(result => {
      if (generation !== current) return
      setEntries(result.entries ?? []); setLoaded(true)
    }).catch(e => { if (generation === current) setError(String(e.message ?? e)) })
      .finally(() => { if (generation === current) setBusy(false) })
    onCleanup(() => { generation++ })
  })
  async function act(action: UrlAction): Promise<UrlResult | null> {
    if (busy()) return null
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
  function edit(entry?: LocalUrlEntry) {
    setEditing(entry?.id ?? ''); setUrl(entry?.url ?? ''); setLabel(entry?.label ?? ''); setError('')
  }
  return <details class="vp-local-urls" onClick={e => e.stopPropagation()} onContextMenu={e => e.stopPropagation()} onDragStart={e => e.stopPropagation()}>
    <summary>ローカル URL <span>{entries().length || '＋'}</span><Show when={error()}> · 確認が必要</Show></summary>
    <div class="vp-local-url-body">
      <style>{LOCAL_URLS_CSS}</style>
      <For each={entries()}>{entry => <div class="vp-local-url-entry">
        <button type="button" class="vp-local-url-open" title={`${entry.url} をブラウザで開く`} disabled={busy()} onClick={() => void act({ action: 'open', id: entry.id })}>{entry.label}</button>
        <span class="vp-local-url-address" title={entry.url}>{entry.url}</span>
        <span class="vp-local-url-state" title={probes()[entry.id]?.state === 'failed' ? (probes()[entry.id] as {message: string}).message : undefined}>{probeLabel(probes()[entry.id])}</span>
        <div class="vp-local-url-actions">
          <button type="button" disabled={busy()} onClick={async () => { const result = await act({ action: 'probe', id: entry.id }); if (result?.probe) setProbes(p => ({ ...p, [entry.id]: result.probe! })) }}>確認</button>
          <button type="button" disabled={busy()} onClick={() => edit(entry)}>編集</button>
          <button type="button" disabled={busy()} onClick={() => void save(entries().filter(e => e.id !== entry.id))}>削除</button>
        </div>
      </div>}</For>
      <Show when={error()}><p class="vp-local-url-error" role="alert">{error()}</p><button type="button" disabled={busy()} onClick={async () => { const result = await act({action:'load'}); if(result?.entries){setEntries(result.entries);setLoaded(true);setProbes({})} }}>再読み込み</button></Show>
      <Show when={editing() !== null} fallback={<button type="button" disabled={busy() || !loaded()} onClick={() => edit()}>URLを追加</button>}>
        <form class="vp-local-url-form" onSubmit={e => {
          e.preventDefault(); e.stopPropagation()
          const entry = { id: editing() || crypto.randomUUID(), url: url().trim(), label: label().trim() }
          void save(editing() ? entries().map(old => old.id === editing() ? entry : old) : [...entries(), entry])
        }}>
          <label>URL<input aria-label="URL" required value={url()} placeholder="http://localhost:12889" onInput={e => setUrl(e.currentTarget.value)} disabled={busy()} /></label>
          <label>用途<input aria-label="用途" required maxlength={120} value={label()} placeholder="Editor preview" onInput={e => setLabel(e.currentTarget.value)} disabled={busy()} /></label>
          <div class="vp-local-url-actions"><button type="submit" disabled={busy()}>保存</button><button type="button" disabled={busy()} onClick={() => setEditing(null)}>キャンセル</button></div>
        </form>
      </Show>
      <Show when={busy()}><span role="status">処理中…</span></Show>
    </div>
  </details>
}

export const LOCAL_URLS_CSS = `
.vp-local-urls{flex:0 0 100%;min-width:0;font-size:var(--sb-text-meta,11px);cursor:default;color:var(--color-text-secondary,#a8b1bb)}
.vp-local-urls summary{cursor:pointer;padding:3px 0;user-select:none;font-size:var(--sb-text-micro,10px)}
.vp-local-urls summary span{font-variant-numeric:tabular-nums;opacity:.7}
.vp-local-url-body{display:flex;flex-direction:column;gap:8px;padding:6px 0}
.vp-local-url-entry{display:flex;flex-direction:column;gap:3px;border-bottom:1px solid var(--color-surface-border,#ffffff16);padding-bottom:8px;min-width:0}
.vp-local-urls button{font:inherit;color:inherit;border:1px solid var(--color-surface-border,#ffffff20);border-radius:4px;background:transparent;padding:3px 7px;cursor:pointer;text-align:left}
.vp-local-urls button:hover{background:#ffffff0c}.vp-local-urls button:disabled{opacity:.45;cursor:default}
.vp-local-urls .vp-local-url-open{border:0;padding:0;color:var(--color-text-primary,#e7edf0);text-decoration:underline;text-underline-offset:3px}
.vp-local-url-address{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--typography-family-mono,monospace);font-size:10px}
.vp-local-url-state{font-size:10px;opacity:.8}.vp-local-url-actions{display:flex;gap:5px;flex-wrap:wrap}
.vp-local-url-form{display:flex;flex-direction:column;gap:7px}.vp-local-url-form label{display:flex;flex-direction:column;gap:3px}
.vp-local-url-form input{box-sizing:border-box;width:100%;min-width:0;font:inherit;color:var(--color-text-primary,#e7edf0);background:var(--color-surface-bg-base,#15191d);border:1px solid var(--color-surface-border,#ffffff30);border-radius:4px;padding:6px}
.vp-local-url-error{color:var(--color-status-error,#ef8b84);margin:0;overflow-wrap:anywhere}
.vp-session-close{font:inherit;color:inherit;background:transparent;border:1px solid var(--color-surface-border,#ffffff20);border-radius:4px;cursor:pointer;padding:1px 5px}
`
