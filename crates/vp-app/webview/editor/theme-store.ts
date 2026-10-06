import { THEME_IDS, type ThemeId } from '@chronista-club/creo-ui-editor-host'

export const THEME_STORAGE_KEY = 'vp:editor:themes:v1'
export const RUNTIME_NAMESPACE = 'vp:editor:runtime'
const LEGACY_NAMESPACE = '@chronista-club/creo-ui-editor-host'
export type Values = Record<string, string | number | boolean>
export type CustomTheme = { id: string; name: string; base: ThemeId; values: Values }
export type ThemeLibrary = { version: 1; selected: string; drafts: Record<string, Values>; custom: CustomTheme[] }
export type StoragePort = Pick<Storage, 'getItem' | 'setItem' | 'removeItem' | 'key' | 'length'>
export const isThemeId = (value: unknown): value is ThemeId => THEME_IDS.includes(value as ThemeId)
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value)

function valuesOf(value: unknown): Values {
  if (!record(value)) return {}
  return Object.fromEntries(Object.entries(value).filter((entry): entry is [string, Values[string]] => {
    const [id, v] = entry
    return id !== '__proto__' && id !== 'constructor' && id !== 'prototype' &&
    (typeof v === 'string' || typeof v === 'boolean' || (typeof v === 'number' && Number.isFinite(v)))
  }))
}

/** Keep old field storage intact; import it only when the VP library does not exist. */
export function readThemeLibrary(storage: StoragePort | undefined, fallback: ThemeId): ThemeLibrary {
  const empty: ThemeLibrary = { version: 1, selected: fallback, drafts: {}, custom: [] }
  if (!storage) return empty
  try {
    const raw = storage.getItem(THEME_STORAGE_KEY)
    if (raw === null) {
      const values: Values = {}
      const prefix = `${LEGACY_NAMESPACE}:field:`
      for (let i = 0; i < storage.length; i++) {
        const key = storage.key(i)
        if (!key?.startsWith(prefix)) continue
        try { Object.assign(values, valuesOf({ [key.slice(prefix.length)]: JSON.parse(storage.getItem(key)!) })) } catch { /* skip one damaged field */ }
      }
      empty.drafts[fallback] = values
      return empty
    }
    const parsed: unknown = JSON.parse(raw)
    if (!record(parsed) || parsed.version !== 1) return empty
    const seen = new Set<string>()
    const custom: CustomTheme[] = []
    for (const item of Array.isArray(parsed.custom) ? parsed.custom : []) {
      if (!record(item) || typeof item.id !== 'string' || !/^custom:[a-z0-9-]+$/i.test(item.id) || seen.has(item.id) ||
        typeof item.name !== 'string' || !item.name.trim() || !isThemeId(item.base)) continue
      custom.push({ id: item.id, name: item.name.trim().slice(0, 80), base: item.base, values: valuesOf(item.values) })
      seen.add(item.id)
    }
    const exists = (id: unknown): id is string => isThemeId(id) || (typeof id === 'string' && seen.has(id))
    const drafts: Record<string, Values> = {}
    for (const [id, values] of Object.entries(record(parsed.drafts) ? parsed.drafts : {})) {
      if (exists(id)) drafts[id] = valuesOf(values)
    }
    return { version: 1, selected: exists(parsed.selected) ? parsed.selected : fallback, drafts, custom }
  } catch { return empty }
}

export function writeThemeLibrary(storage: StoragePort | undefined, library: ThemeLibrary): boolean {
  try { if (!storage) return false; storage.setItem(THEME_STORAGE_KEY, JSON.stringify(library)); return true } catch { return false }
}

/** The library is the durable source; the provider namespace is only a compatibility cache. */
export function clearRuntimeStorage(storage: StoragePort | undefined): void {
  try {
    if (!storage) return
    const prefix = `${RUNTIME_NAMESPACE}:field:`
    const keys = Array.from({ length: storage.length }, (_, i) => storage.key(i))
    for (const key of keys) if (key?.startsWith(prefix)) storage.removeItem(key)
  } catch { /* denied storage must not prevent opening the Editor */ }
}

export function themeBase(library: ThemeLibrary, id = library.selected): ThemeId {
  return isThemeId(id) ? id : library.custom.find(theme => theme.id === id)?.base ?? 'contrast-dark'
}
export function themeValues(library: ThemeLibrary, id = library.selected): Values {
  return library.drafts[id] ?? library.custom.find(theme => theme.id === id)?.values ?? {}
}
