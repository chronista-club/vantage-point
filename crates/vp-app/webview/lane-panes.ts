/**
 * lane-panes — lane 内 tiling を creo-ui-layout の場に写す（doc 49 LE-P4 PR2 → doc 51 §1 A1）。
 *
 * 旧 pane-shell.ts（doc 46 P1 の PaneLayout / LaneLayouts / PaneShell）の後継。
 * lane ごとの独立状態は engine の **scope**（`lane:<addr>`）がそのまま担う — 旧
 * LaneLayouts（lane → PaneLayout の Map）は scope key に畳まれて消える。
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: `TERM_PANE_REF` + session 由来の動的 refs（`lanePaneRefs` — doc 50 P1 で動的化）
 * - **calculations**: `lanePaneRefs` / `syncPaneColumns` / `newPaneChoices` — 純関数（vitest で固定）
 * - **actions**: `installLanePanes` — engine 購読 + DOM への反映（display / rect / class）
 *
 * ## 投影規則
 *
 * - 非表示 pane は **display:none**（旧 `.pane-minimized` と同じ畳み方）。lane-host の
 *   xterm は今日までこの隠し方で運用されてきた実績に合わせ、app-panes（全面透明方式）
 *   とは意図的に流儀を変えない
 * - keyboard focus は protocol の関心外（LE-20）— focus ring は本 module の状態で持つ
 * - **表示は既定 tiling**（doc 51 §1、mako 2026-07-24 — 同時注視）。session pane は既定で
 *   並び、新しい pane は可視 raw 平均の share で入場する（creo-ui-layout `admit` と同じ
 *   入場規則）。「畳んで取っておく」中間状態と下端の帯（pane chip）は退役 —
 *   旧「1 枚ずつ = showOnly」（mako 2026-07-21）は doc 47 §1 決着までの暫定だった
 *
 * ## pane の顔ぶれ（roster）と Act の関係（doc 51 §2）
 *
 * 同じ session の term / chat 同時 2 枚は**原理的に不可**（1 会話 1 プロセス。Act I = TUI
 * 常駐、Act II = headless stream-json、切替 = resume handoff）。pre-A6（xterm は lane に
 * 1 枚）では term になれるのは root session だけなので、roster は console_mode で決まる:
 *
 * - mode == tui:  Console（= root session の Act I 面）+ 非 root session の chat pane
 * - mode == chat: 全 session の chat pane（root も chat。xterm は表示しない — set_mode chat
 *   後の PTY は抜け殻で、見せると「死んだ console」が台に並ぶ）
 */

import {
	type Layout,
	resolve,
	setShare,
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

/** roster の入力になる session の最小形（'vp:echoes-sessions' bus の 1 要素）。 */
export type PaneSession = { key: number; stand: string; root?: boolean };

/** Act I（xterm）の代表 pane。World A の xterm re-key（doc 50 P3 = A6）まで lane に 1 枚 */
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

/** lane の pane の顔ぶれ（純関数）。mode と root で term / chat を排他にする（冒頭 doc の
 *  roster 規則 — 同じ session を 2 枚にしない）。 */
export function lanePaneRefs(
	sessions: readonly PaneSession[],
	mode: "tui" | "chat",
): PaneRef[] {
	const chatPane = (v: PaneSession): PaneRef => ({
		id: chatHostId(v.key),
		label: `${sessionChipPrefix(v.stand)}#${v.key}`,
		session: v.key,
	});
	if (mode === "chat") return sessions.map(chatPane);
	return [TERM_PANE_REF, ...sessions.filter((v) => !v.root).map(chatPane)];
}

/** 入場 share = 可視 pane の raw 平均（creo-ui-layout `admit` の既定と同じ規則。
 *  可視 pane が居なければ 1）。tiling 既定の実体 — 新 pane は畳まれず、並んで生まれる。 */
export function enterShare(layout: Layout): number {
	const vis = Object.values(layout.attention).filter((v) => v > 0);
	if (vis.length === 0) return 1;
	return vis.reduce((a, b) => a + b, 0) / vis.length;
}

/** layout の列を refs に同期する（純関数）。
 *  - refs に居るが structure に無い pane: 右端に列 append、**可視で入場**（enterShare —
 *    tiling 既定。旧「attention 0 = chip に生えるだけ」は帯とともに退役）
 *  - structure に居るが refs から消えた pane（closed session / roster 除外）: 列から除去
 *  往復（sync → sync）は不動点 = 冪等 */
export function syncPaneColumns(layout: Layout, ids: readonly string[]): Layout {
	const want = new Set(ids);
	const columns = layout.structure.columns
		.map((c) => ({ panes: c.panes.filter((v) => want.has(v)) }))
		.filter((c) => c.panes.length > 0);
	const present = new Set(columns.flatMap((c) => c.panes));
	const enter = enterShare(layout);
	const attention: Record<string, number> = {};
	for (const id of ids) {
		if (!present.has(id)) columns.push({ panes: [id] });
		attention[id] = layout.attention[id] ?? enter;
	}
	return { structure: { columns }, attention };
}

/** 要件 3: フォーカスの視認 ring（CSS は main_area.rs `#lane-panes > .pane-focused`） */
export const CLASS_FOCUSED = "pane-focused";

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

/** 消えていた Pane に focus を当て直す時の share（2 枚構成なら等分に戻る） */
const RESTORE_SHARE = 0.5;

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
	/** focus を当てる。消えていた Pane を指したら復元も行う（旧 PaneLayout.focus）。
	 *  まだ生えていない pane（boot 窓）は保留し、session 一覧の到着時に当て直す */
	focusPane(paneId: string): void;
}

export interface LanePanesDeps {
	/** Pane host 要素の解決（id → 要素）。テストから差し替え可能にするため関数で受ける */
	hostOf: (id: string) => HTMLElement | null;
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
	/** lane → session 一覧（'vp:echoes-sessions' の鏡。roster は mode と合成して導出） */
	const sessionsByLane = new Map<string, PaneSession[]>();
	/** lane → console_mode（'vp:console-mode' の鏡。未着 lane は tui = boot 既定） */
	const modeByLane = new Map<string, "tui" | "chat">();
	/** 表示中 lane の動的 host の dispose（host id → SessionChatView の unmount） */
	const dynDisposers = new Map<string, () => void>();
	/** focusPane が「まだ生えていない pane」を指した時の保留先（boot 窓: applyConsoleMode は
	 *  session 一覧の到着前に走る）。到着時に 1 回だけ当てて消費する。 */
	let pendingFocus: string | null = null;

	/** pane が layout の構造に居るか（focus の前提確認）。 */
	const paneExists = (scope: string, id: string): boolean =>
		layoutEngine.current(scope).structure.columns.some((c) => c.panes.includes(id));

	const modeOf = (lane: string): "tui" | "chat" => modeByLane.get(lane) ?? "tui";

	const refsOf = (lane: string): PaneRef[] =>
		lanePaneRefs(sessionsByLane.get(lane) ?? [], modeOf(lane));

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

	/** roster を layout 列へ写す（sessions / mode どちらの変化でも通る一本道）。
	 *  構造の同期は「人の配置」でも「AI の提案」でもないデータ追従 = author は 'scene' */
	const syncRoster = (lane: string): void => {
		const scope = ensure(lane);
		layoutEngine.update(scope, (l) =>
			syncPaneColumns(
				l,
				refsOf(lane).map((v) => v.id),
			),
		);
		layoutEngine.settle(scope, "scene");
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
		// focus が消えた Pane を指していたら残った先頭へ（focus を失わせない）
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
		// ⚠️ roster **外**の常設 host（chat mode の #lane-host）を明示的に隠す。上のループは
		// roster しか触らないため、mode 切替で roster から外れた lane-host は「見えないのに
		// display のまま」chat host の下に残る。DOM から消してはいけない（World A の xterm を
		// 保持する境界規律、doc 33 §8）が、隠さないと xterm viewport（overflow-y:scroll +
		// 巨大 scrollback）が同じ矩形に残り、WebKit の async-scroll hit-test が**奥の見えない
		// viewport に wheel を奪う** — chat が「wheel 不動 / PgDn は動く」になる
		//（2026-07-24 実機再現。A1 帯撤去の regression）。
		if (!refs.some((p) => p.id === TERM_PANE_REF.id)) {
			const el = deps.hostOf(TERM_PANE_REF.id);
			if (el) {
				el.style.display = "none";
				el.classList.toggle(CLASS_FOCUSED, false);
			}
		}
	};

	// 表示 lane の scope が外（AI / MCP / fleet / layout_set）から動いた時も追従する
	layoutEngine.subscribe((scope) => {
		if (activeLane && scope === laneScope(activeLane)) render();
	});

	// session 一覧（SP truth の鏡、chatview.installChatView が dispatch）→ pane の顔ぶれを同期。
	// doc 46 §1.5 の実装点: session が増減すると pane / layout 列が追従する。
	document.addEventListener("vp:echoes-sessions", (e) => {
		const d = (
			e as CustomEvent<{ lane: string; sessions?: PaneSession[] }>
		).detail;
		if (!d?.lane) return;
		sessionsByLane.set(d.lane, d.sessions ?? []);
		if (d.lane !== activeLane) return; // 非表示 lane は一覧だけ更新（DOM は表示時に作る）
		syncRoster(d.lane);
		// 保留中の focus を消費する（boot 窓の救済）。保留先が「もう存在しない session の
		// host」なら、意図（= focused の chat pane を見せる）に読み替えて現 focused に当てる
		//（applyConsoleMode 時点の focusedOf は一覧未着で 1 に化けている事があるため）。
		if (pendingFocus !== null) {
			let target = pendingFocus;
			const refs = refsOf(d.lane);
			if (sessionOfHostId(target) !== null && !refs.some((v) => v.id === target)) {
				target = chatHostId(focusedOf(d.lane));
			}
			if (refs.some((v) => v.id === target)) {
				pendingFocus = null;
				focusById.set(d.lane, target);
			}
		}
		render();
	});

	// console_mode（Act）→ roster を同期。mode == tui は root の chat pane を持たず、
	// mode == chat は Console を持たない（冒頭 doc の roster 規則 — 排他は roster で保証）。
	document.addEventListener("vp:console-mode", (e) => {
		const d = (e as CustomEvent<{ lane: string; mode: "tui" | "chat" }>).detail;
		if (!d?.lane) return;
		modeByLane.set(d.lane, d.mode);
		if (d.lane !== activeLane) return;
		syncRoster(d.lane);
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
			pendingFocus = null; // 保留は旧 lane の意図 — 新 lane は applyConsoleMode が当て直す
			activeLane = lane;
			syncRoster(lane);
			render();
		},
		focusPane(paneId) {
			if (!activeLane) return;
			const scope = ensure(activeLane);
			if (!paneExists(scope, paneId)) {
				// まだ生えていない pane（boot 窓）— session 一覧の到着時に当てる
				pendingFocus = paneId;
				return;
			}
			if ((layoutEngine.current(scope).attention[paneId] ?? 0) <= 0) {
				// 消えていた pane を指したら復元も行う（旧 PaneLayout.focus と同じ）
				layoutEngine.update(scope, (l) => setShare(l, paneId, RESTORE_SHARE));
				layoutEngine.settle(scope, "human");
			}
			focusById.set(activeLane, paneId);
			render();
		},
	};
}
