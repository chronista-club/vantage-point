/** Creo spec 33 read model. Portable: no VP types, stores, or runtime imports.
 * Preserve the catalog's input order for unordered siblings. Placement is deliberately ignored.
 */
export interface AtlasNode { id: string; parentId?: string | null; isApps?: boolean }
export interface AtlasRow<T> { node: T; depth: number; parent: string }
const record = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)
const strings = (value: unknown): string[] => Array.isArray(value) ? value.filter((id): id is string => typeof id === 'string') : []

export function buildAtlasTree<T extends AtlasNode>(nodes: readonly T[], settings: unknown): {rows: AtlasRow<T>[]; invalid: boolean} {
  const invalid = settings != null && (!record(settings) || settings.version !== 1)
  const config = !invalid && record(settings) ? settings : {}
  const order = record(config.order) ? config.order : {}
  const hidden = new Set(strings(config.hidden))
  const byId = new Map(nodes.map(node => [node.id, node]))
  const parents = new Map(nodes.map(node => [node.id,
    node.parentId && node.parentId !== node.id && byId.has(node.parentId) ? node.parentId : null]))
  const concealed = new Set<string>()
  // Conceal before cutting cycles: a hidden ancestor hides every descendant in its ring too.
  for (const node of nodes) {
    const seen = new Set<string>()
    let id: string | null = node.id
    while (id !== null && !seen.has(id)) {
      seen.add(id)
      if (hidden.has(id) || byId.get(id)?.isApps) { concealed.add(node.id); break }
      id = parents.get(id) ?? null
    }
  }
  // Match Creo: cut the smallest ID IN each ring, never its hanging descendants.
  for (const node of [...nodes].sort((a,b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0)) {
    if (concealed.has(node.id)) continue
    const seen = new Set<string>([node.id])
    let id = parents.get(node.id) ?? null
    while (id !== null) {
      if (id === node.id) { parents.set(node.id, null); break }
      if (seen.has(id)) break
      seen.add(id)
      id = parents.get(id) ?? null
    }
  }
  const children = new Map<string | null, T[]>()
  for (const node of nodes) {
    if (concealed.has(node.id)) continue
    const parent = parents.get(node.id) ?? null
    const siblings = children.get(parent) ?? []
    siblings.push(node)
    children.set(parent, siblings)
  }
  for (const [parent, siblings] of children) {
    const key = parent ?? 'root'
    const rank = new Map(strings(Object.hasOwn(order, key) ? order[key] : undefined).map((id,i) => [id,i]))
    siblings.sort((a,b) => (rank.get(a.id) ?? Infinity) - (rank.get(b.id) ?? Infinity))
  }
  const rows: AtlasRow<T>[] = []
  const visit = (parent: string | null, depth: number) => {
    for (const node of children.get(parent) ?? []) {
      rows.push({node,depth,parent:parent ?? 'root'})
      visit(node.id,depth + 1)
    }
  }
  visit(null,0)
  return {rows,invalid}
}
