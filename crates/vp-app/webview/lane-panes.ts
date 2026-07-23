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
 * - **data**: `TERM_PANE_REF` + session 由来の動的 refs（`lanePaneRefs` — doc 50 P1 で動的化）
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
import { focusedOf } from "./console";
import { sessionChipPrefix } from "./EchoesHeader";

/** lane に並ぶ Pane の 1 参照（id = host 要素の DOM id）。 */
export type PaneRef = {
	id: string;
	label: string;
	/** chat session pane なら session key（doc 46 §1.5 session ↔ Pane 1:1）。term pane は無し */
	session?: number;
};

/** Act I（xterm）の代表 pane。World A の xterm re-key（doc 50 P3）まで lane に 1 枚 */
export const TERM_PANE_REF: PaneRef = { id: "lane-host", label: "Console" };

/** chat session pane の host DOM id。表示中 lane の host にだけ使う（lane 切替で作り直すため
 *  lane を id に含めない — DOM には常に 1 lane 分しか存在しない） */
export function chatHostId(session: number): string {
	return `chat-session-${session}`;
}

/** host DOM id → session key（chatHostId の逆写像。term pane / 未知 id は null）。 */
export function sessionOfHostId(id: string): number | null {
	const m = id.match(/^chat-session-(\d+)$/);
	return m ? Number(m[1]) : null;
}

/** lane の pane の顔ぶれ（純関数）: Console + chat session 群。
 *  旧 LANE_PANE_REFS（固定 2 枚: lane-host / console-chat-host）の後継 — 「Chat」という
 *  固定 1 枚は session ↔ Pane 1:1（doc 46 §1.5）に反していたため、session の数だけ生える */
export function lanePaneRefs(
	sessions: readonly { key: number; stand: string }[],
): PaneRef[] {
	return [
		TERM_PANE_REF,
		...sessions.map((v) => ({
			id: chatHostId(v.key),
			label: `${sessionChipPrefix(v.stand)}#${v.key}`,
			session: v.key,
		})),
	];
}

/** layout の列を refs に同期する（純関数）。
 *  - refs に居るが structure に無い pane: 右端に列 append（attention は 0 = chip に生えるだけ。
 *    開くのは mode 切替の showOnly / chip click / focus が担う）
 *  - structure に居るが refs から消えた pane（closed session）: 列から除去
 *  往復（sync → sync）は不動点 = 冪等 */
export function syncPaneColumns(layout: Layout, ids: readonly string[]): Layout {
	const want = new Set(ids);
	const columns = layout.structure.columns
		.map((c) => ({ panes: c.panes.filter((v) => want.has(v)) }))
		.filter((c) => c.panes.length > 0);
	const present = new Set(columns.flatMap((c) => c.panes));
	for (const id of ids) if (!present.has(id)) columns.push({ panes: [id] });
	const attention: Record<string, number> = {};
	for (const id of ids) attention[id] = layout.attention[id] ?? 0;
	return { structure: { columns }, attention };
}

/** 要件 3: フォーカスの視認 ring（CSS は main_area.rs `#lane-panes > .pane-focused`） */
export const CLASS_FOCUSED = "pane-focused";
/** タブエリアの開閉（畳まれた Pane がある時だけ区切り線を出す） */
export const CLASS_TABS_ACTIVE = "pane-tabs-active";

/** lane → engine scope key（doc §12: scope 分離で app 全体 engine と入れ子両立） */
export function laneScope(lane: string): string {
	return `lane:${lane}`;
}

/** 初期配置: lane-host（Console）1 枚が全面。chat session pane は session 一覧の到着後に
 *  syncPaneColumns で生える（boot 窓に空の chat host が xterm を覆う #880 系の問題は、
 *  「無い host は覆えない」の形で構造的に消えた） */
export function initialLaneLayout(): Layout {
	return {
		structure: { columns: [{ panes: [TERM_PANE_REF.id] }] },
		attention: { [TERM_PANE_REF.id]: 1 },
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
	/** chat session host を生やす親（#lane-panes）。動的 pane は render が生成/破棄する */
	container: HTMLElement;
	/** chat session pane の中身を host に mount する（chatview.mountSession）。返り値 = dispose */
	mountChat: (host: HTMLElement, lane: string, session: number) => () => void;
}

/**
 * lane panes を DOM に配線する（actions）。engine の notify（将来の AI / MCP 駆動も
 * 含む）で表示 lane の scope が動けば再描画される。
 */
export function installLanePanes(deps: LanePanesDeps): LanePanesController {
	let activeLane: string | null = null;
	/** lane → focus を持つ pane id（LE-20: focus は場の外 = module 状態） */
	const focusById = new Map<string, string>();
	/** lane → pane の顔ぶれ（session 一覧由来。未着 lane は Console のみ） */
	const refsByLane = new Map<string, PaneRef[]>();
	/** 表示中 lane の動的 host の dispose（host id → SessionChatView の unmount） */
	const dynDisposers = new Map<string, () => void>();
	/** showOnly が「まだ生えていない pane」を指した時の保留先（boot 窓: applyConsoleMode は
	 *  session 一覧の到着前に走る）。到着時に 1 回だけ貼って消費する — 保留にせず solo すると
	 *  存在しない id への solo で全 pane が消える（2026-07-24 実機で観測した空白画面）。 */
	let pendingShowOnly: string | null = null;

	/** pane が layout の構造に居るか（showOnly / focus の前提確認）。 */
	const paneExists = (scope: string, id: string): boolean =>
		layoutEngine.current(scope).structure.columns.some((c) => c.panes.includes(id));

	const refsOf = (lane: string): PaneRef[] =>
		refsByLane.get(lane) ?? [TERM_PANE_REF];

	// boot 既定を **同期で** DOM に書く（旧 PaneShell.dock() が bundle init 時に同期 render
	// していたのと同じ「event を待たず DOM 確定」）。boot 時点の refs は Console のみ —
	// chat host は session 一覧の到着後に生成されるので、空 host が xterm を覆う boot 窓
	// （#880 と同族）は「無い host は覆えない」の形で構造ごと消えた。
	{
		const bootResolved = resolve(initialLaneLayout());
		const el = deps.hostOf(TERM_PANE_REF.id);
		const r = bootResolved[TERM_PANE_REF.id];
		if (el && r) {
			el.style.display = "";
			el.style.left = `${r.rect.x * 100}%`;
			el.style.top = `${r.rect.y * 100}%`;
			el.style.width = `${r.rect.w * 100}%`;
			el.style.height = `${r.rect.h * 100}%`;
			el.classList.toggle(CLASS_FOCUSED, true);
		}
	}

	/** scope の初期化（未訪問 lane は Console 全面で始める）。戻り値は scope key */
	const ensure = (lane: string): string => {
		const scope = laneScope(lane);
		if (layoutEngine.current(scope).structure.columns.length === 0) {
			layoutEngine.update(scope, () => initialLaneLayout());
		}
		return scope;
	};

	/** 表示中 lane の動的 host（chat session pane）を refs に同期する — 無ければ生成 + mount、
	 *  消えた session の host は dispose + DOM 除去。生成直後は display:none（render が
	 *  可視性を決めるまで何も覆わない — #880 の教訓）。 */
	const syncDynHosts = (lane: string, refs: PaneRef[]): void => {
		const want = new Map(
			refs.filter((v) => v.session !== undefined).map((v) => [v.id, v.session as number]),
		);
		// 消えた host の破棄
		for (const [id, dispose] of [...dynDisposers]) {
			if (want.has(id)) continue;
			dispose();
			dynDisposers.delete(id);
			deps.hostOf(id)?.remove();
		}
		// 足りない host の生成 + mount
		for (const [id, session] of want) {
			if (dynDisposers.has(id)) continue;
			const host = document.createElement("div");
			host.id = id;
			host.className = "chat-session-host";
			host.style.display = "none";
			deps.container.appendChild(host);
			dynDisposers.set(id, deps.mountChat(host, lane, session));
		}
	};

	const visibleOf = (scope: string, refs: PaneRef[]): string[] => {
		const resolved = layoutEngine.resolved(scope);
		return refs
			.filter((v) => {
				const r = resolved[v.id];
				return !!r && r.rect.w > 0 && r.rect.h > 0;
			})
			.map((v) => v.id);
	};

	const render = (): void => {
		if (!activeLane) return;
		const lane = activeLane;
		const refs = refsOf(lane);
		syncDynHosts(lane, refs);
		const scope = laneScope(lane);
		const resolved = layoutEngine.resolved(scope);
		const visible = visibleOf(scope, refs);
		// focus が畳まれた Pane を指していたら残った先頭へ（focus を失わせない）
		const stored = focusById.get(lane);
		const focused = stored && visible.includes(stored) ? stored : (visible[0] ?? null);

		for (const p of refs) {
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
		renderChips(lane, refs, visible);
	};

	// タブエリアは **全 Pane のスイッチャー**（旧 PaneShell.render と同じ設計 — 畳んだもの
	// だけ並べると「並んでいる Pane を畳む」入口が UI から消える）。render はべき等:
	// 状態から chip を作り直すだけで差分を追わない（数枚前提、entry 側の MutationObserver
	// が「+ New」を毎回付け直す規約も従来どおり）
	const renderChips = (
		lane: string,
		refs: readonly PaneRef[],
		visible: readonly string[],
	): void => {
		deps.tabs.replaceChildren();
		let hiddenCount = 0;
		for (const p of refs) {
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

	// session 一覧（SP truth の鏡、chatview.installChatView が dispatch）→ pane の顔ぶれを同期。
	// doc 46 §1.5 の実装点: session が増減すると pane / chip / layout 列が追従する。
	document.addEventListener("vp:echoes-sessions", (e) => {
		const d = (
			e as CustomEvent<{
				lane: string;
				sessions?: { key: number; stand: string }[];
			}>
		).detail;
		if (!d?.lane) return;
		const refs = lanePaneRefs(d.sessions ?? []);
		refsByLane.set(d.lane, refs);
		if (d.lane !== activeLane) return; // 非表示 lane は refs だけ更新（DOM は表示時に作る）
		const scope = ensure(d.lane);
		layoutEngine.update(scope, (l) =>
			syncPaneColumns(
				l,
				refs.map((v) => v.id),
			),
		);
		// 構造の同期は「人の配置」でも「AI の提案」でもないデータ追従 = author は 'scene'
		layoutEngine.settle(scope, "scene");
		// 保留中の showOnly を消費する（boot 窓の救済）。保留先が「もう存在しない session の
		// host」なら、意図（= focused の chat pane を見せる）に読み替えて現 focused に貼る
		//（applyConsoleMode 時点の focusedOf は一覧未着で 1 に化けている事があるため）。
		if (pendingShowOnly !== null) {
			let target = pendingShowOnly;
			if (sessionOfHostId(target) !== null && !refs.some((v) => v.id === target)) {
				target = chatHostId(focusedOf(d.lane));
			}
			if (refs.some((v) => v.id === target)) {
				pendingShowOnly = null;
				layoutEngine.update(scope, (l) => solo(l, target));
				layoutEngine.settle(scope, "human");
				focusById.set(d.lane, target);
			}
		}
		render();
	});

	return {
		setActiveLane(lane) {
			if (activeLane === lane) return;
			// 前 lane の動的 host は lane ごと破棄（chat の再 render は安価。xterm と違い
			// DOM 保持の必要が無く、保持すると全 lane × 全 session の host が DOM に堆積する）
			for (const [id, dispose] of dynDisposers) {
				dispose();
				deps.hostOf(id)?.remove();
			}
			dynDisposers.clear();
			pendingShowOnly = null; // 保留は旧 lane の意図 — 新 lane は applyConsoleMode が貼り直す
			activeLane = lane;
			const scope = ensure(lane);
			// 既知の refs（過去に受けた session 一覧）があれば layout 列も先に同期しておく
			layoutEngine.update(scope, (l) =>
				syncPaneColumns(
					l,
					refsOf(lane).map((v) => v.id),
				),
			);
			render();
		},
		showOnly(paneId) {
			if (!activeLane) return;
			const scope = ensure(activeLane);
			if (!paneExists(scope, paneId)) {
				// まだ生えていない pane（boot 窓）— session 一覧の到着時に貼る
				pendingShowOnly = paneId;
				return;
			}
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
			if (!paneExists(scope, paneId)) return; // 消えた session の host への click 残留
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
