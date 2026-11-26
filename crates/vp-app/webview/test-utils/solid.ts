import type { Plugin } from 'esbuild'
import type { Window } from 'happy-dom'
import { solidPlugin } from '../solid-plugin.mjs'

/** 本番 plugin でコンパイルし、その bundle の Solid scheduler を公開する。 */
export function solidTestPlugin(): Plugin {
  return {
    name: 'solid-dom-test',
    setup(build) {
      const stdin = build.initialOptions.stdin
      if (!stdin) throw new Error('Solid DOM test requires an inline entry')
      stdin.contents += `\nimport { flush as flushSolidTest } from 'solid-js';
        (window.__vpTestFlushers ??= []).push(flushSolidTest); flushSolidTest();`
      solidPlugin().setup(build)
    },
  }
}

/** ブラウザがイベント間で処理する microtask を、同期 happy-dom 呼出しでも反映する。 */
export function flushDom(window: object | null | undefined): void {
  for (const flush of (window as { __vpTestFlushers?: (() => void)[] } | null | undefined)?.__vpTestFlushers ?? []) flush()
}

export function evalDom(window: Window, source: string): unknown {
  flushDom(window)
  const value = window.eval(source)
  flushDom(window)
  return value
}

export function clickDom(element: { click(): void; ownerDocument?: { defaultView: object | null } }): void {
  element.click()
  flushDom(element.ownerDocument?.defaultView)
}

export function dispatchDom<E>(target: { dispatchEvent(event: E): boolean }, event: E): boolean {
  const result = target.dispatchEvent(event)
  const node = target as { ownerDocument?: { defaultView: object | null }; defaultView?: object | null; document?: unknown }
  const window = node.ownerDocument?.defaultView ?? node.defaultView ?? (node.document ? node as unknown as Window : undefined)
  if (window) flushDom(window)
  return result
}

/** native callback を1タスクとして呼び、bundle の保留中更新を確定する。 */
export function callDom<T>(window: object, action: () => T): T {
  const value = action()
  flushDom(window)
  return value
}
