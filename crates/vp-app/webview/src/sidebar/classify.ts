/**
 * sidebar の Repo (= Runtime Process) 稼働判定ロジック。
 *
 * 旧「稼働中 / 一時停止中」 タブ分割 (Shell) は撤去済 (2026-07-10)。 現在の唯一の消費者は
 * RepoAccordion の per-repo 出し分け (repo が無い repo に起動 ▶ affordance を出す)。
 *
 * 判定は `RepoPaneState.state` 文字列で行う。 これは Rust 側
 * `ProcessStatus` (`client.rs`) を `as_str()` した値で、 正規語彙は
 * `starting` / `running` / `stopping` / `stopped` / `error` の 5 つ。
 * 稼働中の述語は Rust の `ProcessStatus::is_alive()` の mirror。
 */
import type { RepoPaneState } from '../generated/RepoPaneState'

/**
 * repo の Process / tmux が存在する (= 稼働中) と見なす state。
 * Rust `ProcessStatus::is_alive()` (`Starting | Running | Stopping`) と一致させる。
 */
const RUNNING_STATES: ReadonlySet<string> = new Set(['starting', 'running', 'stopping'])

/**
 * Repo が「稼働中」 (repo の Process あり) かを判定する。
 *
 * `false` (= repo なし = 停止済 / crash / 未起動) の repo には RepoAccordion が
 * 起動 ▶ affordance を出す。 `state` が未設定・`stopped`・`error` はいずれも停止扱い。
 */
export function isRunningProcess(proc: RepoPaneState): boolean {
  return proc.state != null && RUNNING_STATES.has(proc.state)
}
