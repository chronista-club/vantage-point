/**
 * History Strip — PP body の下行に直近 N=10 件の thumbnail を新→古 順で表示する component。
 *
 * doc 19 PP Canvas Stack Model (2026-05-27) Phase 2。 canvas-handler.ts の `CanvasState`
 * を購読し、 items × cursor を 1 row の thumbnail strip として render。
 *
 * 動作:
 *  - thumbnail click → `setCursor(id)` で main pane の表示を切替 (= items 順は不変)
 *  - thumbnail ✕ click → `deleteItem(id)` で items から削除 (= cursor 中なら fallback)
 *  - cursor が指す cell は brand color frame で強調
 *  - title は 8 chars truncate + ellipsis、 hover で HTML `title` attribute の OS tooltip で full
 *
 * mount target: main_area.rs HTML 側で `<div id="pp-history-strip">` を保証。
 * 関連 doc: docs/design/19-canvas-stack-model.md
 */

import { For, createSignal, onCleanup, onMount } from 'solid-js'
import { render } from 'solid-js/web'
import { CreoIcon } from 'creoui-icons-web'
import type { IconName } from 'creoui-icons-web'
import {
  deleteItem,
  getCanvasState,
  setCursor,
  subscribeCanvasState,
  type CanvasItem,
} from './canvas-handler'

/** content_type → Phosphor icon mapping (= doc 19 §5.1)。 */
const CONTENT_TYPE_ICON: Record<string, IconName> = {
  markdown: 'ph:file-text',
  html: 'ph:browser',
  text: 'ph:list', // log / plain text
}

/** title default 表示の最大文字数 (= doc 19 §5.1)。 codepoint count で算出。 */
const TITLE_MAX_CHARS = 8

/** title 未指定時に content 先頭を fallback として返す。 */
function aliasOf(item: CanvasItem): string {
  if (item.title && item.title.trim().length > 0) {
    return item.title.trim()
  }
  const first = item.content.trim().split('\n')[0] ?? ''
  return first.length > 0 ? first : '(empty)'
}

/** 8 chars 以内に truncate、 超えたら "…" を付ける。 codepoint 単位で count。 */
function truncate(s: string, n: number): string {
  const codepoints = [...s]
  if (codepoints.length <= n) return s
  return codepoints.slice(0, n).join('') + '…'
}

function HistoryStrip() {
  // canvas-handler の state を SolidJS signal に bridge。 listener で再 read して reactivity 化。
  const [state, setState] = createSignal(getCanvasState())

  onMount(() => {
    const unsub = subscribeCanvasState(() => setState(getCanvasState()))
    onCleanup(unsub)
  })

  return (
    <div class="pp-history-strip-inner">
      <For each={state().items}>
        {(item) => {
          const alias = aliasOf(item)
          const isActive = () => state().cursor === item.id
          return (
            <div
              class="pp-history-cell"
              classList={{ active: isActive() }}
              onClick={() => setCursor(item.id)}
              title={alias}
            >
              <CreoIcon name={CONTENT_TYPE_ICON[item.contentType] ?? 'ph:file'} size={12} />
              <span class="pp-history-title">{truncate(alias, TITLE_MAX_CHARS)}</span>
              <button
                class="pp-history-close"
                type="button"
                title="この item を削除"
                onClick={(e) => {
                  e.stopPropagation()
                  deleteItem(item.id)
                }}
              >
                <CreoIcon name="ph:x" size={10} />
              </button>
            </div>
          )
        }}
      </For>
    </div>
  )
}

/**
 * `<div id="pp-history-strip">` (main_area.rs HTML 側で保証) に component を mount する。
 * entry.tsx の boot 経路から呼ばれる。 target が見つからない場合は warn して skip (= 旧
 * HTML で boot した時の防御、 v1 fully roll-out 後は不要)。
 */
export function mountHistoryStrip(): void {
  const target = document.getElementById('pp-history-strip')
  if (!target) {
    console.warn('[history-strip] target #pp-history-strip not found, skip mount')
    return
  }
  render(() => <HistoryStrip />, target)
}

/**
 * History strip の CSS。 entry.tsx で `<style>` 注入する。
 * creoui token (= var(--color-*)) は既存 PP body と同じく inline 済前提。
 */
export const HISTORY_STRIP_CSS = `
/* doc 19 PP Canvas Stack Model: bottom history strip layout */
.pp-history-strip{flex:0 0 auto;border-top:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-subtle);padding:6px 8px;overflow:hidden;}
.pp-history-strip-inner{display:flex;gap:4px;overflow-x:auto;align-items:center;}
.pp-history-strip-inner::-webkit-scrollbar{height:4px;}
.pp-history-strip-inner::-webkit-scrollbar-thumb{background:var(--color-surface-border,#1f2233);
  border-radius:2px;}

/* 1 cell: icon + title + close */
.pp-history-cell{display:flex;align-items:center;gap:5px;
  padding:4px 6px;border:1px solid var(--color-surface-border,#1f2233);
  border-radius:4px;background:var(--color-surface-bg-raised);
  font-size:11px;cursor:pointer;flex:0 0 auto;
  max-width:140px;min-width:90px;height:24px;
  color:var(--color-text-secondary);
  transition:border-color .12s ease,background .12s ease,color .12s ease;}
.pp-history-cell:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.pp-history-cell.active{border-color:var(--color-brand-primary);
  background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);}
.pp-history-cell.active:hover{background:var(--color-brand-primary-subtle);}

.pp-history-title{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;
  color:inherit;font-family:inherit;}

.pp-history-close{border:none;background:transparent;padding:1px;
  display:inline-flex;align-items:center;cursor:pointer;
  color:var(--color-text-tertiary);opacity:.55;border-radius:2px;
  transition:opacity .1s ease,background .1s ease;}
.pp-history-close:hover{opacity:1;background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.pp-history-cell.active .pp-history-close{color:var(--color-brand-primary);}

/* pane-body を flex column 化して main + strip を縦並びにする。
   pp-content は flex:1 で残り全部 (= main pane の高さ確保)、
   pp-history-strip は flex:0 0 auto で content 高さ分だけ。 */
#pane-paisley-park .pane-body{display:flex;flex-direction:column;height:100%;}
#pane-paisley-park .pane-body .pp-content{flex:1;overflow-y:auto;}
#pane-paisley-park .pane-body .pp-history-strip{flex:0 0 auto;}
`
