// @vitest-environment happy-dom
/**
 * 額縁 = Principal Layout の 3x3（doc 87 §2 / §7 / §8 slices 1・4; mem_1ChP3kQTUGaY6sF5fNDDj7）— 実物の静的 DOM で確かめる。
 *
 * 額縁は Rust の静的 HTML（`src/webview/main_area.rs` の `MAIN_AREA_HTML`）にある。ここでは
 * その `<body>` と app の `<style>` を**ファイルからそのまま**読み込み、region の名札と
 * pane の住所が揃っていることを検査する（写しを持たない — 写しは本物が変わっても古いまま残る）。
 *
 * ## ⚠️ 守るもの
 *
 * - region は位置語彙の `data-region`（PL-2）で名乗る。`vp shot` / DOM 検査で 3x3 が読める
 *   （AI の address、doc 51）
 * - 各スケールで中身の無い edgeは **DOM を描かない**（PL: CUEdgeShell の `<Show>` と同じ）
 * - `#host` の中身は動かさない（doc 49 R1 — xterm は再生成できない）。額縁の改修は属性と CSS だけ
 * - 旧 frame-engine の語彙 `data-frame-id` は残っていない（doc 87 §1.1）
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { APP_PANE_REGISTRY } from "./app-pane-registry";
import { _resetForTest, applyAppScene, installAppPanes } from "./app-panes";

// ⚠️ happy-dom 環境では global の URL が happy-dom 製になり readFileSync が受け付けないので、
// 文字列の import.meta.url から path を組む。
const here = dirname(fileURLToPath(import.meta.url));
const rs = readFileSync(join(here, "../src/webview/main_area.rs"), "utf8");
const body = rs.match(/<body>([\s\S]*?)<\/body>/)![1];
/** app 側の `<style>`（xterm.css の直後に続く raw string — `html,body{` から始まる塊）。 */
const appCss = rs.match(/r#"\n(html,body\{[\s\S]*?)<\/style>/)![1];

let unsubscribe: (() => void) | null = null;

beforeEach(() => {
	document.body.className = "";
	document.head.innerHTML = `<style>${appCss}</style>`;
	// bundle の <script> は読み込まない（DOM と CSS だけを見る）
	document.body.innerHTML = body.replace(/<script[\s\S]*?<\/script>/g, "");
	_resetForTest();
});

afterEach(() => {
	unsubscribe?.();
	unsubscribe = null;
});

const region = (name: string) => document.querySelectorAll(`#app-shell > [data-region="${name}"]`);

describe("3x3 の region（PL-2 位置語彙）", () => {
	it("leftEdge = #sidebar-root / center = #host / rightEdge = #right-sidebar", () => {
		expect(Array.from(region("leftEdge")).map((e) => e.id)).toEqual(["sidebar-root"]);
		expect(Array.from(region("center")).map((e) => e.id)).toEqual(["host"]);
		expect(Array.from(region("rightEdge")).map((e) => e.id)).toEqual(["right-sidebar"]);
	});

	it("中身の無い edge は描かない（upEdge は grid の track だけ、downEdge は無し）", () => {
		expect(region("upEdge")).toHaveLength(0);
		expect(region("downEdge")).toHaveLength(0);
	});

	it("#app-shell は CUEdgeShell と同じ area 名の grid（up / left / center / right / down）", () => {
		const shell = document.getElementById("app-shell")!;
		const style = getComputedStyle(shell);
		expect(style.display).toBe("grid");
		const areas = style.gridTemplateAreas.replace(/\s+/g, " ");
		for (const name of ["up", "left", "center", "right", "down"]) {
			expect(areas, `${name} の area が無い`).toMatch(new RegExp(`\\b${name}\\b`));
		}
	});

	it("各 region が自分の area に載る", () => {
		const area = (id: string) => getComputedStyle(document.getElementById(id)!).gridArea;
		expect(area("sidebar-root")).toMatch(/^left\b/);
		expect(area("host")).toMatch(/^center\b/);
		expect(area("right-sidebar")).toMatch(/^right\b/);
		// 境界の取っ手は region ではないが、専用の track に載る（無いと自動配置で up の行に落ちる）
		expect(area("shell-resizer-left")).toMatch(/^left-handle\b/);
		expect(area("shell-resizer-right")).toMatch(/^right-handle\b/);
	});

	it("⚠️ #host の中身（pane 群）は #host の直下のまま（doc 49 R1）", () => {
		const host = document.getElementById("host")!;
		for (const { domId } of APP_PANE_REGISTRY) {
			expect(document.getElementById(domId)!.parentElement, domId).toBe(host);
		}
		expect(document.getElementById("lane-panes")!.parentElement!.id).toBe("pane-lane");
	});
});

describe("lane の 3x3 額縁（app の center 内で自己相似）", () => {
	const el = (id: string) => document.getElementById(id)!;
	const style = (id: string) => getComputedStyle(el(id));

	it("lane の直下だけが upEdge / center / rightEdge を名乗り、空の left / down は描かない", () => {
		expect(Array.from(document.querySelectorAll("#pane-lane > [data-region]"))
			.map((node) => [node.id, node.getAttribute("data-region")])).toEqual([
			["lane-header", "upEdge"], ["lane-panes", "center"], ["edge-rail", "rightEdge"],
		]);
		expect(el("pane-lane").parentElement).toBe(el("host"));
	});

	it("lane の grid は右レールを全高に保ち、空の edge は auto track で畳む", () => {
		expect(style("pane-lane").display).toBe("grid");
		expect(style("pane-lane").gridTemplateAreas.replace(/\s+/g, " "))
			.toBe('"up up right" "left center right" "down down right"');
		expect(style("pane-lane").gridTemplateColumns).toBe("auto minmax(0,1fr) auto");
		expect(style("pane-lane").gridTemplateRows).toBe("auto minmax(0,1fr) auto");
		for (const [id, area] of [["lane-header", "up"], ["lane-panes", "center"], ["edge-rail", "right"]]) {
			expect(style(id).gridArea, id).toBe(area);
			expect(style(id).position, id).not.toBe("absolute");
		}
		expect(style("lane-panes").position).toBe("relative");
		expect(style("lane-panes").minWidth).toBe("0");
		expect(style("lane-panes").minHeight).toBe("0");
	});

	it("header は lane がある時だけ共有の名札の高さを持つ", () => {
		expect(style("lane-header").height).toBe("0px");
		el("pane-lane").classList.add("lane-header-active");
		expect(style("lane-header").height).toBe("28px");
		el("pane-lane").style.setProperty("--vp-nameplate-h", "42px");
		expect(style("lane-header").height).toBe("42px");
		el("pane-lane").classList.remove("lane-header-active");
		expect(style("lane-header").height).toBe("0px");
	});

	it.each(["lane 無し", "lane 有り", "R 開", "R 閉"])("%s: rail の表示だけで auto track を切り替え、手書き退避を持たない", (state) => {
		const rail = el("edge-rail");
		if (state !== "lane 無し") rail.style.display = "";
		if (state === "R 開" || state === "R 閉") document.body.classList.add("rsb-open");
		if (state === "R 閉") document.body.classList.remove("rsb-open");
		expect(style("edge-rail").display).toBe(state === "lane 無し" || state === "R 開" ? "none" : "flex");
		for (const id of ["lane-header", "lane-panes"]) {
			// happy-dom は track の px 計算をしない。ここは退避を持たず grid に任せる契約を検査。
			expect(["", "auto"]).toContain(style(id).right);
			expect(["", "auto"]).toContain(style(id).top);
		}
		expect(el("lane-panes").parentElement).toBe(el("pane-lane"));
		for (const id of ["lane-board", "lane-code"]) {
			expect(el(id).parentElement, id).toBe(el("lane-panes"));
		}
	});

	it("switching / empty は center に重なり、header と rail の track を覆わない", () => {
		for (const id of ["console-switching", "lane-empty"]) {
			el(id).classList.add("active");
			expect(style(id).gridArea, id).toBe("center");
			expect(style(id).position, id).toBe("absolute");
			expect(style(id).inset, id).toBe("0");
			expect(el(id).parentElement).toBe(el("pane-lane"));
		}
	});
});

describe("pane の住所（旧 data-frame-id → data-app-pane）", () => {
	it("旧語彙 data-frame-id は残っていない", () => {
		// 改名の経緯はコメントに残してよい — 生きた属性と CSS セレクタに残っていないことを見る
		expect(document.querySelectorAll("[data-frame-id]")).toHaveLength(0);
		expect(appCss.replace(/\/\*[\s\S]*?\*\//g, "")).not.toContain("data-frame-id");
	});

	// 住所と pane kind registry の 1:1 は shell-registries.dom.test.ts（3 つの registry を並べて固定）

	it("配置の投影は data-app-pane を引く（lane-focus で lane だけが見える）", () => {
		unsubscribe = installAppPanes(document);
		expect(applyAppScene("lane-focus")).toBe(true);
		const visible = Array.from(document.querySelectorAll<HTMLElement>(".pane[data-app-pane]"))
			.filter((el) => el.style.visibility === "visible")
			.map((el) => el.dataset.appPane);
		expect(visible).toEqual(["lane"]);
		expect(document.getElementById("pane-lane")!.classList.contains("active")).toBe(true);
	});
});
