/**
 * lane-panes — lane 内 tiling を creo-ui-layout の場に写す（doc 49 LE-P4 PR2）。
 *
 * 旧 pane-shell.ts（doc 46 P1 の PaneLayout / LaneLayouts / PaneShell）の後継。
 * lane ごとの独立状態は engine の **scope**（`lane:<addr>`）がそのまま担う — 旧
 * LaneLayouts（lane → PaneLayout の Map）は scope key に畳まれて消える。
 * 旧語彙との対応: minimize = mute（attention 0）/ restore = setShare / 「最後の
 * 1 枚は畳ませない」= gestures.mute の全零 guard（protocol が既に持っている規則）。
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: `LANE_PANE_REFS` — lane に並ぶ Pane の顔ぶれ（lane によらず共通）
 * - **calculations**: `toggleLanePane` / `newPaneChoices` — 純関数（vitest で固定）
 * - **actions**: `installLanePanes` — engine 購読 + DOM への反映（display / rect / class）
 *
 * ## 投影規則
 *
 * - 非表示 pane は **display:none**（旧 `.pane-minimized` と同じ畳み方）。lane-host の
 *   xterm は今日までこの隠し方で運用されてきた実績に合わせ、app-panes（全面透明方式）
 *   とは意図的に流儀を変えない
 * - keyboard focus は protocol の関心外（LE-20）— focus ring は本 module の状態で持つ
 * - 表示は既定「1 枚ずつ」（mako 2026-07-21、mode 切替 = showOnly = solo）。chip で
 *   並列表示に戻せる（機能は消さない）。doc 47 §1 の決着後に既定を tiling へ戻す時は
 *   applyConsoleMode の showOnly を focus だけにする
 */

import {
	type Layout,
	mute,
	resolve,
	setShare,
	solo,
	visibleIds,
} from "@chronista-club/creo-ui-layout";
import { layoutEngine } from "./layout-host";

/** lane に並ぶ Pane の顔ぶれ（id = host 要素の DOM id）。lane によらず共通 */
export const LANE_PANE_REFS = [
	{ id: "lane-host", label: "Console" },
	{ id: "console-chat-host", label: "Chat" },
] as const;

/** 要件 3: フォーカスの視認 ring（CSS は main_area.rs `#lane-panes > .pane-focused`） */
export const CLASS_FOCUSED = "pane-focused";
/** タブエリアの開閉（畳まれた Pane がある時だけ区切り線を出す） */
export const CLASS_TABS_ACTIVE = "pane-tabs-active";

/** lane → engine scope key（doc §12: scope 分離で app 全体 engine と入れ子両立） */
export function laneScope(lane: string): string {
	return `lane:${lane}`;
}

/** 初期配置: 全 Pane が横並び・等分（旧 PaneShell の template seed と同じ既定） */
export function initialLaneLayout(): Layout {
	const attention: Record<string, number> = {};
	for (const p of LANE_PANE_REFS) attention[p.id] = 1;
	return {
		structure: { columns: LANE_PANE_REFS.map((p) => ({ panes: [p.id] })) },
		attention,
	};
}

/** 畳んだ Pane を開き直す時の share（2 枚構成なら等分に戻る） */
const RESTORE_SHARE = 0.5;

/**
 * chip の 1 クリック往復（純 calculation）。
 * 可視 → mute（最後の 1 枚は mute の全零 guard が拒否 = 同一参照が返る）/
 * 非可視 → setShare で復帰。構造は不変なので「元の位置へ戻る」は自明に成立する。
 */
export function toggleLanePane(layout: Layout, id: string): Layout {
	if ((layout.attention[id] ?? 0) > 0) return mute(layout, id);
	return setShare(layout, id, RESTORE_SHARE);
}

/** 新 Pane の選択肢 1 つ（doc 46 P2 要件 4: Engine × Act）。 */
export type NewPaneChoice = {
	/** stand 名（`echoes` / `codex` / `grok` …）。 */
	engine: string;
	/** 表示名（engine の人間可読名）。 */
	engineLabel: string;
	act: "tui" | "chat";
};

/**
 * Engine × Act の総当たりを作る（doc 46 P2 要件 4、純関数）。
 *
 * `chatCapable` が false の engine は **Act II（chat）を出さない** — chat host を持たない
 * engine で chat Pane を作ると「作れるが submit がエラーになるだけ」の行き止まりになる
 * （doc 38 Phase 3 が tab の「+」で同じ判断をしている）。Act I（tui）は login shell に
 * 流し込むだけなのでどの engine でも成立する。
 */
export function newPaneChoices(
	stands: readonly { name: string; label?: string; chat_capable?: boolean }[],
): NewPaneChoice[] {
	const out: NewPaneChoice[] = [];
	for (const s of stands) {
		if (!s.name) continue;
		const engineLabel = s.label && s.label.length > 0 ? s.label : s.name;
		out.push({ engine: s.name, engineLabel, act: "tui" });
		if (s.chat_capable) out.push({ engine: s.name, engineLabel, act: "chat" });
	}
	return out;
}

/** 表示中 lane に対する操作面（lane の指定は setActiveLane に一本化）。 */
export interface LanePanesController {
	/** 表示 lane を切り替え、その lane の配置を DOM へ写し直す（doc 47 §3） */
	setActiveLane(lane: string): void;
	/** 指定 Pane だけを見せる（mode 切替の既定 = 旧 minimizeOthers。focus も移す） */
	showOnly(paneId: string): void;
	/** focus を当てる。畳まれた Pane を指したら復元も行う（旧 PaneLayout.focus） */
	focusPane(paneId: string): void;
}

export interface LanePanesDeps {
	/** Pane host 要素の解決（id → 要素）。テストから差し替え可能にするため関数で受ける */
	hostOf: (id: string) => HTMLElement | null;
	/** chip を並べるタブエリア（#pane-tabs） */
	tabs: HTMLElement;
	/** タブエリアの開閉 class を載せる要素（#pane-terminal） */
	frame: HTMLElement;
}

/**
 * lane panes を DOM に配線する（actions）。engine の notify（将来の AI / MCP 駆動も
 * 含む）で表示 lane の scope が動けば再描画される。
 */
export function installLanePanes(deps: LanePanesDeps): LanePanesController {
	let activeLane: string | null = null;
	/** lane → focus を持つ pane id（LE-20: focus は場の外 = module 状態） */
	const focusById = new Map<string, string>();

	// boot 既定を **同期で** DOM に書く（旧 PaneShell.dock() が bundle init 時に同期 render
	// していたのと同じ「event を待たず DOM 確定」）。これが無いと vp:console-mode 到着までの
	// 窓で、CSS 既定 inset:0 のまま DOM 後発の空 chat host が xterm host を覆う
	// （#880 と同族の boot 窓 — team-b review #1）。初期の見た目も旧実装と同じ等分並び。
	{
		const bootResolved = resolve(initialLaneLayout());
		for (const p of LANE_PANE_REFS) {
			const el = deps.hostOf(p.id);
			if (!el) continue;
			const r = bootResolved[p.id];
			if (!r) continue;
			el.style.display = "";
			el.style.left = `${r.rect.x * 100}%`;
			el.style.top = `${r.rect.y * 100}%`;
			el.style.width = `${r.rect.w * 100}%`;
			el.style.height = `${r.rect.h * 100}%`;
			// 旧実装は最初に dock した Pane（Console）が focus を持っていた
			el.classList.toggle(CLASS_FOCUSED, p.id === LANE_PANE_REFS[0].id);
		}
	}

	/** scope の初期化（未訪問 lane は等分並びで始める）。戻り値は scope key */
	const ensure = (lane: string): string => {
		const scope = laneScope(lane);
		if (layoutEngine.current(scope).structure.columns.length === 0) {
			layoutEngine.update(scope, () => initialLaneLayout());
		}
		return scope;
	};

	const visibleOf = (scope: string): string[] => {
		const resolved = layoutEngine.resolved(scope);
		return LANE_PANE_REFS.filter((p) => {
			const r = resolved[p.id];
			return !!r && r.rect.w > 0 && r.rect.h > 0;
		}).map((p) => p.id);
	};

	const render = (): void => {
		if (!activeLane) return;
		const lane = activeLane;
		const scope = laneScope(lane);
		const resolved = layoutEngine.resolved(scope);
		const visible = visibleOf(scope);
		// focus が畳まれた Pane を指していたら残った先頭へ（focus を失わせない）
		const stored = focusById.get(lane);
		const focused = stored && visible.includes(stored) ? stored : (visible[0] ?? null);

		for (const p of LANE_PANE_REFS) {
			const el = deps.hostOf(p.id);
			if (!el) continue;
			const r = resolved[p.id];
			const isVisible = visible.includes(p.id);
			// 投影規則: 非表示 = display:none（旧 .pane-minimized と同じ畳み方 — 冒頭 doc）
			el.style.display = isVisible ? "" : "none";
			if (isVisible && r) {
				el.style.left = `${r.rect.x * 100}%`;
				el.style.top = `${r.rect.y * 100}%`;
				el.style.width = `${r.rect.w * 100}%`;
				el.style.height = `${r.rect.h * 100}%`;
			}
			el.classList.toggle(CLASS_FOCUSED, isVisible && p.id === focused);
		}
		renderChips(lane, visible);
	};

	// タブエリアは **全 Pane のスイッチャー**（旧 PaneShell.render と同じ設計 — 畳んだもの
	// だけ並べると「並んでいる Pane を畳む」入口が UI から消える）。render はべき等:
	// 状態から chip を作り直すだけで差分を追わない（数枚前提、entry 側の MutationObserver
	// が「+ New」を毎回付け直す規約も従来どおり）
	const renderChips = (lane: string, visible: readonly string[]): void => {
		deps.tabs.replaceChildren();
		let hiddenCount = 0;
		for (const p of LANE_PANE_REFS) {
			const isVisible = visible.includes(p.id);
			if (!isVisible) hiddenCount += 1;
			const chip = document.createElement("button");
			chip.type = "button";
			chip.className = isVisible ? "pane-tab docked" : "pane-tab";
			chip.dataset.paneId = p.id;
			chip.textContent = p.label;
			chip.title = isVisible ? `${p.label} を畳む` : `${p.label} を開く`;
			chip.addEventListener("click", () => togglePane(lane, p.id));
			deps.tabs.appendChild(chip);
		}
		deps.frame.classList.toggle(CLASS_TABS_ACTIVE, hiddenCount > 0);
	};

	const togglePane = (lane: string, paneId: string): void => {
		const scope = ensure(lane);
		const before = layoutEngine.current(scope);
		const wasVisible = (before.attention[paneId] ?? 0) > 0;
		layoutEngine.update(scope, (l) => toggleLanePane(l, paneId));
		layoutEngine.settle(scope, "human");
		if (!wasVisible) {
			// 畳んだものを開く = 見たいはず（旧 restore と同じ focus 移動）
			focusById.set(lane, paneId);
		} else if (focusById.get(lane) === paneId) {
			focusById.set(lane, visibleIds(layoutEngine.current(scope))[0] ?? paneId);
		}
		render();
	};

	// 表示 lane の scope が外（将来の AI / MCP / fleet）から動いた時も追従する
	layoutEngine.subscribe((scope) => {
		if (activeLane && scope === laneScope(activeLane)) render();
	});

	return {
		setActiveLane(lane) {
			if (activeLane === lane) return;
			activeLane = lane;
			ensure(lane);
			render();
		},
		showOnly(paneId) {
			if (!activeLane) return;
			const scope = ensure(activeLane);
			layoutEngine.update(scope, (l) => solo(l, paneId));
			// ⚠️ solo 直後の settle は**意図的**（team-b review #2 の明文化）。protocol の
			// solo は「un-solo = restoreLastSettle」の一時 view だが、ここでの showOnly は
			// mode 切替 = 形の確定（旧 minimizeOthers が layout を恒久 mutate していたのと
			// 同じ意味論）。復帰は restoreLastSettle でなく chip の toggleLanePane が担う
			layoutEngine.settle(scope, "human");
			focusById.set(activeLane, paneId);
			render();
		},
		focusPane(paneId) {
			if (!activeLane) return;
			const scope = ensure(activeLane);
			if ((layoutEngine.current(scope).attention[paneId] ?? 0) <= 0) {
				// minimized を指したら復元も行う（旧 PaneLayout.focus と同じ）
				layoutEngine.update(scope, (l) => setShare(l, paneId, RESTORE_SHARE));
				layoutEngine.settle(scope, "human");
			}
			focusById.set(activeLane, paneId);
			render();
		},
	};
}
