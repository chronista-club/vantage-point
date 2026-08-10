/**
 * Action handlers — directive の実処理（GPUI 借用 #2 の decouple）。
 *
 * 旧 `keybindings.ts` の `dispatchDirective` if-chain + context helper + `d`/`l` stateful mode を
 * ここ（leaf module）に移設。 `registry.ts` の各 `Action.run` がこれらを呼び（keyboard / ⌘K palette
 * 共通の処理本体）、 `keybindings.ts` は thin な installer に縮小する。
 *
 * import 方向: handlers(leaf) ← registry ← keybindings(installer) で循環なし。
 */
import { sidebar } from "../store";
import { resolveRepoOrder } from "../dnd";
import { sendIpc } from "../ipc";
import {
	collapseSidebar,
	expandSidebar,
	formToRestoreOnExit,
	sidebarForm,
	toggleSidebarForm,
} from "../form";
import type { SidebarForm } from "../form";
import { isPerformerLane, laneAddressKey, laneShortcutNumber } from "../lane";
import { PANEL_BUCKETS } from "../actions-panel/model";
import { appendAction, openBuckets, toggleBucket } from "../actions-panel/store";
import { focusActionRow } from "../actions-panel/ActionRow";
import {
	openAddPerformerFor,
	setCaptureHintLabel,
	setCaptureHintVisible,
	setDeleteHintLabel,
	setDeleteHintVisible,
	setLaneSelectHintLabel,
	setLaneSelectHintVisible,
} from "../directive-state";

// =============================================================================
// context lookup helpers
// =============================================================================

/** `sidebar.lanes_by_repo` を逆引きして address → repo_path を解決。 */
export function resolveRepoPathFromAddress(
	address: string,
): string | undefined {
	const map = sidebar.lanes_by_repo ?? {};
	for (const [repoPath, lanes] of Object.entries(map)) {
		if (
			Array.isArray(lanes) &&
			lanes.some((l) => laneAddressKey(l) === address)
		) {
			return repoPath;
		}
	}
	return undefined;
}

/** address に対応する LaneInfo を逆引き（= isPerformerLane 判定等で必要）。 */
function findLaneByAddress(address: string) {
	const map = sidebar.lanes_by_repo ?? {};
	for (const lanes of Object.values(map)) {
		if (Array.isArray(lanes)) {
			const lane = lanes.find((l) => laneAddressKey(l) === address);
			if (lane) return lane;
		}
	}
	return null;
}

/** active な context から repo_path を 1 つ取り出す（directive `n` 等）。 */
function activeRepoPath(): string | undefined {
	if (sidebar.active_lane_address) {
		return resolveRepoPathFromAddress(sidebar.active_lane_address);
	}
	if (sidebar.active_component) {
		return sidebar.active_component.repo_path;
	}
	return undefined;
}

// =============================================================================
// `d` directive — 2-click confirm の pending state（module-scope singleton）
// =============================================================================
const DELETE_CONFIRM_WINDOW_MS = 1000;

type PendingDeleteTarget =
	| { kind: "lane"; path: string; address: string }
	| { kind: "repo"; path: string };

let pendingDelete: { target: PendingDeleteTarget; expireAt: number } | null =
	null;
let pendingDeleteTimer: ReturnType<typeof setTimeout> | null = null;

function clearPendingDelete(): void {
	pendingDelete = null;
	if (pendingDeleteTimer !== null) {
		clearTimeout(pendingDeleteTimer);
		pendingDeleteTimer = null;
	}
	setDeleteHintVisible(false);
}

function targetMatches(
	a: PendingDeleteTarget,
	b: PendingDeleteTarget,
): boolean {
	if (a.kind !== b.kind) return false;
	if (a.kind === "lane" && b.kind === "lane") {
		return a.path === b.path && a.address === b.address;
	}
	return a.path === b.path;
}

// =============================================================================
// `l` directive — Lane number switcher mode
// =============================================================================
const LANE_SELECT_MODE_MS = 5000;

interface VisibleLane {
	path: string;
	address: string;
	label: string;
	/** 打つ番号（[`laneShortcutNumber`] 由来 = **repo の位置**）。配列 index とは限らない。 */
	number: number;
}

let laneSelectModeTimer: ReturnType<typeof setTimeout> | null = null;
let laneSelectModeTargets: VisibleLane[] = [];

/**
 * ショートカットの宛先＝**各 repo の root lane**を repo の並び順で収集（1〜9）。
 *
 * ⚠️ **畳んでいる repo も含める**。番号は repo の位置（[`laneShortcutNumber`]）なので、
 * 展開状態で動かない＝筋肉記憶が付く。畳んだ先を選んだ場合は選択が展開を促す。
 *
 * ⚠️ **performer lane は対象外**（mako 2026-08-09「root lane だけがショートカットを持つ」）。
 * 旧実装は展開中 repo の全 lane を上から数えており、repo を畳むだけで番号が総入れ替えになった。
 */
function collectVisibleLanes(): VisibleLane[] {
	const out: VisibleLane[] = [];
	const map = sidebar.lanes_by_repo ?? {};
	// ⚠️ 番号は **repo の位置**から出す（積んだ数ではない）。root がまだ立っていない repo を
	// 飛ばすときに `out.length` で数えると、そこから先が 1 つずつ手前にずれ、sidebar の
	// `#N` badge（同じ位置から出す）と宛先が食い違う。
	// ⚠️ **表示順で数える**。`sidebar.processes` の生順ではなく、user が drag で並べ替えた
	// 順（`resolveRepoOrder`）が画面の順 = badge の番号。ここを揃えないと飛び先がずれる。
	resolveRepoOrder(sidebar.processes, sidebar.currents_order).forEach(
		(proc, repoIndex) => {
		const number = laneShortcutNumber(repoIndex);
		if (number === null) return; // 10 個目以降は番号を持たない
		const root = (map[proc.path] ?? []).find((l) => !isPerformerLane(l));
		if (!root) return; // root 不在 = その番号は空席のまま（後続を繰り上げない）
		out.push({
			path: proc.path,
			address: laneAddressKey(root),
			label: proc.path.split("/").pop() ?? proc.path,
			number,
		});
		},
	);
	return out;
}

function exitLaneSelectMode(): void {
	setLaneSelectHintVisible(false);
	setLaneSelectHintLabel("");
	laneSelectModeTargets = [];
	if (laneSelectModeTimer !== null) {
		clearTimeout(laneSelectModeTimer);
		laneSelectModeTimer = null;
	}
	window.removeEventListener("keydown", laneNumberHandler, true);
}

function laneNumberHandler(e: KeyboardEvent): void {
	const target = e.target as HTMLElement | null;
	if (target) {
		const tag = target.tagName;
		if (tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable) {
			exitLaneSelectMode();
			return;
		}
	}
	if (e.key === "Escape") {
		e.preventDefault();
		exitLaneSelectMode();
		return;
	}
	if (e.key >= "1" && e.key <= "9") {
		e.preventDefault();
		// ⚠️ 配列 index ではなく **number で引く**。root 不在の repo は空席なので、
		// `targets[2]` が `#3` とは限らない。
		const n = parseInt(e.key, 10);
		const sel = laneSelectModeTargets.find((t) => t.number === n);
		if (sel) {
			sendIpc({ t: "lane:select", path: sel.path, address: sel.address });
		}
		exitLaneSelectMode();
		return;
	}
	if (
		e.key === "Meta" ||
		e.key === "Control" ||
		e.key === "Shift" ||
		e.key === "Alt"
	) {
		return;
	}
	exitLaneSelectMode();
}

/**
 * `Cmd + N` — sidebar の `#N` badge が指す root lane へ**直接**飛ぶ。
 *
 * ⚠️ 宛先は `⌘ hold l` と**同じ `collectVisibleLanes`** から引く。番号の解決を 2 箇所に
 * 持つと、badge・二段操作・直接 chord の三者がずれる余地が生まれる。
 *
 * 空席（root がまだ立っていない repo の番号）は**何もしない** — 近くの lane に飛ぶと
 * 「押した番号と違う所へ行く」になり、番号を覚える意味が消える。
 */
export function selectLaneByNumber(n: number): void {
	const sel = collectVisibleLanes().find((t) => t.number === n);
	if (!sel) {
		console.debug(`[lane #${n}] 該当なし（空席 or 範囲外）`);
		return;
	}
	sendIpc({ t: "lane:select", path: sel.path, address: sel.address });
}

function enterLaneSelectMode(): void {
	const targets = collectVisibleLanes();
	if (targets.length === 0) {
		console.debug("[directive l] visible lane なし、 mode skip");
		return;
	}
	laneSelectModeTargets = targets;
	// ⚠️ ヒントも **`t.number`** で書く。`i + 1` に戻すと空席のある並びでずれる。
	const label = targets.map((t) => `${t.number}. ${t.label}`).join("   ");
	setLaneSelectHintLabel(label);
	setLaneSelectHintVisible(true);

	if (laneSelectModeTimer !== null) clearTimeout(laneSelectModeTimer);
	window.removeEventListener("keydown", laneNumberHandler, true);

	window.addEventListener("keydown", laneNumberHandler, true);
	laneSelectModeTimer = setTimeout(exitLaneSelectMode, LANE_SELECT_MODE_MS);
}

// =============================================================================
// handlers（= 旧 dispatchDirective の if-block。 registry.ts の Action.run が呼ぶ）
// =============================================================================

/** `f` — File Explorer overlay を open + sidebar focus 移動。 */
export function runFileExplorer(): void {
	const address = sidebar.active_lane_address;
	if (!address) {
		console.debug("[directive f] active lane なし、 picker open skip");
		return;
	}
	window.vpFilePicker?.open(address);
}

/** `p` — send current/selected to board。 */
export function runSendToPP(): void {
	if (window.vpFilePicker?.sendSelectedToPP) {
		window.vpFilePicker.sendSelectedToPP();
	} else {
		console.debug("[directive p] no current selection (picker not visible)");
	}
}

/** `r` — restart context polymorphic: active_lane → lane:restart、 active_component → process:restart。 */
export function runRestart(): void {
	const addr = sidebar.active_lane_address;
	if (addr) {
		const path = resolveRepoPathFromAddress(addr);
		if (path) {
			sendIpc({ t: "lane:restart", path, address: addr });
		} else {
			console.warn(
				"[directive r] active lane address の repo 不明、 skip:",
				addr,
			);
		}
		return;
	}
	if (sidebar.active_component) {
		sendIpc({ t: "process:restart", path: sidebar.active_component.repo_path });
		return;
	}
	console.debug("[directive r] active lane / agent なし、 skip");
}

/** `n` — active repo の AddPerformer form を keyboard で open。 */
export function runNewPerformer(): void {
	const path = activeRepoPath();
	if (!path) {
		console.warn("[directive n] active repo 不明、 form open skip");
		return;
	}
	const opened = openAddPerformerFor(path);
	if (!opened) {
		console.warn("[directive n] AddPerformer setter not registered for", path);
	}
}

/** `d` — delete focused entity（2-click confirm: 1 秒以内に 2 回目で execute）。 */
export function runDelete(): void {
	let target: PendingDeleteTarget | null = null;
	const addr = sidebar.active_lane_address;
	if (addr) {
		const lane = findLaneByAddress(addr);
		const path = resolveRepoPathFromAddress(addr);
		if (lane && path && isPerformerLane(lane)) {
			target = { kind: "lane", path, address: addr };
		} else {
			console.debug("[directive d] target が Performer でない or path 不明、 skip");
			return;
		}
	} else if (sidebar.active_component) {
		target = { kind: "repo", path: sidebar.active_component.repo_path };
	} else {
		console.debug("[directive d] active lane / agent なし、 skip");
		return;
	}

	if (
		pendingDelete &&
		Date.now() < pendingDelete.expireAt &&
		targetMatches(pendingDelete.target, target)
	) {
		const t = pendingDelete.target;
		clearPendingDelete();
		if (t.kind === "lane") {
			sendIpc({ t: "lane:delete", path: t.path, address: t.address });
		} else {
			sendIpc({ t: "repo:delete", path: t.path });
		}
		return;
	}

	if (pendingDeleteTimer !== null) clearTimeout(pendingDeleteTimer);
	pendingDelete = { target, expireAt: Date.now() + DELETE_CONFIRM_WINDOW_MS };
	const label =
		target.kind === "lane"
			? `⌘d again to delete performer: ${target.address}`
			: `⌘d again to delete repo: ${target.path}`;
	setDeleteHintLabel(label);
	setDeleteHintVisible(true);
	pendingDeleteTimer = setTimeout(clearPendingDelete, DELETE_CONFIRM_WINDOW_MS);
}

/** `s` — Lane / repo switcher picker overlay。 */
export function runSwitcher(): void {
	window.vpLanePicker?.open?.();
}

/** `l` — Lane number switcher mode に突入。 */
export function runLaneSelectMode(): void {
	enterLaneSelectMode();
}

/** `[` — 左 sidebar をフル ⇄ スリム帯に変身（sidebar view modes）。 */
export function runSidebarForm(): void {
	toggleSidebarForm();
}

// =============================================================================
// `b` directive — ACTIONS capture mode（doc 57 §0）
// =============================================================================
//
// ⚠️ ここが ACTIONS の**本命の入口**。サイドバーへマウスを伸ばした時点で既に「中断」して
// いるので、差し込みの緩衝には「打鍵だけで置ける」経路が要る。骨格は上の lane number mode と同型。
//
// 文字が `b`（buffer）で `a` でないのは、**⌘A = Select All**（doc 18 §C.4 で system として keep）
// を奪わないため — directive は capture phase で preventDefault する。

const CAPTURE_MODE_MS = 5000;

let captureModeTimer: ReturnType<typeof setTimeout> | null = null;

/**
 * 一時展開する前の形。`null` = 展開していない。
 *
 * ⚠️ mode に**入るときだけ**書き、抜けるときに必ず `null` へ戻す。`b` を連打しても
 * 上書きしない（2 回目に「展開後の full」を覚えると slim を失う）。
 */
let formBeforeCapture: SidebarForm | null = null;

/**
 * capture mode を抜ける。**離脱経路 5 本（Escape / 数字選択 / 無関係キー / 5 秒 timeout /
 * 入力欄への focus）がすべてここを通る**ので、一時展開の後始末もここ 1 箇所で足りる。
 *
 * @param selected 区画を選んで抜けたか。**選んだときは畳まない** — 新しい行に focus が
 *   当たっていて user はこれから打ち込む（`captureNumberHandler` の数字枝）
 */
function exitCaptureMode(selected = false): void {
	setCaptureHintVisible(false);
	setCaptureHintLabel("");
	if (captureModeTimer !== null) {
		clearTimeout(captureModeTimer);
		captureModeTimer = null;
	}
	window.removeEventListener("keydown", captureNumberHandler, true);
	// 一時展開の後始末。⚠️ 記録を**先に**消す（畳む側が何かの拍子に再入しても二度走らない）。
	const next = formToRestoreOnExit(formBeforeCapture, selected);
	formBeforeCapture = null;
	if (next === "slim") collapseSidebar();
}

function captureNumberHandler(e: KeyboardEvent): void {
	const target = e.target as HTMLElement | null;
	if (target) {
		const tag = target.tagName;
		// 入力中なら数字入力を妨げない（mode を黙って抜ける）。
		if (tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable) {
			exitCaptureMode();
			return;
		}
	}
	if (e.key === "Escape") {
		e.preventDefault();
		exitCaptureMode();
		return;
	}
	const n = Number.parseInt(e.key, 10);
	if (Number.isInteger(n) && n >= 1 && n <= PANEL_BUCKETS.length) {
		e.preventDefault();
		const bucket = PANEL_BUCKETS[n - 1];
		// ⚠️ ここだけ `selected` — 下で行に focus を当てるので、畳むと編集中に潰れる。
		exitCaptureMode(true);
		// 区画を開いてから足す — 閉じたままだと行が DOM に出ず focus が当たらない。
		if (!openBuckets().has(bucket.id)) toggleBucket(bucket.id);
		focusActionRow(appendAction(bucket.id));
		return;
	}
	if (
		e.key === "Meta" ||
		e.key === "Control" ||
		e.key === "Shift" ||
		e.key === "Alt"
	) {
		return;
	}
	exitCaptureMode();
}

/** `b` — ACTIONS の捕捉 mode に突入（1-5 で区画を選ぶ）。 */
export function runCaptureMode(): void {
	// スリム帯だと行が描画されないので、**一時的に**フルへ広げる。取り消して抜けたら
	// `exitCaptureMode` が畳んで戻す（形の変更は永続化されるので、戻さないと slim が消える）。
	// ⚠️ 連打で上書きしない — 2 回目に「展開後の full」を覚えると戻す先を失う。
	if (formBeforeCapture === null) formBeforeCapture = sidebarForm();
	expandSidebar();
	setCaptureHintLabel(
		PANEL_BUCKETS.map((b, i) => `${i + 1}. ${b.label}`).join("   "),
	);
	setCaptureHintVisible(true);

	if (captureModeTimer !== null) clearTimeout(captureModeTimer);
	window.removeEventListener("keydown", captureNumberHandler, true);

	window.addEventListener("keydown", captureNumberHandler, true);
	// ⚠️ 関数を直接渡さない — timer は callback に引数を足せるので、`selected` が
	// 意図せず真になる形に将来変わりうる。取り消し扱いであることを明示する。
	captureModeTimer = setTimeout(() => exitCaptureMode(), CAPTURE_MODE_MS);
}

/** `]` — 右を edge rail ⇄ R sidebar（debug log）に変身。
 *  実体（DOM / 開閉 state / tail 購読）は main bundle の right-sidebar.ts が持つので、
 *  共有 bus（doc 47 §6、統合 WebView の同一 document）で依頼だけ投げる。 */
export function runRightSidebarToggle(): void {
	document.dispatchEvent(new CustomEvent("vp:right-sidebar-toggle"));
}
