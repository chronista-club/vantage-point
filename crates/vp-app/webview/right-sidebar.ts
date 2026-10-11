/**
 * R sidebar — edge rail の**伸びた形**（doc 82。sidebar view modes 2026-08-01 の debug log
 * viewer から作り直し）。
 *
 * `Cmd+]`（directive `]`、発火は sidebar bundle の handlers.ts → 共有 bus
 * `vp:right-sidebar-toggle`）で rail ⇄ R sidebar を行き来する。左 sidebar のフル ⇄ スリムと
 * 対称に、**同じ住人の見え方の切替**: rail の住人（+ New / しまった pane / board 取っ手）は
 * 開いている間 R sidebar の Lane 節へ**付け替わり**、label 付きで並ぶ（閉じれば rail へ戻る）。
 * 住人を 2 つ描かないので、状態（menu / badge / 開閉）は 1 つのまま。
 *
 * ## 中身 = 縦の accordion、起点は 3 箇所（doc 82 §規約）
 *
 * 縦の帯の要素は上詰めで積まない — **上 / 中央 / 下の 3 起点**があり、各節はどれかに属する:
 *
 * | 起点 | 節 | 既定 |
 * |---|---|---|
 * | 上 | Lane（rail の住人） | 開 |
 * | 中央 | Pull Requests（表示中 lane の repo の open PR） | 開 |
 * | 下 | Debug Log（app / daemon の log tail） | 閉 |
 *
 * 閉じた節は見出し 1 行だけがその起点に残る。複数同時に開ける。開閉は localStorage に保持。
 *
 * ## 層の分離
 *
 * - **data**: `open` / `source` / `sections` / `lane`（この module が SSOT）。行 buffer は DOM（#rsb-log）
 * - **calculations**: `trimToTail` / `parseSections`（純関数、vitest 済）+ pr-list.ts
 * - **actions**: `installRightSidebar`（toggle 購読 + 節の開閉 + 住人の付け替え + tail / PR の購読）
 *
 * ## 購読 model（消費者主導 — 見えている物だけ取る）
 *
 * - Debug Log: **R が開いていて Log 節も開いている間だけ** `debuglog:watch`、それ以外は
 *   `debuglog:unwatch`。source 切替も watch の送り直しで、Rust 側は**最後の watch が勝つ**単一
 *   tail。応答は毎回 `reset=true` の backlog から始まるので取りこぼしという概念が無い
 * - PR: **R が開いていて PR 節も開いている間だけ**、即 1 回 + 60 秒ごとに `lane:prs_fetch`
 *   （pr-list.ts の `createPrPoller`）。閉じたら止める。rail の badge は最後に取った値
 */

import {
	createPrPoller,
	type LanePrs,
	PR_POLL_MS,
} from "./pr-list";
import { mountPrSection } from "./right-sidebar-prs";

export type DebugLogSource = "app" | "daemon";

/** 表示を保つ最大行数。超えたら古い側から捨てる（tail の意味論）。 */
const MAX_LINES = 2000;
/** trim の hysteresis: 毎 chunk で split しないための余裕。 */
const TRIM_AT = MAX_LINES + 500;

/** 全文 text から末尾 `max` 行だけ残す（純関数）。 */
export function trimToTail(text: string, max: number): string {
	const lines = text.split("\n");
	if (lines.length <= max) return text;
	return lines.slice(lines.length - max).join("\n");
}

/** accordion の節。並び（上 / 中央 / 下）は DOM の slot が持つ。 */
export type SectionId = "lane" | "prs" | "log";
export type SectionState = Record<SectionId, boolean>;

/** 既定: Lane 開 / PR 開 / Debug Log 閉（mako 2026-10-11）。 */
export const DEFAULT_SECTIONS: SectionState = { lane: true, prs: true, log: false };
const SECTIONS_KEY = "vp.rsb.sections";

/** 保存値（JSON 文字列 or null）を読む。壊れた値 / 欠けた key は既定に倒す（純関数）。 */
export function parseSections(raw: string | null): SectionState {
	const out = { ...DEFAULT_SECTIONS };
	if (!raw) return out;
	try {
		const v = JSON.parse(raw) as Record<string, unknown>;
		for (const k of Object.keys(out) as SectionId[]) {
			if (typeof v?.[k] === "boolean") out[k] = v[k] as boolean;
		}
	} catch {
		// 壊れていれば既定のまま
	}
	return out;
}

export interface RightSidebarDeps {
	/** #rsb-log（行の表示先 + scroll container）。 */
	log: HTMLElement;
	/** .rsb-tab 群（data-src="app" | "daemon"）。 */
	tabs: HTMLElement[];
	/** .rsb-section 群（data-section="lane" | "prs" | "log"）。 */
	sections: HTMLElement[];
	/** Lane 節の中身（住人の付け替え先）。 */
	laneBody: HTMLElement;
	/** rail の住人（+ New host / しまった pane host / board 取っ手）。並びは rail の上から。 */
	residents: HTMLElement[];
	/** PR 節の中身（Solid の mount 先）。 */
	prsBody: HTMLElement;
	/** PR 節の見出しの件数表示。 */
	prsCount: HTMLElement | null;
	/** PR 節の見出しの ↻（手動の取り直し）。 */
	prsRefresh: HTMLElement | null;
}

export interface RightSidebarApi {
	/** push envelope `debuglog:lines` の受け口（dispatch.ts から接続）。 */
	handleLines(source: string, reset: boolean, lines: string[]): void;
	/** push envelope `lane:prs` の受け口（dispatch.ts から接続）。 */
	handlePrs(lane: string, payload: unknown): void;
	/** 表示 lane の追従（entry.tsx の applyActivePane から。null = lane 不在）。 */
	setLane(addr: string | null): void;
}

export function installRightSidebar(deps: RightSidebarDeps): RightSidebarApi {
	let open = false;
	let source: DebugLogSource = "app";
	let lane: string | null = null;
	let logWatching = false;
	const sections = loadSections();

	const sendIpc = (payload: Record<string, unknown>): void => {
		const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } })
			.ipc;
		ipc?.postMessage(JSON.stringify(payload));
	};

	// ---- PR 節 ----------------------------------------------------------------
	const prs = mountPrSection(deps.prsBody);
	const poller = createPrPoller({
		fetch: (l) => sendIpc({ t: "lane:prs_fetch", lane: l }),
		intervalMs: PR_POLL_MS,
		setInterval: (fn, ms) => window.setInterval(fn, ms),
		clearInterval: (id) => window.clearInterval(id),
	});
	/** gh 不在と分かった lane では PR 節ごと出さない（分かるまでは出す）。 */
	const prsAvailable = (): boolean => prs.current()?.available !== false;

	// ---- 住人の付け替え（rail ⇄ Lane 節）---------------------------------------
	// 家（rail 内の親 + 次の兄弟）を install 時に覚える。戻す時はそこへ insertBefore —
	// 兄弟（#rail-pr-host 等）は動かない静的 DOM なので位置が崩れない。
	const homes = deps.residents.map((el) => ({
		el,
		parent: el.parentElement,
		next: el.nextSibling,
	}));
	const placeResidents = (): void => {
		for (const h of homes) {
			if (open) deps.laneBody.appendChild(h.el);
			else h.parent?.insertBefore(h.el, h.next);
		}
	};

	// ---- 反映 -----------------------------------------------------------------
	const sectionEl = (id: SectionId) =>
		deps.sections.find((s) => s.dataset.section === id);

	/** 節の開閉 / 出し入れ + 各購読の開始・停止を、今の状態から 1 回で決め直す。 */
	const apply = (): void => {
		for (const s of deps.sections) {
			const id = s.dataset.section as SectionId;
			s.classList.toggle("open", sections[id]);
			s.querySelector(".rsb-sec-head")?.setAttribute(
				"aria-expanded",
				String(sections[id]),
			);
		}
		// lane 級の 2 節は lane が無ければ出さない（rail が帯ごと消えるのと対）
		sectionEl("lane")?.classList.toggle("rsb-absent", !lane);
		sectionEl("prs")?.classList.toggle("rsb-absent", !lane || !prsAvailable());

		const wantLog = open && sections.log;
		if (wantLog !== logWatching) {
			logWatching = wantLog;
			sendIpc(wantLog ? { t: "debuglog:watch", source } : { t: "debuglog:unwatch" });
		}
		poller.update({ active: open && sections.prs && prsAvailable(), lane });
	};

	/** R の開閉: body class + 住人の付け替え + shell layout への通知 + 購読。 */
	const applyOpen = (): void => {
		document.body.classList.toggle("rsb-open", open);
		placeResidents();
		// shell layout（shell-layout.ts）へ「開閉が変わった」を伝える。あちらが取っ手の
		// 出し入れと永続化を持つ。⚠️ **この dispatch が無いと取っ手が出ない**（開いたことを
		// 知らないので display:none のまま = R を掴めない）。
		document.dispatchEvent(
			new CustomEvent("vp:right-sidebar-state", { detail: { open } }),
		);
		apply();
	};

	const toggleSection = (id: SectionId, value = !sections[id]): void => {
		if (sections[id] === value) return;
		sections[id] = value;
		saveSections(sections);
		apply();
	};

	// `]` directive（sidebar bundle の handlers.ts）からの依頼。統合 WebView の
	// 同一 document なので共有 bus（doc 47 §6）がそのまま届く。
	document.addEventListener("vp:right-sidebar-toggle", () => {
		open = !open;
		applyOpen();
	});

	// rail の PR ボタン（EdgeRail.tsx）: R を開いて PR 節へ。節が閉じていれば開く。
	document.addEventListener("vp:right-sidebar-reveal", (e) => {
		const id = (e as CustomEvent<{ section?: SectionId }>).detail?.section;
		if (!id) return;
		if (!open) {
			open = true;
			applyOpen();
		}
		toggleSection(id, true);
		sectionEl(id)?.scrollIntoView({ block: "nearest" });
	});

	// 復元（shell-layout.ts の `vp:shell-restore`）。boot で 1 回だけ来る。
	// ⚠️ 既に同じ状態なら `applyOpen` を呼ばない — 呼ぶと購読を無駄撃ちする。
	document.addEventListener("vp:shell-restore", (e) => {
		const d = (e as CustomEvent<{ rightOpen?: boolean }>).detail;
		if (typeof d?.rightOpen !== "boolean" || d.rightOpen === open) return;
		open = d.rightOpen;
		applyOpen();
	});

	for (const s of deps.sections) {
		const id = s.dataset.section as SectionId;
		s.querySelector(".rsb-sec-head")?.addEventListener("click", (ev) => {
			// 見出しの中の操作（tab / ↻）は開閉にしない
			if ((ev.target as HTMLElement).closest(".rsb-sec-tools")) return;
			toggleSection(id);
		});
	}

	deps.prsRefresh?.addEventListener("click", () => poller.refresh());

	for (const tab of deps.tabs) {
		tab.addEventListener("click", () => {
			const next = tab.dataset.src as DebugLogSource;
			if (next === source) return;
			source = next;
			for (const t of deps.tabs) t.classList.toggle("active", t === tab);
			// watch の送り直し（Rust は最後の watch が勝つ）。応答の reset=true が表示を差し替える。
			// 見ていない間は撃たない（次に見えた時の watch が新 source で始まる）。
			if (logWatching) sendIpc({ t: "debuglog:watch", source });
		});
	}

	/** rail の badge へ件数を伝える（EdgeRail.tsx が lane ごとに覚える）。 */
	const publishCount = (l: string, d: LanePrs): void => {
		document.dispatchEvent(
			new CustomEvent("vp:lane-prs-count", {
				detail: { lane: l, count: d.prs.length, available: d.available },
			}),
		);
	};

	apply();

	return {
		handleLines(src, reset, lines) {
			// source 切替直後に届く旧 source の残 chunk は捨てる（表示の混線防止）。
			if (src !== source) return;
			// tail -f の流儀: 末尾に居た時だけ追従する（遡って読んでいる最中は動かさない）。
			const el = deps.log;
			const atBottom =
				reset || el.scrollHeight - el.scrollTop - el.clientHeight < 8;
			if (reset) el.textContent = "";
			el.append(`${lines.join("\n")}\n`);
			if (el.textContent && el.textContent.split("\n").length > TRIM_AT) {
				el.textContent = trimToTail(el.textContent, MAX_LINES);
			}
			if (atBottom) el.scrollTop = el.scrollHeight;
		},
		handlePrs(l, payload) {
			const d = prs.handle(l, payload);
			publishCount(l, d);
			if (l !== lane) return;
			if (deps.prsCount) deps.prsCount.textContent = d.error ? "!" : String(d.prs.length);
			// gh 不在と分かった → 節ごと隠し、poll も止める（apply が両方決め直す）
			apply();
		},
		setLane(addr) {
			if (addr === lane) return;
			lane = addr;
			prs.setLane(addr);
			const d = prs.current();
			if (deps.prsCount) deps.prsCount.textContent = d ? String(d.prs.length) : "";
			apply();
		},
	};
}

function loadSections(): SectionState {
	try {
		return parseSections(globalThis.localStorage?.getItem(SECTIONS_KEY) ?? null);
	} catch {
		return { ...DEFAULT_SECTIONS };
	}
}

function saveSections(s: SectionState): void {
	try {
		globalThis.localStorage?.setItem(SECTIONS_KEY, JSON.stringify(s));
	} catch {
		// 保存できない環境では「開閉が次回戻る」だけ — 表示は壊さない
	}
}
