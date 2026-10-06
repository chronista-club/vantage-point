export const EDITOR_CSS = `
/* Keep contrast-dark's established VP palette. Other presets inherit the selected Creo theme. */
:root[data-vp-editor-theme]:not([data-vp-editor-theme="contrast-dark"]) {
  --lg-void:var(--color-surface-bg-base); --lg-void-2:var(--color-surface-bg-subtle);
  --lg-panel:var(--color-surface-surface); --lg-hot:var(--color-text-primary);
  --lg-mute:var(--color-text-secondary); --lg-mute-2:var(--color-text-tertiary);
  --lg-hairline:var(--color-surface-border); --lg-grid:var(--color-surface-border);
  --lg-cyan-dim:var(--color-brand-primary); --sb-conn-auto:var(--color-brand-primary);
  --sb-selection-bg:var(--color-surface-bg-emphasis); --sb-selection-border:var(--color-text-tertiary);
}
:root[data-vp-editor-theme$="-light"] {
  --sb-activity-thinking:#73529d; --sb-activity-working:#35638d; --sb-activity-waiting:#b63845;
  --sb-activity-completed:#355e9d; --sb-activity-idle:#596777; --sb-activity-error:#b63845;
}
.vp-editor-overlay { position:fixed; inset:0; pointer-events:none; z-index:2147483000; }
.vp-editor { --ed-bg:#111820; --ed-panel:#19232d; --ed-line:#30404f; --ed-text:#e4edf3; --ed-muted:#9dabb9; --ed-accent:#9bd7c3;
  pointer-events:auto; position:absolute; top:24px; right:24px; width:380px; max-width:calc(100vw - 24px); max-height:calc(100dvh - 48px);
  display:flex; flex-direction:column; background:var(--ed-bg); color:var(--ed-text); border:1px solid var(--ed-line); border-radius:16px;
  box-shadow:0 16px 64px #0007; font:13px/1.55 system-ui,sans-serif; text-align:left; overflow:hidden; }
.vp-editor *, .vp-editor *::before, .vp-editor *::after { box-sizing:border-box; }
.vp-editor button, .vp-editor input, .vp-editor select, .vp-editor textarea { font:inherit; color:inherit; }
.vp-editor button { border:1px solid var(--ed-line); border-radius:7px; background:var(--ed-panel); padding:6px 10px; cursor:pointer; }
.vp-editor button:hover:not(:disabled) { border-color:var(--ed-accent); }
.vp-editor button:disabled { opacity:.4; cursor:default; }
.vp-editor :focus-visible { outline:2px solid var(--ed-accent); outline-offset:3px; }
.vp-editor input:not([type="range"]):not([type="checkbox"]), .vp-editor select, .vp-editor textarea { min-width:0; width:100%; border:1px solid var(--ed-line); border-radius:6px; background:var(--ed-panel); padding:6px 8px; }
.vp-editor input[aria-invalid="true"] { border-color:#ef777d; }
.vp-editor input[type="range"] { accent-color:var(--ed-accent); min-width:0; width:100%; }
.vp-editor-header { display:flex; align-items:center; justify-content:space-between; padding:16px 18px 12px; }
.vp-editor-header small { color:var(--ed-muted); letter-spacing:.15em; font-size:9px; }
.vp-editor-header h2 { margin:0; font-size:22px; font-weight:550; letter-spacing:-.03em; }
.vp-editor-header button { border:0; background:transparent; color:var(--ed-muted); font-size:24px; padding:0 6px; }
.vp-editor-tabs { display:flex; flex-direction:column; flex-shrink:0; padding:0 12px 10px; gap:2px; border-bottom:1px solid var(--ed-line); }
.vp-editor-tabs button { background:transparent; border:0; border-left:2px solid transparent; border-radius:0; padding:7px 12px; text-align:left; color:var(--ed-muted); font-size:11px; letter-spacing:.06em; }
.vp-editor-tabs button[aria-selected="true"] { color:var(--ed-accent); border-left-color:var(--ed-accent); background:var(--ed-panel); }
.vp-editor-content { overflow:auto; padding:0 18px 18px; min-height:0; overscroll-behavior:contain; }
.vp-editor-intro { margin:18px 0; }
.vp-editor h3 { font-size:14px; font-weight:550; margin:0 0 5px; }
.vp-editor p { color:var(--ed-muted); margin:6px 0 12px; font-size:12px; }
.vp-editor .vp-editor-primary { color:var(--ed-bg); background:var(--ed-accent); border-color:var(--ed-accent); width:100%; padding:9px; font-weight:600; }
.vp-editor .vp-editor-primary[aria-pressed="true"] { color:var(--ed-accent); background:var(--ed-panel); }
.vp-editor-empty { padding:20px 8px; text-align:center; }
.vp-editor-selection { padding:12px; background:var(--ed-panel); border-radius:8px; margin-bottom:12px; }
.vp-editor-selection strong { font-size:13px; }
.vp-editor-selection button { float:right; font-size:10px; padding:2px 6px; }
.vp-editor-selection p { margin:8px 0 0; clear:both; }
.vp-editor-group { border-top:1px solid var(--ed-line); margin-top:12px; }
.vp-editor summary { cursor:pointer; color:var(--ed-text); padding:12px 0; font-size:12px; }
.vp-editor-group summary span { float:right; color:var(--ed-muted); font-size:10px; }
.vp-editor-field { margin:0 0 13px; }
.vp-editor-field-head { display:flex; align-items:center; justify-content:space-between; margin-bottom:4px; }
.vp-editor-field-head label { font-size:12px; }
.vp-editor .vp-editor-reset { border:0; background:transparent; padding:0 4px; color:var(--ed-muted); }
.vp-editor-number { display:grid; grid-template-columns:1fr 65px 20px; align-items:center; gap:6px; }
.vp-editor-number span { font-size:10px; color:var(--ed-muted); }
.vp-editor-color { display:flex; gap:8px; }
.vp-editor .vp-editor-color input[type="color"] { padding:2px; width:34px; height:32px; flex:none; }
.vp-editor-color input[type="text"] { font-family:ui-monospace,monospace; font-size:11px; }
.vp-editor-label { display:block; font-size:12px; }
.vp-editor-label select { margin-top:5px; }
.vp-editor-save { margin-top:12px; padding:12px; background:var(--ed-panel); border-radius:8px; }
.vp-editor-save p { font-size:11px; }
.vp-editor-save form { display:flex; gap:6px; margin-bottom:8px; }
.vp-editor-save form button { white-space:nowrap; font-size:11px; }
.vp-editor-save > button { font-size:10px; margin:2px 5px 2px 0; padding:4px 7px; }
.vp-editor-discovery, .vp-editor-export { margin-top:16px; border-top:1px solid var(--ed-line); }
.vp-editor-tree { list-style:none; margin:0; padding-left:12px; }
.vp-editor-tree li > button { width:100%; border:0; background:transparent; text-align:left; font-size:11px; padding:4px; }
.vp-editor-tree button span { float:right; color:var(--ed-muted); }
.vp-editor-export textarea { height:140px; resize:vertical; font:10px/1.5 ui-monospace,monospace; }
.vp-editor footer { display:flex; justify-content:space-between; gap:12px; padding:9px 18px; border-top:1px solid var(--ed-line); color:var(--ed-muted); font-size:9px; }
.vp-editor .vp-editor-storage-error { padding:8px 18px; color:#ef777d; margin:0; }
.vp-editor-outline { position:fixed; outline:2px solid #71d9b4; outline-offset:2px; pointer-events:none; border-radius:3px; }
.vp-editor-outline > span { position:absolute; left:0; bottom:100%; margin-bottom:5px; padding:2px 6px; background:#143e32; color:#c7f3e2; border-radius:3px; white-space:nowrap; font:11px/1.5 system-ui,sans-serif; }
@media(max-width:600px) { .vp-editor { top:12px; right:12px; max-height:calc(100dvh - 24px); } }
`
