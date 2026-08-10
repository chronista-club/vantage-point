/**
 * Directive 実装のための shared state + helper (= PR 445)。
 *
 * sidebar 内に **component scope を跨ぐ state / helper** を集約する module。 既存の sidebar store
 * (= Rust から push される SidebarState) とは別の **directive 専用の小さな state** を保持する場所。
 * 例: `n` directive が RepoAccordion 内 local signal の addSubOpen を mutate するための registry、
 * `d` directive の 2-click confirm pending state + visual hint signal、 など。
 */
import { createSignal } from 'solid-js'

// =============================================================================
// `n` directive — AddSub form open registry
// =============================================================================
//
// AddSub form の open 状態は RepoAccordion 内 local signal (`addSubOpen`) で管理されている。
// `n` directive (= keyboard で active repo の form を open) を実装するために、
// 各 RepoAccordion が mount 時に「repo_path → setter」 を本 registry に register し、
// directive 発火時に該当 setter を呼ぶ。

const addSubOpenRegistry = new Map<string, (open: boolean) => void>()

/**
 * RepoAccordion が mount 時に呼ぶ。 戻り値の unregister を onCleanup で呼ぶこと。
 */
export function registerAddSubOpenSetter(
  repoPath: string,
  setter: (open: boolean) => void,
): () => void {
  addSubOpenRegistry.set(repoPath, setter)
  return () => {
    if (addSubOpenRegistry.get(repoPath) === setter) {
      addSubOpenRegistry.delete(repoPath)
    }
  }
}

/**
 * directive `n` の発火経路。 該当 repo の form を open する。
 * 戻り値: setter が登録されていたら true、 されていなければ false。
 */
export function openAddSubFor(repoPath: string): boolean {
  const setter = addSubOpenRegistry.get(repoPath)
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
/** hint bar に表示する label (= "delete sub: foo/sub/bar" 等)。 */
export const [deleteHintLabel, setDeleteHintLabel] = createSignal('')

// =============================================================================
// `l` directive — Lane number switcher mode (v0.6 / PR 447)
// =============================================================================
//
// `Cmd hold l` 発火で「lane number mode」 に突入: 5 秒以内に 1-9 を modifier なしで打つと、
// visible lane (= expanded repo の中) を上から N 番目で lane:select 発火。 mode 中は
// hint bar (= sidebar 下端) に「Press 1-9 / Esc to cancel」 を表示。
//
// state は module-scope (= 1 sidebar に対して singleton)。

/** lane number mode hint bar の visible state。 Shell.tsx で render。 */
export const [laneSelectHintVisible, setLaneSelectHintVisible] = createSignal(false)
/** lane number mode hint bar に表示する候補 lane の一覧 (= "1. repo/root  2. repo/sub/foo  ...")。 */
export const [laneSelectHintLabel, setLaneSelectHintLabel] = createSignal('')

// =============================================================================
// `a` directive — ACTIONS capture mode (doc 57 §0、2026-08-02)
// =============================================================================
//
// `Cmd hold a` で捕捉 mode に突入: 5 秒以内に 1-6 を打つと、その区画に空の Action を 1 行足して
// focus が移る。**作業を止めずに置ける**ことが ACTIONS の存在理由なので、マウスを伸ばさずに
// 完結する経路が要る（doc 57 §0 ①）。
//
// 骨格は上の `l`（lane number mode）と同型 — mode 突入 → hint bar → 数字 → 実行 → 退出。

/** capture mode hint bar の visible state。 Shell.tsx で render。 */
export const [captureHintVisible, setCaptureHintVisible] = createSignal(false)
/** capture mode hint bar に出す区画の一覧 (= "1. NEXTs  2. WAITs  ...")。 */
export const [captureHintLabel, setCaptureHintLabel] = createSignal('')
