// Local, daemon-free preview. Run: bun editor/preview.mjs [port]
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { fileURLToPath } from 'node:url'

const result = await build({
  stdin: { contents: `
    import { createComponent } from 'solid-js';
    import { render } from 'solid-js/web';
    import { VpEditor } from './editor/VpEditor';
    import { Shell, SHELL_CSS } from './src/sidebar/Shell';
    import { applySidebarState, emptyState } from './src/sidebar/store';
    import tokens from '@chronista-club/creo-ui/tokens.css';
    import components from '@chronista-club/creo-ui/components.css';
    import shim from '@chronista-club/creo-ui/token-shim.css';
    const style = document.createElement('style'); style.textContent = tokens + components + shim + SHELL_CSS; document.head.append(style);
    window.ipc = { postMessage(message) { document.getElementById('preview-status').textContent = 'Preview · ' + JSON.parse(message).t } };
    const lane = (name, phase) => ({ address: { repo: 'vantage-point', name, key: 'vantage-point/' + name }, state: 'running', agent: 'codex', pid: 42, cwd: '/preview', branch: 'wip/' + name, flow_state: phase, sub_status: null, sessions: null });
    applySidebarState({ ...emptyState(), processes: [{ path: '/preview', name: 'Vantage Point', state: 'running', expanded: true, port: 123 }], lanes_by_repo: { '/preview': [lane('main', 'working'), lane('editor-theme', 'working'), lane('review', 'awaiting_user')] }, active_lane_address: 'vantage-point/editor-theme' });
    render(() => createComponent(Shell, {}), document.getElementById('sidebar-root'));
    render(() => createComponent(VpEditor, {}), document.getElementById('editor-root'));
    window.vpEditorHost.enable();
  `, loader: 'ts', resolveDir: fileURLToPath(new URL('..', import.meta.url)) },
  bundle: true, write: false, format: 'iife', platform: 'browser', plugins: [solidPlugin()], loader: { '.css': 'text' }, define: { 'process.env.NODE_ENV': '"production"' },
})
const html = `<!doctype html><html lang="ja" data-theme="contrast-dark"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>VP Editor — Preview</title>
<style>
html,body{margin:0;height:100%;background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:system-ui,sans-serif}
#app-shell{display:flex;height:100vh}#sidebar-root{width:270px;flex:none}#host{flex:1;padding:60px 440px 40px 48px;overflow:auto;min-width:0}
h1{font-size:30px;font-weight:500;margin:0 0 12px}p{line-height:1.8;color:var(--color-text-secondary)}small{font-size:11px;color:var(--color-text-tertiary)}
.sample-card{padding:24px;border:1px solid var(--color-surface-border);border-radius:16px;background:var(--color-surface-surface);margin:30px 0}
.creo-btn{padding:var(--_btn__pad-y,8px) var(--_btn__pad-x,16px);border-radius:var(--_btn__radius,8px)}
.sample-actions{display:flex;gap:12px;flex-wrap:wrap}#preview-status{position:fixed;left:285px;bottom:12px;font-size:10px;color:var(--color-text-tertiary)}
@media(max-width:1100px){#host{padding:40px 32px}}
</style>
<body><div id="app-shell"><div id="sidebar-root"></div><main id="host">
<small>EDITOR THEME / LOCAL PREVIEW</small><h1>自分の作業台を整える</h1><p>右の Editor から、サイドバーや画面全体を調整できます。<br>「画面から選ぶ」で、下のボタンを選んでみてください。</p>
<div class="sample-card creo-card"><h2>ひとつ選ぶと、同じ種類へ。</h2><p>同じボタンの余白や角丸が、一緒に変わります。<br>このプレビューの操作は、実際のセッションには送られません。</p><div class="sample-actions"><button class="creo-btn">新しい作業</button><button class="creo-btn">履歴を見る</button></div></div>
<p>テーマはプリセットから選ぶほか、調整した状態に名前を付けて保存できます。</p></main></div><div id="editor-root"></div><span id="preview-status">Preview · isolated</span><script src="/preview.js"></script></body></html>`
const port = Number(process.argv[2] ?? 12889)
const server = Bun.serve({ hostname: '127.0.0.1', port, fetch(request) { const path = new URL(request.url).pathname; return path === '/preview.js' ? new Response(result.outputFiles[0].text, { headers: { 'Content-Type': 'text/javascript; charset=utf-8' } }) : path === '/' ? new Response(html, { headers: { 'Content-Type': 'text/html; charset=utf-8' } }) : new Response('Not found', { status: 404 }) } })
console.log(`Editor preview: ${server.url}`)
