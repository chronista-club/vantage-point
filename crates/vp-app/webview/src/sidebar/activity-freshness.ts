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

/**
 * quiet 表示に落とす閾値の**既定** (ms)。`vp now` の運用単位（サブタスク切れ目）より長め。
 *
 * ⚠️ 実際に使う値は daemon の settings.kdl（`idle-timeout-minutes`）由来で、
 * `/api/health` → `ActivitySnapshot.idle_timeout_minutes` として届く（doc 59 P3）。
 * ここはその値が無い時（旧 daemon / オフライン）の落としどころ。
 *
 * **client 側に閾値の真実を持たない**のが要点 — engine を落とす猶予（daemon 判定）と
 * この表示閾値は意図的に同値なので、2 箇所に定数を置くと片方だけ動かせてしまう。
 */
export const QUIET_AFTER_MS = 5 * 60_000;

/**
 * 実効の quiet 閾値 (ms)。daemon 由来の分数を ms に直す。**0 / 未取得は既定に倒す**
 * （旧 daemon は field を返さないので 0 が来る）。
 */
export function quietAfterMs(
	idleTimeoutMinutes: number | bigint | undefined | null,
): number {
	// ⚠️ `bigint` も受ける — 同じ値が **2 つの生成器を通って別の型で届く**。
	// ts-rs は Rust の `u64` を `bigint` に写し（ActivitySnapshot 経由）、
	// club-kdl-codegen は `int` を `number` に写す（settings:result 経由）。
	const m = Number(idleTimeoutMinutes ?? 0);
	return Number.isFinite(m) && m > 0 ? m * 60_000 : QUIET_AFTER_MS;
}

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
	thresholdMs: number = QUIET_AFTER_MS,
): string | null {
	if (typeof lastActivityAt !== "number" || lastActivityAt <= 0) return null;
	const age = now - lastActivityAt;
	if (age < thresholdMs) return null;
	const min = Math.floor(age / 60_000);
	if (min < 60) return `${min}分`;
	return `${Math.floor(min / 60)}時間`;
}
