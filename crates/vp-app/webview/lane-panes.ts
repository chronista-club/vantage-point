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
 * ## pane の顔ぶれ（roster）と Act の関係（doc 50 §4.6 A6）
 *
 * **roster = session 一覧 × 各 session の act**。1 session = 1 Pane（doc 46 §1.5）で、
 * act がその Pane の kind（term = Act I の PTY 面 / chat = Act II の構造化面）を決める。
 *
 * 同じ session の term / chat 同時 2 枚は**原理的に不可**（1 往復路 = Active な化身 高々 1。
 * Act I = TUI 常駐、Act II = headless stream-json、切替 = resume handoff）。roster が
 * session ごとに 1 枚しか作らないので、この不変条件は構造で保たれる。
 *
 * > pre-A6 は xterm が lane に 1 枚しか無く、term になれるのは root だけだった。そのため
 * > roster は lane 単位の console_mode で決まっていた（mode==tui なら Console 1 枚 + 非 root の
 * > chat、mode==chat なら全部 chat）。A6 で xterm が (lane, session) へ re-key され、この
 * > 制約と lane 単位 mode の概念は消えた。
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
	/** session pane なら session key（doc 46 §1.5 session ↔ Pane 1:1）。board pane は無し。
	 *  ⚠️ **kind の代用にしないこと** — A6 以前は「session を持つ = chat pane」が成り立ち、
	 *  `session !== undefined` が判別に使えたが、A6 で term pane も session を持つように
	 *  なったのでこの対応は壊れた。種類は `kind` を見る。 */
	session?: number;
	/** この pane の種類（doc 50 §4.6 A6）。host を誰が作るか / 何を mount するかが変わる:
	 *  - `term`: host は World A（xterm）が所有。SolidJS は名札だけを差し込む
	 *  - `chat`: host も中身も SolidJS が作る
	 *  - `board`: lane に 1 枚の静的 host（session と直交） */
	kind: "term" | "chat" | "board";
};

/** roster の入力になる session の最小形（'vp:echoes-sessions' bus の 1 要素）。
 *  doc 50 §4.6 A6: `act` がこの session の見え方（term / chat）を決める **唯一の入力**。
 *  欠落（旧 SP）は "tui" に倒す（従来の既定 = Act I）。 */
export type PaneSession = {
	key: number;
	stand: string;
	root?: boolean;
	act?: "tui" | "chat";
};

/** term pane の host DOM id（World A の `ensureTermHost` と対。root は静的 #lane-host）。 */
export function termHostId(session: number, isRoot: boolean): string {
	return isRoot ? TERM_PANE_REF.id : `term-session-${session}`;
}

/** root session の term pane（= 静的 host `#lane-host`）。非 root の term は
 *  `termHostId` が返す動的 host（`#term-session-<n>`）に載る（doc 50 §4.6 A6）。
 *  root だけ静的なのは、layout 永続 / boot 既定の id を変えないため。 */
export const TERM_PANE_REF: PaneRef = {
	id: "lane-host",
	label: "Console",
	kind: "term",
};

/** board（PP）の pane。lane-host と同じく **lane に 1 枚の静的 host**（board は lane-scoped で
 *  1 lane 1 枚、表示 lane は常に 1 つ = xterm と同じ性質。動的生成は不要、位置決めだけ動く）。
 *  roster に載るのは board が非空のときだけ（doc 52 §10 wave 0 — board 非空で自動）。 */
export const BOARD_PANE_REF: PaneRef = {
	id: "lane-board",
	label: "Paisley Park",
	kind: "board",
};

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

/** lane address（`<project>/root` | `<project>/performer/<name>`）→ board の flat lane key
 *  （root/lead = `conductor` / performer = `<name>`）。board-handler は BoardUpdated.lane を
 *  flat name（None→'conductor'）で扱うが、lane-panes は address で lane を追う。'vp:board-presence'
 *  は board-handler の flat key で飛んでくるので、突合のためここで address → flat を写す。
 *  entry.tsx の laneNameFromAddress と同型（あちらは null=conductor、こちらは 'conductor' 文字列）。 */
export function boardLaneKeyOf(address: string): string {
	if (address.endsWith("/root") || address.endsWith("/lead")) return "conductor";
	const m = address.match(/\/(?:performer|wing)\/(.+)$/);
	return m ? (m[1] ?? "conductor") : "conductor";
}

/** lane の pane の顔ぶれ（純関数、doc 50 §4.6 A6）。
 *
 *  **各 session の act がその session の Pane kind を決める** — 1 session = 1 Pane
 *  （doc 46 §1.5）で、term / chat は同じ往復路の見え方違い。同じ session が 2 枚になることは
 *  原理的に無い（1 往復路 = Active な化身 高々 1）ので、旧実装の「mode で排他にする」規則は
 *  不要になった（あれは「term になれるのは root だけ」という物理制約の投影だった）。
 *
 *  board pane は engine session と直交する lane-level の面なので、act を問わず board が
 *  非空なら**末尾に**足す（doc 52 §2 — board は掲示板/計器盤/中継台/対話面の役割を持つ
 *  lane の道具で、どの Act で作業していても同じ台に並ぶ）。 */
export function lanePaneRefs(
	sessions: readonly PaneSession[],
	boardPresent = false,
): PaneRef[] {
	const sessionPanes = sessions.map((v): PaneRef => {
		const label = `${sessionChipPrefix(v.stand)}#${v.key}`;
		return v.act === "chat"
			? { id: chatHostId(v.key), label, session: v.key, kind: "chat" }
			: { id: termHostId(v.key, !!v.root), label, session: v.key, kind: "term" };
	});
	return boardPresent ? [...sessionPanes, BOARD_PANE_REF] : sessionPanes;
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
	/** term pane の名札を host に差し込む（doc 50 §4.6 A6 ②）。返り値 = dispose。
	 *
	 *  host（`#lane-host` / `#term-session-<n>`）と中の xterm は World A の持ち物なので、
	 *  **名札 DOM を足すだけ**にとどめる（xterm container には触れない — doc 33 §8）。
	 *  xterm を名札の高さぶん下げるのは World A 側の CSS（`.has-term-plate`）。 */
	mountTermPlate: (host: HTMLElement, lane: string, session: number) => () => void;
}

/**
 * lane panes を DOM に配線する（actions）。engine の notify（将来の AI / MCP 駆動も
 * 含む）で表示 lane の scope が動けば再描画される。
 */
export function installLanePanes(deps: LanePanesDeps): LanePanesController {
	let activeLane: string | null = null;
	/** lane → focus を持つ pane id（LE-20: focus は場の外 = module 状態） */
	const focusById = new Map<string, string>();
	/** lane → session 一覧（'vp:echoes-sessions' の鏡。roster は各 session の act から導出）。
	 *  doc 50 §4.6 A6: lane 単位 console_mode の鏡（旧 `modeByLane`）は退役 — 見え方は
	 *  session の属性になったので、lane 単位の mode を持つ理由が無くなった。 */
	const sessionsByLane = new Map<string, PaneSession[]>();
	/** board flat key（'conductor' / performer 名）→ board が非空か（'vp:board-presence' の鏡。
	 *  board-handler は flat key で presence を飛ばすので、address 空間の他の Map とは別 key 系。
	 *  lookup は boardLaneKeyOf(address) で写して引く）。 */
	const boardByLane = new Map<string, boolean>();
	/** 表示中 lane の動的 host の dispose（host id → SessionChatView の unmount） */
	const dynDisposers = new Map<string, () => void>();
	/** focusPane が「まだ生えていない pane」を指した時の保留先（boot 窓: applyConsoleMode は
	 *  session 一覧の到着前に走る）。到着時に 1 回だけ当てて消費する。 */
	let pendingFocus: string | null = null;

	/** pane が layout の構造に居るか（focus の前提確認）。 */
	const paneExists = (scope: string, id: string): boolean =>
		layoutEngine.current(scope).structure.columns.some((c) => c.panes.includes(id));

	const refsOf = (lane: string): PaneRef[] =>
		lanePaneRefs(
			sessionsByLane.get(lane) ?? [],
			boardByLane.get(boardLaneKeyOf(lane)) ?? false,
		);

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

	/** 表示中 lane の session pane を refs に同期する（doc 50 §4.6 A6 で term も対象に）。
	 *
	 *  kind で扱いが分かれる:
	 *  - **chat**: host も中身も SolidJS。無ければ生成 + mount、消えたら dispose + DOM 除去。
	 *    生成直後は display:none（render が可視性を決めるまで何も覆わない — #880 の教訓）
	 *  - **term**: host は World A（xterm）の持ち物なので**作らない・消さない**。名札だけを
	 *    host に差し込み、消えたら名札だけ外す（xterm 本体には触れない = doc 33 §8 の境界）
	 */
	const syncDynHosts = (lane: string, refs: PaneRef[]): void => {
		const want = new Map(
			refs
				.filter((v) => v.session !== undefined && v.kind !== "board")
				.map((v) => [v.id, v]),
		);
		// 消えた pane の後始末（chat は host ごと、term は名札だけ）
		for (const [id, dispose] of [...dynDisposers]) {
			if (want.has(id)) continue;
			dispose();
			dynDisposers.delete(id);
			// chat host は SolidJS が作ったものなので除去する。term host は World A の
			// 持ち物なので残す（dispose 側が名札 DOM だけを片付ける）。
			const host = deps.hostOf(id);
			if (host?.classList.contains("chat-session-host")) host.remove();
		}
		// 足りない pane の mount
		for (const [id, ref] of want) {
			if (dynDisposers.has(id)) continue;
			const session = ref.session as number;
			if (ref.kind === "chat") {
				const host = document.createElement("div");
				host.id = id;
				host.className = "chat-session-host";
				host.style.display = "none";
				deps.container.appendChild(host);
				dynDisposers.set(id, deps.mountChat(host, lane, session));
			} else {
				// term: host は World A が ensureLane で用意する。まだ無ければ次の同期に回す
				// （boot 窓 — session 一覧が先に届き、xterm 生成が後になることがある）。
				const host = deps.hostOf(id);
				if (!host) continue;
				dynDisposers.set(id, deps.mountTermPlate(host, lane, session));
			}
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
		// ⚠️ roster **外**の常設 host（#lane-host / #lane-board）を明示的に隠す。上のループは
		// roster しか触らないため、roster から外れた常設 host は「見えないのに display のまま」
		// 他 host の下に残る。DOM から消してはいけない（#lane-host は World A の xterm を保持する
		// 境界規律、doc 33 §8。#lane-board も静的 host を作り直さない）が、隠さないと中身の
		// viewport（xterm の overflow-y:scroll + 巨大 scrollback / board の overflow-y:auto）が
		// 同じ矩形に残り、WebKit の async-scroll hit-test が**奥の見えない viewport に wheel を
		// 奪う** — 手前の pane が「wheel 不動 / PgDn は動く」になる（2026-07-24 実機再現）。
		for (const staticId of [TERM_PANE_REF.id, BOARD_PANE_REF.id]) {
			if (refs.some((p) => p.id === staticId)) continue;
			const el = deps.hostOf(staticId);
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

	// session act（見え方）の変化 → roster を同期（doc 50 §4.6 A6、旧 'vp:console-mode' の後継）。
	// 名札の kind badge → session_set_act → SP → SessionActApplied → vpConsole.setSessionAct が
	// この bus を撃つ。当該 session の Pane kind が in-place で入れ替わる（位置は不変 —
	// syncPaneColumns が新 id を旧 id の代わりに置くのではなく、旧 id が消えて新 id が入場する。
	// 位置の連続性は §4.6 ② の狙いなので、focus を新 host に引き継いで「その場で変身」に見せる）。
	document.addEventListener("vp:session-act", (e) => {
		const d = (
			e as CustomEvent<{ lane: string; session: number; act: "tui" | "chat" }>
		).detail;
		if (!d?.lane || !d.session) return;
		const list = sessionsByLane.get(d.lane);
		const entry = list?.find((v) => v.key === d.session);
		if (entry) entry.act = d.act;
		if (d.lane !== activeLane) return;
		// 変身前に focus を持っていたなら、新しい kind の host へ移す（視線の連続性）。
		const prevId =
			d.act === "chat"
				? termHostId(d.session, !!entry?.root)
				: chatHostId(d.session);
		const nextId =
			d.act === "chat"
				? chatHostId(d.session)
				: termHostId(d.session, !!entry?.root);
		if (focusById.get(d.lane) === prevId) focusById.set(d.lane, nextId);
		syncRoster(d.lane);
		render();
	});

	// board 非空 → roster に board pane を出す（board-handler が BoardUpdated 受信で dispatch。
	// doc 52 §10 wave 0）。fresh = live 新着なら board pane に focus を寄せる（旧 maybeAutoOpenPP =
	// pp-overlay app scene の後継。「配送されたのに見えない」を防ぐ）。畳んだ pane も新着で復元される
	// のは focusPane（消えていた pane を指すと RESTORE_SHARE で戻す）が担う。
	document.addEventListener("vp:board-presence", (e) => {
		const d = (
			e as CustomEvent<{ lane: string; present: boolean; fresh?: boolean }>
		).detail;
		if (!d?.lane) return;
		// d.lane は board-handler の flat key（'conductor' / performer 名）。boardByLane も flat
		// key で持つ。active 判定は activeLane（address）を flat に写して突合する。
		boardByLane.set(d.lane, d.present);
		if (!activeLane || boardLaneKeyOf(activeLane) !== d.lane) return;
		syncRoster(activeLane);
		render();
		if (d.present && d.fresh) controller.focusPane(BOARD_PANE_REF.id);
	});

	const controller: LanePanesController = {
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
	return controller;
}
