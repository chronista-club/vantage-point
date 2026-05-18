/**
 * sidebar の Project (= Runtime Process) 分類ロジック。
 *
 * v1.0 柱 2 PR-2。 sidebar は Project を 2 セクションに分けて表示する:
 *
 * - **稼働中**   — SP の Process / tmux session が存在する状態。
 * - **一時停止中** — SP の Process / tmux session が無い状態
 *   (停止済 / crash / 未起動)。
 *
 * 判定は `ProcessPaneState.state` 文字列で行う。 これは Rust 側
 * `ProcessStatus` (`client.rs`) を `as_str()` した値で、 正規語彙は
 * `starting` / `running` / `stopping` / `stopped` / `error` の 5 つ。
 * 稼働中の述語は Rust の `ProcessStatus::is_alive()` の mirror。
 */
import type { ProcessPaneState } from '../generated/ProcessPaneState'

/**
 * SP の Process / tmux が存在する (= 稼働中) と見なす state。
 * Rust `ProcessStatus::is_alive()` (`Starting | Running | Stopping`) と一致させる。
 */
const RUNNING_STATES: ReadonlySet<string> = new Set(['starting', 'running', 'stopping'])

/**
 * Project が「稼働中」 (SP の Process / tmux あり) かを判定する。
 *
 * `false` = 「一時停止中」。 `state` が未設定 (未起動)・`stopped`・`error` は
 * いずれも Process が無い状態なので一時停止中に倒す。
 */
export function isRunningProcess(proc: ProcessPaneState): boolean {
  return proc.state != null && RUNNING_STATES.has(proc.state)
}
