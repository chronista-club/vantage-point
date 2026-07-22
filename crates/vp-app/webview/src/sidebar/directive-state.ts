/**
 * Directive 実装のための shared state + helper (= PR 445)。
 *
 * sidebar 内に **component scope を跨ぐ state / helper** を集約する module。 既存の sidebar store
 * (= Rust から push される SidebarState) とは別の **directive 専用の小さな state** を保持する場所。
 * 例: `n` directive が ProjectAccordion 内 local signal の addPerformerOpen を mutate するための registry、
 * `d` directive の 2-click confirm pending state + visual hint signal、 など。
 */
import { createSignal } from 'solid-js'

// =============================================================================
// `n` directive — AddPerformer form open registry
// =============================================================================
//
// AddPerformer form の open 状態は ProjectAccordion 内 local signal (`addPerformerOpen`) で管理されている。
// `n` directive (= keyboard で active project の form を open) を実装するために、
// 各 ProjectAccordion が mount 時に「project_path → setter」 を本 registry に register し、
// directive 発火時に該当 setter を呼ぶ。

const addPerformerOpenRegistry = new Map<string, (open: boolean) => void>()

/**
 * ProjectAccordion が mount 時に呼ぶ。 戻り値の unregister を onCleanup で呼ぶこと。
 */
export function registerAddPerformerOpenSetter(
  projectPath: string,
  setter: (open: boolean) => void,
): () => void {
  addPerformerOpenRegistry.set(projectPath, setter)
  return () => {
    if (addPerformerOpenRegistry.get(projectPath) === setter) {
      addPerformerOpenRegistry.delete(projectPath)
    }
  }
}

/**
 * directive `n` の発火経路。 該当 project の form を open する。
 * 戻り値: setter が登録されていたら true、 されていなければ false。
 */
export function openAddPerformerFor(projectPath: string): boolean {
  const setter = addPerformerOpenRegistry.get(projectPath)
  if (!setter) return false
  setter(true)
  return true
}

// =============================================================================
// `d` directive — 2-click delete confirm hint
// =============================================================================
//
// `d` directive の 1 回目押下で「pending」 状態、 1 秒以内に 2 回目で execute、 timeout で abort。
// pending 中は sidebar 下端に hint bar を出して user に visual feedback する。
// state は module-scope (= 1 sidebar に対して singleton)。

/** hint bar 表示の visible state。 Shell.tsx で `<Show when={deleteHintVisible()}>` で render。 */
export const [deleteHintVisible, setDeleteHintVisible] = createSignal(false)
/** hint bar に表示する label (= "delete performer: foo/performer/bar" 等)。 */
export const [deleteHintLabel, setDeleteHintLabel] = createSignal('')

// =============================================================================
// `l` directive — Lane number switcher mode (v0.6 / PR 447)
// =============================================================================
//
// `Cmd hold l` 発火で「lane number mode」 に突入: 5 秒以内に 1-9 を modifier なしで打つと、
// visible lane (= expanded project の中) を上から N 番目で lane:select 発火。 mode 中は
// hint bar (= sidebar 下端) に「Press 1-9 / Esc to cancel」 を表示。
//
// state は module-scope (= 1 sidebar に対して singleton)。

/** lane number mode hint bar の visible state。 Shell.tsx で render。 */
export const [laneSelectHintVisible, setLaneSelectHintVisible] = createSignal(false)
/** lane number mode hint bar に表示する候補 lane の一覧 (= "1. project/root  2. project/performer/foo  ...")。 */
export const [laneSelectHintLabel, setLaneSelectHintLabel] = createSignal('')
