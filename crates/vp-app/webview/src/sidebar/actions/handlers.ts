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
import { sendIpc } from "../ipc";
import { isPerformerLane, laneAddressKey } from "../lane";
import {
	openAddPerformerFor,
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
}

let laneSelectModeTimer: ReturnType<typeof setTimeout> | null = null;
let laneSelectModeTargets: VisibleLane[] = [];

/** expanded repo の lane を上から flat list で収集（= 1-9 で indexing する候補）。 */
function collectVisibleLanes(): VisibleLane[] {
	const out: VisibleLane[] = [];
	const map = sidebar.lanes_by_repo ?? {};
	for (const proc of sidebar.processes) {
		if (!proc.expanded) continue;
		const lanes = map[proc.path] ?? [];
		const repoName = proc.path.split("/").pop() ?? proc.path;
		for (const lane of lanes) {
			const addr = laneAddressKey(lane);
			// doc 44 P2: lane の種別は消え、開発起点は予約名で表される。
			const name = lane.address.name;
			const label = isPerformerLane(lane)
				? `${repoName} / ${name}`
				: `${repoName} / Conductor`;
			out.push({ path: proc.path, address: addr, label });
		}
	}
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
		const n = parseInt(e.key, 10) - 1;
		const sel = laneSelectModeTargets[n];
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

function enterLaneSelectMode(): void {
	const targets = collectVisibleLanes();
	if (targets.length === 0) {
		console.debug("[directive l] visible lane なし、 mode skip");
		return;
	}
	laneSelectModeTargets = targets;
	const label = targets
		.slice(0, 9)
		.map((t, i) => `${i + 1}. ${t.label}`)
		.join("   ");
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
