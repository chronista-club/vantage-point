/**
 * Editor Link の接続設定（doc 86）— Rust（`app/boot.rs` の `link_init_script`）が webview の
 * 読み込み前に `window.__VP_LINK__` へ置いた値を、editor-host の `LinkOptions` に落とす。
 *
 * creo-ui の既定 URL は 32000 固定だが、vp-app は自分が使う daemon の port（brew 32000 /
 * dev 32100）を Rust から受け取るので VP_PROFILE の分離（#643）がそのまま効く。面の id は
 * `vp-app#<instance>`（window ごとに固定。tab ごとに UUID を振る site と違い、再起動しても同じ名）。
 *
 * 値が無い / 形が違う（preview / test / 旧 Rust）ときは undefined = Link を張らない。
 */
import type { LinkOptions } from '@chronista-club/creo-ui-editor-host'

export interface VpLinkGlobal {
  url?: unknown
  surface?: unknown
  app?: unknown
}

/** 純 calculation。`window.__VP_LINK__` の中身から LinkOptions を作る */
export function linkOptionsFrom(global: unknown): LinkOptions | undefined {
  if (!global || typeof global !== 'object') return undefined
  const g = global as VpLinkGlobal
  if (typeof g.url !== 'string' || !/^wss?:\/\//.test(g.url)) return undefined
  const out: LinkOptions = { url: g.url }
  if (typeof g.surface === 'string' && g.surface) out.surface = g.surface
  if (typeof g.app === 'string' && g.app) out.app = g.app
  return out
}

/** `window.__VP_LINK__` を読む（window が無ければ undefined） */
export function linkOptionsFromWindow(): LinkOptions | undefined {
  if (typeof window === 'undefined') return undefined
  return linkOptionsFrom((window as unknown as { __VP_LINK__?: unknown }).__VP_LINK__)
}
