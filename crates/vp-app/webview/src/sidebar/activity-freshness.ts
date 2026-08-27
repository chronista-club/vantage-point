/**
 * 「quiet N分」— session の最終活動時刻から沈黙時間を導く（now-line 鮮度検知）。
 *
 * データ源は roster の `last_activity_at`（server 実測、分粒度に量子化済み。tui = PTY 出力 /
 * gui = ConversationEvent — Rust `LanePool::session_activity`）。閾値判定は client 側 —
 * server は事実（時刻）だけ運ぶ。
 *
 * ## なぜ client 時計で引き算してよいか
 *
 * daemon と GUI は同一マシン（fold-in 後の repo は daemon プロセス内）なので時計は同源。
 * 量子化（1 分）と snapshot push 周期（5s）のズレは閾値（5 分）に対して十分小さい。
 */
import { createSignal } from "solid-js";

/** これ以上活動が無いと「quiet」表示に落とす閾値 (ms)。`vp now` の運用単位（サブタスク切れ目）より長め。 */
export const QUIET_AFTER_MS = 5 * 60_000;

/** 経過表示の再計算 tick。分表示なので 30s で十分（それ以上細かくしても表示は変わらない）。 */
const TICK_MS = 30_000;

const [clockNow, setClockNow] = createSignal(Date.now());

let started = false;

/**
 * 鮮度時計を開始する（sidebar bundle の入口で 1 回）。module import の副作用で
 * interval を張らないのは、test（vitest）が interval を抱えたままになるのを避けるため。
 */
export function startFreshnessClock(): void {
	if (started) return;
	started = true;
	setInterval(() => setClockNow(Date.now()), TICK_MS);
}

/** reactive な現在時刻 (ms)。`quietLabel` の now に渡す（読んだ component が tick で再評価される）。 */
export { clockNow };

/**
 * 沈黙ラベル（"12分" / "2時間"）。閾値未満（活動が新しい）/ 時刻不明（undefined / 0 以下）は
 * null = 何も出さない（「語ることが無い行は黙る」— cwd と同じ流儀）。
 */
export function quietLabel(
	lastActivityAt: number | null | undefined,
	now: number,
): string | null {
	if (typeof lastActivityAt !== "number" || lastActivityAt <= 0) return null;
	const age = now - lastActivityAt;
	if (age < QUIET_AFTER_MS) return null;
	const min = Math.floor(age / 60_000);
	if (min < 60) return `${min}分`;
	return `${Math.floor(min / 60)}時間`;
}
