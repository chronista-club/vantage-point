// @vitest-environment happy-dom
/**
 * setActivePane の受け口（Rust `push_active_view` → `window.setActivePane`）— kind の解決を
 * pane kind registry（app-pane-registry.ts、doc 87 §8.2）1 枚に寄せたことの検証。
 *
 * ## ⚠️ ここで守るのは「2 つの受け口が同じ kind を同じ pane に解決する」こと
 *
 * 受け口は 2 段ある: ①DOM の可視性（`applyPaneSwitch` — どの `.pane` に `.active` を付けるか）と
 * ②配置（entry.tsx の `applyActivePane` — どの場へ行くか。分岐は `activePaneRouteOf`）。
 * 旧形は両者が kind の表を別々に持ち、旧名 `terminal` を①は empty に、②は lane に解決していた
 * （①が `pane-empty` を active にしたまま、②が lane の場を開く）。表を 1 枚にしたので食い違えない。
 *
 * 静的 DOM は main_area.rs の `<body>` をファイルからそのまま読む。
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it } from "vitest";
import { activePaneRouteOf, applyPaneSwitch } from "./active-pane";
import { APP_PANE_REGISTRY } from "./app-pane-registry";

// ⚠️ happy-dom 環境では global の URL が happy-dom 製になり readFileSync が受け付けないので、
// 文字列の import.meta.url から path を組む。
const here = dirname(fileURLToPath(import.meta.url));
const rs = readFileSync(join(here, "../src/webview/main_area.rs"), "utf8");
/** iframe は同じ id の div に置き換える — happy-dom は iframe を挿すと src を読み込みに行く（applyPaneSwitch は
 *  src 属性を書くだけなので、要素の種類は問わない）。 */
const body = rs
	.match(/<body>([\s\S]*?)<\/body>/)![1]
	.replace(/<script[\s\S]*?<\/script>/g, "")
	.replace(/<iframe\b([^>]*)><\/iframe>/g, "<div$1></div>");

const LANE = "vp/lane/lead";

describe("activePaneRouteOf — setActivePane の行き先（純関数）", () => {
	it("info 無し / kind 無し / empty は「何も選んでいない」", () => {
		expect(activePaneRouteOf(null)).toEqual({ to: "empty" });
		expect(activePaneRouteOf({ kind: null })).toEqual({ to: "empty" });
		expect(activePaneRouteOf({ kind: "empty" })).toEqual({ to: "empty" });
	});

	it("lane + address は lane の切替", () => {
		expect(activePaneRouteOf({ kind: "lane", pane_id: LANE })).toEqual({ to: "lane", address: LANE });
	});

	it("旧名 terminal も lane の別名として lane の切替になる", () => {
		expect(activePaneRouteOf({ kind: "terminal", pane_id: LANE })).toEqual({
			to: "lane",
			address: LANE,
		});
	});

	it("address の無い lane は lane pane の訪問（lane が無い時の placeholder が出る）", () => {
		expect(activePaneRouteOf({ kind: "lane" })).toEqual({ to: "visit", paneId: "lane" });
	});

	it("lane / empty 以外の pane は、表の kind どれで来てもその pane の訪問", () => {
		for (const p of APP_PANE_REGISTRY) {
			if (p.id === "lane" || p.id === "empty") continue;
			for (const kind of p.kinds) {
				expect(activePaneRouteOf({ kind }), kind).toEqual({ to: "visit", paneId: p.id });
			}
		}
	});

	it("表に無い kind（退役した board 等）は unknown — 呼び手が empty に倒して警告する", () => {
		expect(activePaneRouteOf({ kind: "board" })).toEqual({ to: "unknown", kind: "board" });
	});
});

describe("applyPaneSwitch — DOM の可視性（main_area.rs の静的 pane）", () => {
	let shown: { addr: string | null; isChat: boolean }[];

	beforeEach(() => {
		document.body.innerHTML = body;
		shown = [];
		(window as unknown as { showLane: (a: string | null, c: boolean) => void }).showLane = (
			addr,
			isChat,
		) => shown.push({ addr, isChat });
	});

	const active = () =>
		Array.from(document.querySelectorAll(".pane.active")).map((el) => el.id);

	it("表の kind は全部、その pane の DOM 要素 1 枚だけを active にする", () => {
		for (const p of APP_PANE_REGISTRY) {
			for (const kind of p.kinds) {
				applyPaneSwitch({ kind, pane_id: p.id === "lane" ? LANE : null });
				expect(active(), kind).toEqual([p.domId]);
			}
		}
	});

	it("kind 無し・表に無い kind は empty の pane に落ちる", () => {
		applyPaneSwitch(null);
		expect(active()).toEqual(["pane-empty"]);
		applyPaneSwitch({ kind: "board" });
		expect(active()).toEqual(["pane-empty"]);
	});

	it("⚠️ 旧名 terminal でも lane の xterm を出す（showLane に address が届く）", () => {
		applyPaneSwitch({ kind: "terminal", pane_id: LANE, chat: true });
		expect(active()).toEqual(["pane-lane"]);
		expect(shown).toEqual([{ addr: LANE, isChat: true }]);
		expect(document.getElementById("pane-lane")!.getAttribute("data-pane-id")).toBe(LANE);
	});

	it("preview は iframe に URL を載せる", () => {
		applyPaneSwitch({ kind: "preview", preview_url: "http://localhost:3000/" });
		expect(document.getElementById("preview-frame")!.getAttribute("src")).toBe("http://localhost:3000/");
	});

	it("⚠️ 配置の住所 data-app-pane は書き換えない（VP-141 — data-pane-id とは別の属性）", () => {
		applyPaneSwitch({ kind: "lane", pane_id: LANE });
		expect(document.getElementById("pane-lane")!.getAttribute("data-app-pane")).toBe("lane");
	});
});
