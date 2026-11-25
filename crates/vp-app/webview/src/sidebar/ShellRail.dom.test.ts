/**
 * 左 edge の Rail（doc 87 §2 / §8 slice 1）— creo-ui `CURail`（0.31.0〜）を VP に繋ぐ薄い wrapper。
 *
 * ## ⚠️ ここで守るのは「VP が頼っている DOM 契約」
 *
 * 0.30.0 の時点では CURail が VP の左 edge を表せず、同じ DOM 契約の仮実装を置いていた
 * （doc 87 §8.1）。要望が 0.31.0 に入ったので本物に差し替えた。契約はこのテストが引き続き固定する
 * — creo-ui 側が形を変えたら、VP の CSS / directive / AI の address が頼る所がここで落ちる。
 *
 * creo-ui 0.30.0 の `CURail` は peek 幅 240px 固定（doc 87 §6 要望②）・slim（選んだまま peek だけ畳む）不可・
 * 列に registry の icon 以外を置けない、の 3 点で VP の左 edge を表せない（後の 2 点は slice 1 の実装で
 * 見つけた追加の要望候補 — doc 87 §8.1）。
 * そこで VP 側に仮実装を置くが、**DOM の契約は CURail と同じ**にする:
 *
 * - root = `[data-region="leftEdge"]`
 * - 列 = `nav[aria-label="Rail"]`、icon = `button[data-rail]`、選択中だけ `aria-current="page"`
 * - peek = `[data-expanded]`
 *
 * 要望が creo-ui に着地したら CURail に差し替える。その時に AI / テスト / CSS が DOM を
 * 引き直さずに済むよう、契約をここで固定する。
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { build } from "esbuild";
import { solidPlugin } from "esbuild-plugin-solid";
import { Window } from "happy-dom";
import { fileURLToPath } from "node:url";

type Probe = Window & {
	clicked: string[];
	setPath(p: string): void;
	setPeekOpen(v: boolean): void;
};

let bundle: string;
const windows: Window[] = [];

beforeAll(async () => {
	const result = await build({
		stdin: {
			contents: `
        import { render } from 'solid-js/web';
        import { createComponent, createSignal } from 'solid-js';
        import { ShellRail, SHELL_RAIL_CSS } from './ShellRail';
        import shellsCss from '@chronista-club/creo-ui/shells/index.css';
        // 本番と同じ順（creo-ui の shells → VP の上書き、sidebar.tsx）で CSS を当てる
        const style = document.createElement('style');
        style.textContent = [shellsCss, SHELL_RAIL_CSS].join(' ');
        document.head.append(style);
        const [path, setPath] = createSignal('/lanes');
        const [peekOpen, setPeekOpen] = createSignal(true);
        window.setPath = setPath;
        window.setPeekOpen = setPeekOpen;
        window.clicked = [];
        const icon = (id) => () => { const s = document.createElement('i'); s.className = 'icon-' + id; return s; };
        const peek = (id) => () => { const d = document.createElement('div'); d.className = 'peek-' + id; d.textContent = id; return d; };
        const rails = [
          { id: 'settings', route: '/settings', order: 2, labelKey: '設定', anchor: 'bottom', icon: icon('settings'), peek: peek('settings') },
          { id: 'lanes', route: '/lanes', order: 0, labelKey: 'Lanes', anchor: 'top', icon: icon('lanes'), peek: peek('lanes') },
          { id: 'actions', route: '/actions', order: 1, labelKey: 'Actions', anchor: 'top', icon: icon('actions'), peek: peek('actions') },
        ];
        render(() => createComponent(ShellRail, {
          registry: rails,
          get pathname() { return path(); },
          get peekOpen() { return peekOpen(); },
          onSelect: (id) => window.clicked.push(id),
          get columnCenter() { const d = document.createElement('div'); d.className = 'slot-center'; return d; },
          get columnBottom() { const d = document.createElement('div'); d.className = 'slot-bottom'; return d; },
          get peekFooter() { const d = document.createElement('div'); d.className = 'slot-footer'; return d; },
        }), document.getElementById('sidebar-root'));
      `,
			resolveDir: fileURLToPath(new URL(".", import.meta.url)),
			loader: "tsx",
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		preserveSymlinks: true,
		plugins: [solidPlugin()],
		loader: { ".css": "text" },
		define: { "process.env.NODE_ENV": '"production"' },
	});
	bundle = result.outputFiles[0].text;
});

function fixture(): Probe {
	const win = new Window({
		settings: {
			enableJavaScriptEvaluation: true,
			suppressInsecureJavaScriptEnvironmentWarning: true,
		},
	}) as unknown as Probe;
	windows.push(win);
	// 本番と同じ mount 先（VP の上書き CSS は #sidebar-root 配下に効く）
	win.document.body.innerHTML = '<div id="sidebar-root"></div>';
	win.eval(bundle);
	return win;
}

afterEach(async () => {
	for (const win of windows.splice(0)) await win.happyDOM.abort();
});

/** happy-dom の NodeList は DOM lib の Iterable<Element> と型が合わないので、読む口だけを要求する。 */
const ids = (els: { length: number; [i: number]: { getAttribute(n: string): string | null } }) =>
	Array.from({ length: els.length }, (_, i) => els[i].getAttribute("data-rail"));

describe("DOM 契約（CURail と同じ形）", () => {
	it("leftEdge の中に Rail の nav と peek が並ぶ", () => {
		const win = fixture();
		const root = win.document.querySelector('[data-region="leftEdge"]')!;
		expect(root).not.toBeNull();
		expect(root.querySelector(':scope > nav[aria-label="Rail"]')).not.toBeNull();
		expect(root.querySelector(":scope > [data-expanded]")).not.toBeNull();
	});

	it("icon は order 順、下起点の rail は列の下端の slot に入る", () => {
		const win = fixture();
		const nav = win.document.querySelector('nav[aria-label="Rail"]')!;
		expect(ids(nav.querySelectorAll("button[data-rail]"))).toEqual([
			"lanes",
			"actions",
			"settings",
		]);
		expect(ids(nav.querySelectorAll(".anchor-top button[data-rail]"))).toEqual([
			"lanes",
			"actions",
		]);
		expect(ids(nav.querySelectorAll(".anchor-bottom button[data-rail]"))).toEqual([
			"settings",
		]);
	});

	it("label は aria-label と title に出る（icon だけの列でも読める）", () => {
		const win = fixture();
		const gear = win.document.querySelector('button[data-rail="settings"]')!;
		expect(gear.getAttribute("aria-label")).toBe("設定");
		expect(gear.getAttribute("title")).toBe("設定");
		expect(gear.querySelector(".icon-settings")).not.toBeNull();
	});
});

describe("選択 = route（PL-4）", () => {
	it("route が選ぶ rail だけ aria-current=page", () => {
		const win = fixture();
		const current = () =>
			ids(win.document.querySelectorAll('button[data-rail][aria-current="page"]'));
		expect(current()).toEqual(["lanes"]);
		win.setPath("/settings/updates");
		expect(current()).toEqual(["settings"]);
		win.setPath("/nowhere");
		expect(current()).toEqual([]);
	});

	it("click は選択を自分で変えず、呼び手に rail を渡すだけ", () => {
		const win = fixture();
		(win.document.querySelector('button[data-rail="actions"]') as unknown as HTMLElement).click();
		expect(win.clicked).toEqual(["actions"]);
		expect(
			win.document.querySelector('button[data-rail="lanes"]')!.getAttribute("aria-current"),
		).toBe("page");
	});
});

describe("peek（選択中 rail の中身）", () => {
	const visiblePeeks = (win: Probe) =>
		Array.from(win.document.querySelectorAll("[data-peek]"))
			.filter((el) => !el.hasAttribute("hidden"))
			.map((el) => el.getAttribute("data-peek"));

	it("開いている間は選択中の rail の中身だけが見える", () => {
		const win = fixture();
		const peek = win.document.querySelector("[data-expanded]")!;
		expect(peek.getAttribute("data-expanded")).toBe("true");
		expect(visiblePeeks(win)).toEqual(["lanes"]);
		win.setPath("/actions");
		expect(visiblePeeks(win)).toEqual(["actions"]);
	});

	it("⚠️ 選択外の peek は CSS でも消えている（hidden 属性だけでなく、表示が none）", () => {
		// peek の section は author CSS で display:flex を持ち、UA の `[hidden]{display:none}` に勝つ。
		// creo-ui の `[hidden]` の規則か、それを VP の上書きが打ち消すと、3 枚が縦に並んだまま
		// 属性だけ正しい状態になる（属性だけ見るテストでは落ちない）
		const win = fixture();
		const display = (id: string) =>
			win.getComputedStyle(win.document.querySelector(`[data-peek="${id}"]`)!).display;
		expect(display("lanes")).not.toBe("none");
		expect(display("actions")).toBe("none");
		expect(display("settings")).toBe("none");
	});

	it("⚠️ rail を移っても他の peek は mount されたまま（展開状態・入力中の form を失わない）", () => {
		const win = fixture();
		const lanes = win.document.querySelector(".peek-lanes");
		win.setPath("/actions");
		win.setPath("/lanes");
		expect(win.document.querySelector(".peek-lanes")).toBe(lanes);
	});

	it("slim（peekOpen=false）では選んだまま peek を畳む", () => {
		const win = fixture();
		win.setPeekOpen(false);
		const peek = win.document.querySelector("[data-expanded]")!;
		expect(peek.getAttribute("data-expanded")).toBe("false");
		expect(win.document.querySelector(".peek-lanes")).toBeNull();
		expect(
			win.document.querySelector('button[data-rail="lanes"]')!.getAttribute("aria-current"),
		).toBe("page");
	});

	it("どの rail も選ばれていなければ peek は閉じる（CURail と同じ）", () => {
		const win = fixture();
		win.setPath("/nowhere");
		expect(
			win.document.querySelector("[data-expanded]")!.getAttribute("data-expanded"),
		).toBe("false");
	});
});

describe("列の追加 slot（creo-ui への要望 — slim の repo badge 列と daemon dot の置き場）", () => {
	it("中央と下端に consumer の要素を置ける", () => {
		const win = fixture();
		const nav = win.document.querySelector('nav[aria-label="Rail"]')!;
		expect(nav.querySelector(".anchor-center .slot-center")).not.toBeNull();
		// 下端の slot は下起点の rail（歯車）より上 = 歯車が最下端に残る
		const bottom = nav.querySelector(".anchor-bottom")!;
		const order = Array.from(bottom.children).map(
			(el) => el.getAttribute("data-rail") ?? el.className,
		);
		expect(order).toEqual(["slot-bottom", "settings"]);
	});
});

describe("peek の下端 slot（machine 帯の置き場 — どの rail でも同じ位置に残る）", () => {
	it("開いている間は peek の最下端に出て、rail を移っても同じ要素のまま", () => {
		const win = fixture();
		const peek = win.document.querySelector("[data-expanded]")!;
		const footer = peek.querySelector(":scope > .slot-footer");
		expect(footer).not.toBeNull();
		expect(peek.lastElementChild).toBe(footer);
		win.setPath("/actions");
		expect(peek.querySelector(":scope > .slot-footer")).toBe(footer);
	});

	it("slim では描かない", () => {
		const win = fixture();
		win.setPeekOpen(false);
		expect(win.document.querySelector(".slot-footer")).toBeNull();
	});
});

describe("drag 中は peek の幅の transition を止める（creo-ui の resizing）", () => {
	it("shell の取っ手を掴んでいる間（body.shell-resizing）だけ data-resizing が付く", async () => {
		const win = fixture();
		const peek = () => win.document.querySelector("[data-expanded]")!;
		expect(peek().getAttribute("data-resizing")).toBeNull();
		win.document.body.classList.add("shell-resizing");
		await new Promise((r) => win.setTimeout(r, 0));
		expect(peek().getAttribute("data-resizing")).toBe("true");
		win.document.body.classList.remove("shell-resizing");
		await new Promise((r) => win.setTimeout(r, 0));
		expect(peek().getAttribute("data-resizing")).toBeNull();
	});

	it("peek の幅は VP の保存値（--vp-peek-w）を creo-ui の口に渡す", () => {
		const win = fixture();
		const root = win.document.querySelector('[data-region="leftEdge"]') as unknown as {
			style: { getPropertyValue(n: string): string };
		};
		expect(root.style.getPropertyValue("--cu-rail-peek-width")).toContain("--vp-peek-w");
	});
});
